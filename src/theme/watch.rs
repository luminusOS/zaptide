//! Filesystem notifications wake theme loading without periodic UI repaints.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

pub(super) struct ThemeWatch {
    _watcher: RecommendedWatcher,
    changed: Arc<AtomicBool>,
}

impl ThemeWatch {
    pub(super) fn new(local: &Path, waker: Arc<dyn crate::backend::Wake>) -> notify::Result<Self> {
        let changed = Arc::new(AtomicBool::new(false));
        let signal = changed.clone();
        let wake = waker;
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let changed = event.is_ok_and(|event| {
                    event.kind.is_create() || event.kind.is_modify() || event.kind.is_remove()
                });
                if changed && !signal.swap(true, Ordering::AcqRel) {
                    wake.wake();
                }
            })?;
        watcher.watch(local, RecursiveMode::NonRecursive)?;
        Ok(Self {
            _watcher: watcher,
            changed,
        })
    }

    pub(super) fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, Instant},
    };

    #[test]
    fn a_palette_replacement_signals_once_without_idle_notifications() {
        let directory = tempfile::tempdir().unwrap();
        let local = directory.path().join("themes");
        fs::create_dir_all(&local).unwrap();
        let watch = ThemeWatch::new(&local, Arc::new(crate::backend::Waker)).unwrap();
        assert!(!watch.take_changed());
        let temporary = directory.path().join("next");
        fs::write(&temporary, "{}").unwrap();
        fs::rename(temporary, local.join("palette.json")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !watch.take_changed() {
            assert!(Instant::now() < deadline, "theme change was not delivered");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!watch.take_changed());
    }
}
