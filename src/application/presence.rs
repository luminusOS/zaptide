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
    /// Set while only inactivity keeps ZapTide offline. Pointer motion is
    /// frequent, so only the first input after idling may wake `update`.
    waiting_for_input: Rc<Cell<bool>>,
    /// Whether the window had focus on an unlocked screen at the last report.
    looking: bool,
    /// When you left ZapTide; `None` while you look at it or before you first do.
    left_at: Option<Instant>,
    /// When the scheduled presence check runs.
    next_check: Option<Instant>,
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

/// `MotionNotify` counts, `EnterNotify` does not: GTK synthesizes crossing
/// events when the layout changes under a still pointer.
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
fn limit(seconds: u32) -> Option<Duration> {
    (seconds > 0).then(|| Duration::from_secs(u64::from(seconds)))
}

/// Whether you count as using ZapTide: looking at it, or away for less than the leave limit.
fn present(looking: bool, away: Duration, leave_limit: Option<Duration>) -> bool {
    looking || leave_limit.is_none_or(|limit| away < limit)
}

/// Whether to show you as online: until `away` passes the leave limit and
/// until `quiet`, the time since the last input, passes the inactivity limit.
fn should_be_online(
    looking: bool,
    away: Duration,
    quiet: Duration,
    leave_limit: Option<Duration>,
    idle_limit: Option<Duration>,
) -> bool {
    present(looking, away, leave_limit) && idle_limit.is_none_or(|limit| quiet < limit)
}

/// The outcome of one presence evaluation.
#[derive(Debug, PartialEq, Eq)]
struct Decision {
    online: bool,
    /// Set only when a new wake-up must be scheduled for this time.
    wake_at: Option<Instant>,
}

impl PresenceWatch {
    fn evaluate(
        &mut self,
        now: Instant,
        looking: bool,
        leave_limit: Option<Duration>,
        idle_limit: Option<Duration>,
    ) -> Decision {
        if looking {
            self.left_at = None;
        } else if self.looking {
            self.left_at = Some(now);
        }
        self.looking = looking;
        let away = self.left_at.map_or(Duration::MAX, |left| now - left);
        let last_input = self.last_input.get();
        let online = should_be_online(looking, away, now - last_input, leave_limit, idle_limit);
        self.waiting_for_input
            .set(!online && present(looking, away, leave_limit));
        let mut wake_at = None;
        if online {
            let left = self
                .left_at
                .zip(leave_limit)
                .map(|(left, limit)| left + limit);
            let idle = idle_limit.map(|limit| last_input + limit);
            let due = left.into_iter().chain(idle).min();
            // A later check is replaced by the earlier one; a stale one only evaluates again.
            if let Some(due) = due
                && self.next_check.is_none_or(|planned| due < planned)
            {
                self.next_check = Some(due);
                wake_at = Some(due);
            }
        }
        Decision { online, wake_at }
    }

    /// Frees the check for `due` once its wake-up arrived.
    pub(super) fn woke(&mut self, due: Instant) {
        if self.next_check == Some(due) {
            self.next_check = None;
        }
    }

    /// Whether the backend must be told `online`; remembers what it was told.
    fn report(&mut self, online: bool) -> bool {
        self.reported.replace(online) != Some(online)
    }
}

impl NativeApplication {
    /// Records input in the main window and follows the screensaver.
    pub(super) fn watch_presence(&self, sender: &ComponentSender<Self>) {
        let input = gtk::EventControllerLegacy::new();
        // On the window in the Capture phase, so input counts wherever it lands
        // and no widget can stop it first.
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
        let leave_limit = limit(self.settings.offline_after_leaving_seconds);
        let idle_limit = limit(self.settings.offline_after_inactive_seconds);
        let now = Instant::now();
        let decision = self
            .presence_watch
            .evaluate(now, looking, leave_limit, idle_limit);
        if let Some(due) = decision.wake_at {
            let check = sender.clone();
            gtk::glib::timeout_add_local_once(due.saturating_duration_since(now), move || {
                check.input(Input::PresenceDue(due))
            });
        }
        if let Some(backend) = &self.backend
            && self.presence_watch.report(decision.online)
        {
            backend.send(crate::backend::Command::SetOnline(decision.online));
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
            should_be_online(
                false,
                Duration::from_secs(away),
                busy,
                TEN_SECONDS,
                FIVE_MINUTES,
            )
        };
        assert!(should_be_online(
            true,
            Duration::MAX,
            busy,
            TEN_SECONDS,
            FIVE_MINUTES
        ));
        assert!(left(9));
        assert!(!left(10));
        assert!(should_be_online(
            false,
            Duration::MAX,
            busy,
            None,
            FIVE_MINUTES
        ));
    }

    #[test]
    fn inactivity_goes_offline_even_while_looking() {
        let quiet = Duration::from_secs(300);
        assert!(!should_be_online(
            true,
            Duration::MAX,
            quiet,
            TEN_SECONDS,
            FIVE_MINUTES
        ));
        assert!(!should_be_online(
            false,
            Duration::MAX,
            quiet,
            None,
            FIVE_MINUTES
        ));
        assert!(should_be_online(false, Duration::MAX, quiet, None, None));
    }

    #[test]
    fn zero_means_never() {
        assert_eq!(limit(0), None);
        assert_eq!(limit(300), FIVE_MINUTES);
    }

    fn seconds(count: u64) -> Duration {
        Duration::from_secs(count)
    }

    fn watch(start: Instant) -> PresenceWatch {
        let watch = PresenceWatch::default();
        watch.last_input.set(start);
        watch
    }

    #[test]
    fn leaving_and_returning_inside_the_delay_stays_online() {
        let start = Instant::now();
        let mut watch = watch(start);
        assert!(watch.evaluate(start, true, TEN_SECONDS, None).online);
        assert!(
            watch
                .evaluate(start + seconds(1), false, TEN_SECONDS, None)
                .online
        );
        assert!(
            watch
                .evaluate(start + seconds(5), true, TEN_SECONDS, None)
                .online
        );
        assert!(watch.left_at.is_none());
        assert!(
            watch
                .evaluate(start + seconds(100), true, TEN_SECONDS, None)
                .online
        );
    }

    #[test]
    fn leaving_past_the_delay_goes_offline_at_the_due_time() {
        let start = Instant::now();
        let mut watch = watch(start);
        watch.evaluate(start, true, TEN_SECONDS, None);
        let left = watch.evaluate(start + seconds(1), false, TEN_SECONDS, None);
        assert_eq!(left.wake_at, Some(start + seconds(11)));
        assert!(
            !watch
                .evaluate(start + seconds(11), false, TEN_SECONDS, None)
                .online
        );
    }

    #[test]
    fn starting_unfocused_is_offline() {
        let start = Instant::now();
        let mut watch = watch(start);
        let decision = watch.evaluate(start, false, TEN_SECONDS, FIVE_MINUTES);
        assert_eq!(
            decision,
            Decision {
                online: false,
                wake_at: None
            }
        );
        assert!(!watch.waiting_for_input.get());
    }

    #[test]
    fn idle_goes_offline_and_waits_for_the_next_input() {
        let start = Instant::now();
        let mut watch = watch(start);
        let first = watch.evaluate(start, true, TEN_SECONDS, FIVE_MINUTES);
        assert_eq!(first.wake_at, Some(start + seconds(300)));
        assert!(!watch.waiting_for_input.get());
        let idle = start + seconds(300);
        watch.woke(idle);
        assert!(!watch.evaluate(idle, true, TEN_SECONDS, FIVE_MINUTES).online);
        assert!(watch.waiting_for_input.get());
        watch.last_input.set(idle + seconds(1));
        assert!(watch.waiting_for_input.replace(false));
        let back = watch.evaluate(idle + seconds(1), true, TEN_SECONDS, FIVE_MINUTES);
        assert!(back.online);
        assert_eq!(back.wake_at, Some(idle + seconds(301)));
    }

    #[test]
    fn leaving_does_not_wait_for_input() {
        let start = Instant::now();
        let mut watch = watch(start);
        watch.evaluate(start, true, TEN_SECONDS, None);
        watch.evaluate(start + seconds(1), false, TEN_SECONDS, None);
        assert!(
            !watch
                .evaluate(start + seconds(11), false, TEN_SECONDS, None)
                .online
        );
        assert!(!watch.waiting_for_input.get());
    }

    #[test]
    fn never_leaving_still_obeys_the_idle_limit() {
        let start = Instant::now();
        let mut watch = watch(start);
        watch.evaluate(start, true, None, FIVE_MINUTES);
        let away = watch.evaluate(start + seconds(100), false, None, FIVE_MINUTES);
        assert!(away.online);
        assert!(
            !watch
                .evaluate(start + seconds(300), false, None, FIVE_MINUTES)
                .online
        );
        assert!(watch.waiting_for_input.get());
    }

    #[test]
    fn one_wake_up_is_scheduled_per_due_time() {
        let start = Instant::now();
        let mut watch = watch(start);
        let first = watch.evaluate(start, true, None, FIVE_MINUTES);
        assert_eq!(first.wake_at, Some(start + seconds(300)));
        for second in 1..4 {
            let again = watch.evaluate(start + seconds(second), true, None, FIVE_MINUTES);
            assert_eq!(again.wake_at, None);
        }
    }

    #[test]
    fn a_late_input_rearms_the_wake_up_once_the_old_one_arrives() {
        let start = Instant::now();
        let mut watch = watch(start);
        let due = start + seconds(300);
        watch.evaluate(start, true, None, FIVE_MINUTES);
        watch.last_input.set(start + seconds(200));
        assert_eq!(
            watch
                .evaluate(start + seconds(200), true, None, FIVE_MINUTES)
                .wake_at,
            None
        );
        watch.woke(due);
        let rearmed = watch.evaluate(due, true, None, FIVE_MINUTES);
        assert!(rearmed.online);
        assert_eq!(rearmed.wake_at, Some(start + seconds(500)));
    }

    #[test]
    fn an_earlier_due_time_replaces_the_pending_one() {
        let start = Instant::now();
        let mut watch = watch(start);
        watch.evaluate(start, true, None, FIVE_MINUTES);
        let left = watch.evaluate(start + seconds(1), false, TEN_SECONDS, FIVE_MINUTES);
        assert_eq!(left.wake_at, Some(start + seconds(11)));
        watch.woke(start + seconds(300));
        assert_eq!(watch.next_check, Some(start + seconds(11)));
    }

    #[test]
    fn the_backend_is_told_only_about_changes() {
        let mut watch = PresenceWatch::default();
        assert!(watch.report(true));
        assert!(!watch.report(true));
        assert!(watch.report(false));
        watch.reported = None;
        assert!(watch.report(false));
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
