use tessera_timeline::{ClipId, Time, Timeline};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snap {
    pub shift: Time,
    pub target: Time,
}

pub struct SnapTargets(Vec<Time>);

impl SnapTargets {
    pub fn new(timeline: &Timeline, playhead: Time, moving: &[ClipId]) -> Self {
        let edges = timeline
            .tracks
            .iter()
            .flat_map(|track| track.clips())
            .filter(|clip| !moving.contains(&clip.id))
            .map(|clip| clip.timeline_range())
            .flat_map(|range| [range.start, range.end()]);
        Self([Time::ZERO, playhead].into_iter().chain(edges).collect())
    }

    pub fn snap(&self, edges: &[Time], tolerance: Time) -> Option<Snap> {
        edges
            .iter()
            .flat_map(|&edge| {
                self.0.iter().map(move |&target| Snap {
                    shift: target - edge,
                    target,
                })
            })
            .filter(|snap| snap.shift.flicks().abs() <= tolerance.flicks())
            .min_by_key(|snap| snap.shift.flicks().abs())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroI64;

    use tessera_timeline::{AssetId, Clip, TimeRange, Track, TrackKind};

    use super::*;

    fn seconds(value: i64) -> Time {
        Time::from_seconds(value)
    }

    fn millis(value: i64) -> Time {
        Time::from_rational(value, NonZeroI64::new(1000).unwrap())
    }

    fn timeline() -> Timeline {
        let mut track = Track::new(TrackKind::Video);
        for (id, start) in [(0, 2), (1, 10)] {
            track
                .insert(Clip {
                    id: ClipId(id),
                    asset: AssetId(0),
                    source: TimeRange::new(Time::ZERO, seconds(3)),
                    start: seconds(start),
                })
                .unwrap();
        }
        Timeline {
            tracks: vec![track],
        }
    }

    #[test]
    fn snaps_the_nearest_edge_to_the_nearest_target_within_tolerance() {
        let targets = SnapTargets::new(&timeline(), seconds(20), &[]);
        let tolerance = millis(200);
        let edges = [seconds(5) + millis(150), seconds(10) - millis(100)];
        assert_eq!(
            targets.snap(&edges, tolerance),
            Some(Snap {
                shift: millis(100),
                target: seconds(10),
            })
        );
        assert_eq!(targets.snap(&[seconds(7)], tolerance), None);
        assert_eq!(
            targets.snap(&[seconds(20) - millis(50)], tolerance),
            Some(Snap {
                shift: millis(50),
                target: seconds(20),
            })
        );
    }

    #[test]
    fn a_moving_clip_does_not_snap_to_itself() {
        let targets = SnapTargets::new(&timeline(), seconds(20), &[ClipId(0)]);
        assert_eq!(targets.snap(&[seconds(2) + millis(10)], millis(200)), None);
        assert_eq!(
            targets.snap(&[millis(100)], millis(200)),
            Some(Snap {
                shift: millis(-100),
                target: Time::ZERO,
            })
        );
    }
}
