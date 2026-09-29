use std::{
    num::{NonZeroI64, NonZeroU32},
    ops::{Add, Mul, Sub},
    time::Duration,
};

pub const FLICKS_PER_SECOND: i64 = 705_600_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time(i64);

impl Time {
    pub const ZERO: Self = Self(0);
    pub const MIN: Self = Self(i64::MIN);
    pub const MAX: Self = Self(i64::MAX);

    pub const fn from_flicks(flicks: i64) -> Self {
        Self(flicks)
    }

    pub const fn flicks(self) -> i64 {
        self.0
    }

    pub const fn from_seconds(seconds: i64) -> Self {
        Self(seconds.saturating_mul(FLICKS_PER_SECOND))
    }

    pub fn from_rational(numerator: i64, denominator: NonZeroI64) -> Self {
        let flicks =
            i128::from(numerator) * i128::from(FLICKS_PER_SECOND) / i128::from(denominator.get());
        Self::saturated(flicks)
    }

    pub fn from_samples(samples: i64, sample_rate: NonZeroU32) -> Self {
        let flicks = i128::from(samples) * i128::from(FLICKS_PER_SECOND);
        Self::saturated(flicks.div_euclid(i128::from(sample_rate.get())))
    }

    pub fn to_samples(self, sample_rate: NonZeroU32) -> i64 {
        let scaled = i128::from(self.0) * i128::from(sample_rate.get());
        saturated_i64(scaled.div_euclid(i128::from(FLICKS_PER_SECOND)))
    }

    pub fn as_seconds_f64(self) -> f64 {
        self.0 as f64 / FLICKS_PER_SECOND as f64
    }

    pub fn from_duration(duration: Duration) -> Self {
        let flicks = duration.as_nanos() * FLICKS_PER_SECOND as u128 / 1_000_000_000;
        Self(i64::try_from(flicks).unwrap_or(i64::MAX))
    }

    pub fn to_duration(self) -> Duration {
        let flicks = self.0.max(0);
        let seconds = flicks / FLICKS_PER_SECOND;
        let remainder = flicks % FLICKS_PER_SECOND;
        let nanos = i128::from(remainder) * 1_000_000_000 / i128::from(FLICKS_PER_SECOND);
        Duration::new(seconds as u64, nanos as u32)
    }

    pub const fn checked_add(self, other: Self) -> Option<Self> {
        match self.0.checked_add(other.0) {
            Some(flicks) => Some(Self(flicks)),
            None => None,
        }
    }

    pub const fn checked_sub(self, other: Self) -> Option<Self> {
        match self.0.checked_sub(other.0) {
            Some(flicks) => Some(Self(flicks)),
            None => None,
        }
    }

    fn saturated(flicks: i128) -> Self {
        Self(saturated_i64(flicks))
    }
}

fn saturated_i64(value: i128) -> i64 {
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

impl Add for Time {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl Mul<i64> for Time {
    type Output = Self;

    fn mul(self, factor: i64) -> Self {
        Self(self.0.saturating_mul(factor))
    }
}

impl Sub for Time {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameRate {
    numerator: NonZeroU32,
    denominator: NonZeroU32,
}

impl FrameRate {
    pub const FPS_24: Self = Self::whole(24, 1);
    pub const FPS_25: Self = Self::whole(25, 1);
    pub const FPS_30: Self = Self::whole(30, 1);
    pub const FPS_60: Self = Self::whole(60, 1);
    pub const NTSC_24: Self = Self::whole(24_000, 1_001);
    pub const NTSC_30: Self = Self::whole(30_000, 1_001);
    pub const NTSC_60: Self = Self::whole(60_000, 1_001);

    pub const fn new(numerator: u32, denominator: u32) -> Option<Self> {
        match (NonZeroU32::new(numerator), NonZeroU32::new(denominator)) {
            (Some(numerator), Some(denominator)) => Some(Self {
                numerator,
                denominator,
            }),
            _ => None,
        }
    }

    const fn whole(numerator: u32, denominator: u32) -> Self {
        Self::new(numerator, denominator).expect("a constant rate has non-zero parts")
    }

    pub const fn numerator(self) -> u32 {
        self.numerator.get()
    }

    pub const fn denominator(self) -> u32 {
        self.denominator.get()
    }

    pub fn frame_duration(self) -> Time {
        self.frame_to_time(1)
    }

    pub fn frame_to_time(self, frame: i64) -> Time {
        let scaled =
            i128::from(frame) * i128::from(self.denominator()) * i128::from(FLICKS_PER_SECOND);
        Time::saturated(scaled / i128::from(self.numerator()))
    }

    pub fn time_to_frame(self, time: Time) -> i64 {
        let scaled = i128::from(time.flicks()) * i128::from(self.numerator());
        let per_frame = i128::from(self.denominator()) * i128::from(FLICKS_PER_SECOND);
        saturated_i64(scaled.div_euclid(per_frame))
    }

    pub fn frame_start(self, time: Time) -> Time {
        self.frame_to_time(self.time_to_frame(time))
    }

    pub fn nominal_frames_per_second(self) -> i64 {
        i64::from(self.numerator().div_ceil(self.denominator()))
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.numerator()) / f64::from(self.denominator())
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

    pub const fn checked_end(self) -> Option<Time> {
        self.start.checked_add(self.duration)
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

    #[test]
    fn durations_convert_to_flicks_and_back() {
        let duration = Duration::from_millis(1_500);
        let time = Time::from_duration(duration);
        assert_eq!(time, Time::from_flicks(FLICKS_PER_SECOND * 3 / 2));
        assert_eq!(time.to_duration(), duration);
        assert_eq!(Time::from_duration(Duration::from_nanos(1)), Time::ZERO);
        assert_eq!(
            Time::from_duration(Duration::MAX),
            Time::from_flicks(i64::MAX)
        );
    }

    #[test]
    fn times_scale_by_signed_factors() {
        assert_eq!(Time::from_seconds(2) * 4, Time::from_seconds(8));
        assert_eq!(Time::from_seconds(2) * -1, Time::from_seconds(-2));
    }

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
            let flicks_per_frame = i128::from(FLICKS_PER_SECOND) * i128::from(rate.denominator());
            assert_eq!(
                flicks_per_frame % i128::from(rate.numerator()),
                0,
                "{rate:?}"
            );
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
    fn frame_start_rounds_down_to_the_frame_boundary() {
        let rate = FrameRate::NTSC_30;
        let start = rate.frame_to_time(7);
        assert_eq!(rate.frame_start(start), start);
        assert_eq!(rate.frame_start(start + Time::from_flicks(1)), start);
        assert_eq!(
            rate.frame_start(start - Time::from_flicks(1)),
            rate.frame_to_time(6)
        );
    }

    #[test]
    fn nominal_rate_rounds_ntsc_up() {
        assert_eq!(FrameRate::NTSC_24.nominal_frames_per_second(), 24);
        assert_eq!(FrameRate::NTSC_30.nominal_frames_per_second(), 30);
        assert_eq!(FrameRate::FPS_25.nominal_frames_per_second(), 25);
    }

    #[test]
    fn ntsc_frame_duration_is_exact() {
        assert_eq!(FrameRate::NTSC_30.frame_duration().flicks(), 23_543_520);
        assert_eq!(FrameRate::FPS_24.frame_duration().flicks(), 29_400_000);
    }

    fn hz(rate: u32) -> NonZeroU32 {
        NonZeroU32::new(rate).unwrap()
    }

    const SAMPLE_RATES: [u32; 9] = [
        8_000, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 192_000,
    ];

    #[test]
    fn every_common_sample_rate_has_an_integral_sample_duration() {
        for rate in SAMPLE_RATES {
            assert_eq!(FLICKS_PER_SECOND % i64::from(rate), 0, "{rate}");
        }
    }

    #[test]
    fn samples_round_trip_through_time() {
        for rate in SAMPLE_RATES.map(hz) {
            for samples in [-48_001, -1, 0, 1, 2, 44_100, 48_000, 1_000_000_007] {
                let time = Time::from_samples(samples, rate);
                assert_eq!(time.to_samples(rate), samples, "{rate}");
            }
        }
        assert_eq!(
            Time::from_samples(48_000, hz(48_000)),
            Time::from_seconds(1)
        );
        assert_eq!(Time::from_samples(1, hz(44_100)).flicks(), 16_000);
    }

    #[test]
    fn time_inside_a_sample_floors_to_that_sample() {
        let rate = hz(48_000);
        let start = Time::from_samples(7, rate);
        let almost_next = Time::from_samples(8, rate) - Time::from_flicks(1);
        assert_eq!(start.to_samples(rate), 7);
        assert_eq!(almost_next.to_samples(rate), 7);
        assert_eq!(Time::from_flicks(-1).to_samples(rate), -1);
        assert_eq!(
            (Time::from_samples(-3, rate) + Time::from_flicks(1)).to_samples(rate),
            -3
        );
    }

    #[test]
    fn samples_at_rates_without_an_integral_duration_floor() {
        assert_eq!(Time::from_samples(1, hz(7)).flicks(), 100_800_000);
        assert_eq!(Time::from_samples(-1, hz(11)).flicks(), -64_145_455);
        assert_eq!(Time::from_samples(1, hz(11)).to_samples(hz(11)), 0);
    }

    #[test]
    fn sample_conversions_saturate_instead_of_wrapping() {
        let rate = hz(192_000);

        assert_eq!(Time::from_samples(i64::MAX, rate), Time::MAX);
        assert_eq!(Time::from_samples(i64::MIN, rate), Time::MIN);
        assert_eq!(Time::MAX.to_samples(rate), Time::MAX.flicks() / 3_675);
        assert_eq!(Time::MIN.to_samples(hz(u32::MAX)), i64::MIN);
    }

    #[test]
    fn arithmetic_saturates_at_the_ends_of_time() {
        let second = Time::from_seconds(1);

        assert_eq!(Time::MAX + second, Time::MAX);
        assert_eq!(Time::MIN - second, Time::MIN);
        assert_eq!(Time::MIN + Time::MAX, Time::from_flicks(-1));
        assert_eq!(second * i64::MAX, Time::MAX);
        assert_eq!(second * i64::MIN, Time::MIN);
        assert_eq!(Time::from_seconds(i64::MAX), Time::MAX);
        assert_eq!(Time::from_seconds(i64::MIN), Time::MIN);

        assert_eq!(Time::MAX.checked_add(second), None);
        assert_eq!(Time::MIN.checked_sub(second), None);
        assert_eq!(second.checked_add(second), Some(Time::from_seconds(2)));
        assert_eq!(second.checked_sub(second), Some(Time::ZERO));
    }

    #[test]
    fn rational_times_saturate() {
        let one = NonZeroI64::new(1).unwrap();
        let minus_one = NonZeroI64::new(-1).unwrap();

        assert_eq!(Time::from_rational(i64::MAX, one), Time::MAX);
        assert_eq!(Time::from_rational(i64::MAX, minus_one), Time::MIN);
        assert_eq!(
            Time::from_rational(3, NonZeroI64::new(2).unwrap()),
            Time::from_flicks(FLICKS_PER_SECOND * 3 / 2)
        );
    }

    #[test]
    fn frame_rates_need_non_zero_parts() {
        assert_eq!(FrameRate::new(0, 1), None);
        assert_eq!(FrameRate::new(30, 0), None);
        assert_eq!(FrameRate::new(0, 0), None);
        assert_eq!(FrameRate::new(30_000, 1_001), Some(FrameRate::NTSC_30));
        assert_eq!(FrameRate::NTSC_30.numerator(), 30_000);
        assert_eq!(FrameRate::NTSC_30.denominator(), 1_001);
    }

    #[test]
    fn frame_conversions_saturate_at_extreme_rates() {
        let fastest = FrameRate::new(u32::MAX, 1).unwrap();
        let slowest = FrameRate::new(1, u32::MAX).unwrap();

        assert_eq!(slowest.frame_to_time(i64::MAX), Time::MAX);
        assert_eq!(slowest.frame_to_time(i64::MIN), Time::MIN);
        assert_eq!(fastest.time_to_frame(Time::MAX), i64::MAX);
        assert_eq!(fastest.time_to_frame(Time::MIN), i64::MIN);
        assert_eq!(fastest.nominal_frames_per_second(), i64::from(u32::MAX));
        assert_eq!(slowest.nominal_frames_per_second(), 1);
    }

    #[test]
    fn range_ends_saturate_unless_checked() {
        let near_the_end = TimeRange::new(Time::MAX - Time::from_seconds(1), Time::from_seconds(2));
        let fitting = TimeRange::new(Time::from_seconds(1), Time::from_seconds(2));

        assert_eq!(near_the_end.end(), Time::MAX);
        assert_eq!(near_the_end.checked_end(), None);
        assert_eq!(fitting.checked_end(), Some(Time::from_seconds(3)));
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
