use std::collections::{BTreeMap, BTreeSet};

use super::{AssetId, Clip, ClipId, EditError, LinkId, Project, TrackKind};
use crate::time::Time;

impl Project {
    pub fn partners(&self, id: ClipId) -> Vec<(usize, Clip)> {
        let Some(link) = self.find_clip(id).and_then(|(_, clip)| clip.link) else {
            return Vec::new();
        };
        self.timeline
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(index, track)| {
                track
                    .clips()
                    .iter()
                    .filter(move |other| other.link == Some(link) && other.id != id)
                    .map(move |other| (index, *other))
            })
            .collect()
    }

    pub fn with_partners(&self, ids: &[ClipId]) -> Vec<ClipId> {
        let mut seen = BTreeSet::new();
        let mut all = Vec::new();
        for &id in ids {
            let group = std::iter::once(id).chain(self.partners(id).into_iter().map(|(_, c)| c.id));
            for member in group {
                if seen.insert(member) {
                    all.push(member);
                }
            }
        }
        all
    }

    pub fn partner_track(&self, track: usize) -> Option<usize> {
        let tracks = &self.timeline.tracks;
        let kind = tracks.get(track)?.kind;
        let other = match kind {
            TrackKind::Video => TrackKind::Audio,
            TrackKind::Audio => TrackKind::Video,
        };
        let ordinal = tracks[..track]
            .iter()
            .filter(|track| track.kind == kind)
            .count();
        let others: Vec<usize> = tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.kind == other)
            .map(|(index, _)| index)
            .collect();
        others.get(ordinal).or(others.first()).copied()
    }

    pub fn linked_clips_for(
        &self,
        asset: AssetId,
        track: usize,
        start: Time,
    ) -> Result<Vec<(usize, Clip)>, EditError> {
        let clip = self.clip_for(asset, track, start)?;
        let held = self.asset(asset).ok_or(EditError::UnknownAsset(asset))?;
        let partner_track = self
            .partner_track(track)
            .filter(|&other| self.track_accepting(held, other).is_ok());
        let Some(other) = partner_track else {
            return Ok(vec![(track, clip)]);
        };
        let link = Some(self.next_ids.link);
        let partner = Clip {
            id: ClipId(clip.id.0 + 1),
            link,
            ..clip
        };
        self.timeline.tracks[other].check_insert(&partner)?;
        Ok(vec![(track, Clip { link, ..clip }), (other, partner)])
    }

    pub fn place_linked(
        &mut self,
        asset: AssetId,
        track: usize,
        start: Time,
    ) -> Result<Vec<Clip>, EditError> {
        let planned = self.linked_clips_for(asset, track, start)?;
        self.atomically(|project| {
            let link = (planned.len() > 1).then(|| project.next_ids.take_link());
            let mut placed = Vec::new();
            for (track, planned) in planned {
                let clip = Clip {
                    id: project.next_ids.take_clip(),
                    link,
                    ..planned
                };
                project.timeline.tracks[track].insert(clip)?;
                placed.push(clip);
            }
            Ok(placed)
        })
    }

    pub fn split_clips(
        &mut self,
        ids: &[ClipId],
        at: Time,
    ) -> Result<Vec<(Clip, Clip)>, EditError> {
        let ids = self.with_partners(ids);
        self.atomically(|project| {
            let mut tail_links: BTreeMap<LinkId, LinkId> = BTreeMap::new();
            let mut split = Vec::new();
            for id in ids {
                let link = project.find_clip(id).and_then(|(_, clip)| clip.link);
                let tail_link = link.map(|link| {
                    *tail_links
                        .entry(link)
                        .or_insert_with(|| project.next_ids.take_link())
                });
                split.push(project.split_alone(id, at, tail_link)?);
            }
            Ok(split)
        })
    }

    pub fn unlink_clips(&mut self, ids: &[ClipId]) -> Result<(), EditError> {
        let links: BTreeSet<LinkId> = ids
            .iter()
            .map(|&id| {
                self.find_clip(id)
                    .map(|(_, clip)| clip.link)
                    .ok_or(EditError::UnknownClip(id))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        self.drop_links(&links);
        Ok(())
    }

    pub(super) fn settle_links(&mut self) {
        let mut groups: BTreeMap<LinkId, Vec<Clip>> = BTreeMap::new();
        for clip in self.timeline.tracks.iter().flat_map(|track| track.clips()) {
            if let Some(link) = clip.link {
                groups.entry(link).or_default().push(*clip);
            }
        }
        let broken: BTreeSet<LinkId> = groups
            .into_iter()
            .filter(|(_, members)| !in_step(members))
            .map(|(link, _)| link)
            .collect();
        self.drop_links(&broken);
    }

    fn drop_links(&mut self, links: &BTreeSet<LinkId>) {
        let dropped = |clip: &Clip| clip.link.is_some_and(|link| links.contains(&link));
        for track in &mut self.timeline.tracks {
            if track.clips().iter().any(dropped) {
                for clip in track.clips_mut().iter_mut().filter(|clip| dropped(clip)) {
                    clip.link = None;
                }
            }
        }
    }
}

pub(crate) fn in_step(members: &[Clip]) -> bool {
    let [first, rest @ ..] = members else {
        return false;
    };
    !rest.is_empty()
        && rest.iter().all(|member| {
            member.asset == first.asset
                && member.start == first.start
                && member.source == first.source
        })
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use super::*;
    use crate::{
        media::{AudioStream, MediaInfo, Stream, VideoStream},
        project::ClipEdge,
    };

    const V1: usize = 0;
    const A1: usize = 1;

    fn seconds(seconds: i64) -> Time {
        Time::from_seconds(seconds)
    }

    fn audio() -> Stream {
        Stream::Audio(AudioStream {
            index: 1,
            codec: "aac".into(),
            sample_rate: NonZero::new(48_000).unwrap(),
            channels: NonZero::new(2).unwrap(),
        })
    }

    fn video() -> Stream {
        Stream::Video(VideoStream {
            index: 0,
            codec: "h264".into(),
            width: NonZero::new(640).unwrap(),
            height: NonZero::new(360).unwrap(),
            frame_rate: None,
        })
    }

    fn project_with(streams: Vec<Stream>) -> (Project, AssetId) {
        let mut project = Project::new("links");
        let info = MediaInfo {
            duration: Some(seconds(4)),
            streams,
        };
        let asset = project.add_asset("a.mkv".into(), info);
        (project, asset)
    }

    fn placed_pair() -> (Project, AssetId, Clip, Clip) {
        let (mut project, asset) = project_with(vec![video(), audio()]);
        let placed = project.place_linked(asset, V1, seconds(2)).unwrap();
        let [picture, sound] = placed[..] else {
            panic!("expected two clips, got {placed:?}");
        };
        (project, asset, picture, sound)
    }

    fn clip(project: &Project, id: ClipId) -> (usize, Clip) {
        project
            .find_clip(id)
            .map(|(track, clip)| (track, *clip))
            .unwrap()
    }

    #[test]
    fn placing_media_with_sound_and_picture_links_a_clip_on_each_kind_of_track() {
        let (project, _, picture, sound) = placed_pair();

        assert_eq!(clip(&project, picture.id).0, V1);
        assert_eq!(clip(&project, sound.id).0, A1);
        assert!(picture.link.is_some() && picture.link == sound.link);
        assert_eq!((sound.start, sound.source), (picture.start, picture.source));
        assert_eq!(project.partners(picture.id), [(A1, sound)]);
        assert_eq!(project.with_partners(&[sound.id]), [sound.id, picture.id]);

        let (mut silent, asset) = project_with(vec![video()]);
        let placed = silent.place_linked(asset, V1, Time::ZERO).unwrap();

        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].link, None);
    }

    #[test]
    fn a_partner_that_would_overlap_refuses_the_whole_placement() {
        let (mut project, asset, ..) = placed_pair();
        let audio_only = project.add_asset(
            "b.wav".into(),
            MediaInfo {
                duration: Some(seconds(4)),
                streams: vec![audio()],
            },
        );
        project.place_clip(audio_only, A1, seconds(10)).unwrap();
        let before = project.clone();

        let refused = project.place_linked(asset, V1, seconds(9));

        assert!(matches!(refused, Err(EditError::Overlapping(_))));
        assert_eq!(project, before);
    }

    #[test]
    fn moving_one_clip_moves_its_partner_by_the_same_time_on_its_own_track() {
        let (mut project, _, picture, sound) = placed_pair();

        project.move_clip(picture.id, V1, seconds(5)).unwrap();

        assert_eq!(
            clip(&project, sound.id),
            (
                A1,
                Clip {
                    start: seconds(5),
                    ..sound
                }
            )
        );

        let second_video = project.timeline.add_track(TrackKind::Video);
        let sound_track = clip(&project, sound.id).0;
        project
            .move_clip(picture.id, second_video, seconds(1))
            .unwrap();

        assert_eq!(clip(&project, picture.id).0, second_video);
        assert_eq!(
            clip(&project, sound.id),
            (
                sound_track,
                Clip {
                    start: seconds(1),
                    ..sound
                }
            )
        );
    }

    #[test]
    fn trimming_one_clip_trims_its_partner_as_far_as_both_can_go() {
        let (mut project, _, picture, sound) = placed_pair();
        let audio_only = project.add_asset(
            "b.wav".into(),
            MediaInfo {
                duration: Some(seconds(4)),
                streams: vec![audio()],
            },
        );
        project
            .trim_clip(picture.id, ClipEdge::End, seconds(4))
            .unwrap();

        assert_eq!(
            clip(&project, sound.id).1.timeline_range().end(),
            seconds(4)
        );

        project.place_clip(audio_only, A1, seconds(5)).unwrap();
        let trimmed = project
            .trim_clip(picture.id, ClipEdge::End, seconds(6))
            .unwrap();

        assert_eq!(trimmed.timeline_range().end(), seconds(5));
        assert_eq!(
            clip(&project, sound.id).1.timeline_range().end(),
            seconds(5)
        );
        assert!(clip(&project, sound.id).1.link.is_some());
    }

    #[test]
    fn splitting_one_clip_splits_its_partner_and_links_the_tails() {
        let (mut project, _, picture, _) = placed_pair();

        let (head, tail) = project.split_clip(picture.id, seconds(3)).unwrap();

        let sound_clips = project.timeline.tracks[A1].clips().to_vec();
        assert_eq!(sound_clips.len(), 2);
        assert_eq!(sound_clips[0].link, head.link);
        assert_eq!(sound_clips[1].link, tail.link);
        assert_ne!(head.link, tail.link);
        assert_eq!(sound_clips[1].start, seconds(3));
        assert_eq!(project.partners(tail.id).len(), 1);
    }

    #[test]
    fn deleting_one_clip_deletes_its_partner_and_ripple_pulls_both_tracks() {
        let (mut project, asset, picture, sound) = placed_pair();
        let later = project.place_linked(asset, V1, seconds(6)).unwrap();

        project.ripple_delete_clip(picture.id).unwrap();

        assert!(project.find_clip(sound.id).is_none());
        for later in later {
            assert_eq!(clip(&project, later.id).1.start, seconds(2));
        }

        let survivor = project.timeline.tracks[V1].clips()[0].id;
        project.delete_clip(survivor).unwrap();

        assert!(
            project
                .timeline
                .tracks
                .iter()
                .all(|track| track.clips().is_empty())
        );
    }

    #[test]
    fn pasting_both_partners_links_the_copies_and_pasting_one_does_not() {
        let (mut project, _, picture, sound) = placed_pair();

        let pair = project
            .paste_clips(&[(picture, V1, seconds(8)), (sound, A1, seconds(8))])
            .unwrap();

        assert!(pair[0].link.is_some() && pair[0].link == pair[1].link);
        assert_ne!(pair[0].link, picture.link);

        let lone = project.paste_clips(&[(sound, A1, seconds(14))]).unwrap();

        assert_eq!(lone[0].link, None);
    }

    #[test]
    fn an_edit_that_puts_partners_out_of_step_unlinks_them() {
        let (mut project, asset, picture, sound) = placed_pair();
        project
            .trim_clip(picture.id, ClipEdge::End, seconds(4))
            .unwrap();

        project.slip_clip(sound.id, seconds(1)).unwrap();

        assert_eq!(clip(&project, picture.id).1.link, None);
        assert_eq!(clip(&project, sound.id).1.link, None);

        let pair = project.place_linked(asset, V1, seconds(8)).unwrap();
        project.unlink_clips(&[pair[1].id]).unwrap();

        assert!(project.partners(pair[0].id).is_empty());
    }
}
