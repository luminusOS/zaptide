//! Sends deletions confirmed in this window to the phone and the other linked
//! devices.
//!
//! Remote deletion cannot be undone, so the only thing that can start one is
//! a [`Command`] the person confirmed in a dialog. The request is checked
//! against the archive, recorded in the encrypted `remote_deletions` queue in
//! the same savepoint as the local removal, and sent later by
//! [`Worker::pump_remote_deletions`] once the session is healthy. Nothing
//! else queues a deletion: incoming phone events change only the archive, and
//! rows the archive lacks never become requests. Chat-level requests carry
//! the timestamp of the newest message the archive held, so messages this
//! window never saw are not covered.

use super::*;
use crate::archive::{RemoteDeletion, RemoteDeletionKind};

/// Failed attempts before a queued deletion is abandoned. The local deletion
/// stays; the other devices keep the message.
const MAX_ATTEMPTS: u32 = 6;
/// A queued request older than this no longer says what the person wants.
const EXPIRY_SECONDS: i64 = 14 * 24 * 60 * 60;
/// A patch that neither succeeds nor fails in this time is retried. It may
/// have reached the phone, and repeating a deletion is harmless.
const SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// The one deletion in flight and the backoff after a failure. The archive
/// owns the durable queue; this only limits work on the connection, so a
/// broken collection is not retried by a crowd of tasks.
#[derive(Default)]
pub(super) struct RemoteDeleteSync {
    in_flight: Option<(u64, String)>,
    next_attempt: u64,
    retry_at: Option<Instant>,
    failures: u32,
}

impl RemoteDeleteSync {
    fn ready(&self, now: Instant) -> bool {
        self.in_flight.is_none() && self.retry_at.is_none_or(|retry| now >= retry)
    }

    fn start(&mut self, key: &str, now: Instant) -> Option<u64> {
        if !self.ready(now) {
            return None;
        }
        self.next_attempt = self.next_attempt.wrapping_add(1);
        self.in_flight = Some((self.next_attempt, key.to_owned()));
        Some(self.next_attempt)
    }

    /// Ignores a completion from an attempt that is no longer ours.
    fn finish(&mut self, attempt: u64, key: &str, success: bool, now: Instant) -> bool {
        if !self
            .in_flight
            .as_ref()
            .is_some_and(|(active, active_key)| *active == attempt && active_key == key)
        {
            return false;
        }
        self.in_flight = None;
        if success {
            self.failures = 0;
            self.retry_at = None;
        } else {
            self.failures = self.failures.saturating_add(1);
            let seconds = (30 * (1_u64 << (self.failures - 1).min(5))).min(15 * 60);
            self.retry_at = Some(now + Duration::from_secs(seconds));
            log::warn!("remote deletion paused; retrying in {seconds} seconds");
        }
        true
    }
}

/// What the worker knows about the session when it considers sending.
#[derive(Clone, Copy, Debug)]
struct Readiness {
    /// A client exists and the link is up.
    connected: bool,
    /// History is still arriving, so the archive may be incomplete.
    syncing: bool,
    /// Privacy ids are resolved, so chat ids are canonical.
    privacy_ready: bool,
    /// An unlink left the archive in an unknown state.
    cleanup_failed: bool,
}

impl Readiness {
    /// Why queued deletions must wait, or `None` when they may be sent.
    fn blocker(self) -> Option<&'static str> {
        if !self.connected {
            Some("not connected")
        } else if self.syncing {
            Some("history sync in progress")
        } else if !self.privacy_ready {
            Some("privacy ids unresolved")
        } else if self.cleanup_failed {
            Some("archive cleanup unfinished")
        } else {
            None
        }
    }
}

/// The sender to name for a message, which only a group message from someone
/// else needs. `Err` when that sender cannot be addressed.
fn participant_of(chat: &str, from_me: bool, sender: &str) -> Result<Option<String>, &'static str> {
    if ChatKind::from_id(chat) != ChatKind::Group || from_me {
        return Ok(None);
    }
    match Worker::jid_of(sender) {
        Some(jid) if !jid.user.is_empty() => Ok(Some(sender.to_owned())),
        _ => Err("sender cannot be addressed"),
    }
}

impl Worker {
    /// `Command::DeleteLocal`: removes one message here and queues the same
    /// deletion for the other devices.
    pub(super) fn delete_message_everywhere(&mut self, chat: ChatId, id: String) {
        let chat = self.canonical_str(&chat);
        let message = match self.archive.message(&chat, &id) {
            Ok(Some(message)) => message,
            // Already gone: a repeated request has nothing left to propagate.
            Ok(None) => return,
            Err(error) => {
                log::warn!("could not read a message to delete: {error}");
                self.emit(Event::Error("Could not delete the message".to_owned()));
                return;
            }
        };
        let participant = participant_of(&chat, message.from_me, &message.sender);
        let request = participant.as_ref().ok().map(|participant| RemoteDeletion {
            kind: RemoteDeletionKind::Message,
            chat: chat.clone(),
            message_id: id.clone(),
            participant: participant.clone(),
            from_me: message.from_me,
            bound: message.timestamp,
        });
        let request = request.filter(|_| Self::addressable(&chat));
        let now = jiff::Timestamp::now().as_second();
        let result = self.archive.atomically(|archive| {
            let deleted = archive.delete_message(&chat, &id)?;
            if deleted && let Some(request) = &request {
                archive.queue_remote_deletion(request, now)?;
            }
            Ok(deleted)
        });
        match result {
            Ok(true) => {
                self.emit(Event::MessageDeleted {
                    chat: chat.clone(),
                    id,
                });
                self.emit_chat(&chat);
                self.emit_pin(&chat);
                self.after_local_deletion(request.is_some());
            }
            Ok(false) => {}
            Err(error) => {
                log::warn!("could not delete a message: {error}");
                self.emit(Event::Error("Could not delete the message".to_owned()));
            }
        }
    }

    /// `Command::ClearChat`: clears a chat here and queues the same clear.
    /// Starred messages stay, as they do on the phone's default clear.
    pub(super) fn clear_chat_everywhere(&mut self, chat: ChatId) {
        self.remove_chat_everywhere(chat, RemoteDeletionKind::Clear);
    }

    /// `Command::DeleteChat`: deletes a chat here and queues the same
    /// deletion.
    pub(super) fn delete_chat_everywhere(&mut self, chat: ChatId) {
        self.remove_chat_everywhere(chat, RemoteDeletionKind::Chat);
    }

    fn remove_chat_everywhere(&mut self, chat: ChatId, kind: RemoteDeletionKind) {
        let chat = self.canonical_str(&chat);
        let newest = match self.archive.newest_message(&chat) {
            Ok(newest) => newest,
            Err(error) => {
                log::warn!("could not read a chat to remove: {error}");
                self.emit(Event::Error("Could not remove the chat".to_owned()));
                return;
            }
        };
        // Without a message, the archive cannot say how far the person's
        // request reaches. It may be new, reset or only partly synced, so the
        // other devices are left alone and only the local row goes.
        let request = newest.as_ref().and_then(|newest| {
            let participant = participant_of(&chat, newest.from_me, &newest.sender).ok()?;
            Self::addressable(&chat).then(|| RemoteDeletion {
                kind,
                chat: chat.clone(),
                message_id: newest.id.clone(),
                participant,
                from_me: newest.from_me,
                bound: newest.timestamp,
            })
        });
        let bound = newest.as_ref().map_or(i64::MAX, |newest| newest.timestamp);
        let now = jiff::Timestamp::now().as_second();
        let result = self.archive.atomically(|archive| {
            let removed = match kind {
                RemoteDeletionKind::Chat => archive.delete_chat(&chat, bound)?,
                _ => {
                    archive.clear_chat_messages(&chat, bound, true)?;
                    false
                }
            };
            if let Some(request) = &request {
                archive.queue_remote_deletion(request, now)?;
            }
            Ok(removed)
        });
        match result {
            Ok(removed) => {
                if removed {
                    self.emit(Event::ChatDeleted(chat.clone()));
                } else {
                    self.emit(Event::RefreshTranscript);
                    self.emit_chat(&chat);
                }
                self.after_local_deletion(request.is_some());
            }
            Err(error) => {
                log::warn!("could not remove a chat: {error}");
                self.emit(Event::Error("Could not remove the chat".to_owned()));
            }
        }
    }

    /// Chats the other devices can be asked about: not newsletters, status or
    /// broadcast lists, and with an id that parses.
    fn addressable(chat: &str) -> bool {
        ChatKind::from_id(chat) != ChatKind::Broadcast
            && Self::jid_of(chat).is_some_and(|jid| !jid.user.is_empty())
    }

    /// Sends right away when the session allows it, and says so when the
    /// request stayed on this computer.
    fn after_local_deletion(&mut self, queued: bool) {
        if queued {
            self.pump_remote_deletions();
        } else {
            self.emit(Event::Error(
                "Removed on this computer only. Your other devices were not asked to delete it."
                    .to_owned(),
            ));
        }
    }

    fn remote_deletion_readiness(&self) -> Readiness {
        Readiness {
            connected: self.client.is_some() && matches!(self.status, LinkStatus::Connected),
            syncing: self.syncing,
            privacy_ready: self.privacy.privacy_ready,
            cleanup_failed: self.archive_cleanup_failed,
        }
    }

    /// Sends the oldest queued deletion when the session is healthy and
    /// nothing else is in flight. Called every tick and after each request.
    pub(super) fn pump_remote_deletions(&mut self) {
        let now = Instant::now();
        if !self.remote_delete.ready(now) || self.remote_deletion_readiness().blocker().is_some() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let _ = self
            .archive
            .expire_remote_deletions(jiff::Timestamp::now().as_second() - EXPIRY_SECONDS);
        let request = match self.archive.next_remote_deletion() {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(error) => {
                log::warn!("could not read the remote deletion queue: {error}");
                return;
            }
        };
        let key = request.key();
        let jids = self.remote_deletion_jids(&request);
        if jids.is_empty() {
            // Addressability was checked when the row was queued.
            let _ = self.archive.finish_remote_deletion(&key, request.bound);
            return;
        }
        let Some(attempt_id) = self.remote_delete.start(&key, now) else {
            return;
        };
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            let sending = async {
                for jid in &jids {
                    send_remote_deletion(&client, jid, &request).await?;
                }
                Ok::<(), String>(())
            };
            let outcome = tokio::time::timeout(SEND_TIMEOUT, sending).await;
            let success = matches!(outcome, Ok(Ok(())));
            match &outcome {
                Ok(Ok(())) => log::info!("sent a {:?} deletion to linked devices", request.kind),
                Ok(Err(error)) => log::warn!("remote {:?} deletion failed: {error}", request.kind),
                Err(_) => log::warn!("remote {:?} deletion timed out", request.kind),
            }
            let _ = Completion {
                generation: session_generation,
                done: Done::RemoteDeletionFinished {
                    attempt_id,
                    key,
                    bound: request.bound,
                    success,
                },
            }
            .send(&commands);
        });
    }

    /// The ids to address a request to. A chat-level request goes to the
    /// phone-number id and, when one is known, the privacy id of the same
    /// chat, because the phone may key the chat by either and ignores a
    /// chat action that names the other. Repeating it is harmless.
    fn remote_deletion_jids(&self, request: &RemoteDeletion) -> Vec<Jid> {
        let Some(primary) = Self::jid_of(&request.chat) else {
            return Vec::new();
        };
        let mut jids = vec![primary.clone()];
        if request.kind != RemoteDeletionKind::Message
            && !primary.is_lid()
            && let Some((lid, _)) = self
                .identity
                .lid_to_pn
                .iter()
                .find(|(_, phone)| phone.as_str() == primary.user_base())
            && let Some(lid) = Self::jid_of(&format!("{lid}@lid"))
        {
            jids.push(lid);
        }
        jids
    }

    /// Records the outcome of one attempt. Only the phone's acceptance
    /// removes the row; a failure keeps it for a later attempt.
    pub(super) fn remote_deletion_finished(
        &mut self,
        attempt_id: u64,
        key: &str,
        bound: i64,
        success: bool,
    ) {
        if !self
            .remote_delete
            .finish(attempt_id, key, success, Instant::now())
        {
            return;
        }
        if success {
            let _ = self.archive.finish_remote_deletion(key, bound);
            self.pump_remote_deletions();
        } else if let Ok(true) = self.archive.fail_remote_deletion(key, bound, MAX_ATTEMPTS) {
            log::warn!("gave up sending a deletion to the other devices");
            self.emit(Event::Error(
                "A deletion could not reach your other devices, which still have it.".to_owned(),
            ));
        }
    }
}

async fn send_remote_deletion(
    client: &Client,
    jid: &Jid,
    request: &RemoteDeletion,
) -> Result<(), String> {
    let participant = request
        .participant
        .as_deref()
        .and_then(|participant| participant.parse::<Jid>().ok());
    let message_id =
        whatsapp_rust::MessageId::new(&request.message_id).map_err(|error| error.to_string())?;
    let reference =
        whatsapp_rust::MessageRef::new(jid, message_id, participant.as_ref(), request.from_me)
            .map_err(|error| error.to_string())?;
    let actions = client.chat_actions();
    // The range lets every device keep what arrived after the person looked.
    let range =
        || whatsapp_rust::message_range(request.bound, None, [(reference.clone(), request.bound)]);
    match request.kind {
        RemoteDeletionKind::Message => {
            actions
                .delete_message_for_me(&reference, false, Some(request.bound))
                .await
        }
        RemoteDeletionKind::Clear => {
            actions
                .clear_chat_with_options(
                    jid,
                    whatsapp_rust::ClearChatOptions::default(),
                    Some(range()),
                )
                .await
        }
        RemoteDeletionKind::Chat => actions.delete_chat(jid, false, Some(range())).await,
    }
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Readiness {
        Readiness {
            connected: true,
            syncing: false,
            privacy_ready: true,
            cleanup_failed: false,
        }
    }

    #[test]
    fn a_healthy_session_may_send() {
        assert_eq!(ready().blocker(), None);
    }

    #[test]
    fn every_unhealthy_state_blocks_sending() {
        let blocked = [
            Readiness {
                connected: false,
                ..ready()
            },
            Readiness {
                syncing: true,
                ..ready()
            },
            Readiness {
                privacy_ready: false,
                ..ready()
            },
            Readiness {
                cleanup_failed: true,
                ..ready()
            },
        ];
        for state in blocked {
            assert!(state.blocker().is_some(), "{state:?} must hold deletions");
        }
    }

    #[test]
    fn only_a_group_message_from_someone_else_needs_a_participant() {
        assert_eq!(
            participant_of("room@g.us", false, "15550002222@s.whatsapp.net"),
            Ok(Some("15550002222@s.whatsapp.net".to_owned()))
        );
        assert_eq!(participant_of("room@g.us", true, "me"), Ok(None));
        assert_eq!(
            participant_of("15550002222@s.whatsapp.net", false, "anything"),
            Ok(None)
        );
        assert!(participant_of("room@g.us", false, "not a jid").is_err());
    }

    use super::super::tests::worker;
    use crate::archive::tests::message;

    const CHAT: &str = "chat@s.whatsapp.net";
    const GROUP: &str = "room@g.us";

    fn worker_with_messages(chat: &str, timestamps: &[i64]) -> Worker {
        let (worker, _events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(chat, "A").expect("chat");
        for &timestamp in timestamps {
            worker
                .archive
                .insert_message(
                    &message(chat, &format!("m{timestamp}"), timestamp, false),
                    None,
                )
                .expect("insert");
        }
        worker
    }

    fn queued(worker: &Worker) -> Option<RemoteDeletion> {
        worker.archive.next_remote_deletion().expect("reads queue")
    }

    #[test]
    fn a_confirmed_message_deletion_is_removed_here_and_queued() {
        let mut worker = worker_with_messages(CHAT, &[1, 2]);
        worker.delete_message_everywhere(CHAT.to_owned(), "m1".to_owned());
        assert!(worker.archive.message(CHAT, "m1").unwrap().is_none());
        assert!(worker.archive.message(CHAT, "m2").unwrap().is_some());
        let request = queued(&worker).expect("queued");
        assert_eq!(request.kind, RemoteDeletionKind::Message);
        assert_eq!(request.message_id, "m1");
        assert_eq!(request.bound, 1);
    }

    #[test]
    fn repeating_a_deletion_queues_nothing_new() {
        let mut worker = worker_with_messages(CHAT, &[1]);
        worker.delete_message_everywhere(CHAT.to_owned(), "m1".to_owned());
        let first = queued(&worker).expect("queued");
        worker
            .archive
            .finish_remote_deletion(&first.key(), first.bound)
            .expect("sent");
        worker.delete_message_everywhere(CHAT.to_owned(), "m1".to_owned());
        assert_eq!(queued(&worker), None, "the message is already gone");
    }

    #[test]
    fn a_message_the_archive_lacks_is_never_sent_for_deletion() {
        let mut worker = worker_with_messages(CHAT, &[1]);
        worker.delete_message_everywhere(CHAT.to_owned(), "never-archived".to_owned());
        assert_eq!(queued(&worker), None);
        assert!(worker.archive.message(CHAT, "m1").unwrap().is_some());
    }

    #[test]
    fn a_chat_deletion_reaches_no_further_than_the_newest_archived_message() {
        let mut worker = worker_with_messages(CHAT, &[1, 2, 3]);
        worker.delete_chat_everywhere(CHAT.to_owned());
        assert!(worker.archive.chat(CHAT).unwrap().is_none());
        let request = queued(&worker).expect("queued");
        assert_eq!(request.kind, RemoteDeletionKind::Chat);
        assert_eq!(request.message_id, "m3");
        assert_eq!(request.bound, 3);
    }

    #[test]
    fn clearing_a_chat_queues_a_clear_and_keeps_the_chat() {
        let mut worker = worker_with_messages(CHAT, &[1, 2]);
        worker.clear_chat_everywhere(CHAT.to_owned());
        assert!(worker.archive.chat(CHAT).unwrap().is_some());
        assert!(worker.archive.message(CHAT, "m2").unwrap().is_none());
        assert_eq!(
            queued(&worker).expect("queued").kind,
            RemoteDeletionKind::Clear
        );
    }

    #[test]
    fn an_empty_chat_is_removed_here_without_asking_other_devices() {
        let (mut worker, events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(CHAT, "A").expect("chat");
        worker.delete_chat_everywhere(CHAT.to_owned());
        assert!(worker.archive.chat(CHAT).unwrap().is_none());
        assert_eq!(queued(&worker), None, "an empty archive bounds nothing");
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Error(_)))
        );
    }

    #[test]
    fn a_reset_archive_cannot_produce_a_deletion() {
        let mut worker = worker_with_messages(CHAT, &[1, 2]);
        worker.archive.clear().expect("reset");
        worker.delete_chat_everywhere(CHAT.to_owned());
        worker.clear_chat_everywhere(CHAT.to_owned());
        worker.delete_message_everywhere(CHAT.to_owned(), "m1".to_owned());
        assert_eq!(queued(&worker), None);
    }

    #[test]
    fn a_group_message_from_someone_else_names_its_sender() {
        let mut worker = worker_with_messages(GROUP, &[]);
        let mut incoming = message(GROUP, "g1", 5, false);
        incoming.sender = "15550002222@s.whatsapp.net".into();
        worker
            .archive
            .insert_message(&incoming, None)
            .expect("insert");
        worker.delete_message_everywhere(GROUP.to_owned(), "g1".to_owned());
        let request = queued(&worker).expect("queued");
        assert_eq!(
            request.participant.as_deref(),
            Some("15550002222@s.whatsapp.net")
        );
        assert!(!request.from_me);
    }

    #[test]
    fn a_message_whose_sender_cannot_be_named_stays_local() {
        let (mut worker, events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(GROUP, "Room").expect("chat");
        let mut odd = message(GROUP, "g1", 5, false);
        odd.sender = "not a jid".into();
        worker.archive.insert_message(&odd, None).expect("insert");
        worker.delete_message_everywhere(GROUP.to_owned(), "g1".to_owned());
        assert!(worker.archive.message(GROUP, "g1").unwrap().is_none());
        assert_eq!(queued(&worker), None);
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Error(_)))
        );
    }

    #[test]
    fn a_deletion_stays_queued_while_the_session_is_not_ready() {
        let mut worker = worker_with_messages(CHAT, &[1]);
        worker.delete_message_everywhere(CHAT.to_owned(), "m1".to_owned());
        worker.syncing = true;
        worker.pump_remote_deletions();
        assert!(queued(&worker).is_some());
        assert!(worker.remote_delete.in_flight.is_none());
    }

    #[test]
    fn only_the_phones_acceptance_removes_a_queued_deletion() {
        let mut worker = worker_with_messages(CHAT, &[1]);
        worker.delete_message_everywhere(CHAT.to_owned(), "m1".to_owned());
        let request = queued(&worker).expect("queued");
        let key = request.key();

        let attempt = worker
            .remote_delete
            .start(&key, Instant::now())
            .expect("starts");
        worker.remote_deletion_finished(attempt + 1, &key, request.bound, true);
        assert!(queued(&worker).is_some(), "a stale attempt changes nothing");

        worker.remote_deletion_finished(attempt, &key, request.bound, false);
        assert!(queued(&worker).is_some(), "a failure keeps the request");

        worker.remote_delete.retry_at = None;
        let retry = worker
            .remote_delete
            .start(&key, Instant::now())
            .expect("retry");
        worker.remote_deletion_finished(retry, &key, request.bound, true);
        assert_eq!(queued(&worker), None);
    }

    #[test]
    fn an_in_flight_acknowledgment_preserves_a_newer_chat_deletion() {
        let mut worker = worker_with_messages(CHAT, &[1]);
        worker.delete_chat_everywhere(CHAT.to_owned());
        let first = queued(&worker).unwrap();
        let key = first.key();
        let attempt = worker.remote_delete.start(&key, Instant::now()).unwrap();

        worker.archive.ensure_chat(CHAT, "Test").unwrap();
        worker
            .archive
            .insert_message(&message(CHAT, "m2", 2, false), None)
            .unwrap();
        worker.delete_chat_everywhere(CHAT.to_owned());

        worker.remote_deletion_finished(attempt, &key, first.bound, true);

        assert_eq!(queued(&worker).unwrap().bound, 2);
    }

    #[test]
    fn a_deletion_that_keeps_failing_is_abandoned_and_the_person_is_told() {
        let (mut worker, events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(CHAT, "A").expect("chat");
        worker
            .archive
            .insert_message(&message(CHAT, "m1", 1, false), None)
            .expect("insert");
        worker.delete_message_everywhere(CHAT.to_owned(), "m1".to_owned());
        let request = queued(&worker).expect("queued");
        let key = request.key();
        for _ in 0..MAX_ATTEMPTS {
            worker.remote_delete.retry_at = None;
            let attempt = worker
                .remote_delete
                .start(&key, Instant::now())
                .expect("starts");
            worker.remote_deletion_finished(attempt, &key, request.bound, false);
        }
        assert_eq!(queued(&worker), None);
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Error(_)))
        );
        assert!(
            worker.archive.message(CHAT, "m1").unwrap().is_none(),
            "giving up on the phone never restores the local message"
        );
    }

    #[test]
    fn one_deletion_is_in_flight_and_failures_back_off() {
        let mut sync = RemoteDeleteSync::default();
        let now = Instant::now();
        let first = sync.start("a", now).expect("starts");
        assert!(sync.start("b", now).is_none());
        assert!(!sync.finish(first, "b", true, now), "another key's result");
        assert!(sync.finish(first, "a", false, now));
        assert!(!sync.ready(now + Duration::from_secs(29)));
        assert!(sync.ready(now + Duration::from_secs(30)));
        let retry = sync
            .start("a", now + Duration::from_secs(30))
            .expect("retry");
        assert_ne!(first, retry);
        assert!(
            !sync.finish(first, "a", true, now),
            "a stale attempt cannot release the retry"
        );
        assert!(sync.finish(retry, "a", true, now));
        assert!(sync.ready(now));
    }
}
