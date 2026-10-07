use std::sync::{Arc, Mutex, PoisonError};

use tessera_timeline::{Project, Time};

use crate::{Error, Mixer, Output};

pub struct TimelinePlayback {
    output: Output,
    pending: Arc<Mutex<Option<Project>>>,
    sounding: Project,
}

impl TimelinePlayback {
    pub fn start(project: Project, from: Time) -> Result<Self, Error> {
        let sample_rate = project.settings.sample_rate;
        let pending = Arc::new(Mutex::new(None::<Project>));
        let mut mixer = Mixer::new(project.clone());
        let start = from.to_samples(sample_rate);
        let output = Output::start(sample_rate, {
            let pending = pending.clone();
            move |first: i64, block: &mut [f32]| {
                let updated = pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                if let Some(project) = updated {
                    mixer.set_project(project);
                }
                mixer.render(start + first, block);
            }
        })?;
        Ok(Self {
            output,
            pending,
            sounding: project,
        })
    }

    pub fn elapsed(&self) -> Time {
        Time::from_samples(self.output.position(), self.output.sample_rate())
    }

    pub fn update_project(&mut self, project: Project) {
        if sounds_the_same(&self.sounding, &project) {
            return;
        }
        self.sounding = project.clone();
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(project);
        self.output.flush();
    }
}

fn sounds_the_same(playing: &Project, changed: &Project) -> bool {
    playing.timeline == changed.timeline
        && playing.assets == changed.assets
        && playing.settings == changed.settings
}

#[cfg(test)]
mod tests {
    use tessera_timeline::{AudioStream, MediaInfo, Stream, TrackKind};

    use super::*;

    #[test]
    fn only_a_change_to_what_is_heard_replaces_the_mix() {
        let mut project = Project::new("heard");

        let mut marked = project.clone();
        marked.add_marker(Time::from_seconds(1), "beat");
        marked.set_in_point(Time::from_seconds(2)).unwrap();

        assert!(sounds_the_same(&project, &marked));

        let audio = project
            .timeline
            .tracks
            .iter()
            .position(|track| track.kind == TrackKind::Audio)
            .unwrap();
        let info = MediaInfo {
            duration: Some(Time::from_seconds(2)),
            streams: vec![Stream::Audio(AudioStream::new(
                0,
                "flac",
                std::num::NonZero::new(48_000).unwrap(),
                std::num::NonZero::new(2).unwrap(),
            ))],
        };
        let mut placed = project.clone();
        let asset = placed.add_asset("/media/tone.flac".into(), info);
        placed.place_clip(asset, audio, Time::ZERO).unwrap();
        project.timeline.tracks[audio].muted = true;

        assert!(!sounds_the_same(&marked, &placed));
        assert!(!sounds_the_same(&marked, &project));
    }
}
