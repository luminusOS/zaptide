//! GLib main-context adapter for [`crate::notifier::EventNotifier`].

use std::sync::{Arc, Mutex};

use gtk4::glib;

use crate::notifier::{EventDrain, EventNotifier};

type Callback = Arc<dyn Fn() + Send + Sync>;
type CallbackState = Arc<Mutex<Option<Callback>>>;

/// Owns the main-context side of a coalesced backend event notifier.
pub struct GlibEventDrain {
    drain: Arc<Mutex<EventDrain>>,
    callback: CallbackState,
}

impl GlibEventDrain {
    /// Installs a main-context scheduler before backend startup.
    ///
    /// `on_drain` must capture Relm4 component state weakly. The application must keep `context`
    /// owned by its main loop while backend workers are running.
    pub fn install(
        notifier: &EventNotifier,
        drain: EventDrain,
        context: glib::MainContext,
        on_drain: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let drain = Arc::new(Mutex::new(drain));
        let callback = Arc::new(Mutex::new(Some(Arc::new(on_drain) as Callback)));
        let scheduled_drain = Arc::downgrade(&drain);
        let scheduled_callback = Arc::downgrade(&callback);
        notifier.set_scheduler(move |generation| {
            let Some(drain) = scheduled_drain.upgrade() else {
                return;
            };
            let Some(callback) = scheduled_callback.upgrade() else {
                return;
            };
            context.invoke(move || {
                let ready = {
                    let drain = drain.lock().unwrap_or_else(|p| p.into_inner());
                    drain.try_begin_scheduled_drain(generation)
                };
                if ready {
                    // Hold this lock through invocation so close cannot return while a callback
                    // that may touch component state remains in flight.
                    if let Some(callback) =
                        callback.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
                    {
                        callback();
                    }
                }
            });
        });
        Self { drain, callback }
    }

    /// Invalidates pending callbacks before application state is disposed.
    pub fn close(&self) {
        self.callback
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.drain.lock().unwrap_or_else(|p| p.into_inner()).close();
    }
}

impl Drop for GlibEventDrain {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        thread,
    };

    use super::*;

    #[test]
    fn notifier_wakes_the_glib_main_context_once_per_drain() {
        let context = glib::MainContext::new();
        let _acquire = context.acquire().expect("test owns main context");
        let (notifier, drain) = EventNotifier::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_thread = Arc::new(Mutex::new(None));
        let expected_thread = thread::current().id();
        let observed = Arc::clone(&calls);
        let observed_thread = Arc::clone(&callback_thread);
        let _adapter = GlibEventDrain::install(&notifier, drain, context.clone(), move || {
            observed.fetch_add(1, Ordering::Relaxed);
            *observed_thread.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(thread::current().id());
        });

        let producer = notifier.clone();
        thread::spawn(move || {
            producer.notify();
            producer.notify();
        })
        .join()
        .expect("producer completes");
        while calls.load(Ordering::Relaxed) == 0 {
            context.iteration(true);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            *callback_thread.lock().unwrap_or_else(|p| p.into_inner()),
            Some(expected_thread)
        );
    }

    #[test]
    fn close_before_dispatch_rejects_a_queued_callback() {
        let context = glib::MainContext::new();
        let _acquire = context.acquire().expect("test owns main context");
        let (notifier, drain) = EventNotifier::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let adapter = GlibEventDrain::install(&notifier, drain, context.clone(), move || {
            observed.fetch_add(1, Ordering::Relaxed);
        });

        thread::spawn(move || notifier.notify())
            .join()
            .expect("producer completes");
        adapter.close();
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
