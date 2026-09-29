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

const SNAP_TOLERANCE: f64 = 0.000_5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameRate {
    numerator: NonZeroU32,
    denominator: NonZeroU32,
}

impl FrameRate {
    pub const FPS_24: Self = Self::whole(24, 1);
    pub const FPS_25: Self = Self::whole(25, 1);
    pub const FPS_30: Self = Self::whole(30, 1);
    pub const FPS_48: Self = Self::whole(48, 1);
    pub const FPS_50: Self = Self::whole(50, 1);
    pub const FPS_60: Self = Self::whole(60, 1);
    pub const FPS_100: Self = Self::whole(100, 1);
    pub const FPS_120: Self = Self::whole(120, 1);
    pub const NTSC_24: Self = Self::whole(24_000, 1_001);
    pub const NTSC_30: Self = Self::whole(30_000, 1_001);
    pub const NTSC_48: Self = Self::whole(48_000, 1_001);
    pub const NTSC_60: Self = Self::whole(60_000, 1_001);
    pub const NTSC_120: Self = Self::whole(120_000, 1_001);

    pub const STANDARD: [Self; 13] = [
        Self::NTSC_24,
        Self::FPS_24,
        Self::FPS_25,
        Self::NTSC_30,
        Self::FPS_30,
        Self::NTSC_48,
        Self::FPS_48,
        Self::FPS_50,
        Self::NTSC_60,
        Self::FPS_60,
        Self::FPS_100,
        Self::NTSC_120,
        Self::FPS_120,
    ];

    pub const fn new(numerator: u32, denominator: u32) -> Option<Self> {
        if numerator == 0 || denominator == 0 || !fits_a_flick(numerator, denominator) {
            return None;
        }
        let divisor = gcd(numerator, denominator);
        match (
            NonZeroU32::new(numerator / divisor),
            NonZeroU32::new(denominator / divisor),
        ) {
            (Some(numerator), Some(denominator)) => Some(Self {
                numerator,
                denominator,
            }),
            _ => None,
        }
    }

    pub fn nearest(numerator: u32, denominator: u32) -> Option<Self> {
        let exact = Self::new(numerator, denominator)?;
        Some(Self::nearest_standard(exact.as_f64()).unwrap_or(exact))
    }

    fn nearest_standard(requested: f64) -> Option<Self> {
        let relative_error = |rate: &Self| (requested - rate.as_f64()).abs() / rate.as_f64();
        Self::STANDARD
            .iter()
            .filter(|rate| relative_error(rate) <= SNAP_TOLERANCE)
            .min_by(|a, b| relative_error(a).total_cmp(&relative_error(b)))
            .copied()
    }

    const fn whole(numerator: u32, denominator: u32) -> Self {
        Self::new(numerator, denominator).expect("a constant rate is a valid rate")
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
        Time::saturated(ceiling_div(scaled, i128::from(self.numerator())))
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

fn ceiling_div(dividend: i128, positive_divisor: i128) -> i128 {
    -(-dividend).div_euclid(positive_divisor)
}

const fn fits_a_flick(numerator: u32, denominator: u32) -> bool {
    numerator as u64 <= FLICKS_PER_SECOND as u64 * denominator as u64
}

const fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
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

    #[test]
    fn every_common_rate_has_an_integral_frame_duration() {
        for rate in FrameRate::STANDARD {
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
        for rate in FrameRate::STANDARD {
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
    fn frame_rates_last_at_least_a_flick() {
        assert!(FrameRate::new(705_600_000, 1).is_some());
        assert_eq!(FrameRate::new(705_600_001, 1), None);
        assert_eq!(FrameRate::new(u32::MAX, 1), None);
        assert!(FrameRate::new(u32::MAX, 7).is_some());
        assert!(FrameRate::new(1, u32::MAX).is_some());
    }

    #[test]
    fn frame_rates_are_kept_in_lowest_terms() {
        let doubled = FrameRate::new(60_000, 2_002).unwrap();

        assert_eq!(doubled, FrameRate::NTSC_30);
        assert_eq!(FrameRate::new(60, 2), Some(FrameRate::FPS_30));
        assert_eq!(
            (doubled.numerator(), doubled.denominator()),
            (30_000, 1_001)
        );
    }

    const ARBITRARY_RATES: [(u32, u32); 7] = [
        (44, 1),
        (13, 1),
        (1_000, 33),
        (97, 7),
        (2_997, 100),
        (239_999, 1_000),
        (u32::MAX, 7),
    ];

    #[test]
    fn frames_at_arbitrary_rates_round_trip_through_time() {
        for (numerator, denominator) in ARBITRARY_RATES {
            let rate = FrameRate::new(numerator, denominator).unwrap();
            for frame in [-1_000_003, -2, -1, 0, 1, 2, 3, 1_000_003, 987_654_321] {
                assert_eq!(
                    rate.time_to_frame(rate.frame_to_time(frame)),
                    frame,
                    "{rate:?}"
                );
            }
        }
    }

    #[test]
    fn frames_at_arbitrary_rates_start_on_the_next_flick() {
        let rate = FrameRate::new(44, 1).unwrap();
        let exact_start = |frame: i64| frame as f64 * FLICKS_PER_SECOND as f64 / 44.0;

        for frame in [-45, -1, 1, 2, 3, 43, 44, 45] {
            let start = rate.frame_to_time(frame).flicks();

            assert!(start as f64 >= exact_start(frame), "{frame}");
            assert!(((start - 1) as f64) < exact_start(frame), "{frame}");
        }
        assert_eq!(rate.frame_duration().flicks(), 16_036_364);
        assert_eq!(rate.frame_to_time(44), Time::from_seconds(1));
    }

    #[test]
    fn frames_at_arbitrary_rates_differ_in_length_by_at_most_a_flick() {
        let rate = FrameRate::new(44, 1).unwrap();
        let lengths: Vec<i64> = (0..44)
            .map(|frame| (rate.frame_to_time(frame + 1) - rate.frame_to_time(frame)).flicks())
            .collect();

        assert!(
            lengths
                .iter()
                .all(|&length| (16_036_363..=16_036_364).contains(&length))
        );
        assert_eq!(lengths.iter().sum::<i64>(), FLICKS_PER_SECOND);
    }

    #[test]
    fn nearest_rate_snaps_to_a_standard_rate_close_by() {
        assert_eq!(FrameRate::nearest(2_997, 100), Some(FrameRate::NTSC_30));
        assert_eq!(FrameRate::nearest(1_199, 40), Some(FrameRate::NTSC_30));
        assert_eq!(FrameRate::nearest(2_999, 100), Some(FrameRate::FPS_30));
        assert_eq!(FrameRate::nearest(5_994, 100), Some(FrameRate::NTSC_60));
        assert_eq!(FrameRate::nearest(24_000, 1_001), Some(FrameRate::NTSC_24));
    }

    #[test]
    fn nearest_rate_keeps_other_rates_as_they_are() {
        for (numerator, denominator) in [(15, 1), (25, 2), (44, 1), (3_003, 100), (1, u32::MAX)] {
            assert_eq!(
                FrameRate::nearest(numerator, denominator),
                FrameRate::new(numerator, denominator)
            );
        }
        assert_eq!(FrameRate::nearest(0, 1), None);
        assert_eq!(FrameRate::nearest(44, 0), None);
        assert_eq!(FrameRate::nearest(u32::MAX, 1), None);
    }

    #[test]
    fn frame_conversions_saturate_at_extreme_rates() {
        let fastest = FrameRate::new(705_600_000, 1).unwrap();
        let slowest = FrameRate::new(1, u32::MAX).unwrap();

        assert_eq!(fastest.frame_duration(), Time::from_flicks(1));
        assert_eq!(slowest.frame_to_time(i64::MAX), Time::MAX);
        assert_eq!(slowest.frame_to_time(i64::MIN), Time::MIN);
        assert_eq!(fastest.time_to_frame(Time::MAX), i64::MAX);
        assert_eq!(fastest.time_to_frame(Time::MIN), i64::MIN);
        assert_eq!(fastest.nominal_frames_per_second(), 705_600_000);
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
