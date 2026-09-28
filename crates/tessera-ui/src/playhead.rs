use std::time::{Duration, Instant};

use gpui::{Context, Entity, Task};
use tessera_audio::TimelinePlayback;
use tessera_timeline::{FrameRate, Project, Time};

const MAX_SHUTTLE_SPEED: i64 = 8;
const MIN_TICK: Duration = Duration::from_micros(8_333);

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
    clock: PlaybackClock,
    _ticker: Task<()>,
}

enum PlaybackClock {
    Wall(Instant),
    Audio(TimelinePlayback),
}

impl PlaybackClock {
    fn start(speed: Speed, project: &Project, from: Time) -> Self {
        if speed != Speed::FORWARD {
            return Self::Wall(Instant::now());
        }
        match TimelinePlayback::start(project.clone(), from) {
            Ok(audio) => Self::Audio(audio),
            Err(error) => {
                tracing::warn!(%error, "no audio output, playback follows the wall clock");
                Self::Wall(Instant::now())
            }
        }
    }

    fn elapsed(&self) -> Time {
        match self {
            Self::Wall(started) => Time::from_duration(started.elapsed()),
            Self::Audio(audio) => audio.elapsed(),
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
            if let Some(Playback {
                clock: PlaybackClock::Audio(audio),
                ..
            }) = &playhead.playback
            {
                audio.update_project(project.read(cx).clone());
            }
        })
        .detach();
        Self {
            project,
            time: Time::ZERO,
            playback: None,
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
        let Some(playback) = &self.playback else {
            return false;
        };
        let project = self.project.read(cx);
        let frame_rate = project.settings.frame_rate;
        let Some(last) = last_frame(project.timeline.duration(), frame_rate) else {
            self.pause(cx);
            return false;
        };
        let elapsed = playback.clock.elapsed();
        match advance(playback.from, elapsed, playback.speed, frame_rate, last) {
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

pub(crate) fn last_frame(end: Time, frame_rate: FrameRate) -> Option<Time> {
    (end > Time::ZERO).then(|| frame_rate.frame_start(end - Time::from_flicks(1)))
}

fn advance(from: Time, elapsed: Time, speed: Speed, frame_rate: FrameRate, last: Time) -> Advance {
    let time = frame_rate.frame_start(from + elapsed * speed.factor());
    if time >= last {
        Advance::StopAt(last)
    } else if time <= Time::ZERO && speed.factor() < 0 {
        Advance::StopAt(Time::ZERO)
    } else {
        Advance::To(time)
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
        let last = rate.frame_to_time(299);
        let from = rate.frame_to_time(30);
        let advanced = |millis, speed| {
            let elapsed = Time::from_duration(Duration::from_millis(millis));
            advance(from, elapsed, speed, rate, last)
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
    fn playback_stops_at_either_end() {
        let rate = FrameRate::FPS_30;
        let last = rate.frame_to_time(59);
        let from = rate.frame_to_time(30);
        let stopped = |millis, speed| {
            let elapsed = Time::from_duration(Duration::from_millis(millis));
            advance(from, elapsed, speed, rate, last)
        };
        assert!(matches!(stopped(1_000, Speed::FORWARD), Advance::StopAt(time) if time == last));
        assert!(
            matches!(stopped(2_000, Speed::BACKWARD), Advance::StopAt(time) if time == Time::ZERO)
        );
    }
}
