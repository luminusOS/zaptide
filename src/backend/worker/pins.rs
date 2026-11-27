//! Pinning a message for everyone in a chat, and applying pins from others.

use super::*;
use crate::model::SystemEvent;
use whatsapp_rust::PinDuration;

/// A pin lasts a week when the sender names no duration.
const DEFAULT_PIN_SECONDS: u32 = 604_800;

/// The durations WhatsApp offers when pinning: 24 hours, 7 days, 30 days.
pub(super) fn pin_duration(seconds: u32) -> Option<PinDuration> {
    match seconds {
        86_400 => Some(PinDuration::Hours24),
        604_800 => Some(PinDuration::Days7),
        2_592_000 => Some(PinDuration::Days30),
        _ => None,
    }
}

/// A pin or unpin read from a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PinChange {
    pub(super) target: String,
    pub(super) pinned: bool,
    pub(super) at_ms: i64,
    /// How long the pin lasts; meaningful for a pin only.
    pub(super) seconds: u32,
}

impl PinChange {
    pub(super) fn expires_at_ms(&self) -> Option<i64> {
        self.pinned
            .then(|| self.at_ms.saturating_add(i64::from(self.seconds) * 1000))
    }
}

/// Reads a pin or unpin from a message, or `None` for anything else.
/// `fallback_at_ms` stands in when the sender gave no time.
pub(super) fn pin_change(message: &wa::Message, fallback_at_ms: i64) -> Option<PinChange> {
    use wa::message::pin_in_chat_message::Type;
    let pin = message.pin_in_chat_message.as_option()?;
    let target = pin.key.as_option().and_then(|key| key.id.clone())?;
    let pinned = match pin.r#type {
        Some(Type::PinForAll) => true,
        Some(Type::UnpinForAll) => false,
        _ => return None,
    };
    let seconds = message
        .message_context_info
        .as_option()
        .and_then(|context| context.message_add_on_duration_in_secs)
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_PIN_SECONDS);
    Some(PinChange {
        target,
        pinned,
        at_ms: pin.sender_timestamp_ms.unwrap_or(fallback_at_ms),
        seconds,
    })
}

impl Worker {
    /// `Command::PinMessage` and `Command::UnpinMessage`. The pin shows only
    /// once WhatsApp accepted it, so the bar never claims more than is true.
    pub(super) fn change_pin(&mut self, chat: ChatId, id: String, seconds: Option<u32>) {
        let chat = self.canonical_str(&chat);
        let pinned = seconds.is_some();
        let duration = match seconds {
            Some(seconds) => match pin_duration(seconds) {
                Some(duration) => Some(duration),
                None => return,
            },
            None => None,
        };
        let message = match self.archive.message(&chat, &id) {
            Ok(Some(message)) => message,
            _ => return,
        };
        if matches!(message.content, Content::Revoked | Content::System { .. }) {
            return;
        }
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error(
                "Could not change the pin while disconnected".to_owned(),
            ));
            return;
        };
        // The key names the sender of a group message from someone else; the
        // library resolves our own address for our own messages.
        let participant = (jid.is_group() && !message.from_me)
            .then(|| Self::jid_of(&message.sender))
            .flatten();
        let from_me = message.from_me;
        let at_ms = jiff::Timestamp::now().as_millisecond();
        let seconds = seconds.unwrap_or(0);
        let commands = self.commands.clone();
        let generation = self.session_generation;
        tokio::spawn(async move {
            let result = async {
                let target = whatsapp_rust::MessageId::new(&id)
                    .and_then(|message_id| {
                        whatsapp_rust::MessageRef::new(
                            &jid,
                            message_id,
                            participant.as_ref(),
                            from_me,
                        )
                    })
                    .map_err(|error| error.to_string())?;
                let sent = match duration {
                    Some(duration) => client.pin_message(&target, duration).await,
                    None => client.unpin_message(&target).await,
                };
                sent.map(|sent| sent.message_id.into_string())
                    .map_err(|error| error.to_string())
            }
            .await;
            let _ = Completion {
                generation,
                done: Done::PinFinished {
                    chat,
                    id,
                    pinned,
                    seconds,
                    at_ms,
                    result,
                },
            }
            .send(&commands);
        });
    }

    pub(super) fn pin_finished(
        &mut self,
        chat: ChatId,
        id: String,
        pinned: bool,
        seconds: u32,
        at_ms: i64,
        result: Result<String, String>,
    ) {
        let stanza = match result {
            Ok(stanza) => stanza,
            Err(error) => {
                log::warn!("could not change a pin: {error}");
                self.emit(Event::Error(if pinned {
                    "Could not pin the message".to_owned()
                } else {
                    "Could not unpin the message".to_owned()
                }));
                return;
            }
        };
        let me = self.me();
        let expires = pinned.then(|| at_ms + i64::from(seconds) * 1000);
        let changed = self
            .archive
            .set_pin(&chat, &id, pinned, &me, at_ms, expires)
            .unwrap_or(false);
        if changed && pinned {
            self.store_system_line(
                &chat,
                &stanza,
                me,
                None,
                true,
                at_ms / 1000,
                SystemEvent::Pinned,
            );
        }
        self.emit_pin(&chat);
    }

    /// A pin or unpin that arrived live from another member or device.
    pub(super) fn apply_pin(&mut self, info: &MessageInfo, chat: &str, message: &wa::Message) {
        let Some(change) = pin_change(message, info.timestamp.timestamp_millis()) else {
            return;
        };
        let from_me = info.source.is_from_me;
        let by = if from_me {
            self.me()
        } else {
            self.canonical(&info.source.sender)
        };
        let changed = self
            .archive
            .set_pin(
                chat,
                &change.target,
                change.pinned,
                &by,
                change.at_ms,
                change.expires_at_ms(),
            )
            .unwrap_or(false);
        if !changed {
            return;
        }
        let running = change
            .expires_at_ms()
            .is_none_or(|end| end > jiff::Timestamp::now().as_millisecond());
        if change.pinned && running {
            let name = (!info.push_name.is_empty()).then(|| info.push_name.to_string());
            self.store_system_line(
                chat,
                &info.id,
                by,
                name,
                from_me,
                info.timestamp.timestamp(),
                SystemEvent::Pinned,
            );
        }
        self.emit_pin(chat);
    }

    /// Refreshes the featured pin and context-menu membership together.
    pub(super) fn emit_pin(&self, chat: &str) {
        let chat = self.canonical_str(chat);
        let now_ms = jiff::Timestamp::now().as_millisecond();
        let Ok((pin, active_ids)) = self.pin_snapshot(&chat, now_ms) else {
            log::warn!("could not read the pinned messages");
            return;
        };
        self.emit(Event::Pinned {
            chat,
            pin: Box::new(pin),
            active_ids,
        });
    }

    fn pin_snapshot(
        &self,
        chat: &str,
        now_ms: i64,
    ) -> crate::archive::Result<(Option<crate::archive::PinnedMessage>, HashSet<String>)> {
        let Some(mut featured) = self.archive.pinned_message(chat, now_ms)? else {
            return Ok((None, HashSet::new()));
        };
        let mut active_ids = HashSet::new();
        for id in self.archive.active_pin_targets(chat, now_ms)? {
            if id == featured.message.id
                || self
                    .archive
                    .message(chat, &id)?
                    .is_some_and(|message| !matches!(message.content, Content::Revoked))
            {
                active_ids.insert(id);
            }
        }
        self.polish(&mut featured.message);
        Ok((Some(featured), active_ids))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::prelude::MessageField;

    #[test]
    fn only_the_durations_whatsapp_offers_can_be_pinned() {
        assert!(matches!(pin_duration(86_400), Some(PinDuration::Hours24)));
        assert!(matches!(pin_duration(604_800), Some(PinDuration::Days7)));
        assert!(matches!(pin_duration(2_592_000), Some(PinDuration::Days30)));
        assert!(pin_duration(0).is_none());
        assert!(pin_duration(3_600).is_none());
    }

    fn pin_message(
        kind: wa::message::pin_in_chat_message::Type,
        seconds: Option<u32>,
    ) -> wa::Message {
        from_default!(wa::Message {
            pin_in_chat_message: MessageField::some(from_default!(wa::message::PinInChatMessage {
                key: MessageField::some(from_default!(wa::MessageKey {
                    id: Some("target".into()),
                })),
                r#type: Some(kind),
                sender_timestamp_ms: Some(5_000),
            })),
            message_context_info: MessageField::some(from_default!(wa::MessageContextInfo {
                message_add_on_duration_in_secs: seconds,
            })),
        })
    }

    #[test]
    fn a_pin_reads_its_target_time_and_duration() {
        use wa::message::pin_in_chat_message::Type;
        let change = pin_change(&pin_message(Type::PinForAll, Some(86_400)), 1).unwrap();
        assert_eq!(change.target, "target");
        assert!(change.pinned);
        assert_eq!(change.at_ms, 5_000);
        assert_eq!(change.expires_at_ms(), Some(5_000 + 86_400_000));
    }

    #[test]
    fn a_pin_without_a_duration_lasts_a_week_and_an_unpin_never_expires() {
        use wa::message::pin_in_chat_message::Type;
        let pin = pin_change(&pin_message(Type::PinForAll, None), 1).unwrap();
        assert_eq!(pin.seconds, DEFAULT_PIN_SECONDS);
        let unpin = pin_change(&pin_message(Type::UnpinForAll, Some(86_400)), 1).unwrap();
        assert!(!unpin.pinned);
        assert_eq!(unpin.expires_at_ms(), None);
    }

    #[test]
    fn other_messages_and_unknown_pin_types_are_not_pins() {
        use wa::message::pin_in_chat_message::Type;
        assert!(pin_change(&wa::Message::default(), 1).is_none());
        assert!(pin_change(&pin_message(Type::UnknownType, None), 1).is_none());
    }
    const CHAT: &str = "111@s.whatsapp.net";

    fn pending_pin(worker: &Worker, id: &str, at_ms: i64) {
        worker
            .archive
            .set_pin(CHAT, id, true, CHAT, at_ms, None)
            .unwrap();
    }

    fn shown_pins(events: &std::sync::mpsc::Receiver<Event>) -> Vec<Option<String>> {
        events
            .try_iter()
            .filter_map(|event| match event {
                Event::Pinned { chat, pin, .. } if chat == CHAT => {
                    Some(pin.map(|pin| pin.message.id))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn pin_state_emits_all_active_ids_and_only_the_newest_message() {
        let (mut worker, events, _, _) = super::super::tests::worker();
        for id in ["older", "newest", "expired", "revoked"] {
            worker.store_message(
                crate::archive::tests::message(CHAT, id, 1, false),
                None,
                None,
            );
        }
        pending_pin(&worker, "older", 10);
        pending_pin(&worker, "newest", 20);
        pending_pin(&worker, "revoked", 30);
        pending_pin(&worker, "pending", 40);
        worker
            .archive
            .set_pin(CHAT, "expired", true, CHAT, 50, Some(60))
            .unwrap();
        worker
            .archive
            .set_content(CHAT, "revoked", &Content::Revoked, false)
            .unwrap();

        worker.emit_pin(CHAT);

        let snapshots: Vec<_> = events
            .try_iter()
            .filter_map(|event| match event {
                Event::Pinned {
                    chat,
                    pin,
                    active_ids,
                } if chat == CHAT => Some((pin.map(|pin| pin.message.id), active_ids)),
                _ => None,
            })
            .collect();
        assert_eq!(
            snapshots,
            [(
                Some("newest".into()),
                HashSet::from(["older".into(), "newest".into()])
            )]
        );
    }

    #[test]
    fn alias_only_unpin_refreshes_the_canonical_pin_after_identity_merge() {
        const ALIAS: &str = "222@lid";
        let (mut worker, events, _, _) = super::super::tests::worker();
        worker.store_message(
            crate::archive::tests::message(CHAT, "target", 1, false),
            None,
            None,
        );
        pending_pin(&worker, "target", 10);
        worker.emit_pin(CHAT);
        assert_eq!(shown_pins(&events), [Some("target".into())]);
        worker
            .archive
            .set_pin(ALIAS, "target", false, ALIAS, 10, None)
            .unwrap();
        assert!(worker.archive.chat(ALIAS).unwrap().is_none());
        assert!(worker.archive.contact(ALIAS).unwrap().is_none());
        assert!(worker.archive.message(ALIAS, "target").unwrap().is_none());

        worker.learn_lid("222", "111");

        assert!(worker.archive.pinned_message(CHAT, 20).unwrap().is_none());
        let mut refreshed = false;
        let mut pinned = Vec::new();
        for event in events.try_iter() {
            match event {
                Event::RefreshTranscript => refreshed = true,
                Event::Pinned {
                    chat,
                    pin,
                    active_ids,
                } if chat == CHAT => pinned.push((*pin, active_ids)),
                _ => {}
            }
        }
        assert!(refreshed, "the pin-only merge changed the canonical chat");
        assert_eq!(pinned, [(None, HashSet::new())]);
    }

    fn pin_history(messages: Vec<wa::HistorySyncMsg>) -> ParsedHistory {
        ParsedHistory {
            chats: vec![parse_conversation(from_default!(wa::Conversation {
                id: CHAT.into(),
                messages,
            }))],
            push_names: Vec::new(),
            lids: Vec::new(),
            stickers: Vec::new(),
        }
    }

    fn history_entry(id: &str, message: wa::Message) -> wa::HistorySyncMsg {
        from_default!(wa::HistorySyncMsg {
            message: MessageField::some(from_default!(wa::WebMessageInfo {
                key: MessageField::some(from_default!(wa::MessageKey {
                    remote_jid: Some(CHAT.into()),
                    id: Some(id.into()),
                    from_me: Some(false),
                })),
                message: MessageField::some(message),
                message_timestamp: Some(100),
            })),
        })
    }

    #[test]
    fn pin_state_refreshes_when_its_target_arrives_live() {
        let (mut worker, events, _, _) = super::super::tests::worker();
        pending_pin(&worker, "late", 10);
        let message = crate::archive::tests::message(CHAT, "late", 100, false);

        worker.store_message(message, None, None);

        assert_eq!(shown_pins(&events), [Some("late".into())]);
    }

    #[test]
    fn pin_state_refreshes_when_its_target_arrives_in_history() {
        let (mut worker, events, _, _) = super::super::tests::worker();
        pending_pin(&worker, "late", 10);
        let history = pin_history(vec![history_entry(
            "late",
            from_default!(wa::Message {
                conversation: Some("synthetic".into()),
            }),
        )]);

        assert!(worker.apply_history(history, true).1);

        assert_eq!(shown_pins(&events), [Some("late".into())]);
    }

    #[test]
    fn pin_state_stays_private_while_its_target_is_filed() {
        let (mut worker, events, _, _) = super::super::tests::worker();
        worker.privacy.privacy_ready = false;
        pending_pin(&worker, "late", 10);

        worker.store_message(
            crate::archive::tests::message(CHAT, "late", 100, false),
            None,
            None,
        );

        assert!(worker.archive.pinned_message(CHAT, 20).unwrap().is_some());
        assert!(shown_pins(&events).is_empty());
    }

    #[test]
    fn pin_state_refreshes_after_a_history_revoke() {
        let (mut worker, events, _, _) = super::super::tests::worker();
        worker.store_message(
            crate::archive::tests::message(CHAT, "older", 100, false),
            None,
            None,
        );
        worker.store_message(
            crate::archive::tests::message(CHAT, "target", 101, false),
            None,
            None,
        );
        pending_pin(&worker, "older", 10);
        pending_pin(&worker, "target", 20);
        let history = pin_history(vec![history_entry(
            "revoke",
            from_default!(wa::Message {
                protocol_message: MessageField::some(from_default!(wa::message::ProtocolMessage {
                    r#type: Some(wa::message::protocol_message::Type::REVOKE),
                    key: MessageField::some(from_default!(wa::MessageKey {
                        id: Some("target".into()),
                    })),
                })),
            }),
        )]);

        assert!(worker.apply_history(history, true).1);

        assert_eq!(shown_pins(&events), [Some("older".into())]);
    }

    #[tokio::test]
    async fn pin_state_refreshes_after_a_phone_message_deletion() {
        let (mut worker, events, _, _) = super::super::tests::worker();
        worker.store_message(
            crate::archive::tests::message(CHAT, "target", 100, false),
            None,
            None,
        );
        pending_pin(&worker, "target", 10);
        let update = wa_events::DeleteMessageForMeUpdate::builder()
            .chat_jid(CHAT.parse().unwrap())
            .message_id("target".into())
            .from_me(false)
            .timestamp(whatsapp_rust::wacore::time::from_secs(200).unwrap())
            .action(Box::default())
            .from_full_sync(false)
            .build();

        worker
            .handle_wa_event(Arc::new(wa_events::Event::DeleteMessageForMeUpdate(update)))
            .await;

        assert_eq!(shown_pins(&events), [None]);
    }
}
