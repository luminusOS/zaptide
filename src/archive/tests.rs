use super::*;
use crate::model::{Contact, Content};

pub(crate) fn message(chat: &str, id: &str, timestamp: i64, from_me: bool) -> Message {
    Message {
        id: id.into(),
        chat: chat.into(),
        sender: if from_me { "me@s.whatsapp.net" } else { chat }.into(),
        sender_name: None,
        from_me,
        timestamp,
        content: Content::text(format!("message {id}")),
        status: if from_me {
            Delivery::Pending
        } else {
            Delivery::None
        },
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

#[test]
fn migrations_add_columns_to_an_older_archive() {
    let connection = Connection::open_in_memory().expect("opens");
    connection
            .execute_batch(
                "CREATE TABLE chats (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
                    last_activity INTEGER NOT NULL DEFAULT 0, unread INTEGER NOT NULL DEFAULT 0,
                    archived INTEGER NOT NULL DEFAULT 0, pinned INTEGER NOT NULL DEFAULT 0, muted_until INTEGER);
                 INSERT INTO chats (id, name, kind) VALUES ('1@s.whatsapp.net', 'A', 'direct');",
            )
            .expect("old schema");
    let archive = Archive::prepare(connection).expect("migrates");
    let chats = archive.chats().expect("chats");
    assert_eq!(chats.len(), 1);
    assert!(chats[0].participants.is_empty());
    assert!(!chats[0].read_only);
    assert!(!chats[0].locked, "the lock column migrates in unset");
    let mut with_thumbnail = message("1@s.whatsapp.net", "m1", 1, false);
    with_thumbnail.thumbnail = Some(vec![1, 2, 3]);
    archive
        .insert_message(&with_thumbnail, None)
        .expect("insert");
    assert_eq!(
        archive
            .message("1@s.whatsapp.net", "m1")
            .expect("read")
            .expect("exists")
            .thumbnail,
        Some(vec![1, 2, 3])
    );
    assert_eq!(
        archive
            .oldest("1@s.whatsapp.net")
            .expect("oldest")
            .map(|m| m.id),
        Some("m1".into())
    );
}

#[test]
fn ephemeral_setting_keeps_the_newest_timestamp() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "Ada").expect("chat");

    assert!(archive.set_ephemeral(chat, 604_800, 20).expect("setting"));
    assert!(!archive.set_ephemeral(chat, 86_400, 10).expect("stale"));

    assert_eq!(
        archive.ephemeral_expiration(chat).expect("expiration"),
        Some(604_800)
    );
}

#[test]
fn ephemeral_setting_preserves_explicitly_disabled_timer() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "Ada").expect("chat");

    archive.set_ephemeral(chat, 0, 20).expect("setting");

    assert_eq!(
        archive.ephemeral_expiration(chat).expect("expiration"),
        Some(0)
    );
}

#[test]
fn group_info_is_kept() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1-2@g.us";
    archive.ensure_chat(chat, "Group").expect("chat");
    archive
        .set_group_info(
            chat,
            Some("Rust Berlin"),
            &["a@s.whatsapp.net".into()],
            true,
        )
        .expect("info");
    let row = archive.chat(chat).expect("chat").expect("exists");
    assert_eq!(row.name, "Rust Berlin");
    assert_eq!(row.participants, vec!["a@s.whatsapp.net"]);
    assert!(row.read_only);
    archive
        .set_group_info(chat, None, &[], false)
        .expect("info");
    assert_eq!(
        archive.chat(chat).expect("chat").expect("exists").name,
        "Rust Berlin"
    );
}

#[test]
fn a_button_answer_survives_the_message_being_stored_again() {
    use crate::model::QuickReply;
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open_with_key(&root.path().join("fixture.db"), &[3; 32]).unwrap();
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "Fixture").unwrap();
    let buttons = |answered: Option<&str>| Content::Buttons {
        text: "Pick".into(),
        footer: None,
        buttons: vec![
            QuickReply {
                id: "a".into(),
                label: "Yes".into(),
            },
            QuickReply {
                id: "b".into(),
                label: "No".into(),
            },
        ],
        answered: answered.map(str::to_owned),
    };
    let mut row = message(chat, "M1", 10, false);
    row.content = buttons(None);
    archive.insert_message(&row, None).unwrap();
    archive
        .set_content(chat, "M1", &buttons(Some("b")), false)
        .unwrap();
    // History replay and re-derive decode the message with no answer.
    archive.insert_message(&row, None).unwrap();
    assert_eq!(
        archive.message(chat, "M1").unwrap().unwrap().content,
        buttons(Some("b"))
    );
    archive
        .set_derived(chat, "M1", &buttons(None), &[], None, false)
        .unwrap();
    assert_eq!(
        archive.message(chat, "M1").unwrap().unwrap().content,
        buttons(Some("b"))
    );
    // An answer to a button the sender removed is not carried over.
    let mut edited = buttons(None);
    if let Content::Buttons { buttons, .. } = &mut edited {
        buttons.retain(|button| button.id == "a");
    }
    archive
        .set_derived(chat, "M1", &edited, &[], None, false)
        .unwrap();
    assert_eq!(
        archive.message(chat, "M1").unwrap().unwrap().content,
        edited
    );
}

#[test]
fn server_echo_replaces_a_failed_local_answer_and_its_raw_body() {
    let archive = Archive::in_memory().unwrap();
    let chat = "fixture@s.whatsapp.net";
    archive.ensure_chat(chat, "Fixture").unwrap();
    let mut row = message(chat, "REPLY", 10, true);
    row.status = Delivery::Failed;
    archive.insert_message(&row, Some(b"local")).unwrap();
    row.status = Delivery::Sent;
    row.content = Content::text("Server reply");
    archive.insert_message(&row, Some(b"server")).unwrap();
    let stored = archive.message(chat, "REPLY").unwrap().unwrap();
    assert_eq!(stored.status, Delivery::Sent);
    assert_eq!(stored.content, Content::text("Server reply"));
    assert_eq!(
        archive.raw(chat, "REPLY").unwrap().as_deref(),
        Some(&b"server"[..])
    );
}

#[test]
fn a_list_answer_survives_the_message_being_stored_again() {
    use crate::model::{ListRow, ListSection};
    let root = tempfile::tempdir().unwrap();
    let archive = Archive::open_with_key(&root.path().join("fixture.db"), &[4; 32]).unwrap();
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "Fixture").unwrap();
    let list = |answered: Option<&str>| Content::List {
        title: "Menu".into(),
        description: None,
        button: "Open".into(),
        footer: None,
        sections: vec![ListSection {
            title: None,
            rows: vec![ListRow {
                id: "t".into(),
                title: "Tea".into(),
                description: None,
            }],
        }],
        answered: answered.map(str::to_owned),
    };
    let mut row = message(chat, "L1", 10, false);
    row.content = list(None);
    archive.insert_message(&row, None).unwrap();
    archive
        .set_content(chat, "L1", &list(Some("t")), false)
        .unwrap();
    archive.insert_message(&row, None).unwrap();
    assert_eq!(
        archive.message(chat, "L1").unwrap().unwrap().content,
        list(Some("t"))
    );
    // An answer naming a row that no longer exists is dropped.
    archive
        .set_content(chat, "L1", &list(Some("gone")), false)
        .unwrap();
    archive.insert_message(&row, None).unwrap();
    assert_eq!(
        archive.message(chat, "L1").unwrap().unwrap().content,
        list(None)
    );
}

#[test]
fn mute_and_pin_versions_survive_restart_and_ignore_older_updates() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.db");
    let key = [29; 32];
    let id = "1@s.whatsapp.net";
    {
        let archive = Archive::open_with_key(&path, &key).unwrap();
        archive.ensure_chat(id, "Fixture").unwrap();
        archive.set_muted_at(id, Some(0), 200).unwrap();
        archive.set_pinned_at(id, true, 200).unwrap();
    }
    let archive = Archive::open_with_key(&path, &key).unwrap();
    archive.set_muted_at(id, None, 100).unwrap();
    archive.set_pinned_at(id, false, 100).unwrap();
    archive
        .upsert_chat(&Chat::new(id.into(), "History name".into()))
        .unwrap();
    let chat = archive.chat(id).unwrap().unwrap();
    assert_eq!(chat.name, "History name");
    assert_eq!(chat.muted_until, Some(0));
    assert!(chat.pinned);
    assert_eq!(chat.pinned_at, 200);
    archive.set_muted_at(id, None, 300).unwrap();
    archive.set_pinned_at(id, false, 300).unwrap();
    let chat = archive.chat(id).unwrap().unwrap();
    assert_eq!(chat.muted_until, None);
    assert!(!chat.pinned);
    assert_eq!(chat.pinned_at, 0);
}

#[test]
fn lock_versions_survive_restart_and_ignore_older_updates() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.db");
    let key = [37; 32];
    let id = "491700000001@s.whatsapp.net";
    {
        let archive = Archive::open_with_key(&path, &key).unwrap();
        archive.ensure_chat(id, "Ada").expect("chat");
        archive.set_locked_at(id, true, 200).unwrap();
    }
    let archive = Archive::open_with_key(&path, &key).unwrap();
    // An older replayed patch must not undo the newer lock.
    archive.set_locked_at(id, false, 100).unwrap();
    assert!(archive.chat(id).unwrap().unwrap().locked);
    archive.set_locked_at(id, false, 300).unwrap();
    assert!(!archive.chat(id).unwrap().unwrap().locked);
    // Upserts from history metadata never touch the lock state.
    archive.set_locked(id, true).unwrap();
    archive
        .upsert_chat(&Chat::new(id.into(), "History name".into()))
        .unwrap();
    assert!(archive.chat(id).unwrap().unwrap().locked);
}

#[test]
fn privacy_id_mapping_preserves_history_locks_but_respects_versioned_unlocks() {
    for existing in [false, true] {
        let archive = Archive::in_memory().unwrap();
        let lid = "2@lid";
        let phone = "1@s.whatsapp.net";
        archive.ensure_chat(lid, "Fixture").unwrap();
        archive.set_locked_snapshot(lid, true).unwrap();
        if existing {
            archive.ensure_chat(phone, "Fixture").unwrap();
        }
        archive.put_lid("2", "1").unwrap();
        assert!(archive.chat(phone).unwrap().unwrap().locked);

        // Conflicting unversioned history cannot expose the mapped chat.
        archive.set_locked_snapshot(lid, false).unwrap();
        archive.set_pinned_at(lid, true, 100).unwrap();
        archive.put_lid("2", "1").unwrap();
        assert!(archive.chat(phone).unwrap().unwrap().locked);

        // An authenticated unlock takes precedence over stale history.
        archive.set_locked_at(phone, false, 200).unwrap();
        archive.set_locked_snapshot(lid, true).unwrap();
        archive.put_lid("2", "1").unwrap();
        assert!(!archive.chat(phone).unwrap().unwrap().locked);
    }
}

#[test]
fn chats_order_by_activity_and_carry_their_last_message() {
    let archive = Archive::in_memory().expect("opens");
    let a = "1@s.whatsapp.net";
    let b = "2@s.whatsapp.net";
    archive.ensure_chat(a, "A").expect("chat");
    archive.ensure_chat(b, "B").expect("chat");
    archive
        .insert_message(&message(a, "m1", 100, false), None)
        .expect("insert");
    archive
        .insert_message(&message(b, "m2", 200, true), None)
        .expect("insert");
    archive
        .insert_message(&message(a, "m3", 150, false), None)
        .expect("insert");
    let chats = archive.chats().expect("chats");
    assert_eq!(chats[0].id, b);
    assert_eq!(
        chats[0].last.as_ref().map(|last| last.summary.as_str()),
        Some("message m2")
    );
    assert_eq!(
        chats[0].last.as_ref().map(|last| last.status),
        Some(Delivery::Pending)
    );
    assert_eq!(chats[1].id, a);
    assert_eq!(chats[1].last_activity, 150);
    assert_eq!(
        chats[1].last.as_ref().map(|last| last.summary.as_str()),
        Some("message m3")
    );
}

#[test]
fn a_saved_name_keeps_the_push_name_beside_it() {
    let archive = Archive::in_memory().expect("opens");
    let id = "491700000001@s.whatsapp.net";
    archive
        .upsert_contact(&Contact {
            id: id.into(),
            full_name: None,
            push_name: Some("~slavic".into()),
        })
        .expect("stores");
    archive
        .upsert_contact(&Contact {
            id: id.into(),
            full_name: Some("Slavic".into()),
            push_name: None,
        })
        .expect("renames");
    let stored = archive.contact(id).expect("reads").expect("exists");
    assert_eq!(stored.full_name.as_deref(), Some("Slavic"));
    assert_eq!(stored.push_name.as_deref(), Some("~slavic"));
    assert!(
        archive
            .contact("nobody@s.whatsapp.net")
            .expect("reads")
            .is_none()
    );
}

#[test]
fn statuses_only_move_forward() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    archive
        .insert_message(&message(chat, "m1", 100, true), None)
        .expect("insert");
    assert!(
        archive
            .set_status(chat, "m1", Delivery::Read, 500)
            .expect("status")
    );
    assert!(
        !archive
            .set_status(chat, "m1", Delivery::Delivered, 600)
            .expect("status")
    );
    let stored = archive.message(chat, "m1").expect("read").expect("exists");
    assert_eq!(stored.status, Delivery::Read);
    assert_eq!(stored.read_at, Some(500));
    assert_eq!(stored.delivered_at, None);
    // History replay must not lower an existing Read state.
    archive
        .insert_message(&message(chat, "m1", 100, true), None)
        .expect("insert");
    assert_eq!(
        archive
            .message(chat, "m1")
            .expect("read")
            .expect("exists")
            .status,
        Delivery::Read
    );
    assert!(
        archive
            .set_status(chat, "m1", Delivery::Failed, 700)
            .expect("status")
    );
}

#[test]
fn a_read_receipt_covers_everything_before_it() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    for (id, timestamp) in [("m1", 100), ("m2", 200), ("m3", 300)] {
        archive
            .insert_message(&message(chat, id, timestamp, true), None)
            .expect("insert");
    }
    archive
        .insert_message(&message(chat, "theirs", 250, false), None)
        .expect("insert");
    let changed = archive
        .advance_statuses(chat, 200, Delivery::Read, 400)
        .expect("advance");
    assert_eq!(changed, vec!["m1", "m2"]);
    let messages = archive.messages(chat, None, 10).expect("messages");
    let statuses: Vec<Delivery> = messages.iter().map(|message| message.status).collect();
    assert_eq!(
        statuses,
        vec![
            Delivery::Read,
            Delivery::Read,
            Delivery::None,
            Delivery::Pending
        ]
    );
    assert_eq!(messages[0].read_at, Some(400));
}

#[test]
fn paging_walks_backwards_in_time() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    for index in 0..10 {
        archive
            .insert_message(
                &message(chat, &format!("m{index}"), 100 + index, false),
                None,
            )
            .expect("insert");
    }
    let newest = archive.messages(chat, None, 3).expect("messages");
    assert_eq!(
        newest.iter().map(|m| m.timestamp).collect::<Vec<_>>(),
        vec![107, 108, 109]
    );
    let older = archive
        .messages(chat, Some((107, "m7")), 3)
        .expect("messages");
    assert_eq!(
        older.iter().map(|m| m.timestamp).collect::<Vec<_>>(),
        vec![104, 105, 106]
    );
}

#[test]
fn message_reads_preserve_every_column() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    let mut expected = message(chat, "m1", 100, true);
    expected.sender_name = Some("Sender".into());
    expected.status = Delivery::Read;
    expected.delivered_at = Some(101);
    expected.read_at = Some(102);
    expected.quoted = Some(crate::model::Quoted {
        id: "original".into(),
        sender: chat.into(),
        sender_name: Some("Quoted sender".into()),
        summary: "Earlier message".into(),
        mentions: Vec::new(),
    });
    expected.reactions.push(crate::model::Reaction {
        sender: chat.into(),
        from_me: false,
        emoji: "👍".into(),
    });
    expected.edited = true;
    expected.mentions.push(crate::model::MentionRef {
        user: "@someone".into(),
        id: chat.into(),
        name: None,
    });
    expected.forwarded = true;
    expected.thumbnail = Some(vec![1, 2, 3]);
    archive.insert_message(&expected, None).expect("insert");

    assert_eq!(
        archive.message(chat, "m1").expect("read"),
        Some(expected.clone())
    );
    assert_eq!(
        archive.messages(chat, None, 10).expect("page"),
        vec![expected.clone()]
    );
    assert_eq!(
        archive
            .messages_range(chat, 100, (101, "later"), 10)
            .expect("range"),
        vec![expected]
    );
}

#[test]
fn paging_keeps_every_message_of_a_second() {
    // Cover messages sharing one timestamp across page boundaries.
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    archive
        .insert_message(&message(chat, "before", 99, false), None)
        .expect("insert");
    for index in 0..5 {
        archive
            .insert_message(&message(chat, &format!("a{index}"), 100, false), None)
            .expect("insert");
    }
    let first = archive.messages(chat, None, 3).expect("messages");
    assert_eq!(
        first.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["a2", "a3", "a4"]
    );
    let oldest = &first[0];
    let second = archive
        .messages(chat, Some((oldest.timestamp, &oldest.id)), 3)
        .expect("messages");
    assert_eq!(
        second.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["before", "a0", "a1"],
        "the rest of the second comes next, not the message before it alone"
    );
    let range = archive
        .messages_range(chat, 100, (100, "a2"), 10)
        .expect("range");
    assert_eq!(
        range.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["a0", "a1"]
    );
}

#[test]
fn ranges_and_deletion() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    for index in 0..6 {
        archive
            .insert_message(
                &message(chat, &format!("m{index}"), 100 + index, false),
                None,
            )
            .expect("insert");
    }
    let range = archive
        .messages_range(chat, 102, (105, "m5"), 10)
        .expect("range");
    assert_eq!(
        range.iter().map(|m| m.timestamp).collect::<Vec<_>>(),
        vec![102, 103, 104]
    );
    assert!(archive.delete_message(chat, "m3").expect("delete"));
    assert!(!archive.delete_message(chat, "m3").expect("delete"));
    assert!(archive.message(chat, "m3").expect("read").is_none());
}

#[test]
fn reactions_replace_per_sender() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    archive
        .insert_message(&message(chat, "m1", 100, false), None)
        .expect("insert");
    archive
        .set_reaction(chat, "m1", chat, false, "👍")
        .expect("react");
    let updated = archive
        .set_reaction(chat, "m1", chat, false, "❤️")
        .expect("react")
        .expect("exists");
    assert_eq!(updated.reactions.len(), 1);
    assert_eq!(updated.reactions[0].emoji, "❤️");
    let removed = archive
        .set_reaction(chat, "m1", chat, false, "")
        .expect("react")
        .expect("exists");
    assert!(removed.reactions.is_empty());
}

#[test]
fn a_history_replay_keeps_another_senders_custom_reaction() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    archive
        .insert_message(&message(chat, "m1", 100, false), None)
        .expect("insert");
    archive
        .set_reaction(chat, "m1", "2@s.whatsapp.net", false, "🏆")
        .expect("react");
    archive
        .insert_message(&message(chat, "m1", 100, false), None)
        .expect("replay");
    let stored = archive.message(chat, "m1").expect("read").expect("exists");
    assert_eq!(stored.reactions.len(), 1);
    assert_eq!(stored.reactions[0].emoji, "🏆");
    assert!(!stored.reactions[0].from_me);
}

#[test]
fn a_history_snapshot_replaces_the_reaction_list() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    archive
        .insert_message(&message(chat, "m1", 100, false), None)
        .expect("insert");
    archive
        .set_reaction(chat, "m1", "2@s.whatsapp.net", false, "👍")
        .expect("react");
    archive
        .set_reaction(chat, "m1", "3@s.whatsapp.net", false, "❤️")
        .expect("react");
    let mut replay = message(chat, "m1", 100, false);
    replay.reactions = vec![crate::model::Reaction {
        sender: "3@s.whatsapp.net".into(),
        from_me: false,
        emoji: "🎉".into(),
    }];
    archive.insert_message(&replay, None).expect("replay");
    let stored = archive.message(chat, "m1").expect("read").expect("exists");
    assert_eq!(stored.reactions.len(), 1);
    assert_eq!(stored.reactions[0].sender, "3@s.whatsapp.net");
    assert_eq!(stored.reactions[0].emoji, "🎉");
}

#[test]
fn unread_counts_and_incoming_ids_track_the_other_side() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    archive
        .insert_message(&message(chat, "m1", 100, false), None)
        .expect("insert");
    archive.bump_unread(chat).expect("bump");
    archive
        .insert_message(&message(chat, "mine", 150, true), None)
        .expect("insert");
    archive
        .insert_message(&message(chat, "m2", 200, false), None)
        .expect("insert");
    archive.bump_unread(chat).expect("bump");
    assert_eq!(archive.chat(chat).expect("chat").expect("exists").unread, 2);
    let ids: Vec<String> = archive
        .unread_incoming(chat, 2)
        .expect("ids")
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, vec!["m2", "m1"]);
    archive.mark_read(chat).expect("read");
    assert_eq!(archive.chat(chat).expect("chat").expect("exists").unread, 0);
}

#[test]
fn read_positions_and_pending_sync_survive_reopening_the_archive() {
    let dir = std::env::temp_dir().join(format!("zaptide-read-test-{}", std::process::id()));
    let path = dir.join("archive.db");
    let _ = std::fs::remove_dir_all(&dir);
    let chat = "1@s.whatsapp.net";
    {
        let archive = Archive::open_with_key(&path, &[7; 32]).unwrap();
        archive.ensure_chat(chat, "A").unwrap();
        archive
            .insert_message(&message(chat, "a", 100, false), None)
            .unwrap();
        archive.bump_unread(chat).unwrap();
        archive.mark_read(chat).unwrap();
        archive.queue_read_sync(chat).unwrap();
    }
    {
        let archive = Archive::open_with_key(&path, &[7; 32]).unwrap();
        assert_eq!(archive.read_through(chat).unwrap(), Some(100));
        assert_eq!(archive.pending_reads().unwrap(), vec![(chat.into(), 100)]);
        archive
            .insert_message(&message(chat, "b", 200, false), None)
            .unwrap();
        archive.bump_unread(chat).unwrap();
        archive.mark_read(chat).unwrap();
        archive.queue_read_sync(chat).unwrap();
        archive.finish_read_sync(chat, 100).unwrap();
        assert_eq!(
            archive.pending_reads().unwrap(),
            vec![(chat.into(), 200)],
            "an old completion must not lose the next read"
        );
        archive.finish_read_sync(chat, 200).unwrap();
        assert!(archive.pending_reads().unwrap().is_empty());
        archive.clear().unwrap();
        assert!(archive.read_through(chat).unwrap().is_none());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn media_paths_are_written_into_the_content() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    let mut picture = message(chat, "p1", 100, false);
    picture.content = Content::Image {
        caption: None,
        media: crate::model::Media {
            mime: "image/jpeg".into(),
            size: 10,
            width: None,
            height: None,
            path: None,
            state: Default::default(),
        },
    };
    archive.insert_message(&picture, None).expect("insert");
    let updated = archive
        .set_media_path(chat, "p1", Path::new("/tmp/p1.jpg"))
        .expect("set")
        .expect("exists");
    assert_eq!(
        updated.content.media().and_then(|media| media.path.clone()),
        Some(std::path::PathBuf::from("/tmp/p1.jpg"))
    );
    let reread = archive.message(chat, "p1").expect("read").expect("exists");
    assert_eq!(reread.content, updated.content);
}

#[test]
fn raw_bytes_survive_a_replay_without_them() {
    let archive = Archive::in_memory().expect("opens");
    let chat = "1@s.whatsapp.net";
    archive.ensure_chat(chat, "A").expect("chat");
    archive
        .insert_message(&message(chat, "m1", 100, false), Some(&[1, 2, 3]))
        .expect("insert");
    archive
        .insert_message(&message(chat, "m1", 100, false), None)
        .expect("insert");
    assert_eq!(archive.raw(chat, "m1").expect("raw"), Some(vec![1, 2, 3]));
}
