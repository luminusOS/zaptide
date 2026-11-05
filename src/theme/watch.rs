//! Filesystem notifications wake theme loading without periodic UI repaints.

use std::{cell::Cell, path::Path, rc::Rc, sync::Arc};

use gtk4::gio::{
    self, FileMonitorEvent, FileMonitorFlags,
    prelude::{FileExt, FileMonitorExt},
};

/// Directory monitor whose events arrive on the thread-default main context
/// that was current when it was created, so create it on the context that
/// polls [`ThemeWatch::take_changed`].
pub(super) struct ThemeWatch {
    _monitor: gio::FileMonitor,
    changed: Rc<Cell<bool>>,
    waker: Arc<dyn crate::backend::Wake>,
}

impl ThemeWatch {
    pub(super) fn new(
        local: &Path,
        waker: Arc<dyn crate::backend::Wake>,
    ) -> Result<Self, gtk4::glib::Error> {
        let changed = Rc::new(Cell::new(false));
        let signal = changed.clone();
        let wake = waker.clone();
        let monitor = gio::File::for_path(local)
            .monitor_directory(FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)?;
        monitor.connect_changed(move |_, _, _, event| {
            if is_change(event) && !signal.replace(true) {
                wake.wake();
            }
        });
        Ok(Self {
            _monitor: monitor,
            changed,
            waker,
        })
    }

    pub(super) fn mark_changed(&self) {
        if !self.changed.replace(true) {
            self.waker.wake();
        }
    }

    pub(super) fn take_changed(&self) -> bool {
        self.changed.replace(false)
    }
}

fn is_change(event: FileMonitorEvent) -> bool {
    matches!(
        event,
        FileMonitorEvent::Created
            | FileMonitorEvent::Changed
            | FileMonitorEvent::AttributeChanged
            | FileMonitorEvent::Deleted
            | FileMonitorEvent::Moved
            | FileMonitorEvent::MovedIn
            | FileMonitorEvent::MovedOut
            | FileMonitorEvent::Renamed
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, Instant},
    };

    fn drain(context: &gtk4::glib::MainContext) {
        while context.iteration(false) {}
    }

    #[test]
    fn a_palette_replacement_signals_once_without_idle_notifications() {
        let directory = tempfile::tempdir().unwrap();
        let local = directory.path().join("themes");
        fs::create_dir_all(&local).unwrap();
        let context = gtk4::glib::MainContext::new();
        context
            .with_thread_default(|| {
                let watch = ThemeWatch::new(&local, Arc::new(crate::backend::Waker)).unwrap();
                drain(&context);
                assert!(!watch.take_changed());
                let temporary = directory.path().join("next");
                fs::write(&temporary, "{}").unwrap();
                fs::rename(temporary, local.join("palette.json")).unwrap();
                let deadline = Instant::now() + Duration::from_secs(3);
                while !watch.take_changed() {
                    assert!(Instant::now() < deadline, "theme change was not delivered");
                    if !context.iteration(false) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                drain(&context);
                assert!(!watch.take_changed());
            })
            .unwrap();
    }
}
