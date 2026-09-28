use std::sync::{Arc, Mutex, PoisonError};

use tessera_timeline::{Project, Time};

use crate::{CHANNELS, Error, Mixer, Output};

pub struct TimelinePlayback {
    output: Output,
    pending: Arc<Mutex<Option<Project>>>,
}

impl TimelinePlayback {
    pub fn start(project: Project, from: Time) -> Result<Self, Error> {
        let sample_rate = project.settings.sample_rate;
        let pending = Arc::new(Mutex::new(None::<Project>));
        let mut mixer = Mixer::new(project);
        let mut next = from.to_samples(sample_rate);
        let output = Output::start(sample_rate, {
            let pending = pending.clone();
            move |block: &mut [f32]| {
                let updated = pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                if let Some(project) = updated {
                    mixer.set_project(project);
                }
                mixer.render(next, block);
                next += (block.len() / CHANNELS) as i64;
            }
        })?;
        Ok(Self { output, pending })
    }

    pub fn elapsed(&self) -> Time {
        Time::from_samples(self.output.position(), self.output.sample_rate())
    }

    pub fn update_project(&self, project: Project) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(project);
    }
}
