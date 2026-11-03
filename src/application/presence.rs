//! Whether WhatsApp shows the active account as online. WhatsApp holds the
//! phone's notifications while ZapTide is online, so ZapTide goes offline a
//! short time after you leave it and after a time without input.

use super::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// What the window knows about the use of ZapTide.
pub(super) struct PresenceWatch {
    /// The last input event in the main window.
    last_input: Rc<Cell<Instant>>,
    /// Set while only inactivity keeps ZapTide offline; the next input reports again.
    waiting_for_input: Rc<Cell<bool>>,
    /// Whether the window had focus on an unlocked screen at the last report.
    looking: bool,
    /// When you left ZapTide; `None` while you look at it or before you first do.
    left_at: Option<Instant>,
    /// When the scheduled presence check runs.
    pub(super) next_check: Option<Instant>,
    /// What the active account's backend was last told.
    pub(super) reported: Option<bool>,
}

impl Default for PresenceWatch {
    fn default() -> Self {
        Self {
            last_input: Rc::new(Cell::new(Instant::now())),
            waiting_for_input: Rc::default(),
            looking: false,
            left_at: None,
            next_check: None,
            reported: None,
        }
    }
}

/// Input events that show that a person uses the window.
fn shows_use(event: gtk::gdk::EventType) -> bool {
    use gtk::gdk::EventType;
    matches!(
        event,
        EventType::KeyPress
            | EventType::ButtonPress
            | EventType::MotionNotify
            | EventType::Scroll
            | EventType::TouchBegin
            | EventType::TouchUpdate
            | EventType::TouchpadSwipe
            | EventType::TouchpadPinch
            | EventType::TouchpadHold
    )
}

/// Converts a stored setting to a limit; 0 is never.
fn limit(value: u32, unit: u64) -> Option<Duration> {
    (value > 0).then(|| Duration::from_secs(unit * u64::from(value)))
}

/// Whether to show you as online: until `away` passes the leave limit and
/// until `quiet`, the time since the last input, passes the inactivity limit.
fn online(
    looking: bool,
    away: Duration,
    quiet: Duration,
    leave_limit: Option<Duration>,
    idle_limit: Option<Duration>,
) -> bool {
    let present = looking || leave_limit.is_none_or(|limit| away < limit);
    present && idle_limit.is_none_or(|limit| quiet < limit)
}

impl NativeApplication {
    /// Records input in the main window and follows the screensaver.
    pub(super) fn watch_presence(&self, sender: &ComponentSender<Self>) {
        let input = gtk::EventControllerLegacy::new();
        input.set_propagation_phase(gtk::PropagationPhase::Capture);
        let last_input = self.presence_watch.last_input.clone();
        let waiting = self.presence_watch.waiting_for_input.clone();
        let activity = sender.clone();
        input.connect_event(move |_, event| {
            if shows_use(event.event_type()) {
                last_input.set(Instant::now());
                if waiting.replace(false) {
                    activity.input(Input::PresenceChanged);
                }
            }
            gtk::glib::Propagation::Proceed
        });
        self.window.add_controller(input);
        let screensaver = sender.clone();
        relm4::main_application()
            .connect_screensaver_active_notify(move |_| screensaver.input(Input::PresenceChanged));
    }

    /// Tells the active account's backend whether to show you as online.
    pub(super) fn sync_presence(&mut self, sender: &ComponentSender<Self>) {
        let looking = self.window.is_active()
            && self.window.is_visible()
            && !relm4::main_application().is_screensaver_active();
        let leave_limit = limit(self.settings.offline_after_leaving_seconds, 1);
        let idle_limit = limit(self.settings.offline_after_minutes, 60);
        let watch = &mut self.presence_watch;
        let now = Instant::now();
        if looking {
            watch.left_at = None;
        } else if watch.looking {
            watch.left_at = Some(now);
        }
        watch.looking = looking;
        let away = watch.left_at.map_or(Duration::MAX, |left| now - left);
        let last_input = watch.last_input.get();
        let online = online(looking, away, now - last_input, leave_limit, idle_limit);
        watch
            .waiting_for_input
            .set(!online && self::online(looking, away, Duration::ZERO, leave_limit, idle_limit));
        if online {
            let left = watch
                .left_at
                .zip(leave_limit)
                .map(|(left, limit)| left + limit);
            let idle = idle_limit.map(|limit| last_input + limit);
            let due = left.into_iter().chain(idle).min();
            // A stale check only reports again, so an earlier one never needs removing.
            if let Some(due) = due
                && watch.next_check.is_none_or(|planned| due < planned)
            {
                watch.next_check = Some(due);
                let check = sender.clone();
                gtk::glib::timeout_add_local_once(due.saturating_duration_since(now), move || {
                    check.input(Input::PresenceChanged)
                });
            }
        }
        if watch.reported != Some(online)
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::SetOnline(online));
            watch.reported = Some(online);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEN_SECONDS: Option<Duration> = Some(Duration::from_secs(10));
    const FIVE_MINUTES: Option<Duration> = Some(Duration::from_secs(300));

    #[test]
    fn leaving_zaptide_goes_offline_after_the_leave_limit() {
        let busy = Duration::from_secs(1);
        let left = |away| {
            online(
                false,
                Duration::from_secs(away),
                busy,
                TEN_SECONDS,
                FIVE_MINUTES,
            )
        };
        assert!(online(true, Duration::MAX, busy, TEN_SECONDS, FIVE_MINUTES));
        assert!(left(9));
        assert!(!left(10));
        assert!(online(false, Duration::MAX, busy, None, FIVE_MINUTES));
    }

    #[test]
    fn inactivity_goes_offline_even_while_looking() {
        let quiet = Duration::from_secs(300);
        assert!(!online(
            true,
            Duration::MAX,
            quiet,
            TEN_SECONDS,
            FIVE_MINUTES
        ));
        assert!(!online(false, Duration::MAX, quiet, None, FIVE_MINUTES));
        assert!(online(false, Duration::MAX, quiet, None, None));
    }

    #[test]
    fn zero_means_never() {
        assert_eq!(limit(0, 60), None);
        assert_eq!(limit(5, 60), FIVE_MINUTES);
    }

    #[test]
    fn only_input_counts_as_use() {
        use gtk::gdk::EventType;
        assert!(shows_use(EventType::KeyPress));
        assert!(shows_use(EventType::MotionNotify));
        assert!(!shows_use(EventType::EnterNotify));
        assert!(!shows_use(EventType::FocusChange));
    }
}
