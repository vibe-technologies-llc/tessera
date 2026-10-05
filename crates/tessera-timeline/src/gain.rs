use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Gain(i32);

impl Gain {
    pub const UNITY: Self = Self(0);
    pub const SILENT: Self = Self(-600);
    pub const LOUDEST: Self = Self(120);
    pub const TENTHS_PER_DECIBEL: i32 = 10;

    pub fn from_tenths(tenths: i32) -> Option<Self> {
        (Self::SILENT.0..=Self::LOUDEST.0)
            .contains(&tenths)
            .then_some(Self(tenths))
    }

    pub fn tenths(self) -> i32 {
        self.0
    }

    pub fn adjusted(self, tenths: i32) -> Self {
        Self(
            self.0
                .saturating_add(tenths)
                .clamp(Self::SILENT.0, Self::LOUDEST.0),
        )
    }

    pub fn amplitude(self) -> f32 {
        if self == Self::SILENT {
            0.0
        } else {
            10_f32.powf(self.0 as f32 / (20 * Self::TENTHS_PER_DECIBEL) as f32)
        }
    }
}

impl fmt::Display for Gain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Self::SILENT {
            return formatter.write_str("−∞ dB");
        }
        let sign = match self.0 {
            0 => "",
            1.. => "+",
            _ => "−",
        };
        let magnitude = self.0.unsigned_abs();
        let per = Self::TENTHS_PER_DECIBEL.unsigned_abs();
        write!(
            formatter,
            "{sign}{}.{} dB",
            magnitude / per,
            magnitude % per
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gains_stay_in_range_and_turn_into_amplitudes() {
        assert_eq!(Gain::from_tenths(-601), None);
        assert_eq!(Gain::from_tenths(121), None);
        assert_eq!(Gain::UNITY.adjusted(-1_000), Gain::SILENT);
        assert_eq!(Gain::UNITY.adjusted(i32::MAX), Gain::LOUDEST);

        assert_eq!(Gain::UNITY.amplitude(), 1.0);
        assert_eq!(Gain::SILENT.amplitude(), 0.0);
        assert!((Gain::from_tenths(60).unwrap().amplitude() - 1.995).abs() < 0.001);
        assert!((Gain::from_tenths(-60).unwrap().amplitude() - 0.501).abs() < 0.001);
    }

    #[test]
    fn gains_read_in_decibels() {
        let shown = |tenths| Gain::from_tenths(tenths).unwrap().to_string();

        assert_eq!(shown(0), "0.0 dB");
        assert_eq!(shown(35), "+3.5 dB");
        assert_eq!(shown(-5), "−0.5 dB");
        assert_eq!(shown(-600), "−∞ dB");
    }
}
