//! Bounded backend-event application for the native main-context root.

use crate::{
    backend::{Backend, EVENT_BATCH_LIMIT, Event},
    notifier::EventNotifier,
};

/// Applies one bounded FIFO batch and schedules continuation when it fills.
pub fn drain_backend_events(
    backend: &Backend,
    notifier: &EventNotifier,
    mut apply: impl FnMut(Event),
) -> usize {
    let events = backend.poll_batch(EVENT_BATCH_LIMIT);
    let count = events.len();
    for event in events {
        apply(event);
    }
    if count == EVENT_BATCH_LIMIT {
        notifier.notify();
    }
    count
}
