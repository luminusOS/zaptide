//! Telling the window when the computer woke from a suspend.

use std::time::{Duration, Instant, SystemTime};
use std::{cell::Cell, rc::Rc};

use gtk::gio;
use gtk::glib::variant::ToVariant;
use gtk4 as gtk;
use relm4::ComponentSender;

use super::{Input, NativeApplication, slept_through};

const LOGIN1: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const MATCH_RULE: &str = "type='signal',sender='org.freedesktop.login1',interface='org.freedesktop.login1.Manager',member='PrepareForSleep',path='/org/freedesktop/login1'";

/// Sends `Input::Resumed` after a suspend long enough to kill the sockets.
///
/// logind's `PrepareForSleep` says when the computer sleeps and wakes. Where
/// logind cannot be reached (no system bus, or a Flatpak without
/// `--system-talk-name=org.freedesktop.login1`), a check of the two clocks
/// every 5 s notices the jump instead.
pub(super) fn watch(sender: ComponentSender<NativeApplication>) {
    gtk::glib::spawn_future_local(async move {
        let Some(bus) = logind().await else {
            poll(sender, Rc::new(Cell::new(now())), Duration::from_secs(5));
            return;
        };
        let last = Rc::new(Cell::new(now()));
        let signal_last = Rc::clone(&last);
        let signal_sender = sender.clone();
        let (closed_sender, closed_receiver) = async_channel::bounded(1);
        bus.connect_closed(move |_, _, _| {
            let _ = closed_sender.try_send(());
        });
        let _subscription = bus.subscribe_to_signal(
            Some(LOGIN1),
            Some("org.freedesktop.login1.Manager"),
            Some("PrepareForSleep"),
            Some(LOGIN1_PATH),
            None,
            gio::DBusSignalFlags::NO_MATCH_RULE,
            move |signal| {
                let Some((sleeping,)) = signal.parameters.get::<(bool,)>() else {
                    return;
                };
                let current = now();
                let resumed = sleep_signal(signal_last.get(), sleeping, current);
                if !sleeping {
                    signal_last.set(current);
                }
                if resumed {
                    signal_sender.input(Input::Resumed);
                }
            },
        );
        let match_added = bus
            .call_future(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "AddMatch",
                Some(&(MATCH_RULE,).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                2_000,
            )
            .await
            .is_ok();
        if fallback_needed(match_added, bus.is_closed()) {
            poll(sender, last, Duration::from_secs(5));
            return;
        }
        let watchdog = poll(sender.clone(), Rc::clone(&last), Duration::from_secs(30));
        let _ = closed_receiver.recv().await;
        watchdog.remove();
        drop(_subscription);
        poll(sender, last, Duration::from_secs(5));
    });
}

/// The system bus, if logind answers on it.
async fn logind() -> Option<gio::DBusConnection> {
    let bus = gio::bus_get_future(gio::BusType::System).await.ok()?;
    bus.call_future(
        Some(LOGIN1),
        LOGIN1_PATH,
        "org.freedesktop.DBus.Peer",
        "Ping",
        None,
        None,
        gio::DBusCallFlags::NONE,
        2_000,
    )
    .await
    .ok()?;
    Some(bus)
}

/// Whether waking at `now` follows a sleep that began at `asleep` and lasted
/// as long as the clock check requires.
fn woke_after_sleep(
    asleep: Option<(SystemTime, Instant)>,
    (wall, monotonic): (SystemTime, Instant),
) -> bool {
    asleep.is_some_and(|(slept_wall, slept_monotonic)| {
        slept_through(
            wall.duration_since(slept_wall).unwrap_or_default(),
            monotonic.duration_since(slept_monotonic),
        )
    })
}

type Clocks = (SystemTime, Instant);

fn now() -> Clocks {
    (SystemTime::now(), Instant::now())
}

fn sleep_signal(last: Clocks, sleeping: bool, current: Clocks) -> bool {
    !sleeping && woke_after_sleep(Some(last), current)
}

fn fallback_needed(match_added: bool, closed: bool) -> bool {
    !match_added || closed
}

fn poll(
    sender: ComponentSender<NativeApplication>,
    last: Rc<Cell<Clocks>>,
    interval: Duration,
) -> gtk::glib::SourceId {
    gtk::glib::timeout_add_local(interval, move || {
        let current = now();
        if woke_after_sleep(Some(last.replace(current)), current) {
            sender.input(Input::Resumed);
        }
        gtk::glib::ControlFlow::Continue
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_signal_sequence_detects_one_long_sleep() {
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mono = Instant::now();
        let last = (wall, mono);
        assert!(!sleep_signal(last, true, last));
        let wake = (
            wall + Duration::from_secs(100),
            mono + Duration::from_secs(1),
        );
        assert!(sleep_signal(last, false, wake));
        assert!(!sleep_signal(wake, false, wake));
    }

    #[test]
    fn closed_bus_selects_poll_fallback() {
        assert!(fallback_needed(true, true));
        assert!(fallback_needed(false, false));
        assert!(!fallback_needed(true, false));
    }

    #[test]
    fn watchdog_detects_missed_wake_and_does_not_repeat_it() {
        let last = (SystemTime::UNIX_EPOCH, Instant::now());
        let wake = (
            last.0 + Duration::from_secs(100),
            last.1 + Duration::from_secs(1),
        );
        assert!(!sleep_signal(last, true, last));
        assert!(woke_after_sleep(Some(last), wake));
        assert!(!sleep_signal(wake, false, wake));
    }

    #[test]
    fn waking_counts_only_after_a_long_enough_sleep() {
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let monotonic = Instant::now();
        let asleep = Some((wall, monotonic));
        let later = |wall_secs, monotonic_secs| {
            (
                wall + Duration::from_secs(wall_secs),
                monotonic + Duration::from_secs(monotonic_secs),
            )
        };
        assert!(woke_after_sleep(asleep, later(3_600, 1)));
        assert!(!woke_after_sleep(asleep, later(10, 1)));
        // A sleep that was cancelled leaves both clocks running together.
        assert!(!woke_after_sleep(asleep, later(60, 60)));
        assert!(!woke_after_sleep(None, later(3_600, 1)));
    }

    /// `cargo test application::resume::tests::logind -- --ignored`.
    #[test]
    #[ignore = "needs the system bus"]
    fn logind_answers_on_this_machine() {
        let context = gtk::glib::MainContext::new();
        let reached = context.block_on(logind()).is_some();
        assert!(reached, "logind did not answer on the system bus");
    }
}
