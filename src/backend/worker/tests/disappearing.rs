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
