use std::fmt;

use crate::time::{FrameRate, Time};

const DROP_FRAME_DENOMINATOR: u32 = 1_001;
const DROP_FRAME_BASE: i64 = 30;
const FRAMES_DROPPED_PER_BASE: i64 = 2;
const MINUTES_PER_DROP_CYCLE: i64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timecode {
    pub negative: bool,
    pub hours: i64,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: bool,
}

impl Timecode {
    pub fn new(time: Time, rate: FrameRate) -> Self {
        let frame = rate.time_to_frame(time);
        let nominal = rate.nominal_frames_per_second();
        let drop_frame = uses_drop_frame(rate);
        let counted = if drop_frame {
            with_dropped_labels(frame.abs(), nominal)
        } else {
            frame.abs()
        };
        let total_seconds = counted / nominal;
        Self {
            negative: frame < 0,
            hours: total_seconds / 3600,
            minutes: (total_seconds / 60 % 60) as u8,
            seconds: (total_seconds % 60) as u8,
            frames: (counted % nominal) as u8,
            drop_frame,
        }
    }
}

impl fmt::Display for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.negative { "-" } else { "" };
        let frame_separator = if self.drop_frame { ';' } else { ':' };
        write!(
            f,
            "{sign}{:02}:{:02}:{:02}{frame_separator}{:02}",
            self.hours, self.minutes, self.seconds, self.frames
        )
    }
}

fn uses_drop_frame(rate: FrameRate) -> bool {
    rate.denominator == DROP_FRAME_DENOMINATOR
        && rate.nominal_frames_per_second() % DROP_FRAME_BASE == 0
}

fn with_dropped_labels(frame: i64, nominal: i64) -> i64 {
    let dropped_per_minute = FRAMES_DROPPED_PER_BASE * nominal / DROP_FRAME_BASE;
    let frames_per_minute = nominal * 60 - dropped_per_minute;
    let frames_per_cycle =
        nominal * 60 * MINUTES_PER_DROP_CYCLE - dropped_per_minute * (MINUTES_PER_DROP_CYCLE - 1);
    let cycles = frame / frames_per_cycle;
    let into_cycle = frame % frames_per_cycle;
    let dropped_minutes_in_cycle = if into_cycle < dropped_per_minute {
        0
    } else {
        (into_cycle - dropped_per_minute) / frames_per_minute
    };
    frame + dropped_per_minute * ((MINUTES_PER_DROP_CYCLE - 1) * cycles + dropped_minutes_in_cycle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(rate: FrameRate, frame: i64) -> String {
        Timecode::new(rate.frame_to_time(frame), rate).to_string()
    }

    #[test]
    fn integral_rates_count_whole_seconds() {
        assert_eq!(label(FrameRate::FPS_24, 0), "00:00:00:00");
        assert_eq!(label(FrameRate::FPS_24, 23), "00:00:00:23");
        assert_eq!(label(FrameRate::FPS_24, 24), "00:00:01:00");
        assert_eq!(label(FrameRate::FPS_25, 25 * 3_661 + 7), "01:01:01:07");
    }

    #[test]
    fn ntsc_film_rate_is_non_drop() {
        assert_eq!(label(FrameRate::NTSC_24, 24 * 60), "00:01:00:00");
    }

    #[test]
    fn drop_frame_skips_labels_at_each_minute_but_every_tenth() {
        let rate = FrameRate::NTSC_30;
        assert_eq!(label(rate, 1_799), "00:00:59;29");
        assert_eq!(label(rate, 1_800), "00:01:00;02");
        assert_eq!(label(rate, 17_981), "00:09:59;29");
        assert_eq!(label(rate, 17_982), "00:10:00;00");
        assert_eq!(label(rate, 17_982 + 1_800), "00:11:00;02");
        assert_eq!(label(rate, 107_892), "01:00:00;00");
    }

    #[test]
    fn drop_frame_at_sixty_drops_four_labels() {
        let rate = FrameRate::NTSC_60;
        assert_eq!(label(rate, 3_599), "00:00:59;59");
        assert_eq!(label(rate, 3_600), "00:01:00;04");
        assert_eq!(label(rate, 215_784), "01:00:00;00");
    }

    #[test]
    fn negative_times_keep_their_sign() {
        assert_eq!(label(FrameRate::FPS_30, -31), "-00:00:01:01");
    }

    #[test]
    fn time_inside_a_frame_shows_that_frame() {
        let rate = FrameRate::FPS_24;
        let time = rate.frame_to_time(49) + Time::from_flicks(1);
        assert_eq!(Timecode::new(time, rate).to_string(), "00:00:02:01");
    }
}
