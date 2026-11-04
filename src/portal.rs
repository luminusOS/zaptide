//! The runtime every ashpd portal call runs on.
//!
//! ashpd keeps one session bus connection for the whole process, and a
//! connection stops working when the runtime that opened it is dropped. One
//! runtime that lives as long as the process keeps it usable from any thread.

use std::future::Future;
use std::sync::LazyLock;
use std::sync::mpsc;

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
