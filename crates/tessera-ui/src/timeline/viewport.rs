use gpui::{Pixels, px};
use tessera_timeline::{FLICKS_PER_SECOND, Time};

const DEFAULT_PIXELS_PER_SECOND: f32 = 48.;
const MIN_PIXELS_PER_SECOND: f32 = 0.02;
const MAX_PIXELS_PER_SECOND: f32 = 2400.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pixels_per_second: f32,
    start: Time,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            pixels_per_second: DEFAULT_PIXELS_PER_SECOND,
            start: Time::ZERO,
        }
    }
}

impl Viewport {
    pub fn fitting(duration: Time, width: Pixels) -> Self {
        if duration <= Time::ZERO || width <= px(0.) {
            return Self::default();
        }
        let pixels_per_second = f32::from(width) / duration.as_seconds_f64() as f32;
        Self {
            pixels_per_second: pixels_per_second
                .clamp(MIN_PIXELS_PER_SECOND, MAX_PIXELS_PER_SECOND),
            start: Time::ZERO,
        }
    }

    pub fn pixels_per_second(self) -> f32 {
        self.pixels_per_second
    }

    pub fn start(self) -> Time {
        self.start
    }

    pub fn x_at(self, time: Time) -> Pixels {
        self.width_of(time - self.start)
    }

    pub fn width_of(self, duration: Time) -> Pixels {
        px(duration.as_seconds_f64() as f32 * self.pixels_per_second)
    }

    pub fn time_at(self, offset: Pixels) -> Time {
        self.start + self.duration_of(offset)
    }

    pub fn duration_of(self, width: Pixels) -> Time {
        let seconds = f64::from(f32::from(width)) / f64::from(self.pixels_per_second);
        Time::from_flicks((seconds * FLICKS_PER_SECOND as f64).round() as i64)
    }

    fn starting_at(self, start: Time, limit: Time) -> Self {
        Self {
            start: start.min(limit).max(Time::ZERO),
            ..self
        }
    }

    pub fn clamped(self, limit: Time) -> Self {
        self.starting_at(self.start, limit)
    }

    pub fn scrolled_by(self, delta: Pixels, limit: Time) -> Self {
        self.starting_at(self.time_at(delta), limit)
    }

    pub fn zoomed(self, factor: f32, anchor: Pixels, limit: Time) -> Self {
        let anchored = self.time_at(anchor);
        let zoomed = Self {
            pixels_per_second: (self.pixels_per_second * factor)
                .clamp(MIN_PIXELS_PER_SECOND, MAX_PIXELS_PER_SECOND),
            ..self
        };
        zoomed.starting_at(anchored - zoomed.duration_of(anchor), limit)
    }

    pub fn following(self, time: Time, width: Pixels, limit: Time) -> Self {
        let x = self.x_at(time);
        if x < px(0.) {
            self.starting_at(time - self.duration_of(width), limit)
        } else if x >= width {
            self.starting_at(time, limit)
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Viewport {
        Viewport::default().starting_at(Time::from_seconds(seconds), Time::from_seconds(3600))
    }

    const LIMIT: Time = Time::from_seconds(100);

    #[test]
    fn offsets_and_times_convert_both_ways_from_the_left_edge() {
        let viewport = at(10);
        let time = Time::from_seconds(13);
        assert_eq!(viewport.x_at(time), px(3. * DEFAULT_PIXELS_PER_SECOND));
        assert_eq!(viewport.time_at(viewport.x_at(time)), time);
        assert_eq!(
            viewport.time_at(px(DEFAULT_PIXELS_PER_SECOND / 2.)),
            Time::from_seconds(10) + Time::from_flicks(FLICKS_PER_SECOND / 2)
        );
        assert_eq!(
            viewport.width_of(Time::from_seconds(2)),
            px(2. * DEFAULT_PIXELS_PER_SECOND)
        );
    }

    #[test]
    fn scrolling_stays_between_zero_and_the_limit() {
        let viewport = at(10);
        let one_second = px(DEFAULT_PIXELS_PER_SECOND);
        assert_eq!(
            viewport.scrolled_by(one_second * 2., LIMIT).start(),
            Time::from_seconds(12)
        );
        assert_eq!(
            viewport.scrolled_by(one_second * -20., LIMIT).start(),
            Time::ZERO
        );
        assert_eq!(
            viewport.scrolled_by(one_second * 200., LIMIT).start(),
            LIMIT
        );
    }

    #[test]
    fn clamping_pulls_a_view_past_the_limit_back_to_it() {
        assert_eq!(at(40).clamped(Time::from_seconds(100)), at(40));
        assert_eq!(
            at(40).clamped(Time::from_seconds(25)).start(),
            Time::from_seconds(25)
        );
    }

    #[test]
    fn zooming_keeps_the_time_under_the_anchor() {
        let viewport = at(10);
        let anchor = px(240.);
        let zoomed = viewport.zoomed(2., anchor, LIMIT);
        assert_eq!(zoomed.pixels_per_second(), DEFAULT_PIXELS_PER_SECOND * 2.);
        assert_eq!(zoomed.time_at(anchor), viewport.time_at(anchor));
        let unzoomed = zoomed.zoomed(0.5, anchor, LIMIT);
        assert_eq!(unzoomed, viewport);
    }

    #[test]
    fn zoom_is_bounded() {
        let viewport = Viewport::default();
        assert_eq!(
            viewport.zoomed(1e9, px(0.), LIMIT).pixels_per_second(),
            MAX_PIXELS_PER_SECOND
        );
        assert_eq!(
            viewport.zoomed(1e-9, px(0.), LIMIT).pixels_per_second(),
            MIN_PIXELS_PER_SECOND
        );
        let zoomed_out = at(10).zoomed(0.1, px(480.), LIMIT);
        assert_eq!(zoomed_out.start(), Time::ZERO);
    }

    #[test]
    fn fitting_shows_the_whole_duration() {
        let fitted = Viewport::fitting(Time::from_seconds(20), px(960.));
        assert_eq!(fitted.start(), Time::ZERO);
        assert_eq!(fitted.pixels_per_second(), 48.);
        assert_eq!(Viewport::fitting(Time::ZERO, px(960.)), Viewport::default());
        assert_eq!(
            Viewport::fitting(Time::from_seconds(20), px(0.)),
            Viewport::default()
        );
    }

    #[test]
    fn following_pages_to_a_time_out_of_view() {
        let viewport = at(10);
        let width = px(10. * DEFAULT_PIXELS_PER_SECOND);
        let inside = Time::from_seconds(15);
        assert_eq!(viewport.following(inside, width, LIMIT), viewport);
        let past = Time::from_seconds(20);
        assert_eq!(viewport.following(past, width, LIMIT).start(), past);
        let before = Time::from_seconds(8);
        let paged_back = viewport.following(before, width, LIMIT);
        assert_eq!(paged_back.start(), Time::ZERO);
        let later = at(40).following(Time::from_seconds(35), width, LIMIT);
        assert_eq!(later.start(), Time::from_seconds(25));
    }
}
