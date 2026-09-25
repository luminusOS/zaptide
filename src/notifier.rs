//! Coalesced cross-thread notification without a toolkit dependency.

use std::sync::{Arc, Mutex, mpsc};

struct Shared {
    state: Mutex<NotifierState>,
    scheduler: Mutex<Option<Scheduler>>,
}

#[derive(Default)]
struct NotifierState {
    pending: bool,
    scheduled: bool,
    schedule_generation: u64,
    closed: bool,
}

type Scheduler = Arc<dyn Fn(u64) + Send + Sync>;

/// Cloneable sender held by backend and worker services.
#[derive(Clone)]
pub struct EventNotifier {
    shared: Arc<Shared>,
    wake: mpsc::Sender<()>,
}

/// Receiver held only by the application main-context integration.
pub struct EventDrain {
    shared: Arc<Shared>,
    wake: mpsc::Receiver<()>,
}

impl EventNotifier {
    /// Creates a coalesced sender/receiver pair.
    pub fn new() -> (Self, EventDrain) {
        let shared = Arc::new(Shared {
            state: Mutex::new(NotifierState::default()),
            scheduler: Mutex::new(None),
        });
        let (wake, receiver) = mpsc::channel();
        (
            Self {
                shared: Arc::clone(&shared),
                wake,
            },
            EventDrain {
                shared,
                wake: receiver,
            },
        )
    }

    /// Schedules one main-context drain while work is pending.
    pub fn notify(&self) {
        let schedule = {
            let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.closed || state.pending {
                return;
            }
            state.pending = true;

            if self.wake.send(()).is_err() {
                state.closed = true;
                state.pending = false;
                return;
            }
            if state.scheduled {
                None
            } else if let Some(schedule) = self
                .shared
                .scheduler
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
            {
                state.scheduled = true;
                state.schedule_generation = state.schedule_generation.wrapping_add(1);
                Some((schedule, state.schedule_generation))
            } else {
                None
            }
        };

        if let Some((schedule, generation)) = schedule {
            schedule(generation);
        }
    }

    /// Installs the application-main-context scheduling hook.
    ///
    /// The hook must arrange one main-context turn that calls
    /// [`EventDrain::try_begin_scheduled_drain`] with its generation and capture application state
    /// weakly. It is never called while notifier state is locked.
    pub fn set_scheduler(&self, scheduler: impl Fn(u64) + Send + Sync + 'static) {
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        let scheduler: Scheduler = Arc::new(scheduler);
        *self
            .shared
            .scheduler
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&scheduler));
        let schedule_now = state.pending && !state.closed;
        if schedule_now {
            state.scheduled = true;
            state.schedule_generation = state.schedule_generation.wrapping_add(1);
        }
        let generation = state.schedule_generation;
        drop(state);
        if schedule_now {
            scheduler(generation);
        }
    }
}

impl EventDrain {
    /// Takes a drain scheduled by the current main-context scheduler generation.
    pub fn try_begin_scheduled_drain(&self, generation: u64) -> bool {
        self.try_begin(Some(generation))
    }

    fn try_begin(&self, generation: Option<u64>) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed
            || !state.pending
            || generation.is_some_and(|generation| generation != state.schedule_generation)
        {
            return false;
        }
        if self.wake.try_recv().is_ok() {
            state.pending = false;
            state.scheduled = false;
            true
        } else {
            state.pending = false;
            state.scheduled = false;
            false
        }
    }

    /// Makes future notifications inert during application shutdown.
    pub fn close(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        state.closed = true;
        state.pending = false;
        state.scheduled = false;
        state.schedule_generation = state.schedule_generation.wrapping_add(1);
        *self
            .shared
            .scheduler
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        while self.wake.try_recv().is_ok() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_scheduler_reclaims_a_pending_wake() {
        let (notifier, drain) = EventNotifier::new();
        let old_generation = Arc::new(Mutex::new(None));
        let observed = Arc::clone(&old_generation);
        notifier.set_scheduler(move |generation| {
            *observed.lock().unwrap_or_else(|p| p.into_inner()) = Some(generation);
        });
        notifier.notify();
        let old_generation = old_generation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .expect("first scheduler invoked");

        let new_generation = Arc::new(Mutex::new(None));
        let observed = Arc::clone(&new_generation);
        notifier.set_scheduler(move |generation| {
            *observed.lock().unwrap_or_else(|p| p.into_inner()) = Some(generation);
        });
        let new_generation = new_generation
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .expect("replacement scheduler invoked");

        assert_ne!(old_generation, new_generation);
        assert!(!drain.try_begin_scheduled_drain(old_generation));
        assert!(drain.try_begin_scheduled_drain(new_generation));
    }
}
