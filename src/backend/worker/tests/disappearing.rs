//! Pins, disappearing messages and the lines that report them.

use super::worker;
use super::*;
use crate::model::SystemEvent;
use whatsapp_rust::prelude::MessageField;

const CHAT: &str = "123-456@g.us";
const SENDER: &str = "111@s.whatsapp.net";

fn info(id: &str, timestamp: i64) -> MessageInfo {
    let mut info = group_message_id_info(SENDER);
    info.id = id.into();
    info.timestamp = whatsapp_rust::wacore::time::from_secs(timestamp).unwrap();
    info
}

fn timed_text(expiration: u32) -> wa::Message {
    from_default!(wa::Message {
        extended_text_message: MessageField::some(from_default!(
            wa::message::ExtendedTextMessage {
                text: Some("synthetic".into()),
                context_info: MessageField::some(from_default!(wa::ContextInfo {
                    expiration: Some(expiration),
                })),
            }
        )),
    })
}

fn keep_of(target: &str, kind: Option<wa::KeepType>, at_ms: i64) -> Arc<wa::Message> {
    Arc::new(from_default!(wa::Message {
        keep_in_chat_message: MessageField::some(from_default!(wa::message::KeepInChatMessage {
            key: MessageField::some(from_default!(wa::MessageKey {
                id: Some(target.into()),
            })),
            keep_type: kind,
            timestamp_ms: Some(at_ms),
        })),
    }))
}

#[test]
fn keep_pending_target_survives_an_overdue_replay() {
    let (mut worker, events, _, _) = worker();
    worker.ingest(
        &keep_of("target", Some(wa::KeepType::KeepForAll), 21_001),
        &info("keep", 21),
    );
    let target = Arc::new(timed_text(60));
    let target_info = info("target", 20);
    worker.ingest(&target, &target_info);
    worker.pump_expiry();

    assert!(worker.archive.is_kept(CHAT, "target").unwrap());
    assert_eq!(worker.archive.messages(CHAT, None, 5).unwrap().len(), 1);
    assert_eq!(worker.archive.chat(CHAT).unwrap().unwrap().unread, 0);
    let updates = emitted(&events);
    assert!(!updates.iter().any(|event| matches!(
        event,
        Event::Incoming { .. } | Event::MessageDeleted { .. } | Event::Messages { .. }
    )));
    assert!(updates.iter().any(|event| matches!(
        event, Event::KeptChanged { restored: Some(message), .. } if message.id == "target"
    )));

    worker.ingest(&target, &target_info);

    let updates = emitted(&events);
    assert!(!updates.iter().any(|event| matches!(
        event,
        Event::Incoming { .. } | Event::MessageDeleted { .. } | Event::Messages { .. }
    )));
    assert!(
        updates
            .iter()
            .any(|event| matches!(event, Event::MessageUpdated(message) if message.id == "target"))
    );
    assert_eq!(worker.archive.chat(CHAT).unwrap().unwrap().unread, 0);
}

#[test]
fn keep_unknown_and_older_changes_do_not_undo_a_confirmed_keep() {
    let (mut worker, _, _, _) = worker();
    worker.ingest(&Arc::new(timed_text(60)), &info("target", 20));
    for (id, kind, at_ms) in [
        ("keep", Some(wa::KeepType::KeepForAll), 24_005),
        ("unknown", None, 25_000),
        ("old-undo", Some(wa::KeepType::UndoKeepForAll), 24_001),
    ] {
        worker.ingest(&keep_of("target", kind, at_ms), &info(id, at_ms / 1000));
    }

    assert!(worker.archive.is_kept(CHAT, "target").unwrap());
}

#[test]
fn keep_change_cannot_retain_a_message_sent_without_a_timer() {
    let (mut worker, _, _, _) = worker();
    worker.ingest(&Arc::new(wa_text("synthetic")), &info("plain", 20));
    worker.ingest(
        &keep_of("plain", Some(wa::KeepType::KeepForAll), 21_000),
        &info("keep", 21),
    );

    assert!(!worker.archive.is_kept(CHAT, "plain").unwrap());
}

#[test]
fn direct_keep_uses_the_envelope_chat_not_the_senders_target_address() {
    let (mut worker, _, _, _) = worker();
    let direct = |id, timestamp| MessageInfo {
        id,
        source: MessageSource {
            chat: SENDER.parse().unwrap(),
            sender: SENDER.parse().unwrap(),
            ..Default::default()
        },
        timestamp: whatsapp_rust::wacore::time::from_secs(timestamp).unwrap(),
        ..Default::default()
    };
    worker.ingest(&Arc::new(timed_text(60)), &direct("target".into(), 20));
    let mut keep = keep_of("target", Some(wa::KeepType::KeepForAll), 21_000);
    Arc::make_mut(&mut keep)
        .keep_in_chat_message
        .as_option_mut()
        .unwrap()
        .key
        .as_option_mut()
        .unwrap()
        .remote_jid = Some(worker.me());

    worker.ingest(&keep, &direct("keep".into(), 21));

    assert!(worker.archive.is_kept(SENDER, "target").unwrap());
}

fn history_message(id: &str, message: wa::Message) -> wa::HistorySyncMsg {
    from_default!(wa::HistorySyncMsg {
        message: MessageField::some(from_default!(wa::WebMessageInfo {
            key: MessageField::some(from_default!(wa::MessageKey {
                id: Some(id.into()),
                remote_jid: Some(CHAT.into()),
                participant: Some(SENDER.into()),
            })),
            message: MessageField::some(message),
            message_timestamp: Some(20),
        })),
    })
}

#[test]
fn keep_in_history_retains_its_target_without_adding_an_action_row() {
    let (mut worker, _, _, _) = worker();
    let keep = keep_of("target", Some(wa::KeepType::KeepForAll), 25_000);
    let chat = parse_conversation(from_default!(wa::Conversation {
        id: CHAT.into(),
        messages: vec![
            history_message("keep", (*keep).clone()),
            history_message("target", timed_text(60)),
        ],
    }));
    let (_, committed) = worker.apply_history(
        ParsedHistory {
            chats: vec![chat],
            push_names: Vec::new(),
            lids: Vec::new(),
            stickers: Vec::new(),
        },
        true,
    );

    assert!(committed);
    assert!(worker.archive.is_kept(CHAT, "target").unwrap());
    let page = worker.archive.messages(CHAT, None, 5).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, "target");
}

#[test]
fn keep_snapshot_with_message_duration_is_restored_from_history() {
    let (mut worker, _, _, _) = worker();
    let mut target = history_message("target", wa_text("synthetic"));
    let info = target.message.as_option_mut().unwrap();
    info.ephemeral_duration = Some(60);
    info.keep_in_chat = MessageField::some(from_default!(wa::KeepInChat {
        keep_type: Some(wa::KeepType::KeepForAll),
        client_timestamp_ms: Some(25_000),
    }));
    let chat = parse_conversation(from_default!(wa::Conversation {
        id: CHAT.into(),
        messages: vec![target],
    }));
    let (_, committed) = worker.apply_history(
        ParsedHistory {
            chats: vec![chat],
            push_names: Vec::new(),
            lids: Vec::new(),
            stickers: Vec::new(),
        },
        true,
    );

    assert!(committed);
    assert_eq!(
        worker.archive.keep_state(CHAT, "target").unwrap(),
        Some((80, true))
    );
}

#[test]
fn keep_completion_does_not_override_newer_incoming_unkeep() {
    let (mut worker, events, _, _) = worker();
    worker.ingest(&Arc::new(timed_text(60)), &info("target", 20));
    worker.ingest(
        &keep_of("target", Some(wa::KeepType::UndoKeepForAll), 25_000),
        &info("undo", 25),
    );
    let _ = emitted(&events);
    worker.keep_set(CHAT.into(), "target".into(), true, Ok(24_000));

    assert!(!worker.archive.is_kept(CHAT, "target").unwrap());
    assert!(
        !emitted(&events)
            .iter()
            .any(|event| matches!(event, Event::KeptChanged { .. }))
    );
}

#[test]
fn keep_archive_failure_does_not_publish_a_success() {
    let (mut worker, events, _, _) = worker();
    worker.ingest(&Arc::new(timed_text(60)), &info("target", 20));
    worker
        .archive
        .execute_batch_for_test(
            "CREATE TRIGGER fail_keep BEFORE UPDATE OF kept ON messages
         BEGIN SELECT RAISE(ABORT, 'synthetic'); END;",
        )
        .unwrap();
    let _ = emitted(&events);
    worker.keep_set(CHAT.into(), "target".into(), true, Ok(24_000));

    assert!(!worker.archive.is_kept(CHAT, "target").unwrap());
    let events = emitted(&events);
    assert!(events.iter().any(|event| matches!(event, Event::Error(_))));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::KeptChanged { .. }))
    );
}

#[test]
fn keep_restores_a_hidden_row_without_publishing_a_new_arrival() {
    let (mut worker, events, _, _) = worker();
    worker.ingest(&Arc::new(timed_text(60)), &info("target", 20));
    let _ = emitted(&events);
    worker.ingest(
        &keep_of("target", Some(wa::KeepType::KeepForAll), 21_000),
        &info("keep", 21),
    );

    let updates = emitted(&events);
    assert!(updates.iter().any(|event| matches!(
        event, Event::KeptChanged { restored: Some(message), .. } if message.id == "target"
    )));
    assert!(
        !updates
            .iter()
            .any(|event| matches!(event, Event::Incoming { .. } | Event::Messages { .. }))
    );

    worker.ingest(
        &keep_of("target", Some(wa::KeepType::UndoKeepForAll), 22_000),
        &info("undo", 22),
    );
    assert_eq!(deleted(&events), ["target"]);
    assert!(worker.archive.messages(CHAT, None, 5).unwrap().is_empty());
}

fn pin_of(target: &str, kind: wa::message::pin_in_chat_message::Type, at_ms: i64) -> wa::Message {
    from_default!(wa::Message {
        pin_in_chat_message: MessageField::some(from_default!(wa::message::PinInChatMessage {
            key: MessageField::some(from_default!(wa::MessageKey {
                id: Some(target.into()),
            })),
            r#type: Some(kind),
            sender_timestamp_ms: Some(at_ms),
        })),
        message_context_info: MessageField::some(from_default!(wa::MessageContextInfo {
            message_add_on_duration_in_secs: Some(86_400),
        })),
    })
}

fn deleted(events: &std::sync::mpsc::Receiver<Event>) -> Vec<String> {
    emitted(events)
        .into_iter()
        .filter_map(|event| match event {
            Event::MessageDeleted { id, .. } => Some(id),
            _ => None,
        })
        .collect()
}

#[test]
fn a_live_pin_is_stored_and_leaves_one_unread_free_line() {
    use wa::message::pin_in_chat_message::Type;
    let (mut worker, events, _, _) = worker();
    worker.ingest(&Arc::new(wa_text("synthetic")), &info("target", 10));
    let unread = worker.archive.chat(CHAT).unwrap().map(|chat| chat.unread);
    let now_ms = jiff::Timestamp::now().as_millisecond();
    worker.ingest(
        &Arc::new(pin_of("target", Type::PinForAll, now_ms)),
        &info("pin-line", 11),
    );
    let pin = worker
        .archive
        .pinned_message(CHAT, now_ms)
        .unwrap()
        .unwrap();
    assert_eq!(pin.message.id, "target");
    let line = stored(&worker, CHAT, "pin-line");
    assert_eq!(
        line.content,
        Content::System {
            event: SystemEvent::Pinned
        }
    );
    assert_eq!(
        worker.archive.chat(CHAT).unwrap().map(|chat| chat.unread),
        unread,
        "a pin line is not an unread message"
    );
    assert!(
        emitted(&events)
            .iter()
            .any(|event| matches!(event, Event::Pinned { pin, .. } if pin.is_some()))
    );
}

#[test]
fn an_unpin_clears_the_bar_and_adds_no_line() {
    use wa::message::pin_in_chat_message::Type;
    let (mut worker, _events, _, _) = worker();
    worker.ingest(&Arc::new(wa_text("synthetic")), &info("target", 10));
    let now_ms = jiff::Timestamp::now().as_millisecond();
    worker.ingest(
        &Arc::new(pin_of("target", Type::PinForAll, now_ms)),
        &info("pin-line", 11),
    );
    worker.ingest(
        &Arc::new(pin_of("target", Type::UnpinForAll, now_ms + 1)),
        &info("unpin-line", 12),
    );
    assert!(
        worker
            .archive
            .pinned_message(CHAT, now_ms + 2)
            .unwrap()
            .is_none()
    );
    assert!(
        worker
            .archive
            .message(CHAT, "unpin-line")
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_message_sent_with_a_timer_runs_out_that_long_after_it_was_sent() {
    let (mut worker, _events, _, _) = worker();
    worker.ingest(&Arc::new(timed_text(86_400)), &info("timed", 20));
    worker.ingest(&Arc::new(wa_text("synthetic")), &info("plain", 21));
    let due = |at: i64| -> Vec<String> {
        worker
            .archive
            .expired_messages(at, 10)
            .unwrap()
            .into_iter()
            .map(|message| message.id)
            .collect()
    };
    assert!(due(20 + 86_399).is_empty());
    assert_eq!(due(20 + 86_400), ["timed"]);
}

#[test]
fn the_sweep_removes_only_what_ran_out_and_spares_kept_messages() {
    let (mut worker, events, _, _) = worker();
    worker.ingest(&Arc::new(timed_text(60)), &info("gone", 20));
    worker.ingest(&Arc::new(timed_text(60)), &info("kept", 21));
    worker.ingest(&Arc::new(timed_text(u32::MAX)), &info("later", 22));
    worker.ingest(&Arc::new(wa_text("synthetic")), &info("plain", 23));
    let keep = from_default!(wa::Message {
        keep_in_chat_message: MessageField::some(from_default!(wa::message::KeepInChatMessage {
            key: MessageField::some(from_default!(wa::MessageKey {
                id: Some("kept".into()),
            })),
            keep_type: Some(wa::KeepType::KeepForAll),
        })),
    });
    worker.ingest(&Arc::new(keep), &info("keep-line", 24));
    let _ = emitted(&events);

    worker.pump_expiry();

    assert_eq!(deleted(&events), ["gone"]);
    assert!(worker.archive.message(CHAT, "gone").unwrap().is_none());
    for id in ["kept", "later", "plain"] {
        assert!(worker.archive.message(CHAT, id).unwrap().is_some(), "{id}");
    }
}

#[test]
fn an_overdue_replay_is_not_published_or_counted_as_unread() {
    let (mut worker, events, _, _) = worker();
    let expired = timed_text(60);
    worker.ingest(&Arc::new(expired.clone()), &info("expired", 20));
    worker.pump_expiry();
    let _ = emitted(&events);

    worker.ingest(&Arc::new(expired), &info("expired", 20));

    assert!(
        !emitted(&events)
            .iter()
            .any(|event| matches!(event, Event::Incoming { .. } | Event::Messages { .. }))
    );
    let chat = worker.archive.chat(CHAT).unwrap().unwrap();
    assert_eq!(chat.unread, 0);
}

#[test]
fn an_overdue_stored_message_does_not_appear_before_the_next_sweep() {
    let (mut worker, _, _, _) = worker();
    worker.ingest(&Arc::new(timed_text(60)), &info("expired", 20));
    worker.ingest(&Arc::new(wa_text("synthetic")), &info("current", 21));

    let page = worker.archive.messages(CHAT, None, 1).unwrap();
    assert_eq!(page[0].id, "current");
    let earlier = worker
        .archive
        .messages(CHAT, Some((21, "current")), 5)
        .unwrap();
    assert!(earlier.is_empty());
}

#[test]
fn expiry_backfill_recovers_deadlines_from_the_stored_message_timer() {
    let (mut worker, _, _, _) = worker();
    worker.archive.ensure_chat(CHAT, "Fixture").unwrap();
    worker.archive.set_ephemeral(CHAT, 90 * 86_400, 10).unwrap();
    for (id, timestamp, wire) in [
        ("overdue", 20, timed_text(60)),
        ("future", 100, timed_text(60)),
        ("untimed", 25, wa_text("synthetic")),
    ] {
        worker
            .archive
            .insert_message(
                &crate::archive::tests::message(CHAT, id, timestamp, false),
                Some(&wire.encode_to_vec()),
            )
            .unwrap();
    }
    worker.archive.set_meta("derived", "8").unwrap();
    worker.backfill();

    let due = |at| {
        worker
            .archive
            .expired_messages(at, 10)
            .unwrap()
            .into_iter()
            .map(|message| message.id)
            .collect::<Vec<_>>()
    };
    assert!(due(79).is_empty());
    assert_eq!(due(80), ["overdue"]);
    assert_eq!(due(160), ["overdue", "future"]);
}

#[test]
fn expiry_removes_locally_and_never_queues_a_remote_deletion() {
    let (mut worker, _events, _, _) = worker();
    worker.ingest(&Arc::new(timed_text(60)), &info("gone", 20));
    worker.pump_expiry();
    assert!(worker.archive.message(CHAT, "gone").unwrap().is_none());
    assert!(worker.archive.next_remote_deletion().unwrap().is_none());
}

#[test]
fn the_sweep_runs_at_most_every_half_minute() {
    let (mut worker, _events, _, _) = worker();
    worker.pump_expiry();
    worker.ingest(&Arc::new(timed_text(60)), &info("late", 20));
    worker.pump_expiry();
    assert!(worker.archive.message(CHAT, "late").unwrap().is_some());
}

#[test]
fn a_timer_change_from_another_device_leaves_a_line() {
    let (mut worker, _events, _, _) = worker();
    let setting = from_default!(wa::Message {
        protocol_message: MessageField::some(from_default!(wa::message::ProtocolMessage {
            r#type: Some(wa::message::protocol_message::Type::EPHEMERAL_SETTING),
            ephemeral_expiration: Some(604_800),
        })),
    });
    worker.ingest(&Arc::new(setting), &info("timer-line", 30));
    assert_eq!(
        stored(&worker, CHAT, "timer-line").content,
        Content::System {
            event: SystemEvent::Timer { seconds: 604_800 }
        }
    );
    assert_eq!(
        worker
            .archive
            .chat(CHAT)
            .unwrap()
            .and_then(|chat| chat.ephemeral_expiration),
        Some(604_800)
    );
}

#[test]
fn system_lines_summarize_without_a_name() {
    assert_eq!(
        Content::System {
            event: SystemEvent::Timer { seconds: 0 }
        }
        .summary(),
        "Disappearing messages turned off"
    );
    assert_eq!(SystemEvent::Pinned.line(true, None), "You pinned a message");
    assert_eq!(
        SystemEvent::Pinned.line(false, Some("Ana")),
        "Ana pinned a message"
    );
    assert_eq!(SystemEvent::Pinned.line(false, None), "Pinned a message");
}

#[test]
fn expiry_keeps_the_attachment_of_a_permanent_forward() {
    let (mut worker, events, _, _) = worker();
    let root = tempfile::tempdir().unwrap();
    worker.dirs = AppDirs::under(root.path());
    let media_dir = worker.dirs.media_cache_dir();
    std::fs::create_dir_all(&media_dir).unwrap();
    let shared = media_dir.join("shared.jpg");
    std::fs::write(&shared, b"synthetic attachment").unwrap();

    const DESTINATION: &str = "789-012@g.us";
    for (chat, id, forwarded) in [(CHAT, "source", false), (DESTINATION, "forward", true)] {
        worker.archive.ensure_chat(chat, "Synthetic chat").unwrap();
        let mut message = crate::archive::tests::message(chat, id, 20, forwarded);
        message.forwarded = forwarded;
        message.content = Content::Image {
            caption: None,
            media: crate::model::Media {
                mime: "image/jpeg".into(),
                size: 20,
                width: None,
                height: None,
                path: Some(shared.clone()),
                state: Default::default(),
                view_once: false,
            },
        };
        worker.archive.insert_message(&message, None).unwrap();
    }
    worker
        .archive
        .set_expires_at(CHAT, "source", Some(80))
        .unwrap();

    worker.pump_expiry();

    assert_eq!(deleted(&events), ["source"]);
    assert!(worker.archive.message(CHAT, "source").unwrap().is_none());
    assert_eq!(
        stored(&worker, DESTINATION, "forward")
            .content
            .media()
            .unwrap()
            .path
            .as_deref(),
        Some(shared.as_path())
    );
    assert_eq!(std::fs::read(&shared).unwrap(), b"synthetic attachment");
}
