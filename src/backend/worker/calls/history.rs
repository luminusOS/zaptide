//! Call history: filing calls in their chats and listing them for the Calls tab.

use super::*;
use whatsapp_rust::types::call::MissedCall;

use super::{Offer, is_pending};

const CALL_HISTORY_LIMIT: usize = 200;
/// When the Calls tab was last looked at. The first read starts it at now, so
/// missed calls from before this feature existed do not show as unseen.
const CALLS_SEEN_KEY: &str = "calls_seen_at";
/// Seconds a missed call replayed after a reconnect stays worth a notification.
const FRESH_MISSED_CALL: i64 = 10 * 60;

/// Whether filing a call may raise an unread count and a notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Filing {
    History,
    Notify,
}

impl Filing {
    fn notify_if(notify: bool) -> Self {
        if notify { Self::Notify } else { Self::History }
    }
}

impl Worker {
    pub(in crate::backend::worker) fn emit_calls(&self) {
        match self.archive.calls(CALL_HISTORY_LIMIT) {
            Ok(entries) => self.emit(Event::Calls {
                entries,
                seen_at: self.calls_seen_at(),
            }),
            Err(error) => log::warn!("could not list calls: {error}"),
        }
    }

    fn calls_seen_at(&self) -> i64 {
        if let Ok(Some(stored)) = self.archive.meta(CALLS_SEEN_KEY)
            && let Ok(seen_at) = stored.parse()
        {
            return seen_at;
        }
        let now = crate::util::now();
        let _ = self.archive.set_meta(CALLS_SEEN_KEY, &now.to_string());
        now
    }

    pub(in crate::backend::worker) fn mark_calls_seen(&self) {
        let _ = self
            .archive
            .set_meta(CALLS_SEEN_KEY, &crate::util::now().to_string());
        self.emit_calls();
    }

    pub(in crate::backend::worker) fn flush_call_history(&mut self) {
        if std::mem::take(&mut self.calls.history_dirty) {
            self.emit_calls();
        }
    }

    pub(super) fn record_finished(&mut self, call: &ActiveCall) {
        let session = &call.session;
        let outcome = match session.phase {
            CallPhase::Ended(outcome) => outcome,
            CallPhase::Failed if session.talk_seconds.is_some() => CallOutcome::Completed,
            CallPhase::Failed => CallOutcome::Failed,
            _ => return,
        };
        if !outcome.is_recordable() {
            return;
        }
        let mut session = session.clone();
        if is_pending(&session.id) {
            if outcome != CallOutcome::Failed {
                return;
            }
            session.id = format!("local-{}-{}", session.started_at, call.generation);
        }
        self.record_call(&session, outcome);
    }

    pub(super) fn record_call(&mut self, session: &CallSession, outcome: CallOutcome) {
        let message = self.call_message(session, outcome);
        let notify = outcome == CallOutcome::Missed && !session.is_outgoing();
        self.file_call(message, Filing::notify_if(notify));
    }

    /// Files a call event in its chat. Only a fresh missed call counts as
    /// unread and notifies; history stays quiet.
    fn file_call(&mut self, message: Message, filing: Filing) {
        if filing == Filing::Notify {
            self.store_message(message, None, None);
        } else {
            let chat = message.chat.clone();
            self.ensure_chat(&chat, None);
            if let Err(error) = self.archive.insert_call(&message) {
                log::warn!("could not store a call: {error}");
                return;
            }
            let mut stored = self
                .archive
                .message(&chat, &message.id)
                .ok()
                .flatten()
                .unwrap_or(message);
            self.polish(&mut stored);
            self.emit(Event::Messages {
                request: None,
                chat: chat.clone(),
                messages: vec![stored],
                older: false,
                complete: false,
            });
            self.emit_chat(&chat);
        }
        self.calls.history_dirty = true;
        if !self.syncing {
            self.flush_call_history();
        }
    }

    pub(in crate::backend::worker) fn on_missed_call(&mut self, missed: &MissedCall) {
        self.drop_waiting(&missed.call_id);
        if self.addressed_call(&missed.call_id, &missed.from) {
            self.signal_call(CallSignal::PeerTerminated);
            return;
        }
        let chat = self.canonical(&missed.from);
        let video = match self.calls.offer(&missed.call_id) {
            Some(Offer::Group) => return,
            Some(Offer::Direct { video }) => video,
            None => false,
        };
        if ChatKind::from_id(&chat) != ChatKind::Direct {
            return;
        }
        self.ensure_chat(&chat, None);
        let timestamp = missed.timestamp.timestamp();
        let mut session = CallSession::new(
            missed.call_id.clone(),
            chat,
            CallDirection::Incoming,
            video,
            timestamp,
        );
        session.apply(CallSignal::PeerTerminated, timestamp);
        let fresh = crate::util::now() - timestamp < FRESH_MISSED_CALL;
        let message = self.call_message(&session, CallOutcome::Missed);
        self.file_call(message, Filing::notify_if(fresh));
    }

    fn call_message(&self, session: &CallSession, outcome: CallOutcome) -> Message {
        let outgoing = session.is_outgoing();
        Message {
            id: session.id.clone(),
            chat: session.chat.clone(),
            sender: if outgoing {
                self.me()
            } else {
                session.chat.clone()
            },
            sender_name: None,
            from_me: outgoing,
            timestamp: session.started_at,
            content: Content::Call {
                video: session.video,
                outgoing,
                outcome,
                seconds: session.talk_seconds,
            },
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        }
    }

    /// Files a call the phone placed or took, from its synced call history.
    pub(in crate::backend::worker) fn on_call_log(&mut self, sync: &wa_events::CallLogSync) {
        use wa::call_log_record::CallResult;
        let record = &sync.record;
        if record.group_jid.is_some() || record.is_call_link == Some(true) {
            return;
        }
        let outcome = match record.call_result {
            Some(CallResult::CONNECTED) => CallOutcome::Completed,
            Some(CallResult::REJECTED) => CallOutcome::Declined,
            Some(CallResult::CANCELLED) => CallOutcome::Cancelled,
            Some(CallResult::MISSED) => CallOutcome::Missed,
            Some(CallResult::FAILED) => CallOutcome::Failed,
            _ => return,
        };
        let peer = if sync.from_me {
            record
                .participants
                .iter()
                .filter_map(|participant| participant.user_jid.as_deref())
                .filter_map(|jid| jid.parse::<Jid>().ok())
                .map(|jid| self.canonical(&jid))
                .find(|id| !self.is_me(id))
        } else {
            Some(self.canonical(&sync.call_creator_jid))
        };
        let Some(chat) =
            peer.filter(|chat| !self.is_me(chat) && ChatKind::from_id(chat) == ChatKind::Direct)
        else {
            return;
        };
        let started_at = record
            .start_time
            .filter(|time| *time > 0)
            .unwrap_or_else(|| sync.timestamp.timestamp());
        let mut session = CallSession::new(
            sync.call_id.clone(),
            chat,
            if sync.from_me {
                CallDirection::Outgoing
            } else {
                CallDirection::Incoming
            },
            record.is_video.unwrap_or(false),
            started_at,
        );
        session.talk_seconds = (outcome == CallOutcome::Completed)
            .then(|| record.duration.unwrap_or(0).clamp(0, i64::from(u32::MAX)) as u32);
        let message = self.call_message(&session, outcome);
        self.file_call(message, Filing::History);
    }
}
