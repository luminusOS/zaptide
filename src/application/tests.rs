use super::*;

#[test]
fn avatar_texture_cache_tracks_shared_path_ownership() {
    let mut cache = std::collections::HashMap::from([
        (std::path::PathBuf::from("/synthetic/avatar-a.png"), 1),
        (std::path::PathBuf::from("/synthetic/avatar-b.png"), 2),
    ]);
    let mut references = std::collections::HashMap::from([
        (std::path::PathBuf::from("/synthetic/avatar-a.png"), 2),
        (std::path::PathBuf::from("/synthetic/avatar-b.png"), 1),
    ]);

    update_avatar_texture_references(
        &mut cache,
        &mut references,
        Some(std::path::PathBuf::from("/synthetic/avatar-a.png")),
        Some(std::path::PathBuf::from("/synthetic/avatar-b.png")),
    );

    assert_eq!(
        references.get(std::path::Path::new("/synthetic/avatar-a.png")),
        Some(&1)
    );
    assert_eq!(
        references.get(std::path::Path::new("/synthetic/avatar-b.png")),
        Some(&2)
    );
    update_avatar_texture_references(
        &mut cache,
        &mut references,
        Some(std::path::PathBuf::from("/synthetic/avatar-b.png")),
        Some(std::path::PathBuf::from("/synthetic/avatar-b.png")),
    );
    assert_eq!(
        references.get(std::path::Path::new("/synthetic/avatar-b.png")),
        Some(&2)
    );
    update_avatar_texture_references(
        &mut cache,
        &mut references,
        Some(std::path::PathBuf::from("/synthetic/avatar-a.png")),
        None,
    );
    assert!(!references.contains_key(std::path::Path::new("/synthetic/avatar-a.png")));
    assert!(!cache.contains_key(std::path::Path::new("/synthetic/avatar-a.png")));
}

#[test]
fn an_unreferenced_avatar_path_cannot_repopulate_the_cache() {
    assert!(
        super::dialogs::cached_texture(std::path::Path::new("/synthetic/stale-avatar.png"))
            .is_none()
    );
    assert!(!AVATAR_TEXTURES.with_borrow(|cache| {
        cache.contains_key(std::path::Path::new("/synthetic/stale-avatar.png"))
    }));
}

#[test]
fn avatar_cache_references_can_restart_after_logout_clear() {
    let path = std::path::PathBuf::from("/synthetic/relinked-avatar.png");
    clear_avatar_textures();
    assert!(!avatar_texture_is_referenced(&path));

    update_avatar_texture_reference(None, Some(path.clone()));

    assert!(avatar_texture_is_referenced(&path));
    clear_avatar_textures();
    assert!(!avatar_texture_is_referenced(&path));
}

#[test]
fn composer_sync_only_changes_when_draft_empty_state_changes() {
    assert!(!composer_draft_state_changed(false, false));
    assert!(!composer_draft_state_changed(true, true));
    assert!(composer_draft_state_changed(true, false));
    assert!(composer_draft_state_changed(false, true));
}

#[test]
fn attachment_summary_names_staged_files() {
    let names = ["a.jpg".to_owned(), "b.pdf".to_owned()];
    assert_eq!(attachment_summary(&names[..1], 1), "a.jpg");
    assert_eq!(attachment_summary(&names, 2), "a.jpg and 1 more");
    // A clipboard image counts but has no file name.
    assert_eq!(attachment_summary(&names[..1], 2), "a.jpg and 1 more");
    assert_eq!(attachment_summary(&[], 1), "1 attachment ready");
}

fn grouping_message(
    id: &str,
    sender: &str,
    from_me: bool,
    timestamp: i64,
) -> crate::model::Message {
    crate::model::Message {
        id: id.into(),
        chat: "chat".into(),
        sender: sender.into(),
        sender_name: Some(sender.into()),
        from_me,
        timestamp,
        content: crate::model::Content::text(id),
        status: crate::model::Delivery::None,
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
fn new_chat_lists_only_saved_contacts() {
    let contact = |id: &str, name: Option<&str>| crate::model::Contact {
        id: id.into(),
        full_name: name.map(Into::into),
        push_name: Some("Push".into()),
    };
    let contacts = [
        contact("5511999990001@s.whatsapp.net", None),
        contact("5511999990004@s.whatsapp.net", Some(" ")),
        contact("5511999990002@s.whatsapp.net", Some("bruna")),
        contact("5511999990003@s.whatsapp.net", Some("Ana")),
        contact("123456@lid", Some("Hidden")),
        contact("120363@g.us", Some("Group")),
    ]
    .into_iter()
    .map(|contact| (contact.id.clone(), contact))
    .collect();
    let names: Vec<_> = new_chat_contacts(&contacts)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    assert_eq!(names, ["Ana", "bruna"]);
}

#[test]
fn chat_list_replaces_only_the_changed_span() {
    let span = |old: &[i32], new: &[i32]| {
        super::changed_span(old.len(), new, |position, row| old[position] == *row)
    };
    // A chat moving to the top rewrites only the rows above its old place.
    assert_eq!(span(&[1, 2, 3, 4, 5], &[4, 1, 2, 3, 5]), (0, 4, 4));
    assert_eq!(span(&[1, 2, 3], &[1, 2, 3]), (3, 0, 0));
    assert_eq!(span(&[], &[1, 2]), (0, 0, 2));
    assert_eq!(span(&[1, 2], &[]), (0, 2, 0));
    assert_eq!(span(&[1, 2, 3], &[1, 9, 3]), (1, 1, 1));
    // Repeated rows must not let prefix and suffix overlap.
    assert_eq!(span(&[1, 1], &[1, 1, 1]), (2, 0, 1));
    assert_eq!(span(&[1, 1, 1], &[1]), (1, 2, 0));
}

#[test]
fn consecutive_messages_group_sender_and_timestamp_metadata() {
    let messages = vec![
        grouping_message("a1", "Alice", false, 1_700_000_000),
        grouping_message("a2", "Alice", false, 1_700_000_120),
        grouping_message("b1", "Bob", false, 1_700_000_180),
        grouping_message("a3", "Alice", false, 1_700_000_500),
    ];

    assert_eq!(
        message_group_boundaries(&messages),
        vec![(true, false), (false, true), (true, true), (true, true)]
    );
}

#[test]
fn a_wall_clock_jump_beyond_the_monotonic_one_is_a_suspend() {
    use std::time::Duration;
    let secs = Duration::from_secs;
    assert!(!slept_through(secs(5), secs(5)));
    // A late timer moves both clocks together.
    assert!(!slept_through(secs(40), secs(40)));
    assert!(slept_through(secs(3600), secs(5)));
    assert!(!slept_through(secs(5), secs(40)));
}

#[test]
fn photos_sent_together_form_one_album() {
    use crate::model::{Content, Media, MediaState};
    let photo = |id: &str, from_me: bool, timestamp: i64, caption: Option<&str>| {
        let mut message = grouping_message(id, "Alice", from_me, timestamp);
        message.content = Content::Image {
            media: Media {
                mime: "image/jpeg".into(),
                size: 1,
                width: None,
                height: None,
                path: None,
                state: MediaState::Idle,
            },
            caption: caption.map(Into::into),
        };
        message
    };
    let messages = vec![
        photo("a", true, 100, Some("trip")),
        photo("b", true, 101, None),
        photo("c", true, 102, None),
        // A caption starts a new run; a text or another sender ends one.
        photo("d", true, 103, Some("again")),
        photo("e", true, 104, None),
        grouping_message("t", "Alice", true, 105),
        photo("f", true, 106, None),
        photo("g", false, 107, None),
        // Too long after the previous photo.
        photo("h", false, 1_000, None),
    ];
    let mut separated = vec![false; messages.len()];
    assert_eq!(
        album_roles(&messages, &separated),
        [
            AlbumRole::Leader(3),
            AlbumRole::Follower,
            AlbumRole::Follower,
            AlbumRole::Leader(2),
            AlbumRole::Follower,
            AlbumRole::Single,
            AlbumRole::Single,
            AlbumRole::Single,
            AlbumRole::Single,
        ]
    );
    // A divider above a photo ends the album there.
    separated[2] = true;
    assert_eq!(
        &album_roles(&messages, &separated)[..3],
        [AlbumRole::Leader(2), AlbumRole::Follower, AlbumRole::Single]
    );
}

#[test]
fn forward_chat_picker_filters_locked_and_broadcast_destinations() {
    let mut chat =
        crate::model::Chat::new("15551234567@s.whatsapp.net".into(), "Ada Lovelace".into());
    assert!(forwardable_chat(&chat));
    assert!(dialogs::forward_search_key(&chat).contains("ada lovelace"));
    assert!(dialogs::forward_search_key(&chat).contains("15551234567"));

    let same_name =
        crate::model::Chat::new("15557654321@s.whatsapp.net".into(), "Ada Lovelace".into());
    assert_ne!(
        dialogs::forward_chat_detail(&chat),
        dialogs::forward_chat_detail(&same_name),
        "duplicate chat names remain distinguishable to assistive technology"
    );

    chat.locked = true;
    assert!(!forwardable_chat(&chat));
    chat.locked = false;
    chat.kind = crate::model::ChatKind::Broadcast;
    assert!(!forwardable_chat(&chat));
}

#[test]
fn only_own_text_messages_are_editable() {
    let mut message = crate::model::Message {
        id: "message".into(),
        chat: "chat".into(),
        sender: "me".into(),
        sender_name: None,
        from_me: true,
        timestamp: 0,
        content: crate::model::Content::text("hello"),
        status: Default::default(),
        delivered_at: None,
        read_at: None,
        quoted: None,
        reactions: Vec::new(),
        edited: false,
        mentions: Vec::new(),
        forwarded: false,
        thumbnail: None,
    };

    assert_eq!(editable_text(&message).as_deref(), Some("hello"));
    message.from_me = false;
    assert_eq!(editable_text(&message), None);
}

#[test]
fn transcript_keeps_full_text_message_body() {
    let message = crate::model::Message {
        id: "message".into(),
        chat: "chat".into(),
        sender: "sender".into(),
        sender_name: Some("Contact".into()),
        from_me: false,
        timestamp: 0,
        content: crate::model::Content::text("first line\nsecond line"),
        status: Default::default(),
        delivered_at: None,
        read_at: None,
        quoted: None,
        reactions: Vec::new(),
        edited: false,
        mentions: Vec::new(),
        forwarded: false,
        thumbnail: None,
    };

    assert_eq!(transcript_text(&message), "first line\nsecond line");
}

#[test]
fn copied_transcript_uses_the_name_shown_for_mentions() {
    let mut message = crate::archive::tests::message("group@g.us", "M", 10, false);
    message.content = crate::model::Content::text("hi @15581, not a@15581.com");
    message.mentions.push(crate::model::MentionRef {
        user: "15581".into(),
        id: "5511912345678@s.whatsapp.net".into(),
        name: None,
    });
    let contacts = std::collections::HashMap::from([(
        "5511912345678@s.whatsapp.net".into(),
        crate::model::Contact {
            id: "5511912345678@s.whatsapp.net".into(),
            full_name: Some("Ana".into()),
            push_name: None,
        },
    )]);
    assert_eq!(
        transcript_row(&message, &contacts).text,
        "hi @Ana, not a@15581.com"
    );
}

#[test]
fn edit_completion_matches_only_its_result() {
    let pending = ("chat".into(), "message".into(), "saved".into());

    assert!(is_edit_completion(Some(&pending), "chat", "message"));
    assert!(!is_edit_completion(Some(&pending), "chat", "other"));
    assert!(!is_edit_completion(Some(&pending), "other", "message"));
}

#[test]
fn attachment_caption_omits_whitespace_and_keeps_text() {
    assert_eq!(caption("  "), None);
    assert_eq!(caption("  caption  ").as_deref(), Some("caption"));
}

#[test]
fn native_preference_changes_round_trip_through_existing_settings_json() {
    let directory = tempfile::tempdir().expect("temporary settings directory");
    let path = directory.path().join("settings.json");
    let mut settings = crate::settings::Settings::default();
    crate::native_preferences::PreferenceChange::SetSendTyping(false)
        .apply(&mut settings)
        .expect("valid preference change");
    crate::native_preferences::PreferenceChange::SetNotificationPreviews(false)
        .apply(&mut settings)
        .expect("valid preference change");
    settings.save(&path).expect("persist preference");

    let loaded = crate::settings::Settings::load(&path);
    assert!(!loaded.send_typing);
    assert!(!loaded.notification_previews);
    assert!(crate::settings::Settings::default().notification_previews);
}

#[test]
fn auto_download_requires_unfetched_idle_media_within_size_limit() {
    let media = crate::model::Media {
        mime: "image/jpeg".into(),
        size: 64 * 1024 * 1024,
        width: None,
        height: None,
        path: None,
        state: crate::model::MediaState::Idle,
    };
    assert!(should_auto_download(Some(&media)));

    let too_large = crate::model::Media {
        size: 64 * 1024 * 1024 + 1,
        ..media.clone()
    };
    assert!(!should_auto_download(Some(&too_large)));

    let already_downloading = crate::model::Media {
        state: crate::model::MediaState::Downloading,
        ..media.clone()
    };
    assert!(!should_auto_download(Some(&already_downloading)));

    let downloaded = crate::model::Media {
        path: Some("cached.jpg".into()),
        ..media
    };
    assert!(!should_auto_download(Some(&downloaded)));
    assert!(!should_auto_download(None));
}

#[test]
fn enter_send_respects_setting_control_and_shift() {
    assert!(should_send_on_enter(true, false, false));
    assert!(should_send_on_enter(false, true, false));
    assert!(!should_send_on_enter(false, false, false));
    assert!(!should_send_on_enter(true, true, true));
}

#[test]
fn pairing_and_contact_phone_numbers_normalize_to_international_digits() {
    assert_eq!(
        normalized_phone("+1 (202) 555-0137").as_deref(),
        Some("12025550137")
    );
    assert_eq!(normalized_phone("123456").as_deref(), None);
    assert_eq!(normalized_phone("1234567890123456").as_deref(), None);
}

#[test]
fn notifications_skip_the_visible_muted_archived_and_locked_chats() {
    assert!(notification_should_show(
        true,
        true,
        Some("other"),
        "chat",
        None
    ));
    assert!(!notification_should_show(
        false,
        true,
        Some("other"),
        "chat",
        None
    ));
    assert!(!notification_should_show(
        true,
        true,
        Some("chat"),
        "chat",
        None
    ));
    assert!(notification_should_show(
        true,
        false,
        Some("chat"),
        "chat",
        None
    ));
    assert!(notification_should_show(true, true, None, "chat", None));
    let mut known = crate::model::Chat::new("chat".into(), "Chat".into());
    assert!(notification_should_show(
        true,
        true,
        None,
        "chat",
        Some(&known)
    ));
    known.muted_until = Some(crate::util::now() + 3600);
    assert!(!notification_should_show(
        true,
        true,
        None,
        "chat",
        Some(&known)
    ));
    known.muted_until = None;
    known.archived = true;
    assert!(!notification_should_show(
        true,
        true,
        None,
        "chat",
        Some(&known)
    ));
    known.archived = false;
    known.locked = true;
    assert!(!notification_should_show(
        true,
        true,
        None,
        "chat",
        Some(&known)
    ));
}

#[test]
fn attachment_send_waits_for_every_completion() {
    let failed_path = std::path::PathBuf::from("failed.png");
    let mut pending = PendingSend {
        chat: "chat".into(),
        text: String::new(),
        reply: None,
        attachments: vec!["sent.png".into(), failed_path.clone()],
        failed_attachments: Vec::new(),
        clipboard_image: false,
        remaining: 2,
        failed: false,
    };

    assert!(!pending.complete_attachment("sent.png".into(), true));
    assert!(!pending.failed);
    assert!(pending.complete_attachment(failed_path.clone(), false));
    assert!(pending.failed);
    assert_eq!(pending.failed_attachments, vec![failed_path]);
}

#[test]
fn backend_errors_are_reduced_to_safe_actionable_feedback() {
    assert_eq!(
        sanitized_error_feedback("connect failed for private jid 123"),
        "Connection failed. Check your network and try again."
    );
    assert_eq!(
        sanitized_error_feedback("unclassified private backend detail"),
        "Action failed. Check connection and try again."
    );
}

#[test]
fn linking_projection_shows_only_required_pairing_details() {
    let (_, pairing) = link_page(&LinkStatus::Unlinked {
        qr: Some("private-qr".into()),
        pair_code: Some("123-456".into()),
        pairing_phone: Some("15551234567".into()),
    });
    let (_, failure) = link_page(&LinkStatus::Failed("private protocol detail".into()));

    assert!(pairing.contains("Link with phone number instead"));
    assert!(!pairing.contains("15551234567"));
    assert!(!failure.contains("private protocol detail"));
}

#[test]
fn linking_qr_becomes_a_decodable_native_image() {
    let texture = qr_texture("synthetic-link-payload").expect("QR texture");
    assert!(texture.width() >= 200);
    assert_eq!(texture.width(), texture.height());
}

#[test]
fn conversation_rows_mark_day_changes_and_unread_boundary() {
    let timeline = [
        (1_700_000_000, false),
        (1_700_000_060, false),
        (1_700_086_400, false),
    ];
    let prefixes = conversation_prefixes(&timeline, 2);
    assert!(prefixes[0].contains("──"));
    assert!(prefixes[1].contains("Unread messages"));
    assert!(prefixes[2].contains("──"));
    assert!(!prefixes[0].contains("Unread messages"));
}

#[test]
fn own_messages_never_carry_the_unread_marker() {
    let unread = |prefixes: Vec<String>| {
        prefixes
            .iter()
            .position(|prefix| prefix.contains("Unread messages"))
    };
    // Incoming, then your replies: a stale count must not mark them.
    let replied = [(1, false), (2, true), (3, true), (4, true)];
    assert_eq!(unread(conversation_prefixes(&replied, 1)), None);
    // Only the incoming messages after your last reply are unread.
    let new = [(1, false), (2, true), (3, false), (4, false)];
    assert_eq!(unread(conversation_prefixes(&new, 5)), Some(2));
    assert_eq!(unread(conversation_prefixes(&new, 1)), Some(3));
}

#[test]
fn delivery_projection_distinguishes_each_outgoing_state() {
    assert_eq!(delivery_label(crate::model::Delivery::Pending), " · Queued");
    assert_eq!(delivery_label(crate::model::Delivery::Sent), " · Sent");
    assert_eq!(
        delivery_label(crate::model::Delivery::Delivered),
        " · Delivered"
    );
    assert_eq!(delivery_label(crate::model::Delivery::Read), " · Read");
    assert_eq!(delivery_label(crate::model::Delivery::Played), " · Played");
    assert_eq!(
        delivery_mark(crate::model::Delivery::Sent),
        ("✓", None, false)
    );
    assert!(!delivery_mark(crate::model::Delivery::Delivered).2);
    assert!(delivery_mark(crate::model::Delivery::Read).2);
    assert!(delivery_mark(crate::model::Delivery::Pending).1.is_some());
    assert!(delivery_mark(crate::model::Delivery::Failed).1.is_some());
    assert_eq!(
        delivery_mark(crate::model::Delivery::None),
        ("", None, false)
    );
    assert_eq!(delivery_label(crate::model::Delivery::Failed), " · Failed");
    assert_eq!(delivery_label(crate::model::Delivery::None), "");
}

#[test]
fn reaction_summary_aggregates_counts_and_marks_our_reaction() {
    let reactions = vec![
        crate::model::Reaction {
            sender: "a".into(),
            from_me: true,
            emoji: "👍".into(),
        },
        crate::model::Reaction {
            sender: "b".into(),
            from_me: false,
            emoji: "👍".into(),
        },
    ];

    assert_eq!(reaction_summary(&reactions), "\n👍 × 2 · You");
    let mut reactions = reactions;
    reactions.insert(
        0,
        crate::model::Reaction {
            sender: "c".into(),
            from_me: false,
            emoji: "😂".into(),
        },
    );
    reactions.push(crate::model::Reaction {
        sender: "d".into(),
        from_me: false,
        emoji: String::new(),
    });
    assert_eq!(
        reaction_counts(&reactions),
        [("😂".to_owned(), 1, false), ("👍".to_owned(), 2, true)]
    );
}

#[test]
fn clipboard_texture_channels_become_straight_alpha_rgba() {
    let mut pixels = [0, 0, 128, 128, 200, 20, 10, 0];
    convert_premultiplied_bgra_to_rgba(&mut pixels);

    assert_eq!(&pixels[..4], &[255, 0, 0, 128]);
    assert_eq!(&pixels[4..], &[0, 0, 0, 0]);
}

/// Appends a row to a real list view and samples its opacity frame by frame.
/// Run under a display: `xvfb-run -a cargo test arriving_row -- --ignored --nocapture`.
#[test]
#[ignore = "needs a display"]
fn arriving_row_fades_in_on_first_bind() {
    use relm4::typed_view::list::{RelmListItem, TypedListView};
    use std::time::{Duration, Instant};

    struct Probe(String);
    impl RelmListItem for Probe {
        type Root = gtk::Box;
        type Widgets = ();
        fn setup(_item: &gtk::ListItem) -> (gtk::Box, ()) {
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.set_height_request(40);
            (root, ())
        }
        fn bind(&mut self, _widgets: &mut (), root: &mut gtk::Box) {
            root.set_widget_name(&self.0);
            fade_in_if_arriving(root, &self.0);
        }
    }

    gtk::init().expect("display");
    adw::init().expect("libadwaita");
    // `REDUCED_MOTION=1` switches animations off and expects no fade.
    let reduced = std::env::var_os("REDUCED_MOTION").is_some_and(|value| !value.is_empty());
    if reduced && let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(false);
    }
    let context = gtk::glib::MainContext::default();
    let spin = |duration: Duration, mut each: Box<dyn FnMut()>| {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            context.iteration(false);
            each();
            std::thread::sleep(Duration::from_millis(2));
        }
    };

    let mut list = TypedListView::<Probe, gtk::NoSelection>::new();
    list.extend_from_iter((0..5).map(|i| Probe(format!("m{i}"))));
    let window = gtk::Window::builder()
        .default_width(300)
        .default_height(400)
        .child(&list.view)
        .build();
    window.present();
    spin(Duration::from_millis(400), Box::new(|| {}));

    mark_arriving("new".into());
    list.append(Probe("new".into()));
    let samples = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(u128, f64)>::new()));
    let start = Instant::now();
    let view = list.view.clone();
    let log = samples.clone();
    spin(
        Duration::from_millis(500),
        Box::new(move || {
            let mut child = view.first_child();
            while let Some(current) = child {
                if let Some(root) = current.first_child()
                    && root.widget_name() == "new"
                {
                    log.borrow_mut()
                        .push((start.elapsed().as_millis(), root.opacity()));
                }
                child = current.next_sibling();
            }
        }),
    );
    let samples = samples.borrow();
    let line = samples
        .iter()
        .step_by(8)
        .map(|(ms, opacity)| format!("{ms}ms:{opacity:.2}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("new row opacity: {line}");
    assert!(!samples.is_empty(), "new row was never bound");
    assert_eq!(
        samples.iter().any(|(_, opacity)| *opacity < 1.0),
        !reduced,
        "unexpected fade behaviour: {line}"
    );
    assert_eq!(samples.last().map(|(_, opacity)| *opacity), Some(1.0));
}

/// Synthetic timings for the hot paths behind a long conversation and a
/// large chat list. Run with `cargo test --release bench_synthetic -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark"]
fn bench_synthetic() {
    use std::time::Instant;
    let time = |label: &str, run: &mut dyn FnMut()| {
        let start = Instant::now();
        for _ in 0..20 {
            run();
        }
        println!("{label}: {:?} per run", start.elapsed() / 20);
    };

    let messages = (0..2000)
        .map(|i| {
            let from_me = i % 3 == 0;
            grouping_message(
                &format!("m{i}"),
                if from_me { "me" } else { "them" },
                from_me,
                1_700_000_000 + i * 90,
            )
        })
        .collect::<Vec<_>>();
    let contacts = std::collections::HashMap::new();
    let timeline = messages
        .iter()
        .map(|message| (message.timestamp, message.from_me))
        .collect::<Vec<_>>();
    time("2000 messages: prefixes", &mut || {
        std::hint::black_box(conversation_prefixes(&timeline, 5));
    });
    time("2000 messages: group boundaries", &mut || {
        std::hint::black_box(message_group_boundaries(&messages));
    });
    let separated = vec![false; messages.len()];
    time("2000 messages: album roles", &mut || {
        std::hint::black_box(album_roles(&messages, &separated));
    });
    time("2000 messages: clone snapshots", &mut || {
        std::hint::black_box(messages.clone());
    });
    time("2000 messages: transcript rows", &mut || {
        let rows = messages
            .iter()
            .map(|message| transcript_row(message, &contacts))
            .collect::<Vec<_>>();
        std::hint::black_box(rows);
    });

    let ids = messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
    let snapshots = messages
        .iter()
        .map(|m| (m.id.clone(), ()))
        .collect::<std::collections::HashMap<_, _>>();
    let page = (0..50).map(|i| format!("m{}", i * 40)).collect::<Vec<_>>();
    time("page dedupe, linear scan (old)", &mut || {
        let kept = page
            .iter()
            .filter(|id| !ids.iter().any(|known| known == *id))
            .count();
        std::hint::black_box(kept);
    });
    time("page dedupe, map lookup (new)", &mut || {
        let kept = page
            .iter()
            .filter(|id| !snapshots.contains_key(*id))
            .count();
        std::hint::black_box(kept);
    });

    let chats = (0..10_000)
        .map(|i| {
            let mut chat =
                crate::model::Chat::new(format!("c{i}@s.whatsapp.net"), format!("Chat {i}"));
            chat.last_activity = 1_700_000_000 - i;
            chat
        })
        .collect::<Vec<_>>();
    let mut projection = crate::native_chat_list::ChatListProjection::default();
    time("10000 chats: replace snapshot", &mut || {
        projection.replace_snapshot(chats.clone());
    });
    time("10000 chats: visible", &mut || {
        std::hint::black_box(projection.visible().count());
    });
    projection.set_query("chat 99");
    time("10000 chats: visible with search", &mut || {
        std::hint::black_box(projection.visible().count());
    });
}
