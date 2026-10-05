use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

use gpui::Global;
use tessera_document::EXTENSION;
use tessera_timeline::Project;
use thiserror::Error;

pub const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);
const SOURCE_EXTENSION: &str = "source";
const PROCESSES: &str = "/proc";

static NEXT_SLOT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutosaveDirectory(pub PathBuf);

impl Global for AutosaveDirectory {}

#[derive(Debug, Error)]
pub enum AutosaveError {
    #[error(transparent)]
    Document(#[from] tessera_document::Error),
    #[error("could not record which file {path} belongs to: {source}")]
    Source {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not take over {path}: {source}")]
    Adopt {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    project: PathBuf,
    source: PathBuf,
}

impl Slot {
    pub fn claim(directory: &Path) -> Self {
        let index = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
        Self::named(directory, &format!("{}-{index}", std::process::id()))
    }

    pub(crate) fn named(directory: &Path, stem: &str) -> Self {
        Self {
            project: directory.join(format!("{stem}.{EXTENSION}")),
            source: directory.join(format!("{stem}.{SOURCE_EXTENSION}")),
        }
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    pub fn write(&self, project: &Project, file: Option<&Path>) -> Result<(), AutosaveError> {
        if let Some(directory) = self.project.parent() {
            fs::create_dir_all(directory).map_err(|source| AutosaveError::Source {
                path: directory.to_owned(),
                source,
            })?;
        }
        tessera_document::save(project, &self.project)?;
        let recorded = match file.and_then(Path::to_str) {
            Some(file) => fs::write(&self.source, file),
            None => remove_if_present(&self.source),
        };
        recorded.map_err(|source| AutosaveError::Source {
            path: self.source.clone(),
            source,
        })
    }

    pub fn remove(&self) {
        for path in [&self.project, &self.source] {
            if let Err(error) = remove_if_present(path) {
                tracing::warn!(path = %path.display(), %error, "cannot remove the autosave");
            }
        }
    }

    pub fn adopt(&self, orphan: &Orphan) -> Result<(), AutosaveError> {
        let failed = |path: &Path| {
            let path = path.to_owned();
            move |source| AutosaveError::Adopt { path, source }
        };
        fs::rename(&orphan.slot.project, &self.project).map_err(failed(&orphan.slot.project))?;
        remove_if_present(&self.source).map_err(failed(&self.source))?;
        match fs::rename(&orphan.slot.source, &self.source) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            moved => moved.map_err(failed(&orphan.slot.source)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Orphan {
    pub slot: Slot,
    pub file: Option<PathBuf>,
    pub modified: SystemTime,
}

impl Orphan {
    pub fn recover(&self) -> Result<Project, tessera_document::Error> {
        tessera_document::open(&self.slot.project)
    }

    pub fn discard(&self) {
        self.slot.remove();
    }
}

pub fn orphans(directory: &Path) -> Vec<Orphan> {
    orphans_where(directory, |pid| {
        pid == std::process::id() || Path::new(PROCESSES).join(pid.to_string()).exists()
    })
}

fn orphans_where(directory: &Path, alive: impl Fn(u32) -> bool) -> Vec<Orphan> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found: Vec<Orphan> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()?.to_str()? != EXTENSION {
                return None;
            }
            let stem = path.file_stem()?.to_str()?;
            let pid = stem.split_once('-')?.0.parse::<u32>().ok()?;
            if alive(pid) {
                return None;
            }
            let slot = Slot::named(directory, stem);
            let file = fs::read_to_string(&slot.source)
                .ok()
                .map(|file| PathBuf::from(file.trim_end_matches('\n')));
            let modified = entry.metadata().and_then(|meta| meta.modified()).ok()?;
            Some(Orphan {
                slot,
                file,
                modified,
            })
        })
        .collect();
    found.sort_by_key(|orphan| std::cmp::Reverse(orphan.modified));
    found
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        removed => removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("tessera-autosave-{}-{name}", std::process::id()));
        fs::remove_dir_all(&directory).ok();
        directory
    }

    #[test]
    fn a_slot_left_by_a_process_that_is_gone_is_an_orphan_and_can_be_taken_over() {
        let directory = scratch("orphans");
        let project = Project::new("crashed");
        let gone = Slot::named(&directory, "41-0");
        let running = Slot::named(&directory, "42-0");
        gone.write(&project, Some(Path::new("/projects/cut.tessera")))
            .unwrap();
        running.write(&project, None).unwrap();

        let found = orphans_where(&directory, |pid| pid == 42);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].slot, gone);
        assert_eq!(found[0].file, Some(PathBuf::from("/projects/cut.tessera")));
        assert_eq!(found[0].recover().unwrap().name, "crashed");

        let mine = Slot::named(&directory, "43-0");
        mine.adopt(&found[0]).unwrap();

        assert!(mine.project().exists() && mine.source.exists());
        assert!(orphans_where(&directory, |pid| pid != 41).is_empty());

        mine.remove();
        running.remove();

        assert!(orphans_where(&directory, |_| false).is_empty());
        fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn an_untitled_project_records_no_file_and_forgets_an_old_one() {
        let directory = scratch("untitled");
        let slot = Slot::named(&directory, "41-0");

        slot.write(&Project::new("a"), Some(Path::new("/a.tessera")))
            .unwrap();
        slot.write(&Project::new("a"), None).unwrap();

        assert!(!slot.source.exists());
        assert_eq!(orphans_where(&directory, |_| false)[0].file, None);
        fs::remove_dir_all(&directory).ok();
    }
}
