use std::{fmt, str::FromStr};

use thiserror::Error;

use crate::time::{FrameRate, Time};

const DROP_FRAME_DENOMINATOR: u32 = 1_001;
const DROP_FRAME_BASE: u32 = 30;
const FRAMES_DROPPED_PER_BASE: u64 = 2;
const MINUTES_PER_DROP_CYCLE: u64 = 10;
const SECONDS_PER_MINUTE: u64 = 60;
const MINUTES_PER_HOUR: u64 = 60;
const SECONDS_PER_HOUR: u64 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timecode {
    pub negative: bool,
    pub hours: u64,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u32,
    pub drop_frame: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimecodeField {
    Hours,
    Minutes,
    Seconds,
    Frames,
}

impl fmt::Display for TimecodeField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hours => "hours",
            Self::Minutes => "minutes",
            Self::Seconds => "seconds",
            Self::Frames => "frames",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum ParseTimecodeError {
    #[error("a timecode has hours, minutes, seconds and frames, not {0} fields")]
    FieldCount(usize),
    #[error("the {0} of a timecode are not a number")]
    InvalidField(TimecodeField),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TimecodeError {
    #[error("minute {0} is past the end of an hour")]
    MinutesOutOfRange(u8),
    #[error("second {0} is past the end of a minute")]
    SecondsOutOfRange(u8),
    #[error("frame {frames} is past the {nominal} frames of a second")]
    FramesOutOfRange { frames: u32, nominal: u32 },
    #[error("this frame rate counts drop-frame timecode, written with ';' before the frames")]
    DropFrameRequired,
    #[error("this frame rate has no drop-frame timecode")]
    DropFrameUnsupported,
    #[error("drop-frame timecode skips frame {frames} at the start of minute {minutes}")]
    DroppedLabel { minutes: u8, frames: u32 },
    #[error("the timecode lies past the times Tessera can represent")]
    OutOfRange,
}

impl Timecode {
    pub fn new(time: Time, rate: FrameRate) -> Self {
        let frame = rate.time_to_frame(time);
        let nominal = u64::from(rate.nominal_frames_per_second());
        let drop_frame = uses_drop_frame(rate);

        let label = if drop_frame {
            with_dropped_labels(frame.unsigned_abs(), nominal)
        } else {
            frame.unsigned_abs()
        };
        let total_seconds = label / nominal;

        Self {
            negative: frame < 0,
            hours: total_seconds / SECONDS_PER_HOUR,
            minutes: (total_seconds / SECONDS_PER_MINUTE % MINUTES_PER_HOUR) as u8,
            seconds: (total_seconds % SECONDS_PER_MINUTE) as u8,
            frames: (label % nominal) as u32,
            drop_frame,
        }
    }

    pub fn to_time(self, rate: FrameRate) -> Result<Time, TimecodeError> {
        let nominal = rate.nominal_frames_per_second();
        let drop_frame = uses_drop_frame(rate);

        match (self.drop_frame, drop_frame) {
            (false, true) => return Err(TimecodeError::DropFrameRequired),
            (true, false) => return Err(TimecodeError::DropFrameUnsupported),
            _ => {}
        }
        if u64::from(self.minutes) >= MINUTES_PER_HOUR {
            return Err(TimecodeError::MinutesOutOfRange(self.minutes));
        }
        if u64::from(self.seconds) >= SECONDS_PER_MINUTE {
            return Err(TimecodeError::SecondsOutOfRange(self.seconds));
        }
        if self.frames >= nominal {
            return Err(TimecodeError::FramesOutOfRange {
                frames: self.frames,
                nominal,
            });
        }

        let total_minutes =
            u128::from(self.hours) * u128::from(MINUTES_PER_HOUR) + u128::from(self.minutes);
        let total_seconds =
            total_minutes * u128::from(SECONDS_PER_MINUTE) + u128::from(self.seconds);
        let label = total_seconds * u128::from(nominal) + u128::from(self.frames);
        let magnitude = if drop_frame {
            self.without_dropped_labels(label, total_minutes, u64::from(nominal))?
        } else {
            label
        };

        let unsigned = i128::try_from(magnitude).map_err(|_| TimecodeError::OutOfRange)?;
        let signed = if self.negative { -unsigned } else { unsigned };
        let frame = i64::try_from(signed).map_err(|_| TimecodeError::OutOfRange)?;
        rate.checked_frame_to_time(frame)
            .ok_or(TimecodeError::OutOfRange)
    }

    fn without_dropped_labels(
        self,
        label: u128,
        total_minutes: u128,
        nominal: u64,
    ) -> Result<u128, TimecodeError> {
        let dropped_per_minute = dropped_per_minute(nominal);
        let starts_a_dropping_minute =
            self.seconds == 0 && !u64::from(self.minutes).is_multiple_of(MINUTES_PER_DROP_CYCLE);

        if starts_a_dropping_minute && u64::from(self.frames) < dropped_per_minute {
            return Err(TimecodeError::DroppedLabel {
                minutes: self.minutes,
                frames: self.frames,
            });
        }

        let dropping_minutes = total_minutes - total_minutes / u128::from(MINUTES_PER_DROP_CYCLE);
        Ok(label - u128::from(dropped_per_minute) * dropping_minutes)
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

impl FromStr for Timecode {
    type Err = ParseTimecodeError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let (negative, unsigned) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };

        let fields: Vec<&str> = unsigned.split([':', ';']).collect();
        let [hours, minutes, seconds, frames] = fields[..] else {
            return Err(ParseTimecodeError::FieldCount(fields.len()));
        };

        Ok(Self {
            negative,
            hours: parse_field(hours, TimecodeField::Hours)?,
            minutes: parse_field(minutes, TimecodeField::Minutes)?,
            seconds: parse_field(seconds, TimecodeField::Seconds)?,
            frames: parse_field(frames, TimecodeField::Frames)?,
            drop_frame: unsigned.contains(';'),
        })
    }
}

fn parse_field<T: FromStr>(text: &str, field: TimecodeField) -> Result<T, ParseTimecodeError> {
    let invalid = ParseTimecodeError::InvalidField(field);
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid);
    }
    text.parse().map_err(|_| invalid)
}

fn uses_drop_frame(rate: FrameRate) -> bool {
    rate.denominator() == DROP_FRAME_DENOMINATOR
        && rate
            .nominal_frames_per_second()
            .is_multiple_of(DROP_FRAME_BASE)
}

fn dropped_per_minute(nominal: u64) -> u64 {
    FRAMES_DROPPED_PER_BASE * nominal / u64::from(DROP_FRAME_BASE)
}

fn with_dropped_labels(frame: u64, nominal: u64) -> u64 {
    let dropped_per_minute = dropped_per_minute(nominal);
    let frames_per_minute = nominal * SECONDS_PER_MINUTE - dropped_per_minute;
    let frames_per_cycle = nominal * SECONDS_PER_MINUTE * MINUTES_PER_DROP_CYCLE
        - dropped_per_minute * (MINUTES_PER_DROP_CYCLE - 1);

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

    fn time_of(text: &str, rate: FrameRate) -> Result<Time, TimecodeError> {
        text.parse::<Timecode>().unwrap().to_time(rate)
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
    fn drop_frame_at_one_hundred_twenty_drops_eight_labels() {
        let rate = FrameRate::NTSC_120;

        assert_eq!(label(rate, 7_199), "00:00:59;119");
        assert_eq!(label(rate, 7_200), "00:01:00;08");
        assert_eq!(label(rate, 71_927), "00:09:59;119");
        assert_eq!(label(rate, 71_928), "00:10:00;00");
        assert_eq!(label(rate, 431_568), "01:00:00;00");
    }

    #[test]
    fn frames_count_past_255_at_high_rates() {
        let rate = FrameRate::new(300, 1).unwrap();

        assert_eq!(label(rate, 299), "00:00:00:299");
        assert_eq!(label(rate, 300), "00:00:01:00");
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

    #[test]
    fn the_ends_of_time_have_timecodes() {
        let fastest = FrameRate::new(705_600_000, 1).unwrap();

        let earliest = Timecode::new(Time::MIN, fastest);
        let latest = Timecode::new(Time::MAX, fastest);

        assert_eq!(earliest.to_string(), "-3631020:06:27:587575808");
        assert_eq!(latest.to_string(), "3631020:06:27:587575807");
        assert_eq!(earliest.to_time(fastest), Ok(Time::MIN));
        assert_eq!(latest.to_time(fastest), Ok(Time::MAX));
    }

    #[test]
    fn timecodes_convert_back_to_the_start_of_their_frame() {
        let arbitrary = [(44, 1), (300, 1), (1_000, 33), (u32::MAX, 7)]
            .map(|(numerator, denominator)| FrameRate::new(numerator, denominator).unwrap());

        for rate in FrameRate::STANDARD.into_iter().chain(arbitrary) {
            for frame in [
                -1_000_003,
                -1_800,
                -1,
                0,
                1,
                1_799,
                1_800,
                17_982,
                987_654_321,
            ] {
                let start = rate.frame_to_time(frame);
                let timecode = Timecode::new(start, rate);

                assert_eq!(timecode.to_time(rate), Ok(start), "{rate:?} {frame}");
                assert_eq!(
                    timecode.to_string().parse(),
                    Ok(timecode),
                    "{rate:?} {frame}"
                );
            }
        }
    }

    #[test]
    fn typed_timecodes_parse_into_their_fields() {
        let parsed: Timecode = " -1:02:03;04 ".parse().unwrap();

        assert_eq!(
            parsed,
            Timecode {
                negative: true,
                hours: 1,
                minutes: 2,
                seconds: 3,
                frames: 4,
                drop_frame: true,
            }
        );
        assert_eq!(
            "00;00;00;00".parse::<Timecode>().map(|t| t.drop_frame),
            Ok(true)
        );
        assert_eq!(
            "00:00:00:00".parse::<Timecode>().map(|t| t.drop_frame),
            Ok(false)
        );
    }

    #[test]
    fn malformed_timecodes_are_refused() {
        let parse = |text: &str| text.parse::<Timecode>();

        assert_eq!(parse(""), Err(ParseTimecodeError::FieldCount(1)));
        assert_eq!(parse("01:02:03"), Err(ParseTimecodeError::FieldCount(3)));
        assert_eq!(parse("0:0:0:0:0"), Err(ParseTimecodeError::FieldCount(5)));
        assert_eq!(
            parse("--1:00:00:00"),
            Err(ParseTimecodeError::InvalidField(TimecodeField::Hours))
        );
        assert_eq!(
            parse("00:+1:00:00"),
            Err(ParseTimecodeError::InvalidField(TimecodeField::Minutes))
        );
        assert_eq!(
            parse("00:00:256:00"),
            Err(ParseTimecodeError::InvalidField(TimecodeField::Seconds))
        );
        assert_eq!(
            parse("00:00:00:"),
            Err(ParseTimecodeError::InvalidField(TimecodeField::Frames))
        );
    }

    #[test]
    fn typed_drop_frame_timecodes_skip_the_dropped_labels() {
        let rate = FrameRate::NTSC_30;

        assert_eq!(time_of("00:01:00;02", rate), Ok(rate.frame_to_time(1_800)));
        assert_eq!(time_of("00:10:00;00", rate), Ok(rate.frame_to_time(17_982)));
        assert_eq!(time_of("00:10:00;01", rate), Ok(rate.frame_to_time(17_983)));
        assert_eq!(
            time_of("00:01:00;01", rate),
            Err(TimecodeError::DroppedLabel {
                minutes: 1,
                frames: 1
            })
        );
        assert_eq!(
            time_of("00:01:00;07", FrameRate::NTSC_120),
            Err(TimecodeError::DroppedLabel {
                minutes: 1,
                frames: 7
            })
        );
    }

    #[test]
    fn out_of_range_fields_are_refused() {
        let rate = FrameRate::FPS_30;

        assert_eq!(
            time_of("00:60:00:00", rate),
            Err(TimecodeError::MinutesOutOfRange(60))
        );
        assert_eq!(
            time_of("00:00:60:00", rate),
            Err(TimecodeError::SecondsOutOfRange(60))
        );
        assert_eq!(
            time_of("00:00:00:30", rate),
            Err(TimecodeError::FramesOutOfRange {
                frames: 30,
                nominal: 30
            })
        );
        assert_eq!(
            time_of("18446744073709551615:00:00:00", rate),
            Err(TimecodeError::OutOfRange)
        );
        assert_eq!(
            time_of("-18446744073709551615:00:00:00", rate),
            Err(TimecodeError::OutOfRange)
        );
    }

    #[test]
    fn the_separator_must_match_the_rate() {
        assert_eq!(
            time_of("00:00:01:00", FrameRate::NTSC_30),
            Err(TimecodeError::DropFrameRequired)
        );
        assert_eq!(
            time_of("00:00:01;00", FrameRate::FPS_30),
            Err(TimecodeError::DropFrameUnsupported)
        );
        assert_eq!(
            time_of("00:00:01:00", FrameRate::NTSC_24),
            Ok(FrameRate::NTSC_24.frame_to_time(24))
        );
    }
}
