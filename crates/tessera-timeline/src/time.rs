use std::{
    ops::{Add, Sub},
    time::Duration,
};

pub const FLICKS_PER_SECOND: i64 = 705_600_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time(i64);

impl Time {
    pub const ZERO: Self = Self(0);

    pub const fn from_flicks(flicks: i64) -> Self {
        Self(flicks)
    }

    pub const fn flicks(self) -> i64 {
        self.0
    }

    pub const fn from_seconds(seconds: i64) -> Self {
        Self(seconds * FLICKS_PER_SECOND)
    }

    pub fn from_rational(numerator: i64, denominator: i64) -> Self {
        let flicks =
            i128::from(numerator) * i128::from(FLICKS_PER_SECOND) / i128::from(denominator);
        Self(flicks as i64)
    }

    pub fn as_seconds_f64(self) -> f64 {
        self.0 as f64 / FLICKS_PER_SECOND as f64
    }

    pub fn to_duration(self) -> Duration {
        let flicks = self.0.max(0);
        let seconds = flicks / FLICKS_PER_SECOND;
        let remainder = flicks % FLICKS_PER_SECOND;
        let nanos = i128::from(remainder) * 1_000_000_000 / i128::from(FLICKS_PER_SECOND);
        Duration::new(seconds as u64, nanos as u32)
    }
}

impl Add for Time {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl Sub for Time {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl FrameRate {
    pub const FPS_24: Self = Self::new(24, 1);
    pub const FPS_25: Self = Self::new(25, 1);
    pub const FPS_30: Self = Self::new(30, 1);
    pub const FPS_60: Self = Self::new(60, 1);
    pub const NTSC_24: Self = Self::new(24_000, 1_001);
    pub const NTSC_30: Self = Self::new(30_000, 1_001);
    pub const NTSC_60: Self = Self::new(60_000, 1_001);

    pub const fn new(numerator: u32, denominator: u32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    pub fn frame_duration(self) -> Time {
        self.frame_to_time(1)
    }

    pub fn frame_to_time(self, frame: i64) -> Time {
        Time::from_rational(
            frame * i64::from(self.denominator),
            i64::from(self.numerator),
        )
    }

    pub fn time_to_frame(self, time: Time) -> i64 {
        let scaled = i128::from(time.flicks()) * i128::from(self.numerator);
        let per_frame = i128::from(self.denominator) * i128::from(FLICKS_PER_SECOND);
        scaled.div_euclid(per_frame) as i64
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.numerator) / f64::from(self.denominator)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TimeRange {
    pub start: Time,
    pub duration: Time,
}

impl TimeRange {
    pub const fn new(start: Time, duration: Time) -> Self {
        Self { start, duration }
    }

    pub fn end(self) -> Time {
        self.start + self.duration
    }

    pub fn contains(self, time: Time) -> bool {
        self.start <= time && time < self.end()
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end() && other.start < self.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATES: [FrameRate; 7] = [
        FrameRate::FPS_24,
        FrameRate::FPS_25,
        FrameRate::FPS_30,
        FrameRate::FPS_60,
        FrameRate::NTSC_24,
        FrameRate::NTSC_30,
        FrameRate::NTSC_60,
    ];

    #[test]
    fn every_common_rate_has_an_integral_frame_duration() {
        for rate in RATES {
            let flicks_per_frame = i128::from(FLICKS_PER_SECOND) * i128::from(rate.denominator);
            assert_eq!(flicks_per_frame % i128::from(rate.numerator), 0, "{rate:?}");
        }
    }

    #[test]
    fn frames_round_trip_through_time() {
        for rate in RATES {
            for frame in [0, 1, 2, 1_000, 107_892, 10_000_000] {
                assert_eq!(
                    rate.time_to_frame(rate.frame_to_time(frame)),
                    frame,
                    "{rate:?}"
                );
            }
        }
    }

    #[test]
    fn time_inside_a_frame_maps_to_that_frame() {
        let rate = FrameRate::NTSC_30;
        let start = rate.frame_to_time(42);
        let almost_next = rate.frame_to_time(43) - Time::from_flicks(1);
        assert_eq!(rate.time_to_frame(start), 42);
        assert_eq!(rate.time_to_frame(almost_next), 42);
    }

    #[test]
    fn ntsc_frame_duration_is_exact() {
        assert_eq!(FrameRate::NTSC_30.frame_duration().flicks(), 23_543_520);
        assert_eq!(FrameRate::FPS_24.frame_duration().flicks(), 29_400_000);
    }

    #[test]
    fn ranges_overlap_only_when_they_share_time() {
        let a = TimeRange::new(Time::from_seconds(0), Time::from_seconds(2));
        let b = TimeRange::new(Time::from_seconds(2), Time::from_seconds(1));
        let c = TimeRange::new(Time::from_seconds(1), Time::from_seconds(2));
        assert!(!a.overlaps(b));
        assert!(a.overlaps(c));
        assert!(c.overlaps(b));
    }
}
