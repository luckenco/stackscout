use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify_debouncer_full::{DebounceEventResult, Debouncer, NoCache, new_debouncer_opt, notify};

use crate::model;

const MAX_PATHS: usize = 4096;
pub(super) type Watcher = Debouncer<notify::RecommendedWatcher, NoCache>;

#[derive(Default)]
pub(super) struct Pending {
    pub watching: bool,
    pub recovery_needed: bool,
    pub error: Option<String>,
    epoch: u64,
    rescan: bool,
    paths: BTreeSet<String>,
}

impl Pending {
    pub fn recover(&mut self, error: Option<String>) {
        self.epoch = self.epoch.wrapping_add(1);
        self.rescan = true;
        self.recovery_needed = true;
        self.paths.clear();
        if error.is_some() {
            self.error = error;
        }
    }

    pub fn take(&mut self) -> Option<Option<Vec<String>>> {
        if std::mem::take(&mut self.rescan) {
            self.paths.clear();
            return Some(None);
        }
        if self.paths.is_empty() {
            return None;
        }
        Some(Some(std::mem::take(&mut self.paths).into_iter().collect()))
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn reconciled(&mut self, epoch: u64) {
        if self.epoch == epoch {
            self.recovery_needed = false;
            if self.watching {
                self.error = None;
            }
        }
    }

    fn events(&mut self, root: &Path, result: DebounceEventResult) {
        let events = match result {
            Ok(events) => events,
            Err(errors) => {
                self.watching = false;
                self.recover(Some(format!("watcher error: {errors:?}")));
                return;
            }
        };
        for event in events {
            if event.need_rescan() {
                self.recover(Some("watcher lost events".to_owned()));
                continue;
            }
            if event.kind.is_access() {
                continue;
            }
            for path in &event.paths {
                let Ok(relative) = path.strip_prefix(root) else {
                    self.recover(None);
                    continue;
                };
                // These control files affect discovery even though hidden files are excluded.
                if matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some(".ignore" | ".gitignore" | ".gitconfig")
                ) || relative == Path::new(".git/info/exclude")
                    || relative == Path::new(".git/config")
                {
                    self.recover(None);
                    continue;
                }
                if relative
                    .components()
                    .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
                {
                    continue;
                }
                let source = model::Syntax::for_path(relative).is_some();
                if path.is_dir()
                    || matches!(
                        event.kind,
                        notify::EventKind::Remove(notify::event::RemoveKind::Folder)
                            | notify::EventKind::Create(notify::event::CreateKind::Folder)
                    )
                {
                    self.recover(None);
                    continue;
                }
                if matches!(
                    event.kind,
                    notify::EventKind::Remove(notify::event::RemoveKind::File)
                ) && !source
                {
                    continue;
                }
                if event.kind.is_remove()
                    || matches!(
                        event.kind,
                        notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                    )
                {
                    // Unknown removed/renamed paths can be directories with cached descendants.
                    self.recover(None);
                    continue;
                }
                if !source {
                    continue;
                }
                let Some(relative) = relative.to_str() else {
                    self.recover(Some("non-UTF-8 watcher path".to_owned()));
                    continue;
                };
                if !self.rescan {
                    self.paths.insert(relative.to_owned());
                    if self.paths.len() > MAX_PATHS {
                        self.recover(Some("watcher invalidation queue overflow".to_owned()));
                    }
                }
            }
        }
    }
}

pub(super) fn start(root: PathBuf, pending: Arc<Mutex<Pending>>) -> notify::Result<Watcher> {
    let callback_root = root.clone();
    let callback_pending = pending.clone();
    // Invalidation does not require file-ID rename stitching or a second cached tree walk.
    let mut watcher = new_debouncer_opt(
        Duration::from_millis(100),
        None,
        move |events| {
            callback_pending
                .lock()
                .expect("watch state poisoned")
                .events(&callback_root, events);
        },
        NoCache,
        notify::Config::default().with_follow_symlinks(false),
    )?;
    watcher.watch(&root, notify::RecursiveMode::Recursive)?;
    pending.lock().expect("watch state poisoned").watching = true;
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, Event, EventKind, Flag, ModifyKind, RemoveKind, RenameMode};
    use notify_debouncer_full::DebouncedEvent;
    use std::time::Instant;

    #[test]
    fn non_source_build_output_does_not_request_rescan() {
        let root = Path::new("/repo");
        let mut pending = Pending {
            watching: true,
            ..Pending::default()
        };
        pending.events(
            root,
            Ok(vec![DebouncedEvent::new(
                Event::new(EventKind::Modify(ModifyKind::Any))
                    .add_path(root.join("target/debug/deps/libfoo.rlib")),
                Instant::now(),
            )]),
        );
        assert_eq!(pending.take(), None);
        assert!(!pending.recovery_needed);
        assert_eq!(pending.epoch(), 0);
    }

    #[test]
    fn non_source_file_removal_does_not_request_rescan() {
        let root = Path::new("/repo");
        let mut pending = Pending::default();
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Remove(RemoveKind::File),
        ] {
            pending.events(
                root,
                Ok(vec![DebouncedEvent::new(
                    Event::new(kind).add_path(root.join("target/debug/libfoo.rlib")),
                    Instant::now(),
                )]),
            );
        }
        assert_eq!(pending.take(), None);
        assert!(!pending.recovery_needed);
    }

    #[test]
    fn discovery_changes_still_request_rescan() {
        let root = Path::new("/repo");
        for event in [
            Event::new(EventKind::Remove(RemoveKind::Any)).add_path(root.join("src")),
            Event::new(EventKind::Remove(RemoveKind::Any))
                .add_path(root.join("target/debug/libfoo.rlib")),
            Event::new(EventKind::Remove(RemoveKind::File)).add_path(root.join("src/lib.rs")),
            Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(root.join("target")),
            Event::new(EventKind::Create(CreateKind::Folder)).add_path(root.join("src")),
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path(root.join("target/debug/libfoo.rlib"))
                .add_path(root.join("src/foo.rs")),
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                .add_path(root.join("src")),
            Event::new(EventKind::Create(CreateKind::File)).add_path(root.join(".gitignore")),
            Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join(".git/config")),
        ] {
            let mut pending = Pending::default();
            pending.events(root, Ok(vec![DebouncedEvent::new(event, Instant::now())]));
            assert_eq!(pending.take(), Some(None));
            assert!(pending.recovery_needed);
        }
    }

    #[test]
    fn source_changes_still_invalidate_paths() {
        let root = Path::new("/repo");
        let mut pending = Pending::default();
        pending.events(
            root,
            Ok(vec![DebouncedEvent::new(
                Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join("src/lib.rs")),
                Instant::now(),
            )]),
        );
        assert_eq!(pending.take(), Some(Some(vec!["src/lib.rs".to_owned()])));
        assert!(!pending.recovery_needed);
    }

    #[test]
    fn overflow_and_events_during_recovery_are_not_lost() {
        let root = Path::new("/repo");
        let mut pending = Pending {
            watching: true,
            ..Pending::default()
        };
        let events = (0..=MAX_PATHS)
            .map(|i| {
                DebouncedEvent::new(
                    Event::new(EventKind::Modify(ModifyKind::Any))
                        .add_path(root.join(format!("f{i}.rs"))),
                    Instant::now(),
                )
            })
            .collect();
        pending.events(root, Ok(events));
        assert!(pending.recovery_needed);
        assert!(pending.paths.is_empty());
        assert_eq!(pending.take(), Some(None));
        let epoch = pending.epoch();
        pending.events(
            root,
            Ok(vec![DebouncedEvent::new(
                Event::new(EventKind::Other).set_flag(Flag::Rescan),
                Instant::now(),
            )]),
        );
        pending.reconciled(epoch);
        assert!(pending.recovery_needed);
        assert_eq!(pending.take(), Some(None));
        pending.reconciled(pending.epoch());
        assert!(!pending.recovery_needed);
        assert!(pending.error.is_none());
    }
}
