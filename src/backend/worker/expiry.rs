//! Disappearing messages: changing a chat's timer, removing messages whose
//! time has run out, and letting go of pins that ran out.
//!
//! Removal is local. It never queues a deletion for other devices, because
//! their copy runs out on its own clock, and a message someone kept stays.

use super::*;
use crate::model::SystemEvent;

/// How often the archive is checked for messages that ran out.
const CHECK_EVERY: Duration = Duration::from_secs(30);
/// Most messages removed in one pass, so a long-closed app catches up in steps.
const BATCH: usize = 200;

#[derive(Default)]
pub(super) struct Expiry {
    next: Option<Instant>,
}

impl Worker {
    /// `Command::SetDisappearing`: the new timer applies once WhatsApp took it.
    pub(super) fn set_disappearing(&mut self, chat: ChatId, seconds: u32) {
        if !crate::model::TIMER_CHOICES.contains(&seconds) {
            return;
        }
        let chat = self.canonical_str(&chat);
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error(
                "Could not change disappearing messages while disconnected".to_owned(),
            ));
            return;
        };
        let at = jiff::Timestamp::now().as_second();
        let commands = self.commands.clone();
        let generation = self.session_generation;
        tokio::spawn(async move {
            let result = if jid.is_group() {
                client
                    .groups()
                    .set_ephemeral(jid, seconds)
                    .await
                    .map(|()| String::new())
                    .map_err(|error| error.to_string())
            } else {
                client
                    .set_chat_disappearing_timer(jid, seconds)
                    .await
                    .map(|sent| sent.message_id.into_string())
                    .map_err(|error| error.to_string())
            };
            let _ = Completion {
                generation,
                done: Done::DisappearingSet {
                    chat,
                    seconds,
                    at,
                    result,
                },
            }
            .send(&commands);
        });
    }

    pub(super) fn disappearing_set(
        &mut self,
        chat: ChatId,
        seconds: u32,
        at: i64,
        result: Result<String, String>,
    ) {
        let stanza = match result {
            Ok(stanza) => stanza,
            Err(error) => {
                log::warn!("could not change the disappearing timer: {error}");
                self.emit(Event::Error(
                    "Could not change disappearing messages".to_owned(),
                ));
                return;
            }
        };
        let accepted = self
            .archive
            .set_ephemeral(&chat, seconds, at)
            .unwrap_or(false);
        // A group tells everyone, including us, with a notification that
        // carries its own line; a 1:1 chat has only what we just sent.
        if accepted && !stanza.is_empty() {
            let me = self.me();
            self.store_system_line(
                &chat,
                &stanza,
                me,
                None,
                true,
                at,
                SystemEvent::Timer { seconds },
            );
        }
        self.emit_chat(&chat);
    }

    /// `Command::SetKept`: keeps a disappearing message for everyone in the
    /// chat, or lets it run out again. The archive changes once WhatsApp
    /// took the request, in `keep_set`.
    pub(super) fn set_kept(&mut self, chat: ChatId, id: String, kept: bool) {
        let chat = self.canonical_str(&chat);
        let Ok(Some(message)) = self.archive.message(&chat, &id) else {
            return;
        };
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error(
                "Could not keep the message while disconnected".to_owned(),
            ));
            return;
        };
        // The phone names the sender of a group message from someone else;
        // everywhere else the key has no participant.
        let key = from_default!(wa::MessageKey {
            remote_jid: Some(chat.clone()),
            from_me: Some(message.from_me),
            id: Some(id.clone()),
            participant: (jid.is_group() && !message.from_me).then(|| message.sender.clone()),
        });
        let commands = self.commands.clone();
        let generation = self.session_generation;
        tokio::spawn(async move {
            let result = client
                .keep_message(jid, key, kept)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = Completion {
                generation,
                done: Done::KeepSet {
                    chat,
                    id,
                    kept,
                    result,
                },
            }
            .send(&commands);
        });
    }

    pub(super) fn keep_set(
        &mut self,
        chat: ChatId,
        id: String,
        kept: bool,
        result: Result<(), String>,
    ) {
        match result {
            Ok(()) => {
                let _ = self.archive.set_kept(&chat, &id, kept);
                self.emit(Event::KeptChanged {
                    chat,
                    id,
                    kept,
                    local: true,
                });
            }
            Err(error) => {
                log::warn!("could not change whether a message is kept: {error}");
                self.emit(Event::Error("Could not keep the message".to_owned()));
            }
        }
    }

    /// Stamps when a message runs out, from the timer it was sent with.
    pub(super) fn remember_expiry(
        &self,
        chat: &str,
        id: &str,
        timestamp: i64,
        seconds: Option<u32>,
    ) {
        if let Some(seconds) = seconds.filter(|seconds| *seconds > 0) {
            let _ = self.archive.set_expires_at(
                chat,
                id,
                Some(timestamp.saturating_add(i64::from(seconds))),
            );
        }
    }

    /// Called on every tick; does its work at most every half minute.
    pub(super) fn pump_expiry(&mut self) {
        let now = Instant::now();
        if self.expiry.next.is_some_and(|due| now < due) {
            return;
        }
        self.expiry.next = Some(now + CHECK_EVERY);
        if !self.privacy.privacy_ready {
            return;
        }
        self.remove_expired();
    }

    fn remove_expired(&mut self) {
        let now = jiff::Timestamp::now();
        let mut touched = std::collections::BTreeSet::new();
        match self.archive.expired_messages(now.as_second(), BATCH) {
            Ok(due) => {
                let media_dir = self.effective_media_dir();
                for message in due {
                    // Only a file ZapTide itself downloaded goes with the message.
                    if let Some(path) = &message.media_path
                        && path.starts_with(&media_dir)
                    {
                        let _ = std::fs::remove_file(path);
                    }
                    match self.archive.delete_message(&message.chat, &message.id) {
                        Ok(true) => {
                            self.emit(Event::MessageDeleted {
                                chat: message.chat.clone(),
                                id: message.id,
                            });
                            touched.insert(message.chat);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            log::warn!("could not remove an expired message: {error}");
                            break;
                        }
                    }
                }
            }
            Err(error) => log::warn!("could not list expired messages: {error}"),
        }
        match self.archive.expire_pins(now.as_millisecond()) {
            Ok(chats) => touched.extend(chats),
            Err(error) => log::warn!("could not end expired pins: {error}"),
        }
        for chat in touched {
            self.emit_chat(&chat);
            self.emit_pin(&chat);
        }
    }
}
