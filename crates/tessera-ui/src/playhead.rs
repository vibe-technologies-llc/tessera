use std::{
    num::NonZeroU32,
    time::{Duration, Instant},
};

use gpui::{Context, Entity, Task};
use tessera_audio::TimelinePlayback;
use tessera_timeline::{FrameRate, Project, Time};

const MAX_SHUTTLE_SPEED: i64 = 8;
const MIN_TICK: Duration = Duration::from_micros(8_333);
const AUDIO_STALL_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Speed(i64);

impl Speed {
    pub const PAUSED: Self = Self(0);
    pub const FORWARD: Self = Self(1);
    pub const BACKWARD: Self = Self(-1);

    pub fn factor(self) -> i64 {
        self.0
    }

    pub fn is_paused(self) -> bool {
        self == Self::PAUSED
    }

    fn toggled(self) -> Self {
        if self.is_paused() {
            Self::FORWARD
        } else {
            Self::PAUSED
        }
    }

    fn faster_forward(self) -> Self {
        if self.0 <= 0 {
            Self::FORWARD
        } else {
            Self((self.0 * 2).min(MAX_SHUTTLE_SPEED))
        }
    }

    fn faster_backward(self) -> Self {
        if self.0 >= 0 {
            Self::BACKWARD
        } else {
            Self((self.0 * 2).max(-MAX_SHUTTLE_SPEED))
        }
    }

    fn tick(self, frame_rate: FrameRate) -> Duration {
        let speed = u32::try_from(self.0.unsigned_abs())
            .unwrap_or(u32::MAX)
            .max(1);
        (frame_rate.frame_duration().to_duration() / speed).max(MIN_TICK)
    }
}

struct Playback {
    speed: Speed,
    from: Time,
    sample_rate: NonZeroU32,
    clock: PlaybackClock,
    _ticker: Task<()>,
}

enum PlaybackClock {
    Wall { started: Instant, offset: Time },
    Audio(Box<WatchedAudio<TimelinePlayback>>),
}

trait ElapsedSource {
    fn elapsed(&self) -> Time;
}

impl ElapsedSource for TimelinePlayback {
    fn elapsed(&self) -> Time {
        TimelinePlayback::elapsed(self)
    }
}

struct WatchedAudio<S> {
    source: S,
    last: Time,
    changed_at: Instant,
}

impl<S: ElapsedSource> WatchedAudio<S> {
    fn new(source: S, now: Instant) -> Self {
        Self {
            source,
            last: Time::ZERO,
            changed_at: now,
        }
    }

    fn poll(&mut self, now: Instant) -> Result<Time, Time> {
        let elapsed = self.source.elapsed();
        if elapsed != self.last {
            self.last = elapsed;
            self.changed_at = now;
            return Ok(elapsed);
        }
        let silent_for = now.saturating_duration_since(self.changed_at);
        if silent_for > AUDIO_STALL_AFTER {
            Err(self.last + Time::from_duration(silent_for))
        } else {
            Ok(elapsed)
        }
    }
}

impl PlaybackClock {
    fn wall() -> Self {
        Self::Wall {
            started: Instant::now(),
            offset: Time::ZERO,
        }
    }

    fn start(speed: Speed, project: &Project, from: Time) -> Self {
        if speed != Speed::FORWARD {
            return Self::wall();
        }
        match TimelinePlayback::start(project.clone(), from) {
            Ok(audio) => Self::Audio(Box::new(WatchedAudio::new(audio, Instant::now()))),
            Err(error) => {
                tracing::warn!(%error, "no audio output, playback follows the wall clock");
                Self::wall()
            }
        }
    }

    fn elapsed(&mut self) -> Time {
        match self {
            Self::Wall { started, offset } => *offset + Time::from_duration(started.elapsed()),
            Self::Audio(audio) => match audio.poll(Instant::now()) {
                Ok(elapsed) => elapsed,
                Err(position) => {
                    tracing::warn!("the audio clock stopped, playback follows the wall clock");
                    *self = Self::Wall {
                        started: Instant::now(),
                        offset: position,
                    };
                    position
                }
            },
        }
    }
}

enum Advance {
    To(Time),
    StopAt(Time),
}

pub struct Playhead {
    project: Entity<Project>,
    time: Time,
    playback: Option<Playback>,
}

impl Playhead {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        cx.observe(&project, |playhead, project, cx| {
            playhead.project_changed(project.read(cx).clone());
        })
        .detach();
        Self {
            project,
            time: Time::ZERO,
            playback: None,
        }
    }

    fn project_changed(&mut self, project: Project) {
        let time = self.time;
        let Some(playback) = &mut self.playback else {
            return;
        };
        let sample_rate = project.settings.sample_rate;
        let audible = matches!(playback.clock, PlaybackClock::Audio(_));
        if output_needs_restart(playback.sample_rate, sample_rate, audible) {
            playback.clock = PlaybackClock::start(playback.speed, &project, time);
            playback.from = time;
            playback.sample_rate = sample_rate;
        } else if let PlaybackClock::Audio(audio) = &mut playback.clock {
            audio.source.update_project(project);
        }
    }

    pub fn time(&self) -> Time {
        self.time
    }

    pub fn speed(&self) -> Speed {
        self.playback
            .as_ref()
            .map_or(Speed::PAUSED, |playback| playback.speed)
    }

    pub fn seek(&mut self, time: Time, cx: &mut Context<Self>) {
        self.play_at(Speed::PAUSED, cx);
        self.move_to(time, cx);
    }

    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        self.play_at(self.speed().toggled(), cx);
    }

    pub fn shuttle_forward(&mut self, cx: &mut Context<Self>) {
        self.play_at(self.speed().faster_forward(), cx);
    }

    pub fn shuttle_backward(&mut self, cx: &mut Context<Self>) {
        self.play_at(self.speed().faster_backward(), cx);
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        self.play_at(Speed::PAUSED, cx);
    }

    pub fn step(&mut self, frames: i64, cx: &mut Context<Self>) {
        let frame_rate = self.frame_rate(cx);
        let frame = frame_rate.time_to_frame(self.time) + frames;
        self.seek(frame_rate.frame_to_time(frame), cx);
    }

    fn frame_rate(&self, cx: &Context<Self>) -> FrameRate {
        self.project.read(cx).settings.frame_rate
    }

    fn move_to(&mut self, time: Time, cx: &mut Context<Self>) {
        let time = time.max(Time::ZERO);
        if time != self.time {
            self.time = time;
            cx.notify();
        }
    }

    fn play_at(&mut self, speed: Speed, cx: &mut Context<Self>) {
        if speed == self.speed() {
            return;
        }
        let was_playing = self.playback.take().is_some();
        let project = self.project.read(cx);
        let frame_rate = project.settings.frame_rate;
        let last = last_frame(project.timeline.duration(), frame_rate);
        let start = match last {
            Some(last) if speed.factor() > 0 && self.time >= last => Some(Time::ZERO),
            Some(_) if speed.factor() < 0 && self.time <= Time::ZERO => None,
            Some(_) if !speed.is_paused() => Some(self.time),
            _ => None,
        };
        if let Some(start) = start {
            self.move_to(start, cx);
            let clock = PlaybackClock::start(speed, self.project.read(cx), start);
            self.playback = Some(Playback {
                speed,
                from: start,
                sample_rate: self.project.read(cx).settings.sample_rate,
                clock,
                _ticker: self.spawn_ticker(speed.tick(frame_rate), cx),
            });
            cx.notify();
        } else if was_playing {
            cx.notify();
        }
    }

    fn spawn_ticker(&self, interval: Duration, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                let Ok(true) = this.update(cx, |playhead, cx| playhead.tick(cx)) else {
                    break;
                };
            }
        })
    }

    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        let project = self.project.read(cx);
        let frame_rate = project.settings.frame_rate;
        let end = project.timeline.duration();
        let Some(playback) = &mut self.playback else {
            return false;
        };
        if end <= Time::ZERO {
            self.pause(cx);
            return false;
        }
        let elapsed = playback.clock.elapsed();
        match advance(playback.from, elapsed, playback.speed, frame_rate, end) {
            Advance::To(time) => {
                self.move_to(time, cx);
                true
            }
            Advance::StopAt(time) => {
                self.pause(cx);
                self.move_to(time, cx);
                false
            }
        }
    }
}

fn output_needs_restart(playing_at: NonZeroU32, wanted: NonZeroU32, audible: bool) -> bool {
    audible && playing_at != wanted
}

pub(crate) fn last_frame(end: Time, frame_rate: FrameRate) -> Option<Time> {
    (end > Time::ZERO).then(|| frame_rate.frame_start(end - Time::from_flicks(1)))
}

fn advance(from: Time, elapsed: Time, speed: Speed, frame_rate: FrameRate, end: Time) -> Advance {
    let exact = from + elapsed * speed.factor();
    let time = frame_rate.frame_start(exact);
    let last = frame_rate.frame_start(end - Time::from_flicks(1));
    if speed.factor() > 0 && exact >= end {
        Advance::StopAt(last)
    } else if speed.factor() < 0 && time <= Time::ZERO {
        Advance::StopAt(Time::ZERO)
    } else {
        Advance::To(time.min(last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuttle_doubles_up_to_the_limit_and_reverses_from_normal_speed() {
        let forward: Vec<_> =
            std::iter::successors(Some(Speed::PAUSED), |speed| Some(speed.faster_forward()))
                .skip(1)
                .take(5)
                .map(Speed::factor)
                .collect();
        assert_eq!(forward, [1, 2, 4, 8, 8]);
        assert_eq!(Speed(4).faster_backward(), Speed::BACKWARD);
        assert_eq!(Speed(-2).faster_backward(), Speed(-4));
        assert_eq!(Speed(-8).faster_backward(), Speed(-8));
        assert_eq!(Speed(-4).faster_forward(), Speed::FORWARD);
    }

    #[test]
    fn toggling_pauses_any_speed_and_resumes_at_normal_speed() {
        assert_eq!(Speed::PAUSED.toggled(), Speed::FORWARD);
        assert_eq!(Speed(-4).toggled(), Speed::PAUSED);
        assert_eq!(Speed(8).toggled(), Speed::PAUSED);
    }

    #[test]
    fn faster_speeds_tick_faster_down_to_a_floor() {
        let frame = FrameRate::FPS_30.frame_duration().to_duration();
        assert_eq!(Speed::FORWARD.tick(FrameRate::FPS_30), frame);
        assert_eq!(Speed(-2).tick(FrameRate::FPS_30), frame / 2);
        assert_eq!(Speed(8).tick(FrameRate::FPS_30), MIN_TICK);
    }

    #[test]
    fn last_frame_starts_before_the_timeline_end() {
        let rate = FrameRate::FPS_30;
        assert_eq!(last_frame(Time::ZERO, rate), None);
        assert_eq!(
            last_frame(Time::from_seconds(2), rate),
            Some(rate.frame_to_time(59))
        );
        let ragged = Time::from_seconds(2) + Time::from_flicks(1);
        assert_eq!(last_frame(ragged, rate), Some(rate.frame_to_time(60)));
    }

    #[test]
    fn playback_advances_by_wall_clock_times_speed_on_frame_starts() {
        let rate = FrameRate::FPS_30;
        let end = rate.frame_to_time(300);
        let from = rate.frame_to_time(30);
        let advanced = |millis, speed| {
            let elapsed = Time::from_duration(Duration::from_millis(millis));
            advance(from, elapsed, speed, rate, end)
        };
        assert!(matches!(advanced(0, Speed::FORWARD), Advance::To(time) if time == from));
        assert!(
            matches!(advanced(1_010, Speed::FORWARD), Advance::To(time) if time == rate.frame_to_time(60))
        );
        assert!(
            matches!(advanced(500, Speed(4)), Advance::To(time) if time == rate.frame_to_time(90))
        );
        assert!(
            matches!(advanced(500, Speed(-1)), Advance::To(time) if time == rate.frame_to_time(15))
        );
    }

    #[test]
    fn playback_plays_through_the_last_frame_and_stops_at_either_end() {
        let rate = FrameRate::FPS_30;
        let end = rate.frame_to_time(60);
        let last = rate.frame_to_time(59);
        let from = rate.frame_to_time(30);
        let stopped = |millis, speed| {
            let elapsed = Time::from_duration(Duration::from_millis(millis));
            advance(from, elapsed, speed, rate, end)
        };

        assert!(matches!(stopped(980, Speed::FORWARD), Advance::To(time) if time == last));
        assert!(matches!(stopped(1_000, Speed::FORWARD), Advance::StopAt(time) if time == last));
        assert!(
            matches!(stopped(2_000, Speed::BACKWARD), Advance::StopAt(time) if time == Time::ZERO)
        );
    }

    struct Scripted(std::cell::Cell<i64>);

    impl ElapsedSource for Scripted {
        fn elapsed(&self) -> Time {
            Time::from_seconds(self.0.get())
        }
    }

    #[test]
    fn an_audio_clock_that_stops_advancing_is_reported_with_the_position_to_continue_from() {
        let start = Instant::now();
        let at = |seconds: u64| start + Duration::from_secs(seconds);
        let mut clock = WatchedAudio::new(Scripted(std::cell::Cell::new(0)), start);

        assert_eq!(clock.poll(at(0)), Ok(Time::ZERO));

        clock.source.0.set(2);

        assert_eq!(clock.poll(at(2)), Ok(Time::from_seconds(2)));
        assert_eq!(clock.poll(at(3)), Ok(Time::from_seconds(2)));
        assert_eq!(clock.poll(at(4)), Err(Time::from_seconds(4)));
        assert_eq!(clock.poll(at(6)), Err(Time::from_seconds(6)));

        clock.source.0.set(7);

        assert_eq!(clock.poll(at(7)), Ok(Time::from_seconds(7)));
    }

    #[test]
    fn an_audio_clock_that_never_starts_falls_back_after_the_grace_period() {
        let start = Instant::now();
        let mut clock = WatchedAudio::new(Scripted(std::cell::Cell::new(0)), start);

        assert_eq!(clock.poll(start + AUDIO_STALL_AFTER / 2), Ok(Time::ZERO));
        assert_eq!(
            clock.poll(start + AUDIO_STALL_AFTER + Duration::from_secs(1)),
            Err(Time::from_seconds(2))
        );
    }

    #[test]
    fn only_a_playing_audio_output_restarts_for_a_new_sample_rate() {
        let rate = |hz| NonZeroU32::new(hz).unwrap();

        assert!(output_needs_restart(rate(48_000), rate(44_100), true));
        assert!(!output_needs_restart(rate(48_000), rate(48_000), true));
        assert!(!output_needs_restart(rate(48_000), rate(44_100), false));
    }
}
