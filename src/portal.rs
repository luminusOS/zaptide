//! The runtime every ashpd portal call runs on.
//!
//! ashpd keeps one session bus connection for the whole process, and a
//! connection stops working when the runtime that opened it is dropped. One
//! runtime that lives as long as the process keeps it usable from any thread.
//! This module also owns sandbox detection, [`sandboxed`].

use std::ffi::OsStr;
use std::future::Future;
use std::sync::mpsc;
use std::sync::{LazyLock, OnceLock};

/// True inside a Flatpak, where portals are the way out of the sandbox. Either
/// sign counts: the info file Flatpak mounts, as GLib checks, or a non-empty
/// `FLATPAK_ID`.
pub fn sandboxed() -> bool {
    static SANDBOXED: OnceLock<bool> = OnceLock::new();
    *SANDBOXED.get_or_init(|| {
        is_sandboxed(
            std::path::Path::new("/.flatpak-info").exists(),
            std::env::var_os("FLATPAK_ID").as_deref(),
        )
    })
}

fn is_sandboxed(flatpak_info_exists: bool, flatpak_id: Option<&OsStr>) -> bool {
    flatpak_info_exists || flatpak_id.is_some_and(|id| !id.is_empty())
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("zaptide-portal")
            .enable_all()
            .build()
            .expect("portal runtime")
    });
    &RUNTIME
}

/// Keeps runtime APIs available to synchronous code until the guard is dropped.
pub(crate) fn enter() -> tokio::runtime::EnterGuard<'static> {
    runtime().enter()
}

/// Starts `future` on the portal runtime without waiting for it.
pub fn spawn(future: impl Future<Output = ()> + Send + 'static) {
    runtime().spawn(future);
}

/// Runs `future` on the portal runtime and waits for its result, which blocks
/// the calling thread: use it from a thread that may block, never from the
/// GTK main thread. `None` means the future did not finish.
pub fn run<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> Option<T> {
    let (sender, receiver) = mpsc::sync_channel(1);
    runtime().spawn(async move {
        let _ = sender.send(future.await);
    });
    receiver.recv().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn a_flatpak_is_sandboxed_by_its_info_file_or_its_id() {
        assert!(is_sandboxed(true, None));
        assert!(is_sandboxed(
            false,
            Some(OsStr::new("dev.luminusos.ZapTide"))
        ));
        assert!(!is_sandboxed(false, None));
        assert!(!is_sandboxed(false, Some(&OsString::new())));
    }

    #[test]
    fn a_result_comes_back_from_any_thread() {
        assert_eq!(run(async { 7 }), Some(7));
        let from_another = std::thread::spawn(|| run(async { "ok" }))
            .join()
            .expect("the thread finishes");
        assert_eq!(from_another, Some("ok"));
    }

    #[test]
    fn it_can_be_called_from_inside_another_runtime_s_blocking_thread() {
        let other = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let value = other
            .block_on(async { tokio::task::spawn_blocking(|| run(async { 3 })).await })
            .expect("the blocking task finishes");
        assert_eq!(value, Some(3));
    }

    #[test]
    fn a_panicking_future_gives_none_instead_of_hanging() {
        let result: Option<()> = run(async { panic!("portal task failed on purpose") });
        assert_eq!(result, None);
    }

    #[test]
    fn spawned_work_runs_without_the_caller_waiting() {
        let (sender, receiver) = mpsc::channel();
        spawn(async move {
            let _ = sender.send(1);
        });
        assert_eq!(
            receiver.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(1)
        );
    }
}
