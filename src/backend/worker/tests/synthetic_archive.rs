//! Timings over a generated encrypted archive. Every chat, name and byte is
//! synthetic and derived from its index, so runs are comparable. Run with
//! `cargo test --locked --lib synthetic_archive -- --ignored --nocapture`.

use super::*;
use std::time::Duration;
use whatsapp_rust::prelude::MessageField;

const CHATS: usize = 1000;
const GROUPS: usize = 50;
const LARGE_GROUPS: usize = 3;
const LARGE_GROUP_MEMBERS: usize = 512;
const LARGE_GROUP_MESSAGES: usize = 5000;
const SMALL_GROUP_MEMBERS: usize = 24;
const ARCHIVE_MESSAGES: usize = 70_000;
const RAW_BYTES: usize = 5 * 1024;
const HISTORY_CHATS: usize = 40;
const HISTORY_GROUPS: usize = 4;
const HISTORY_MESSAGES: usize = 20_000;
const BASE_TIME: i64 = 1_700_000_000;

fn chat_id(index: usize) -> String {
    if index < GROUPS {
        format!("120363{index:012}@g.us")
    } else {
        format!("1555{index:07}@s.whatsapp.net")
    }
}

fn history_chat_id(index: usize) -> String {
    if index < HISTORY_GROUPS {
        format!("120364{index:012}@g.us")
    } else {
        format!("1777{index:07}@s.whatsapp.net")
    }
}

fn person(index: usize) -> String {
    format!("1666{index:07}@s.whatsapp.net")
}

fn members(chat: usize) -> Vec<String> {
    let (first, count) = if chat < LARGE_GROUPS {
        (chat * LARGE_GROUP_MEMBERS, LARGE_GROUP_MEMBERS)
    } else {
        (
            LARGE_GROUPS * LARGE_GROUP_MEMBERS + chat * SMALL_GROUP_MEMBERS,
            SMALL_GROUP_MEMBERS,
        )
    };
    (first..first + count).map(person).collect()
}

/// An image message whose encoded protobuf is about `RAW_BYTES`, most of it
/// the inline thumbnail, as with photos from history sync.
fn photo(seed: usize) -> wa::Message {
    let thumbnail = (0..RAW_BYTES - 256)
        .map(|byte| (byte.wrapping_mul(31) ^ seed) as u8)
        .collect();
    wa::Message {
        image_message: MessageField::some(wa::message::ImageMessage {
            caption: Some(format!("synthetic photo {seed}")),
            mimetype: Some("image/jpeg".into()),
            file_length: Some(200_000 + seed as u64),
            width: Some(1280),
            height: Some(960),
            media_key: Some(vec![seed as u8; 32]),
            file_sha256: Some(vec![1; 32]),
            file_enc_sha256: Some(vec![2; 32]),
            direct_path: Some(format!("/v/t62.7118-24/synthetic-{seed}.enc")),
            jpeg_thumbnail: Some(thumbnail),
            ..Default::default()
        }),
        ..Default::default()
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    worker: Worker,
    events: std::sync::mpsc::Receiver<Event>,
    largest_group: String,
    direct: String,
}

/// Fills an encrypted on-disk archive through the same `Archive` calls the
/// worker uses when filing live messages. The fixture is written in one
/// transaction because only what is measured afterwards matters here.
fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("temporary directory");
    let (mut worker, events, _, _) = worker();
    worker.archive = Archive::open_encrypted_for_test(&root.path().join("archive.db"), &[9; 32])
        .expect("encrypted archive");
    worker.archive.execute_batch_for_test("BEGIN").unwrap();
    for chat in 0..CHATS {
        let id = chat_id(chat);
        let mut row = Chat::new(id.clone(), format!("Synthetic chat {chat}"));
        row.last_activity = BASE_TIME;
        worker.archive.upsert_chat(&row).unwrap();
        if chat < GROUPS {
            worker
                .archive
                .set_group_info(&id, None, &members(chat), false)
                .unwrap();
        }
    }
    let people = LARGE_GROUPS * LARGE_GROUP_MEMBERS + GROUPS * SMALL_GROUP_MEMBERS;
    for index in 0..people {
        let contact = Contact {
            id: person(index),
            full_name: (index.is_multiple_of(3)).then(|| format!("Saved person {index}")),
            push_name: Some(format!("Person {index}")),
        };
        worker.archive.upsert_contact(&contact).unwrap();
        worker.contacts.insert(contact.id.clone(), contact);
    }
    let large = LARGE_GROUPS * LARGE_GROUP_MESSAGES;
    for n in 0..ARCHIVE_MESSAGES {
        let chat = if n < large {
            n / LARGE_GROUP_MESSAGES
        } else {
            LARGE_GROUPS + (n - large) % (CHATS - LARGE_GROUPS)
        };
        let id = chat_id(chat);
        let from_me = n.is_multiple_of(4);
        let sender = if from_me {
            worker.me()
        } else if chat < GROUPS {
            let members = members(chat);
            members[n % members.len()].clone()
        } else {
            id.clone()
        };
        let raw = photo(n);
        let mut message = crate::archive::tests::message(
            &id,
            &format!("SYNTH{n:08}"),
            BASE_TIME + n as i64,
            from_me,
        );
        message.sender = sender;
        message.content = classify(&raw).expect("photo");
        message.thumbnail = thumbnail_of(&raw);
        if from_me {
            message.status = Delivery::Read;
        }
        worker
            .archive
            .insert_message(&message, Some(&raw.encode_to_vec()))
            .unwrap();
    }
    worker.archive.set_meta("derived", "6").unwrap();
    worker.archive.execute_batch_for_test("COMMIT").unwrap();
    Fixture {
        _root: root,
        worker,
        events,
        largest_group: chat_id(0),
        direct: chat_id(CHATS - 1),
    }
}

struct Entry<'a> {
    id: String,
    message: wa::Message,
    timestamp: i64,
    sender: Option<&'a str>,
    from_me: bool,
}

fn history_entry(chat: &str, entry: Entry<'_>) -> wa::HistorySyncMsg {
    wa::HistorySyncMsg {
        message: MessageField::some(wa::WebMessageInfo {
            key: MessageField::some(wa::MessageKey {
                remote_jid: Some(chat.into()),
                from_me: Some(entry.from_me),
                id: Some(entry.id),
                participant: entry.sender.map(str::to_owned),
            }),
            message: MessageField::some(entry.message),
            message_timestamp: Some(entry.timestamp as u64),
            push_name: entry.sender.map(|sender| format!("Pushed {sender}")),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A history chunk shaped like the phone's first sync: a few busy groups,
/// many direct chats, push names for every group sender.
fn history() -> ParsedHistory {
    let per_chat = HISTORY_MESSAGES / HISTORY_CHATS;
    let mut push_names = Vec::new();
    let chats = (0..HISTORY_CHATS)
        .map(|chat| {
            let id = history_chat_id(chat);
            let senders = members(GROUPS - 1 - chat);
            let messages = (0..per_chat)
                .map(|offset| {
                    let n = chat * per_chat + offset;
                    let from_me = n.is_multiple_of(4);
                    let sender = (chat < HISTORY_GROUPS && !from_me)
                        .then(|| senders[n % senders.len()].as_str());
                    let message = if n.is_multiple_of(3) {
                        photo(ARCHIVE_MESSAGES + n)
                    } else {
                        wa::Message::text(format!("synthetic history message {n}"))
                    };
                    history_entry(
                        &id,
                        Entry {
                            id: format!("HIST{n:08}"),
                            message,
                            timestamp: BASE_TIME - 1_000_000 + n as i64,
                            sender,
                            from_me,
                        },
                    )
                })
                .collect();
            if chat < HISTORY_GROUPS {
                push_names.extend(
                    senders
                        .iter()
                        .map(|sender| (sender.clone(), format!("Pushed {sender}"))),
                );
            }
            parse_conversation(wa::Conversation {
                id,
                name: Some(format!("Synthetic history chat {chat}")),
                conversation_timestamp: Some(BASE_TIME as u64),
                messages,
                ..Default::default()
            })
        })
        .collect();
    ParsedHistory {
        chats,
        push_names,
        lids: Vec::new(),
        stickers: Vec::new(),
    }
}

/// The largest group's archived messages as the phone replays them in a
/// later sync: same ids, senders, timestamps and raw protobufs.
fn replayed_group() -> ParsedHistory {
    let id = chat_id(0);
    let senders = members(0);
    let messages = (0..LARGE_GROUP_MESSAGES)
        .map(|n| {
            let from_me = n.is_multiple_of(4);
            history_entry(
                &id,
                Entry {
                    id: format!("SYNTH{n:08}"),
                    message: photo(n),
                    timestamp: BASE_TIME + n as i64,
                    sender: (!from_me).then(|| senders[n % senders.len()].as_str()),
                    from_me,
                },
            )
        })
        .collect();
    ParsedHistory {
        chats: vec![parse_conversation(wa::Conversation {
            id,
            conversation_timestamp: Some(BASE_TIME as u64),
            messages,
            ..Default::default()
        })],
        push_names: Vec::new(),
        lids: Vec::new(),
        stickers: Vec::new(),
    }
}

/// Resets the peak resident set size, where the kernel allows it.
fn reset_peak_memory() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

fn report_peak_memory(after: &str) {
    let peak = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("VmHWM:")
                    .map(|value| value.trim().to_owned())
            })
        });
    match peak {
        Some(peak) => eprintln!("peak RSS during {after}: {peak}"),
        None => eprintln!("peak RSS during {after}: unavailable"),
    }
}

fn timed<T>(label: &str, run: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = run();
    let elapsed = started.elapsed();
    eprintln!("{label}: {elapsed:.1?}");
    (value, elapsed)
}

#[test]
#[ignore = "benchmark"]
fn synthetic_archive_timings() {
    let (mut fixture, _) = timed(
        "generate 70k messages in 1000 chats (encrypted, not a measurement)",
        fixture,
    );
    fixture.events.try_iter().for_each(drop);

    let (parsed, _) = timed("build a 20k message history chunk", history);
    reset_peak_memory();
    let (filed, _) = timed("history import of 20k messages", || {
        fixture.worker.apply_history(parsed, true)
    });
    report_peak_memory("the history import");
    let events = fixture.events.try_iter().collect::<Vec<_>>();
    let contacts = events
        .iter()
        .filter(|event| matches!(event, Event::Contacts(_)))
        .count();
    eprintln!(
        "history import filed {} messages and emitted {} events, {contacts} of them Contacts",
        filed.iter().map(|(_, count, _)| count).sum::<usize>(),
        events.len(),
    );

    let replayed = timed("replay of the same 20k message chunk", || {
        fixture.worker.apply_history(history(), true)
    })
    .0;
    assert_eq!(
        replayed.iter().map(|(_, count, _)| count).sum::<usize>(),
        HISTORY_MESSAGES
    );
    timed(
        "replay of 5000 archived messages of the largest group",
        || fixture.worker.apply_history(replayed_group(), true),
    );
    fixture.events.try_iter().for_each(drop);

    fixture.worker.archive.set_meta("derived", "5").unwrap();
    let rows = fixture.worker.archive.rows_with_raw().unwrap().len();
    reset_peak_memory();
    timed(&format!("re-derive {rows} archived rows"), || {
        fixture.worker.backfill()
    });
    report_peak_memory("the re-derive pass");
    assert_eq!(
        fixture.worker.archive.meta("derived").unwrap().as_deref(),
        Some("6")
    );
    fixture.events.try_iter().for_each(drop);

    timed("chat list query (Archive::chats)", || {
        fixture.worker.archive.chats().unwrap().len()
    });
    timed("emit_chats", || fixture.worker.emit_chats());
    let chats = fixture
        .events
        .try_iter()
        .find_map(|event| match event {
            Event::Chats(chats) => Some(chats.len()),
            _ => None,
        })
        .expect("chat list");
    assert_eq!(chats, CHATS + HISTORY_CHATS);

    for (label, chat) in [
        (
            "open the largest group (LoadChat)",
            fixture.largest_group.clone(),
        ),
        ("open a direct chat (LoadChat)", fixture.direct.clone()),
    ] {
        timed(label, || fixture.worker.load_chat(chat.clone(), None));
        let page = fixture
            .events
            .try_iter()
            .find_map(|event| match event {
                Event::Messages { messages, .. } => Some(messages.len()),
                _ => None,
            })
            .expect("a page");
        assert!(page > 0);
    }
}
