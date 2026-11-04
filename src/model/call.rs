//! Voice and video calls: archived call events and the live session state machine.

use serde::{Deserialize, Serialize};

use super::ChatId;
use crate::i18n::gettext;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallDirection {
    Incoming,
    Outgoing,
}

/// How a call ended, from this account's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallOutcome {
    /// The call connected and later ended.
    Completed,
    /// An incoming call nobody answered.
    Missed,
    /// Declined here or by the other person.
    Declined,
    /// An outgoing call that was cancelled or never answered.
    Cancelled,
    Failed,
    /// Answered on another linked device; never archived.
    Elsewhere,
}

impl CallOutcome {
    /// Whether the call leaves an event in the chat and the call history.
    pub fn is_recordable(self) -> bool {
        self != Self::Elsewhere
    }
}

/// Short event text such as "Missed voice call", without the duration.
pub fn call_label(video: bool, outgoing: bool, outcome: CallOutcome) -> String {
    match (outcome, video) {
        (CallOutcome::Completed, false) => gettext("Voice call ended"),
        (CallOutcome::Completed, true) => gettext("Video call ended"),
        (CallOutcome::Missed, false) => gettext("Missed voice call"),
        (CallOutcome::Missed, true) => gettext("Missed video call"),
        (CallOutcome::Declined, false) => gettext("Voice call declined"),
        (CallOutcome::Declined, true) => gettext("Video call declined"),
        (CallOutcome::Cancelled, false) if outgoing => gettext("Voice call, no answer"),
        (CallOutcome::Cancelled, false) => gettext("Voice call cancelled"),
        (CallOutcome::Cancelled, true) if outgoing => gettext("Video call, no answer"),
        (CallOutcome::Cancelled, true) => gettext("Video call cancelled"),
        (CallOutcome::Failed | CallOutcome::Elsewhere, false) => gettext("Voice call failed"),
        (CallOutcome::Failed | CallOutcome::Elsewhere, true) => gettext("Video call failed"),
    }
}

/// Symbolic icon of a call event: missed, outgoing, or incoming.
pub fn call_icon(outgoing: bool, outcome: CallOutcome) -> &'static str {
    match (outcome, outgoing) {
        (CallOutcome::Missed, false) => "call-missed-symbolic",
        (_, true) => "call-outgoing-symbolic",
        (_, false) => "call-incoming-symbolic",
    }
}

/// Missed calls newer than the last visit to the Calls tab, for its counter.
pub fn unseen_missed(entries: &[CallEntry], seen_at: i64) -> usize {
    entries
        .iter()
        .filter(|entry| entry.kind() == CallKind::Missed && entry.timestamp > seen_at)
        .count()
}

/// Event text with the talk time when the call connected: "Voice call ended · 02:18".
pub fn call_summary(
    video: bool,
    outgoing: bool,
    outcome: CallOutcome,
    seconds: Option<u32>,
) -> String {
    let label = call_label(video, outgoing, outcome);
    match (outcome, seconds) {
        (CallOutcome::Completed, Some(seconds)) => with_detail(&label, &call_clock(seconds)),
        _ => label,
    }
}

/// Two parts of one line, such as "Connected · 02:18". Translators may reorder
/// them through the positional placeholders.
pub fn with_detail(first: &str, second: &str) -> String {
    gettext("%1$s · %2$s")
        .replace("%1$s", first)
        .replace("%2$s", second)
}

/// Call timer such as "02:18", or "1:02:18" past an hour.
pub fn call_clock(seconds: u32) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPhase {
    /// An incoming call waiting for the user to answer or decline.
    Ringing,
    /// An outgoing call the other side has not answered yet.
    Calling,
    /// Answered, with the media path still coming up.
    Connecting,
    /// Talking since this Unix time in seconds.
    Connected {
        since: i64,
    },
    Ended(CallOutcome),
    /// The call could not be set up or lost its media path.
    Failed,
}

impl CallPhase {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Ended(_) | Self::Failed)
    }
}

/// What happened to the call, as the state machine sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallSignal {
    LocalAnswered,
    LocalDeclined,
    LocalHangup,
    PeerAnswered,
    PeerDeclined,
    PeerTerminated,
    /// Another linked device answered or declined.
    HandledElsewhere,
    /// The relay media path is live.
    MediaUp,
    /// Setup failed or the media path was lost.
    MediaFailed,
}

/// The state of the one call in progress, owned by the backend and shown by the window.
#[derive(Clone, Debug, PartialEq)]
pub struct CallSession {
    pub id: String,
    pub chat: ChatId,
    pub direction: CallDirection,
    pub video: bool,
    pub phase: CallPhase,
    pub muted: bool,
    pub camera_on: bool,
    /// The other side is sending video.
    pub peer_video: bool,
    /// The other side asked to switch this voice call to video.
    pub video_requested: bool,
    /// The camera is being switched on or off.
    pub camera_busy: bool,
    pub started_at: i64,
    /// Talk time once a connected call has ended.
    pub talk_seconds: Option<u32>,
    answered: bool,
    media_up: bool,
}

impl CallSession {
    pub fn new(
        id: String,
        chat: ChatId,
        direction: CallDirection,
        video: bool,
        started_at: i64,
    ) -> Self {
        Self {
            id,
            chat,
            direction,
            video,
            phase: match direction {
                CallDirection::Incoming => CallPhase::Ringing,
                CallDirection::Outgoing => CallPhase::Calling,
            },
            muted: false,
            camera_on: video && direction == CallDirection::Outgoing,
            peer_video: false,
            video_requested: false,
            camera_busy: false,
            started_at,
            talk_seconds: None,
            answered: false,
            media_up: false,
        }
    }

    /// Applies a signal at Unix time `now`; returns whether anything changed.
    pub fn apply(&mut self, signal: CallSignal, now: i64) -> bool {
        if self.phase.is_finished() {
            return false;
        }
        let before = self.phase;
        let talked = self.seconds(now);
        match signal {
            CallSignal::LocalAnswered if self.phase == CallPhase::Ringing => {
                self.answered = true;
                self.phase = CallPhase::Connecting;
                self.connect_if_ready(now);
            }
            CallSignal::PeerAnswered if self.phase == CallPhase::Calling => {
                self.answered = true;
                self.phase = CallPhase::Connecting;
                self.connect_if_ready(now);
            }
            CallSignal::MediaUp => {
                self.media_up = true;
                self.connect_if_ready(now);
            }
            CallSignal::LocalDeclined if self.phase == CallPhase::Ringing => {
                self.phase = CallPhase::Ended(CallOutcome::Declined);
            }
            CallSignal::PeerDeclined if self.phase == CallPhase::Calling => {
                self.phase = CallPhase::Ended(CallOutcome::Declined);
            }
            CallSignal::HandledElsewhere if self.phase == CallPhase::Ringing => {
                self.phase = CallPhase::Ended(CallOutcome::Elsewhere);
            }
            CallSignal::LocalHangup => self.phase = self.ended_by(true),
            CallSignal::PeerTerminated => self.phase = self.ended_by(false),
            CallSignal::MediaFailed => self.phase = CallPhase::Failed,
            _ => {}
        }
        if self.phase.is_finished() {
            self.talk_seconds = talked;
        }
        self.phase != before
    }

    fn connect_if_ready(&mut self, now: i64) {
        if self.answered && self.media_up && !matches!(self.phase, CallPhase::Connected { .. }) {
            self.phase = CallPhase::Connected { since: now };
        }
    }

    fn ended_by(&self, local: bool) -> CallPhase {
        let outcome = match (self.phase, self.direction, local) {
            (CallPhase::Connected { .. }, _, _) => CallOutcome::Completed,
            (CallPhase::Ringing, _, true) => CallOutcome::Declined,
            (CallPhase::Ringing, _, false) => CallOutcome::Missed,
            (_, CallDirection::Outgoing, _) => CallOutcome::Cancelled,
            (_, CallDirection::Incoming, true) => CallOutcome::Declined,
            (_, CallDirection::Incoming, false) => CallOutcome::Missed,
        };
        CallPhase::Ended(outcome)
    }

    /// Talk time at Unix time `now`, once connected.
    pub fn seconds(&self, now: i64) -> Option<u32> {
        match self.phase {
            CallPhase::Connected { since } => Some(now.saturating_sub(since).max(0) as u32),
            _ => None,
        }
    }

    pub fn is_outgoing(&self) -> bool {
        self.direction == CallDirection::Outgoing
    }
}

/// One finished call as listed in the call history.
#[derive(Clone, Debug, PartialEq)]
pub struct CallEntry {
    pub chat: ChatId,
    pub id: String,
    pub video: bool,
    pub outgoing: bool,
    pub outcome: CallOutcome,
    pub seconds: Option<u32>,
    pub timestamp: i64,
}

impl CallEntry {
    /// Missed, outgoing, or incoming, as the history row words it.
    pub fn kind(&self) -> CallKind {
        match (self.outcome, self.outgoing) {
            (CallOutcome::Missed, false) => CallKind::Missed,
            (_, true) => CallKind::Outgoing,
            (_, false) => CallKind::Incoming,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallKind {
    Missed,
    Outgoing,
    Incoming,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(direction: CallDirection) -> CallSession {
        CallSession::new("call".into(), "chat".into(), direction, false, 100)
    }

    #[test]
    fn outgoing_call_connects_once_answered_and_media_is_up_in_either_order() {
        let mut answered_first = session(CallDirection::Outgoing);
        assert_eq!(answered_first.phase, CallPhase::Calling);
        assert!(answered_first.apply(CallSignal::PeerAnswered, 110));
        assert_eq!(answered_first.phase, CallPhase::Connecting);
        assert!(answered_first.apply(CallSignal::MediaUp, 112));
        assert_eq!(answered_first.phase, CallPhase::Connected { since: 112 });

        let mut media_first = session(CallDirection::Outgoing);
        assert!(!media_first.apply(CallSignal::MediaUp, 105));
        assert_eq!(media_first.phase, CallPhase::Calling);
        assert!(media_first.apply(CallSignal::PeerAnswered, 120));
        assert_eq!(media_first.phase, CallPhase::Connected { since: 120 });
    }

    #[test]
    fn incoming_call_rings_until_answered() {
        let mut call = session(CallDirection::Incoming);
        assert_eq!(call.phase, CallPhase::Ringing);
        assert!(!call.apply(CallSignal::PeerAnswered, 101));
        assert!(call.apply(CallSignal::LocalAnswered, 102));
        assert_eq!(call.phase, CallPhase::Connecting);
        assert!(call.apply(CallSignal::MediaUp, 103));
        assert_eq!(call.phase, CallPhase::Connected { since: 103 });
        assert_eq!(call.seconds(163), Some(60));
    }

    #[test]
    fn unanswered_incoming_call_is_missed_and_declined_one_is_declined() {
        let mut missed = session(CallDirection::Incoming);
        missed.apply(CallSignal::PeerTerminated, 110);
        assert_eq!(missed.phase, CallPhase::Ended(CallOutcome::Missed));

        let mut declined = session(CallDirection::Incoming);
        declined.apply(CallSignal::LocalDeclined, 110);
        assert_eq!(declined.phase, CallPhase::Ended(CallOutcome::Declined));
    }

    #[test]
    fn outgoing_call_outcomes_follow_who_gave_up() {
        let mut cancelled = session(CallDirection::Outgoing);
        cancelled.apply(CallSignal::LocalHangup, 110);
        assert_eq!(cancelled.phase, CallPhase::Ended(CallOutcome::Cancelled));

        let mut unanswered = session(CallDirection::Outgoing);
        unanswered.apply(CallSignal::PeerTerminated, 110);
        assert_eq!(unanswered.phase, CallPhase::Ended(CallOutcome::Cancelled));

        let mut refused = session(CallDirection::Outgoing);
        refused.apply(CallSignal::PeerDeclined, 110);
        assert_eq!(refused.phase, CallPhase::Ended(CallOutcome::Declined));
    }

    #[test]
    fn hanging_up_a_connected_call_completes_it_from_either_side() {
        for signal in [CallSignal::LocalHangup, CallSignal::PeerTerminated] {
            let mut call = session(CallDirection::Outgoing);
            call.apply(CallSignal::PeerAnswered, 101);
            call.apply(CallSignal::MediaUp, 102);
            assert!(call.apply(signal, 200));
            assert_eq!(call.phase, CallPhase::Ended(CallOutcome::Completed));
            assert_eq!(call.talk_seconds, Some(98));
        }
    }

    #[test]
    fn media_failure_fails_the_call_and_a_finished_call_ignores_signals() {
        let mut call = session(CallDirection::Outgoing);
        call.apply(CallSignal::PeerAnswered, 101);
        assert!(call.apply(CallSignal::MediaFailed, 102));
        assert_eq!(call.phase, CallPhase::Failed);
        assert!(!call.apply(CallSignal::PeerTerminated, 103));
        assert_eq!(call.phase, CallPhase::Failed);
    }

    #[test]
    fn answering_on_another_device_is_not_recorded() {
        let mut call = session(CallDirection::Incoming);
        call.apply(CallSignal::HandledElsewhere, 101);
        assert_eq!(call.phase, CallPhase::Ended(CallOutcome::Elsewhere));
        assert!(!CallOutcome::Elsewhere.is_recordable());
        assert!(CallOutcome::Missed.is_recordable());
    }

    #[test]
    fn clock_pads_minutes_and_adds_hours_only_when_needed() {
        assert_eq!(call_clock(0), "00:00");
        assert_eq!(call_clock(138), "02:18");
        assert_eq!(call_clock(3599), "59:59");
        assert_eq!(call_clock(3725), "1:02:05");
    }

    #[test]
    fn event_text_names_the_call_and_adds_talk_time_only_when_connected() {
        assert_eq!(
            call_summary(false, false, CallOutcome::Missed, None),
            "Missed voice call"
        );
        assert_eq!(
            call_summary(true, false, CallOutcome::Missed, None),
            "Missed video call"
        );
        assert_eq!(
            call_summary(false, true, CallOutcome::Completed, Some(138)),
            "Voice call ended · 02:18"
        );
        assert_eq!(
            call_summary(true, true, CallOutcome::Cancelled, Some(9)),
            "Video call, no answer"
        );
        assert_eq!(
            call_summary(false, false, CallOutcome::Cancelled, None),
            "Voice call cancelled"
        );
    }

    #[test]
    fn history_kind_separates_missed_from_ordinary_incoming_calls() {
        let entry = |outgoing, outcome| CallEntry {
            chat: "chat".into(),
            id: "call".into(),
            video: false,
            outgoing,
            outcome,
            seconds: None,
            timestamp: 0,
        };
        assert_eq!(entry(false, CallOutcome::Missed).kind(), CallKind::Missed);
        assert_eq!(
            entry(false, CallOutcome::Completed).kind(),
            CallKind::Incoming
        );
        assert_eq!(
            entry(true, CallOutcome::Cancelled).kind(),
            CallKind::Outgoing
        );
    }

    #[test]
    fn only_missed_calls_after_the_last_visit_count_as_unseen() {
        let entry = |outgoing, outcome, timestamp| CallEntry {
            chat: "chat".into(),
            id: format!("call-{timestamp}"),
            video: false,
            outgoing,
            outcome,
            seconds: None,
            timestamp,
        };
        let entries = [
            entry(false, CallOutcome::Missed, 300),
            entry(false, CallOutcome::Completed, 250),
            entry(false, CallOutcome::Missed, 200),
            entry(true, CallOutcome::Cancelled, 150),
            entry(false, CallOutcome::Missed, 100),
        ];
        assert_eq!(unseen_missed(&entries, 0), 3);
        assert_eq!(unseen_missed(&entries, 150), 2);
        assert_eq!(unseen_missed(&entries, 300), 0);
    }

    #[test]
    fn event_icons_tell_missed_from_outgoing_and_incoming_calls() {
        assert_eq!(
            call_icon(false, CallOutcome::Missed),
            "call-missed-symbolic"
        );
        assert_eq!(
            call_icon(true, CallOutcome::Missed),
            "call-outgoing-symbolic"
        );
        assert_eq!(
            call_icon(true, CallOutcome::Completed),
            "call-outgoing-symbolic"
        );
        assert_eq!(
            call_icon(false, CallOutcome::Declined),
            "call-incoming-symbolic"
        );
    }

    #[test]
    fn two_parts_join_into_one_line_through_the_translatable_template() {
        assert_eq!(with_detail("Connected", "02:18"), "Connected · 02:18");
        assert_eq!(
            with_detail("Missed", "Today · 10:23"),
            "Missed · Today · 10:23"
        );
    }
}
