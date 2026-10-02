use super::*;

mod session;

#[test]
fn fallback_names_read_as_phones_or_ids() {
    assert_eq!(
        fallback_name("393331234567@s.whatsapp.net"),
        "+39 333 123 456 7"
    );
    assert_eq!(fallback_name("1-2@g.us"), "Group");
    assert_eq!(fallback_name("42@lid"), "42");
}

#[test]
fn media_paths_keep_document_names_and_map_mimes() {
    let dir = Path::new("/cache");
    assert_eq!(
        outbound::media_path(dir, "1@s.whatsapp.net", "ABC", "image/jpeg", None),
        PathBuf::from("/cache/1_s_whatsapp_net-ABC.jpg")
    );
    assert_eq!(
        outbound::media_path(
            dir,
            "1@s.whatsapp.net",
            "ABC",
            "application/pdf",
            Some("tax return.pdf")
        ),
        PathBuf::from("/cache/ABC-tax_return.pdf")
    );
    assert_eq!(extension_for("audio/ogg; codecs=opus", None), "ogg");
    assert_eq!(extension_for("application/x-unknown", None), "x-unknown");
}

#[test]
fn classification_reads_quick_reply_buttons_and_lists() {
    use whatsapp_rust::prelude::MessageField;
    let buttons = wa::Message {
        buttons_message: MessageField::some(wa::message::ButtonsMessage {
            content_text: Some("Pick one".into()),
            footer_text: Some("Footer".into()),
            buttons: vec![
                wa::message::buttons_message::Button {
                    button_id: Some("a".into()),
                    button_text: MessageField::some(
                        wa::message::buttons_message::button::ButtonText {
                            display_text: Some("Yes".into()),
                        },
                    ),
                    ..Default::default()
                },
                // No id or label: nothing to answer with, so it is dropped.
                wa::message::buttons_message::Button::default(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        classify(&buttons),
        Some(Content::Buttons {
            text: "Pick one".into(),
            footer: Some("Footer".into()),
            buttons: vec![crate::model::QuickReply {
                id: "a".into(),
                label: "Yes".into()
            }],
            answered: None,
        })
    );
    let list = wa::Message {
        list_message: MessageField::some(wa::message::ListMessage {
            title: Some("Menu".into()),
            button_text: Some("Open".into()),
            sections: vec![wa::message::list_message::Section {
                title: Some("Drinks".into()),
                rows: vec![wa::message::list_message::Row {
                    title: Some("Tea".into()),
                    row_id: Some("t".into()),
                    ..Default::default()
                }],
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let Some(Content::List {
        title,
        button,
        sections,
        ..
    }) = classify(&list)
    else {
        panic!("list expected");
    };
    assert_eq!((title.as_str(), button.as_str()), ("Menu", "Open"));
    assert_eq!(sections[0].rows[0].id, "t");
    // Native-flow buttons, product lists and empty lists are not answerable.
    let native = wa::Message {
        buttons_message: MessageField::some(wa::message::ButtonsMessage {
            buttons: vec![wa::message::buttons_message::Button {
                button_id: Some("x".into()),
                r#type: Some(wa::message::buttons_message::button::Type::NATIVE_FLOW),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(matches!(
        classify(&native),
        Some(Content::Unsupported { .. })
    ));
    let product = wa::Message {
        list_message: MessageField::some(wa::message::ListMessage {
            list_type: Some(wa::message::list_message::ListType::PRODUCT_LIST),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(matches!(
        classify(&product),
        Some(Content::Unsupported { .. })
    ));
}

#[test]
fn business_templates_show_their_text_and_web_buttons() {
    use wa::__buffa::oneof::hydrated_template_button::HydratedButton;
    let url_button = |label: &str, url: &str| wa::HydratedTemplateButton {
        hydrated_button: Some(HydratedButton::UrlButton(Box::new(
            wa::hydrated_template_button::HydratedURLButton {
                display_text: Some(label.into()),
                url: Some(url.into()),
                ..Default::default()
            },
        ))),
        ..Default::default()
    };
    let hydrated = wa::Message {
        template_message: MessageField::some(wa::message::TemplateMessage {
            hydrated_template: MessageField::some(
                wa::message::template_message::HydratedFourRowTemplate {
                    hydrated_content_text: Some("Your assembly is booked".into()),
                    hydrated_buttons: vec![
                        url_button("Assembly status", "https://example.com/status"),
                        url_button("Unsafe", "javascript:alert(1)"),
                    ],
                    ..Default::default()
                },
            ),
            ..Default::default()
        }),
        ..Default::default()
    };
    let Some(Content::Template { text, links, .. }) = classify(&hydrated) else {
        panic!("a hydrated template is shown");
    };
    assert_eq!(text, "Your assembly is booked");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].label, "Assembly status");
    assert_eq!(links[0].url, "https://example.com/status");

    use wa::message::interactive_message as flow;
    let native = wa::Message {
        interactive_message: MessageField::some(wa::message::InteractiveMessage {
            body: MessageField::some(flow::Body {
                text: Some("Track it".into()),
            }),
            interactive_message: Some(
                wa::__buffa::oneof::message::interactive_message::InteractiveMessage::NativeFlowMessage(
                    Box::new(flow::NativeFlowMessage {
                        buttons: vec![
                            flow::native_flow_message::NativeFlowButton {
                                name: Some("cta_url".into()),
                                button_params_json: Some(
                                    r#"{"display_text":"Open","url":"https://example.com/t"}"#
                                        .into(),
                                ),
                            },
                            flow::native_flow_message::NativeFlowButton {
                                name: Some("cta_copy".into()),
                                button_params_json: Some(r#"{"display_text":"Copy"}"#.into()),
                            },
                        ],
                        ..Default::default()
                    }),
                ),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };
    let Some(Content::Template { text, links, .. }) = classify(&native) else {
        panic!("a native-flow message is shown");
    };
    assert_eq!((text.as_str(), links.len()), ("Track it", 1));
    assert_eq!(links[0].url, "https://example.com/t");
}

#[test]
fn button_answers_carry_the_id_label_and_quote() {
    use wa::__buffa::oneof::message::buttons_response_message::Response;
    let context = wa::ContextInfo {
        stanza_id: Some("ORIGINAL".into()),
        ..Default::default()
    };
    let message = buttons_response("a", "Yes", Some(context));
    let response = message.buttons_response_message.as_option().expect("set");
    assert_eq!(response.selected_button_id.as_deref(), Some("a"));
    assert_eq!(
        response
            .context_info
            .as_option()
            .and_then(|context| context.stanza_id.as_deref()),
        Some("ORIGINAL")
    );
    assert!(matches!(
        &response.response,
        Some(Response::SelectedDisplayText(text)) if text == "Yes"
    ));
    assert!(
        !buttons_response("a", "Yes", None)
            .buttons_response_message
            .as_option()
            .expect("set")
            .context_info
            .is_set()
    );
}

#[test]
fn received_interactive_answers_keep_their_text_and_quote() {
    let context = wa::ContextInfo {
        stanza_id: Some("ORIGINAL".into()),
        ..Default::default()
    };
    for message in [
        buttons_response("a", "Yes", Some(context.clone())),
        list_response("t", "Tea", Some("hot".into()), Some(context.clone())),
    ] {
        assert!(matches!(classify(&message), Some(Content::Text { .. })));
        assert_eq!(
            context_of(&message).and_then(|context| context.stanza_id.as_deref()),
            Some("ORIGINAL")
        );
    }
    assert_eq!(
        classify(&buttons_response("a", "Yes", None)),
        Some(Content::text("Yes"))
    );
    assert_eq!(
        classify(&list_response("t", "Tea", None, None)),
        Some(Content::text("Tea"))
    );
}

#[test]
fn reply_uses_full_sender_label_even_when_display_is_clipped() {
    use whatsapp_rust::prelude::MessageField;
    let full = "A".repeat(protocol::MAX_INTERACTIVE_CHARS + 20);
    let raw = wa::Message {
        buttons_message: MessageField::some(wa::message::ButtonsMessage {
            buttons: vec![wa::message::buttons_message::Button {
                button_id: Some("a".into()),
                button_text: MessageField::some(wa::message::buttons_message::button::ButtonText {
                    display_text: Some(full.clone()),
                }),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let Some(Content::Buttons { buttons, .. }) = classify(&raw) else {
        panic!("buttons expected")
    };
    assert_ne!(buttons[0].label, full);
    assert_eq!(full_choice(&raw, "a"), Some((full, None)));
}

#[test]
fn repeated_button_and_row_ids_keep_only_the_first() {
    use whatsapp_rust::prelude::MessageField;
    let row = |title: &str| wa::message::list_message::Row {
        title: Some(title.into()),
        row_id: Some("same".into()),
        ..Default::default()
    };
    let list = wa::Message {
        list_message: MessageField::some(wa::message::ListMessage {
            sections: vec![
                wa::message::list_message::Section {
                    rows: vec![row("First")],
                    ..Default::default()
                },
                wa::message::list_message::Section {
                    rows: vec![row("Second")],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        ..Default::default()
    };
    let Some(Content::List { sections, .. }) = classify(&list) else {
        panic!("list expected");
    };
    // The second section had only the repeated id, so it is gone.
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].rows[0].title, "First");
    let button = |label: &str| wa::message::buttons_message::Button {
        button_id: Some("same".into()),
        button_text: MessageField::some(wa::message::buttons_message::button::ButtonText {
            display_text: Some(label.into()),
        }),
        ..Default::default()
    };
    let buttons = wa::Message {
        buttons_message: MessageField::some(wa::message::ButtonsMessage {
            buttons: vec![button("One"), button("Two")],
            ..Default::default()
        }),
        ..Default::default()
    };
    let Some(Content::Buttons { buttons, .. }) = classify(&buttons) else {
        panic!("buttons expected");
    };
    assert_eq!(buttons.len(), 1);
    assert_eq!(buttons[0].label, "One");
}

#[test]
fn list_answers_carry_the_row_id_title_and_quote() {
    let context = wa::ContextInfo {
        stanza_id: Some("ORIGINAL".into()),
        ..Default::default()
    };
    let message = list_response("t", "Tea", Some("hot".into()), Some(context));
    let response = message.list_response_message.as_option().expect("set");
    assert_eq!(response.title.as_deref(), Some("Tea"));
    assert_eq!(response.description.as_deref(), Some("hot"));
    assert_eq!(
        response
            .single_select_reply
            .as_option()
            .and_then(|reply| reply.selected_row_id.as_deref()),
        Some("t")
    );
    assert_eq!(
        response
            .context_info
            .as_option()
            .and_then(|context| context.stanza_id.as_deref()),
        Some("ORIGINAL")
    );
}

#[test]
fn interactive_messages_are_capped() {
    use whatsapp_rust::prelude::MessageField;
    let button = |index: usize| wa::message::buttons_message::Button {
        button_id: Some(format!("b{index}")),
        button_text: MessageField::some(wa::message::buttons_message::button::ButtonText {
            display_text: Some("é".repeat(1_500)),
        }),
        ..Default::default()
    };
    let buttons = wa::Message {
        buttons_message: MessageField::some(wa::message::ButtonsMessage {
            buttons: (0..15).map(button).collect(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let Some(Content::Buttons { buttons, .. }) = classify(&buttons) else {
        panic!("buttons expected");
    };
    assert_eq!(buttons.len(), protocol::MAX_INTERACTIVE_BUTTONS);
    assert_eq!(
        buttons[0].label.chars().count(),
        protocol::MAX_INTERACTIVE_CHARS + 1
    );
    assert!(buttons[0].label.ends_with('…'));
    let row = |index: usize| wa::message::list_message::Row {
        title: Some(format!("r{index}")),
        row_id: Some(format!("id{index}")),
        ..Default::default()
    };
    let section = |start: usize| wa::message::list_message::Section {
        rows: (start..start + 80).map(row).collect(),
        ..Default::default()
    };
    let list = wa::Message {
        list_message: MessageField::some(wa::message::ListMessage {
            sections: vec![section(0), section(80)],
            ..Default::default()
        }),
        ..Default::default()
    };
    let Some(Content::List { sections, .. }) = classify(&list) else {
        panic!("list expected");
    };
    let rows: usize = sections.iter().map(|section| section.rows.len()).sum();
    assert_eq!(rows, protocol::MAX_INTERACTIVE_ITEMS);
}

#[test]
fn classification_covers_text_and_media() {
    let text = wa::Message::text("hello");
    assert_eq!(classify(&text), Some(Content::text("hello")));
    let image = wa::Message {
        image_message: whatsapp_rust::prelude::MessageField::some(wa::message::ImageMessage {
            caption: Some("look".into()),
            mimetype: Some("image/jpeg".into()),
            file_length: Some(10),
            width: Some(4),
            height: Some(3),
            jpeg_thumbnail: Some(vec![0xff, 0xd8]),
            ..Default::default()
        }),
        ..Default::default()
    };
    match classify(&image) {
        Some(Content::Image { caption, media }) => {
            assert_eq!(caption.as_deref(), Some("look"));
            assert_eq!(media.mime, "image/jpeg");
            assert_eq!((media.width, media.height), (Some(4), Some(3)));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(thumbnail_of(&image), Some(vec![0xff, 0xd8]));
    let place = wa::Message {
        location_message: whatsapp_rust::prelude::MessageField::some(
            wa::message::LocationMessage {
                degrees_latitude: Some(-23.5),
                degrees_longitude: Some(-46.6),
                jpeg_thumbnail: Some(vec![0xff, 0xd8, 1]),
                ..Default::default()
            },
        ),
        ..Default::default()
    };
    assert!(matches!(
        classify(&place),
        Some(Content::Location { latitude, .. }) if latitude == -23.5
    ));
    assert_eq!(thumbnail_of(&place), Some(vec![0xff, 0xd8, 1]));
    assert_eq!(classify(&wa::Message::default()), None);
}

#[test]
fn unsafe_preview_metadata_cannot_launch_a_desktop_handler() {
    let message = wa::Message {
        extended_text_message: MessageField::some(wa::message::ExtendedTextMessage {
            text: Some("Read this".into()),
            matched_text: Some("file:///fixture.exe".into()),
            title: Some("An ordinary title".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(matches!(
        classify(&message),
        Some(Content::Text { preview: None, .. })
    ));
}

#[tokio::test]
async fn cancelling_phone_pairing_returns_to_the_qr_code() {
    let (mut worker, _events, _, _) = worker();
    worker.qr = Some("qr".into());
    worker.pairing_phone = Some("15551234567".into());
    worker.pair_code = Some("ABCD-EFGH".into());
    let request_id = worker.pair_request_id;
    worker.handle_command(Command::CancelPhonePairing).await;
    assert_eq!(
        worker.unlinked(),
        LinkStatus::Unlinked {
            qr: Some("qr".into()),
            pair_code: None,
            pairing_phone: None,
        }
    );
    // The abandoned request's answer no longer shows a code.
    worker
        .handle_command(Command::PairCode {
            request_id,
            result: Ok("LATE-CODE".into()),
        })
        .await;
    assert_eq!(worker.pair_code, None);
}

#[tokio::test]
async fn a_failed_sticker_fetch_is_not_retried_in_the_same_session() {
    let (mut worker, _events, _, _) = worker();
    worker.sticker_fetches.insert("expired".into());
    worker
        .handle_command(Command::StickerFetched {
            hash: "expired".into(),
            session_generation: worker.session_generation,
            result: Err("gone".into()),
        })
        .await;
    assert!(worker.sticker_fetches.contains("expired"));
}

#[tokio::test]
async fn newsletter_sends_are_rejected_before_reaching_the_client() {
    let (mut worker, events, _, _) = worker();
    worker
        .handle_command(Command::SendText {
            chat: "fixture@newsletter".into(),
            text: "Fixture".into(),
            quoting: None,
            mentions: Vec::new(),
        })
        .await;
    let emitted: Vec<_> = events.try_iter().collect();
    assert_eq!(
            emitted
                .iter()
                .filter(|event| matches!(event, Event::Sent { chat, success: false } if chat == "fixture@newsletter"))
                .count(),
            1
        );
    assert_eq!(
        emitted
            .iter()
            .filter(|event| matches!(event, Event::Error(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn button_answer_marks_only_on_success_and_removes_failed_reply() {
    use crate::model::QuickReply;
    const PEER: &str = "fixture@s.whatsapp.net";
    let (mut worker, events, _, _) = worker();
    worker.archive.ensure_chat(PEER, "Fixture").unwrap();
    let buttons = |answered: Option<&str>| Content::Buttons {
        text: "Pick".into(),
        footer: None,
        buttons: vec![QuickReply {
            id: "a".into(),
            label: "Yes".into(),
        }],
        answered: answered.map(str::to_owned),
    };
    let mut original = crate::archive::tests::message(PEER, "BUTTONS", 10, false);
    original.content = buttons(None);
    worker.archive.insert_message(&original, None).unwrap();
    let mut answer = crate::archive::tests::message(PEER, "ANSWER", 11, true);
    answer.content = Content::text("Yes");
    answer.quoted = Some(Quoted {
        id: "BUTTONS".into(),
        sender: PEER.into(),
        sender_name: None,
        summary: "Pick".into(),
        mentions: Vec::new(),
    });
    let response = buttons_response("a", "Yes", None);
    worker
        .archive
        .insert_message(&answer, Some(&response.encode_to_vec()))
        .unwrap();

    let sent = |id: &str, error: Option<&str>| Command::Sent {
        chat: PEER.into(),
        id: id.into(),
        session_generation: 0,
        error: error.map(str::to_owned),
    };
    // Success keeps the answer recorded.
    worker
        .answer_sends
        .insert("ANSWER".into(), (PEER.into(), "BUTTONS".into()));
    worker.handle_command(sent("ANSWER", None)).await;
    assert!(worker.answer_sends.is_empty());
    assert_eq!(
        worker
            .archive
            .message(PEER, "BUTTONS")
            .unwrap()
            .unwrap()
            .content,
        buttons(Some("a"))
    );
    // A new attempt fails: remove only the failed answer, retaining the
    // previously acknowledged selection on the original message.
    worker
        .archive
        .set_content(PEER, "BUTTONS", &buttons(None), false)
        .unwrap();
    let mut failed = answer.clone();
    failed.id = "ANSWER2".into();
    worker
        .archive
        .insert_message(&failed, Some(&response.encode_to_vec()))
        .unwrap();
    worker
        .answer_sends
        .insert("ANSWER2".into(), (PEER.into(), "BUTTONS".into()));
    worker
        .handle_command(sent("ANSWER2", Some("offline")))
        .await;
    assert_eq!(
        worker
            .archive
            .message(PEER, "BUTTONS")
            .unwrap()
            .unwrap()
            .content,
        buttons(None)
    );
    assert!(worker.archive.message(PEER, "ANSWER2").unwrap().is_none());
    // Neither result reaches the composer's pending-send bookkeeping.
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, Event::Sent { .. }))
    );
}

#[tokio::test]
async fn interrupted_answer_is_recovered_without_deleting_other_quoted_messages() {
    use crate::model::QuickReply;
    const PEER: &str = "fixture@s.whatsapp.net";
    let (mut worker, _, _, _) = worker();
    worker.archive.ensure_chat(PEER, "Fixture").unwrap();
    let mut parent = crate::archive::tests::message(PEER, "PARENT", 10, false);
    parent.content = Content::Buttons {
        text: "Pick".into(),
        footer: None,
        buttons: vec![QuickReply {
            id: "a".into(),
            label: "Yes".into(),
        }],
        answered: Some("a".into()),
    };
    worker.archive.insert_message(&parent, None).unwrap();
    let mut answer = crate::archive::tests::message(PEER, "REPLY", 11, true);
    answer.content = Content::text("Yes");
    answer.quoted = Some(Quoted {
        id: "PARENT".into(),
        sender: PEER.into(),
        sender_name: None,
        summary: "Pick".into(),
        mentions: vec![],
    });
    let response = buttons_response("a", "Yes", None);
    worker
        .archive
        .insert_message(&answer, Some(&response.encode_to_vec()))
        .unwrap();
    let mut ordinary = answer.clone();
    ordinary.id = "NORMAL".into();
    ordinary.content = Content::text("Yes");
    worker
        .archive
        .insert_message(
            &ordinary,
            Some(&outgoing_text("Yes".into(), None, &[]).encode_to_vec()),
        )
        .unwrap();
    worker.recover_interrupted_answers();
    assert_eq!(
        worker
            .archive
            .message(PEER, "PARENT")
            .unwrap()
            .unwrap()
            .content
            .answer(),
        None
    );
    assert_eq!(
        worker
            .archive
            .message(PEER, "REPLY")
            .unwrap()
            .unwrap()
            .status,
        Delivery::Failed
    );
    assert!(worker.archive.message(PEER, "NORMAL").unwrap().is_some());
    assert_eq!(
        worker
            .archive
            .message(PEER, "NORMAL")
            .unwrap()
            .unwrap()
            .status,
        Delivery::Pending
    );
    // A server echo of the same id is stronger evidence than the local
    // interrupted-send state and restores the selection.
    answer.status = Delivery::Sent;
    worker.store_message(answer, Some(response.encode_to_vec()), None);
    assert_eq!(
        worker
            .archive
            .message(PEER, "PARENT")
            .unwrap()
            .unwrap()
            .content
            .answer(),
        Some("a")
    );
    assert_eq!(
        worker
            .archive
            .message(PEER, "REPLY")
            .unwrap()
            .unwrap()
            .status,
        Delivery::Sent
    );
    worker
        .answer_sends
        .insert("REPLY".into(), (PEER.into(), "PARENT".into()));
    worker
        .handle_command(Command::Sent {
            chat: PEER.into(),
            id: "REPLY".into(),
            session_generation: worker.session_generation,
            error: Some("late error".into()),
        })
        .await;
    assert_eq!(
        worker
            .archive
            .message(PEER, "REPLY")
            .unwrap()
            .unwrap()
            .status,
        Delivery::Sent
    );
    let reopened = worker
        .archive
        .message(PEER, "PARENT")
        .unwrap()
        .unwrap()
        .content
        .with_answer(None)
        .unwrap();
    worker
        .archive
        .set_content(PEER, "PARENT", &reopened, false)
        .unwrap();
    worker.reconcile_confirmed_answers();
    assert_eq!(
        worker
            .archive
            .message(PEER, "PARENT")
            .unwrap()
            .unwrap()
            .content
            .answer(),
        Some("a")
    );
}

#[test]
fn confirmed_answer_is_reconciled_when_question_arrives_later() {
    use crate::model::QuickReply;
    const PEER: &str = "fixture@s.whatsapp.net";
    let (mut worker, _, _, _) = worker();
    let mut answer = crate::archive::tests::message(PEER, "REPLY", 11, true);
    answer.status = Delivery::Sent;
    answer.content = Content::text("Yes");
    answer.quoted = Some(Quoted {
        id: "PARENT".into(),
        sender: PEER.into(),
        sender_name: None,
        summary: "Pick".into(),
        mentions: vec![],
    });
    worker.store_message(
        answer,
        Some(buttons_response("a", "Yes", None).encode_to_vec()),
        None,
    );
    let mut parent = crate::archive::tests::message(PEER, "PARENT", 10, false);
    parent.content = Content::Buttons {
        text: "Pick".into(),
        footer: None,
        buttons: vec![QuickReply {
            id: "a".into(),
            label: "Yes".into(),
        }],
        answered: None,
    };
    worker.store_message(parent, None, None);
    assert_eq!(
        worker
            .archive
            .message(PEER, "PARENT")
            .unwrap()
            .unwrap()
            .content
            .answer(),
        Some("a")
    );
}

#[test]
fn version_three_archive_rederives_unsupported_buttons() {
    use whatsapp_rust::prelude::MessageField;
    const PEER: &str = "fixture@s.whatsapp.net";
    let (mut worker, _, _, _) = worker();
    worker.archive.ensure_chat(PEER, "Fixture").unwrap();
    let raw = wa::Message {
        buttons_message: MessageField::some(wa::message::ButtonsMessage {
            content_text: Some("Pick".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut row = crate::archive::tests::message(PEER, "OLD", 10, false);
    row.content = Content::Unsupported {
        what: "interactive message".into(),
    };
    worker
        .archive
        .insert_message(&row, Some(&raw.encode_to_vec()))
        .unwrap();
    worker.archive.set_meta("derived", "3").unwrap();
    worker.backfill();
    assert!(matches!(
        worker
            .archive
            .message(PEER, "OLD")
            .unwrap()
            .unwrap()
            .content,
        Content::Buttons { .. }
    ));
    assert_eq!(
        worker.archive.meta("derived").unwrap().as_deref(),
        Some("5")
    );
}

#[test]
fn backfill_does_not_advance_checkpoint_when_a_row_write_fails() {
    use whatsapp_rust::prelude::MessageField;
    const PEER: &str = "fixture@s.whatsapp.net";
    let (mut worker, _, _, _) = worker();
    worker.archive.ensure_chat(PEER, "Fixture").unwrap();
    let raw = wa::Message {
        buttons_message: MessageField::some(wa::message::ButtonsMessage {
            content_text: Some("Pick".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut row = crate::archive::tests::message(PEER, "OLD", 10, false);
    row.content = Content::Unsupported {
        what: "interactive message".into(),
    };
    worker
        .archive
        .insert_message(&row, Some(&raw.encode_to_vec()))
        .unwrap();
    worker.archive.set_meta("derived", "3").unwrap();
    worker
        .archive
        .execute_batch_for_test(
            "CREATE TRIGGER reject_derived_update BEFORE UPDATE ON messages
             BEGIN SELECT RAISE(ABORT, 'synthetic update failure'); END;",
        )
        .unwrap();

    worker.backfill();
    assert_eq!(
        worker.archive.meta("derived").unwrap().as_deref(),
        Some("3")
    );

    worker
        .archive
        .execute_batch_for_test("DROP TRIGGER reject_derived_update;")
        .unwrap();
    worker.backfill();
    assert_eq!(
        worker.archive.meta("derived").unwrap().as_deref(),
        Some("5")
    );
    assert!(matches!(
        worker
            .archive
            .message(PEER, "OLD")
            .unwrap()
            .unwrap()
            .content,
        Content::Buttons { .. }
    ));
}

#[test]
fn mentions_missing_from_the_message_are_recovered_for_known_people_only() {
    let (mut worker, _, _, _) = worker();
    worker
        .lid_to_pn
        .insert("15581".to_owned(), "5511912345678".to_owned());
    let mut message = crate::archive::tests::message("group@g.us", "M1", 10, false);
    message.content = Content::text("oi @15581 e @99999");
    worker.polish(&mut message);
    assert_eq!(message.mentions.len(), 1);
    assert_eq!(message.mentions[0].user, "15581");
    assert_eq!(message.mentions[0].id, "5511912345678@s.whatsapp.net");
    // Nobody has spoken yet, so there is no WhatsApp name to show.
    assert_eq!(message.mentions[0].name, None);
    for text in ["a@15581.com", "@15581_foo", "@15581abc", "@@15581"] {
        let mut invalid = crate::archive::tests::message("group@g.us", "M2", 11, false);
        invalid.content = Content::text(text);
        worker.polish(&mut invalid);
        assert!(invalid.mentions.is_empty(), "unexpected mention for {text}");
    }
    // Once they have, the name they go by is attached for the tooltip,
    // found under the phone id even though the text carries the LID.
    worker.contacts.insert(
        "5511912345678@s.whatsapp.net".to_owned(),
        Contact {
            id: "5511912345678@s.whatsapp.net".to_owned(),
            full_name: None,
            push_name: Some("Bia".to_owned()),
        },
    );
    let mut again = crate::archive::tests::message("group@g.us", "M3", 12, false);
    again.content = Content::text("oi @15581");
    worker.polish(&mut again);
    assert_eq!(again.mentions[0].name.as_deref(), Some("Bia"));
    // A list the sender did provide is left alone.
    let mut listed = crate::archive::tests::message("group@g.us", "M2", 11, false);
    listed.content = Content::text("oi @15581");
    listed.mentions = vec![MentionRef {
        user: "77777".into(),
        id: "77777@s.whatsapp.net".into(),
        name: None,
    }];
    worker.polish(&mut listed);
    assert_eq!(listed.mentions.len(), 1);
    assert_eq!(listed.mentions[0].user, "77777");
}

#[tokio::test]
async fn empty_id_send_failure_completes_once_without_exposing_details() {
    let (mut worker, events, _, _) = worker();
    worker
        .handle_command(Command::Sent {
            chat: "fixture@s.whatsapp.net".into(),
            id: String::new(),
            session_generation: 0,
            error: Some("private body and credential".to_owned()),
        })
        .await;

    let emitted: Vec<_> = events.try_iter().collect();
    assert_eq!(
            emitted
                .iter()
                .filter(|event| matches!(event, Event::Sent { chat, success: false } if chat == "fixture@s.whatsapp.net"))
                .count(),
            1
        );
    assert!(emitted.iter().any(|event| matches!(
        event,
        Event::Error(error) if error == "Could not send message"
    )));
    assert!(!emitted.iter().any(|event| matches!(
        event,
        Event::Error(error) if error.contains("private body") || error.contains("credential")
    )));
}

#[test]
fn attachment_reply_moves_to_first_successful_send_in_requested_order() {
    let mut caption = Some("private caption".to_owned());
    let mut context = Some(wa::ContextInfo::default());
    let mut quoted = Some(Quoted {
        mentions: Vec::new(),
        id: "quoted-id".to_owned(),
        sender_name: None,
        sender: "fixture@s.whatsapp.net".to_owned(),
        summary: "quoted body".to_owned(),
    });
    let mut mentions = vec!["fixture@s.whatsapp.net".to_owned()];

    // A failed first path retains reply metadata for the next path.
    consume_attachment_reply(
        false,
        &mut caption,
        &mut context,
        &mut quoted,
        &mut mentions,
    );
    assert_eq!(caption.as_deref(), Some("private caption"));
    assert!(context.is_some() && quoted.is_some());
    assert_eq!(mentions, ["fixture@s.whatsapp.net"]);

    // First accepted send consumes it; later sends cannot inherit it.
    consume_attachment_reply(true, &mut caption, &mut context, &mut quoted, &mut mentions);
    assert!(caption.is_none() && context.is_none() && quoted.is_none());
    assert!(mentions.is_empty());
    consume_attachment_reply(true, &mut caption, &mut context, &mut quoted, &mut mentions);
    assert!(caption.is_none() && context.is_none() && quoted.is_none());
}

#[test]
fn send_failure_text_is_generic_and_does_not_include_protocol_details() {
    let exposed = sanitized_send_error();
    assert_eq!(exposed, "Could not send message");
    assert!(!exposed.contains("private body"));
    assert!(!exposed.contains("credential"));
}

#[test]
fn privacy_recovery_hides_content_until_a_successful_replay() {
    let (mut worker, events, _, _) = worker();
    const PEER: &str = "fixture@s.whatsapp.net";
    worker.privacy_ready = false;
    worker.archive.ensure_chat(PEER, "Fixture").unwrap();
    worker.emit_chats();
    assert!(events.try_recv().is_err());
    worker.preferences_recovered(0, false);
    assert!(!worker.privacy_ready);
    assert!(
        worker
            .archive
            .meta("chat_privacy_ready_v1")
            .unwrap()
            .is_none()
    );
    worker.archive.set_locked_at(PEER, true, 100).unwrap();
    worker.preferences_recovered(0, true);
    assert!(worker.privacy_ready);
    let chats = events
        .try_iter()
        .find_map(|event| match event {
            Event::Chats(chats) => Some(chats),
            _ => None,
        })
        .unwrap();
    assert!(chats[0].locked);
}

#[test]
fn stale_privacy_recovery_cannot_expose_a_different_linked_account() {
    let (mut worker, events, _, _) = worker();
    worker.privacy_ready = false;
    worker.privacy_recovering = true;
    worker.privacy_generation = 1;
    worker.preferences_recovered(0, true);
    assert!(!worker.privacy_ready);
    assert!(worker.privacy_recovering);
    assert!(events.try_recv().is_err());
    assert!(
        worker
            .archive
            .meta("chat_privacy_ready_v1")
            .unwrap()
            .is_none()
    );
}

#[test]
fn link_previews_and_mentions_come_from_extended_text() {
    let message = wa::Message {
        extended_text_message: whatsapp_rust::prelude::MessageField::some(
            wa::message::ExtendedTextMessage {
                text: Some("see spotifast.rocks @123456@lid".into()),
                matched_text: Some("https://spotifast.rocks/".into()),
                title: Some("spotifast.rocks".into()),
                description: Some("Spotify, native and fast".into()),
                context_info: whatsapp_rust::prelude::MessageField::some(wa::ContextInfo {
                    mentioned_jid: vec!["123456@lid".into()],
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        ..Default::default()
    };
    match classify(&message) {
        Some(Content::Text { preview, .. }) => {
            let preview = preview.expect("preview");
            assert_eq!(preview.url, "https://spotifast.rocks/");
            assert_eq!(preview.title.as_deref(), Some("spotifast.rocks"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(mentioned_of(&message), vec!["123456@lid".to_owned()]);
}

#[test]
fn outgoing_mentions_share_context_with_a_quote() {
    let mentions = vec!["491702222222@s.whatsapp.net".to_owned()];
    let message = outgoing_text(
        "hello @491702222222".to_owned(),
        Some(wa::ContextInfo {
            stanza_id: Some("quoted".to_owned()),
            ..Default::default()
        }),
        &mentions,
    );

    assert_eq!(message.text_content(), Some("hello @491702222222"));
    let context = context_of(&message).expect("text context");
    assert_eq!(context.stanza_id.as_deref(), Some("quoted"));
    assert_eq!(context.mentioned_jid, mentions);
}

#[test]
fn missing_or_disabled_expiration_leaves_message_normal() {
    for expiration in [None, Some(0)] {
        let mut message = wa::Message::text("hello");
        assert_eq!(apply_ephemeral_expiration(&mut message, expiration), None);
        assert_eq!(message.get_ephemeral_expiration(), None);
    }
}

#[test]
fn configured_expiration_is_added_to_text() {
    for expiration in [86_400, 604_800, 7_776_000] {
        let mut message = wa::Message::text("hello");
        assert_eq!(
            apply_ephemeral_expiration(&mut message, Some(expiration)),
            Some(expiration)
        );
        assert_eq!(message.get_ephemeral_expiration(), Some(expiration));
    }
}

#[test]
fn ephemeral_reply_preserves_quote_context() {
    let mut message = outgoing_text(
        "reply".to_owned(),
        Some(wa::ContextInfo {
            stanza_id: Some("quoted".to_owned()),
            ..Default::default()
        }),
        &[],
    );

    apply_ephemeral_expiration(&mut message, Some(604_800));

    let context = context_of(&message).expect("context");
    assert_eq!(context.stanza_id.as_deref(), Some("quoted"));
    assert_eq!(context.expiration, Some(604_800));
}

#[test]
fn ephemeral_media_preserves_caption() {
    let mut message = wa::Message {
        image_message: MessageField::some(wa::message::ImageMessage {
            caption: Some("look".to_owned()),
            ..Default::default()
        }),
        ..Default::default()
    };

    apply_ephemeral_expiration(&mut message, Some(7_776_000));

    let image = message.image_message.as_option().expect("image");
    assert_eq!(image.caption.as_deref(), Some("look"));
    assert_eq!(
        image
            .context_info
            .as_option()
            .and_then(|info| info.expiration),
        Some(7_776_000)
    );
}

#[test]
fn forwards_use_only_the_destination_timer() {
    let context = wa::ContextInfo {
        expiration: Some(7_776_000),
        ephemeral_setting_timestamp: Some(123),
        ephemeral_shared_secret: Some(vec![1, 2, 3]),
        is_forwarded: Some(true),
        forwarding_score: Some(2),
        ..Default::default()
    };
    let text = wa::Message::text_with_context("forward me", context.clone());
    let image = wa::Message {
        image_message: MessageField::some(wa::message::ImageMessage {
            caption: Some("caption".into()),
            direct_path: Some("/media/path".into()),
            context_info: MessageField::some(context.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let contacts = wa::Message {
        contacts_array_message: MessageField::some(wa::message::ContactsArrayMessage {
            context_info: MessageField::some(context),
            ..Default::default()
        }),
        ..Default::default()
    };
    for original in [text, image, contacts] {
        for timer in [None, Some(0), Some(86_400)] {
            let expected = timer.filter(|value| *value > 0);
            let (forward, expiration) = outgoing_forward(&original, timer);
            assert_eq!(expiration, expected);
            assert_eq!(forward.get_ephemeral_expiration(), expected);
            let context = context_of(&forward).unwrap();
            assert_eq!(context.expiration, expected);
            assert_eq!(context.ephemeral_setting_timestamp, None);
            assert_eq!(context.ephemeral_shared_secret, None);
            assert_eq!(context.is_forwarded, Some(true));
            assert_eq!(context.forwarding_score, Some(3));
            if let Some(image) = forward.image_message.as_option() {
                assert_eq!(image.caption.as_deref(), Some("caption"));
                assert_eq!(image.direct_path.as_deref(), Some("/media/path"));
            }
            assert_eq!(original.get_ephemeral_expiration(), Some(7_776_000));
        }
    }
}

#[test]
fn forwarded_rows_keep_content_but_reset_conversation_state() {
    let source = Message {
        id: "source".into(),
        chat: "one@s.whatsapp.net".into(),
        sender: "one@s.whatsapp.net".into(),
        sender_name: Some("Ada".into()),
        from_me: false,
        timestamp: 10,
        content: Content::text("hello"),
        status: Delivery::Read,
        delivered_at: Some(11),
        read_at: Some(12),
        quoted: Some(Quoted {
            id: "quoted".into(),
            sender: "two@s.whatsapp.net".into(),
            sender_name: Some("Bob".into()),
            summary: "earlier".into(),
            mentions: Vec::new(),
        }),
        reactions: vec![Reaction {
            sender: "two@s.whatsapp.net".into(),
            from_me: false,
            emoji: "👍".into(),
        }],
        edited: true,
        mentions: Vec::new(),
        forwarded: false,
        thumbnail: Some(vec![1]),
    };
    let mention = MentionRef {
        user: "3".into(),
        id: "3@s.whatsapp.net".into(),
        name: None,
    };

    let forwarded = forwarded_row(
        source,
        "target@g.us".into(),
        "me@s.whatsapp.net".into(),
        "new".into(),
        20,
        vec![mention.clone()],
        Some(vec![2]),
    );

    assert_eq!(forwarded.id, "new");
    assert_eq!(forwarded.chat, "target@g.us");
    assert_eq!(forwarded.sender, "me@s.whatsapp.net");
    assert!(forwarded.from_me && forwarded.forwarded);
    assert_eq!(forwarded.timestamp, 20);
    assert_eq!(forwarded.status, Delivery::Pending);
    assert!(forwarded.delivered_at.is_none() && forwarded.read_at.is_none());
    assert!(forwarded.quoted.is_none() && forwarded.reactions.is_empty());
    assert!(!forwarded.edited);
    assert_eq!(forwarded.mentions, vec![mention]);
    assert_eq!(forwarded.thumbnail, Some(vec![2]));
    assert_eq!(forwarded.content, Content::text("hello"));
}

#[test]
fn pictures_get_a_thumbnail_and_a_jpeg_body() {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        300,
        200,
        image::Rgba([200, 30, 30, 255]),
    ));
    let jpeg = encode_jpeg(&image, 80).expect("encodes");
    assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    let thumbnail = thumbnail_jpeg(&image).expect("thumbnail");
    let small = image::load_from_memory(&thumbnail).expect("decodes");
    assert!(small.width() <= THUMBNAIL_SIDE && small.height() <= THUMBNAIL_SIDE);
}

#[test]
fn millisecond_timestamps_are_normalised() {
    assert_eq!(seconds(1_700_000_000), 1_700_000_000);
    assert_eq!(seconds(1_700_000_000_000), 1_700_000_000);
    assert_eq!(seconds(-1), 0);
}

pub(in crate::backend::worker) fn worker() -> (
    Worker,
    std::sync::mpsc::Receiver<Event>,
    mpsc::UnboundedReceiver<Command>,
    mpsc::UnboundedReceiver<RuntimeEvent>,
) {
    let (events, events_rx) = std::sync::mpsc::channel();
    let (commands, inbox) = mpsc::unbounded_channel();
    let (wa_sender, wa_events) = mpsc::unbounded_channel();
    let (_test_wa_sender, test_wa_events) = mpsc::unbounded_channel();
    let root = std::env::temp_dir().join(format!("zaptide-worker-test-{}", std::process::id()));
    let worker = Worker {
        privacy_ready: true,
        privacy_recovering: false,
        privacy_generation: 0,
        privacy_retry: Instant::now(),
        dirs: AppDirs::under(&root),
        events,
        commands,
        waker: Arc::new(crate::backend::Waker),
        archive: Archive::in_memory().expect("archive"),
        client: None,
        handle: None,
        wa_sender,
        wa_events,
        me_pn: Some("15550001111@s.whatsapp.net".into()),
        me_lid: None,
        me_name: None,
        me_about: None,
        lid_to_pn: HashMap::new(),
        contacts: HashMap::new(),
        status: LinkStatus::Connected,
        session_generation: 0,
        session_generation_shared: Arc::new(AtomicU64::new(0)),
        forward_tails: HashMap::new(),
        avatar_generations: HashMap::new(),
        session_cache_lock: Arc::new(tokio::sync::Mutex::new(())),
        pairing_phone: None,
        pair_code: None,
        pair_request_id: 0,
        archive_cleanup_failed: false,
        qr: None,
        syncing: false,
        sync_deadline: None,
        group_info_requested: HashSet::new(),
        group_info_queue: std::collections::VecDeque::new(),
        group_info_tries: HashMap::new(),
        group_info_retry: Vec::new(),
        presence_subscribed: HashSet::new(),
        pending_older: HashMap::new(),
        next_older_request_id: 0,
        older_warned: HashSet::new(),
        pending_avatars: HashMap::new(),
        sticker_fetches: HashSet::new(),
        sticker_downloads: HashSet::new(),
        deferred_downloads: Vec::new(),
        next_attachment_batch: 0,
        read_sync: ReadSync::default(),
        poll_decrypting: 0,
        poll_history: Default::default(),
        answer_sends: HashMap::new(),
        pending_revokes: HashMap::new(),
        next_revoke_attempt: 0,
        poll_sending: HashSet::new(),
    };
    (worker, events_rx, inbox, test_wa_events)
}

#[tokio::test]
async fn older_failure_from_expired_attempt_does_not_finish_newer_request() {
    let (mut worker, events, _inbox, _wa) = worker();
    let chat = "1@s.whatsapp.net".to_owned();
    let older_attempt = 40;
    let newer_attempt = 41;
    worker.pending_older.insert(
        chat.clone(),
        PendingOlder {
            request_id: newer_attempt,
            asked: Instant::now(),
            before: (200, "new-boundary".to_owned()),
            protocol_id: Some("new-protocol-id".to_owned()),
            received_count: 0,
            more_on_phone: None,
            early_responses: HashMap::new(),
        },
    );

    worker
        .handle_command(Command::OlderFailed {
            session_generation: worker.session_generation,
            request_id: older_attempt,
            chat: chat.clone(),
            error: "synthetic stale failure".to_owned(),
        })
        .await;

    assert_eq!(
        worker
            .pending_older
            .get(&chat)
            .map(|request| request.request_id),
        Some(newer_attempt)
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn late_history_response_id_does_not_finish_newer_same_chat_request() {
    let (mut worker, events, _inbox, _wa) = worker();
    let chat = "1@s.whatsapp.net".to_owned();
    worker.pending_older.insert(
        chat.clone(),
        PendingOlder {
            request_id: 41,
            asked: Instant::now(),
            before: (200, "new-boundary".to_owned()),
            protocol_id: Some("new-protocol-id".to_owned()),
            received_count: 0,
            more_on_phone: None,
            early_responses: HashMap::new(),
        },
    );

    worker.answer_older(
        vec![(chat.clone(), 5, Some(false))],
        Some("late-old-protocol-id".to_owned()),
    );

    assert_eq!(
        worker
            .pending_older
            .get(&chat)
            .map(|request| request.request_id),
        Some(41)
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn history_response_before_send_completion_is_correlated_later() {
    let (mut worker, events, _inbox, _wa) = worker();
    let chat = "1@s.whatsapp.net".to_owned();
    worker.pending_older.insert(
        chat.clone(),
        PendingOlder {
            request_id: 42,
            asked: Instant::now(),
            before: (200, "boundary".to_owned()),
            protocol_id: None,
            received_count: 0,
            more_on_phone: None,
            early_responses: HashMap::new(),
        },
    );

    worker.answer_older(
        vec![(chat.clone(), 3, None)],
        Some("matching-protocol-id".to_owned()),
    );
    worker.answer_older(
        vec![(chat.clone(), 2, Some(false))],
        Some("matching-protocol-id".to_owned()),
    );
    assert!(worker.pending_older.contains_key(&chat));
    assert!(events.try_recv().is_err());

    worker.older_request_started(chat.clone(), 42, "matching-protocol-id".to_owned());

    assert!(!worker.pending_older.contains_key(&chat));
    assert!(matches!(
        events.try_recv(),
        Ok(Event::Messages { older: true, .. })
    ));
    assert!(matches!(events.try_recv(), Ok(Event::OlderFetched { .. })));
}

#[test]
fn multi_chunk_history_waits_for_transfer_completion_before_finishing_request() {
    let (mut worker, events, _inbox, _wa) = worker();
    let chat = "1@s.whatsapp.net".to_owned();
    worker.pending_older.insert(
        chat.clone(),
        PendingOlder {
            request_id: 43,
            asked: Instant::now(),
            before: (200, "boundary".to_owned()),
            protocol_id: Some("multi-chunk-request".to_owned()),
            received_count: 0,
            more_on_phone: None,
            early_responses: HashMap::new(),
        },
    );

    worker.answer_older(
        vec![(chat.clone(), 20, None)],
        Some("multi-chunk-request".to_owned()),
    );
    assert_eq!(
        worker
            .pending_older
            .get(&chat)
            .map(|request| request.received_count),
        Some(20)
    );
    assert!(events.try_recv().is_err());

    worker.answer_older(
        vec![(chat.clone(), 30, Some(true))],
        Some("multi-chunk-request".to_owned()),
    );
    assert!(!worker.pending_older.contains_key(&chat));
    assert!(matches!(
        events.try_recv(),
        Ok(Event::Messages { older: true, .. })
    ));
    assert!(matches!(
        events.try_recv(),
        Ok(Event::OlderFetched { more: true, .. })
    ));
}

#[tokio::test]
async fn failed_revoke_restores_the_optimistically_hidden_message() {
    let (mut worker, events, _inbox, _wa) = worker();
    let chat = "1@s.whatsapp.net";
    let original = crate::archive::tests::message(chat, "message", 100, true);
    worker.archive.ensure_chat(chat, "A").unwrap();
    worker.archive.insert_message(&original, None).unwrap();
    let attempt_id = worker.begin_revoke(chat, "message").unwrap().unwrap();
    // Ignore the optimistic projection; inspect the completion's update below.
    while events.try_recv().is_ok() {}

    worker
        .handle_command(Command::RevokeFinished {
            session_generation: worker.session_generation,
            attempt_id,
            chat: chat.to_owned(),
            message: "message".to_owned(),
            success: false,
        })
        .await;

    let restored = worker
        .archive
        .message(chat, "message")
        .unwrap()
        .expect("message exists");
    assert_eq!(restored.content, original.content);
    assert!(matches!(events.try_recv(), Ok(Event::MessageUpdated(_))));
}

#[tokio::test]
async fn confirmed_revoke_cannot_be_undone_by_failed_request() {
    use whatsapp_rust::prelude::MessageField;
    for from_history in [false, true] {
        let (mut worker, _events, _inbox, _wa) = worker();
        let chat = "fixture@s.whatsapp.net";
        let original = crate::archive::tests::message(chat, "message", 100, true);
        worker.archive.ensure_chat(chat, "Fixture").unwrap();
        worker.archive.insert_message(&original, None).unwrap();
        let attempt_id = worker.begin_revoke(chat, "message").unwrap().unwrap();

        if from_history {
            let mut conversation = parse_conversation(wa::Conversation {
                id: chat.into(),
                ..Default::default()
            });
            conversation.revoked.push("message".into());
            worker.apply_history(
                ParsedHistory {
                    chats: vec![conversation],
                    push_names: Vec::new(),
                    lids: Vec::new(),
                    stickers: Vec::new(),
                },
                false,
            );
        } else {
            let confirmation = wa::Message {
                protocol_message: MessageField::some(wa::message::ProtocolMessage {
                    r#type: Some(wa::message::protocol_message::Type::REVOKE),
                    key: MessageField::some(wa::MessageKey {
                        id: Some("message".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let info = MessageInfo {
                source: MessageSource {
                    chat: chat.parse().unwrap(),
                    sender: chat.parse().unwrap(),
                    is_from_me: true,
                    ..Default::default()
                },
                timestamp: whatsapp_rust::wacore::time::from_secs(200).unwrap(),
                ..Default::default()
            };
            worker.ingest(&Arc::new(confirmation), &info);
        }

        assert!(
            worker.pending_revokes.is_empty(),
            "confirmation invalidates rollback"
        );
        worker
            .handle_command(Command::RevokeFinished {
                session_generation: worker.session_generation,
                attempt_id,
                chat: chat.into(),
                message: "message".into(),
                success: false,
            })
            .await;
        let stored = worker.archive.message(chat, "message").unwrap().unwrap();
        assert_eq!(stored.content, Content::Revoked, "history={from_history}");
    }
}

#[tokio::test]
async fn revoke_completion_is_scoped_to_its_attempt_and_session() {
    let (mut worker, events, _inbox, _wa) = worker();
    let chat = "fixture@s.whatsapp.net";
    let original = crate::archive::tests::message(chat, "message", 100, true);
    worker.archive.ensure_chat(chat, "Fixture").unwrap();
    worker.archive.insert_message(&original, None).unwrap();
    let first = worker.begin_revoke(chat, "message").unwrap().unwrap();
    assert!(worker.begin_revoke(chat, "message").unwrap().is_none());
    worker
        .handle_command(Command::RevokeFinished {
            session_generation: worker.session_generation,
            attempt_id: first,
            chat: chat.into(),
            message: "message".into(),
            success: false,
        })
        .await;
    let retry = worker.begin_revoke(chat, "message").unwrap().unwrap();
    assert_ne!(first, retry);
    while events.try_recv().is_ok() {}

    for (session_generation, attempt_id) in [(worker.session_generation, first), (1, retry)] {
        worker
            .handle_command(Command::RevokeFinished {
                session_generation,
                attempt_id,
                chat: chat.into(),
                message: "message".into(),
                success: false,
            })
            .await;
        assert!(events.try_recv().is_err());
        assert_eq!(
            worker
                .archive
                .message(chat, "message")
                .unwrap()
                .unwrap()
                .content,
            Content::Revoked
        );
        assert_eq!(worker.pending_revokes.len(), 1);
    }

    worker
        .handle_command(Command::RevokeFinished {
            session_generation: worker.session_generation,
            attempt_id: retry,
            chat: chat.into(),
            message: "message".into(),
            success: true,
        })
        .await;
    assert!(worker.pending_revokes.is_empty());
    assert!(events.try_recv().is_err());
    assert_eq!(
        worker
            .archive
            .message(chat, "message")
            .unwrap()
            .unwrap()
            .content,
        Content::Revoked
    );
}

#[tokio::test]
async fn downloaded_document_with_long_name_can_be_staged_and_saved() {
    let directory = tempfile::tempdir().unwrap();
    let chat = "1@s.whatsapp.net";
    let id = "A".repeat(32);
    let name = format!("{}.pdf", "x".repeat(160));
    let destination = media_path(directory.path(), chat, &id, "application/pdf", Some(&name));
    let staged = download_staging_path(directory.path());
    let generation = AtomicU64::new(0);
    let cache_lock = tokio::sync::Mutex::new(());
    let bytes = b"synthetic document bytes";

    write_session_cache_file(
        directory.path(),
        &staged,
        bytes,
        0,
        &generation,
        None,
        &cache_lock,
    )
    .await
    .expect("a valid document name must not prevent staging");
    std::fs::rename(&staged, &destination).expect("saves the original document name");

    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    assert!(!staged.exists());
}

#[tokio::test]
async fn simultaneous_download_completions_for_same_attachment_both_succeed() {
    let (mut worker, events, _inbox, _wa) = worker();
    let directory = tempfile::tempdir().unwrap();
    let chat = "1@s.whatsapp.net";
    let id = "message";
    let raw = b"synthetic image raw";
    let mut message = crate::archive::tests::message(chat, id, 100, false);
    message.content = Content::Image {
        caption: None,
        media: crate::model::Media {
            mime: "image/jpeg".into(),
            size: 10,
            width: Some(2),
            height: Some(2),
            path: None,
            state: Default::default(),
        },
    };
    worker.archive.ensure_chat(chat, "A").unwrap();
    worker.archive.insert_message(&message, Some(raw)).unwrap();
    let fingerprint = message_raw_fingerprint(raw);
    let destination = media_path(directory.path(), chat, id, "image/jpeg", None);
    let staged = std::array::from_fn::<_, 2, _>(|_| download_staging_path(directory.path()));
    // Both writes finish before the worker processes either completion.
    for path in &staged {
        std::fs::write(path, b"synthetic image bytes").unwrap();
    }
    for path in &staged {
        worker
            .handle_command(Command::Downloaded {
                chat: chat.to_owned(),
                id: id.to_owned(),
                session_generation: worker.session_generation,
                raw_fingerprint: fingerprint,
                destination: destination.clone(),
                result: Ok(path.clone()),
            })
            .await;
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Media { result: Ok(path), .. }) if path == destination
        ));
    }

    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"synthetic image bytes"
    );
    assert!(staged.iter().all(|path| !path.exists()));
    let stored = worker.archive.message(chat, id).unwrap().unwrap();
    assert_eq!(
        stored.content.media().unwrap().path.as_ref(),
        Some(&destination)
    );
}

#[tokio::test]
async fn archive_read_failure_preserves_download_until_revalidation() {
    for resolution in ["retry", "replacement", "shutdown", "logout"] {
        let (mut worker, events, _inbox, _wa) = worker();
        let directory = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(directory.path());
        let chat = "fixture@s.whatsapp.net";
        let id = "sticker";
        let raw = b"current sticker raw";
        let fingerprint = message_raw_fingerprint(raw);
        let key = (chat.to_owned(), id.to_owned(), fingerprint);
        let mut message = crate::archive::tests::message(chat, id, 100, true);
        message.content = Content::Sticker {
            media: crate::model::Media {
                mime: "image/webp".into(),
                size: 10,
                width: Some(2),
                height: Some(2),
                path: None,
                state: Default::default(),
            },
            animated: false,
        };
        worker.archive.ensure_chat(chat, "Fixture").unwrap();
        worker.archive.insert_message(&message, Some(raw)).unwrap();
        worker.sticker_downloads.insert(key.clone());
        let staged = download_staging_path(directory.path());
        let destination = directory.path().join("current.webp");
        std::fs::write(&staged, b"current sticker bytes").unwrap();
        worker.archive.execute_batch_for_test(
            "ALTER TABLE messages RENAME TO messages_fixture;
             CREATE VIEW messages AS SELECT chat, id, json_extract('invalid-json', '$') AS raw FROM messages_fixture;"
        ).unwrap();
        assert!(worker.archive.raw(chat, id).is_err());

        worker
            .handle_command(Command::Downloaded {
                chat: chat.into(),
                id: id.into(),
                session_generation: worker.session_generation,
                raw_fingerprint: fingerprint,
                destination: destination.clone(),
                result: Ok(staged.clone()),
            })
            .await;

        assert!(staged.exists(), "read errors must not discard valid bytes");
        assert!(worker.sticker_downloads.contains(&key));
        assert!(events.try_recv().is_err());
        assert_eq!(worker.deferred_downloads.len(), 1);
        // Early ticks must not consume the completion. A repeated read error
        // keeps the same bytes and exactly one pending validation.
        worker.retry_deferred_downloads(Instant::now()).await;
        assert_eq!(worker.deferred_downloads.len(), 1);
        worker
            .retry_deferred_downloads(Instant::now() + DOWNLOAD_VALIDATION_RETRY)
            .await;
        assert!(staged.exists());
        assert!(worker.sticker_downloads.contains(&key));
        assert_eq!(worker.deferred_downloads.len(), 1);
        assert!(events.try_recv().is_err());
        worker
            .archive
            .execute_batch_for_test(
                "DROP VIEW messages; ALTER TABLE messages_fixture RENAME TO messages;",
            )
            .unwrap();

        if resolution == "shutdown" {
            drop(worker);
            assert!(
                !staged.exists(),
                "shutdown must clean deferred temporary files"
            );
            continue;
        }
        if resolution == "logout" {
            // Stop before reconnecting; this fixture must never start a bot.
            worker
                .archive
                .execute_batch_for_test(
                    "CREATE TRIGGER reject_logout_clear BEFORE DELETE ON messages
                 BEGIN SELECT RAISE(ABORT, 'synthetic clear failure'); END;",
                )
                .unwrap();
            worker.on_logged_out().await;
            assert!(
                !staged.exists(),
                "logout must clean deferred temporary files"
            );
            assert!(worker.deferred_downloads.is_empty());
            continue;
        }

        let replacement_key = (
            chat.to_owned(),
            id.to_owned(),
            message_raw_fingerprint(b"replacement raw"),
        );
        if resolution == "replacement" {
            worker
                .archive
                .insert_message(&message, Some(b"replacement raw"))
                .unwrap();
            worker.sticker_downloads.insert(replacement_key.clone());
        }
        worker
            .retry_deferred_downloads(Instant::now() + DOWNLOAD_VALIDATION_RETRY)
            .await;

        assert!(!staged.exists());
        assert!(worker.deferred_downloads.is_empty());
        assert!(!worker.sticker_downloads.contains(&key));
        if resolution == "replacement" {
            assert!(!destination.exists());
            assert!(worker.sticker_downloads.contains(&replacement_key));
            assert!(events.try_recv().is_err());
        } else {
            assert!(
                matches!(events.try_recv(), Ok(Event::Media { result: Ok(path), .. }) if path == destination)
            );
            assert!(
                matches!(events.try_recv(), Ok(Event::Stickers { recent, .. }) if recent.contains(&destination))
            );
            assert_eq!(
                std::fs::read(&destination).unwrap(),
                b"current sticker bytes"
            );
        }
    }
}

#[tokio::test]
async fn stale_download_cannot_attach_file_to_replaced_message() {
    let (mut worker, events, _inbox, _wa) = worker();
    let directory = tempfile::tempdir().unwrap();
    let staged = download_staging_path(directory.path());
    let destination = directory.path().join("current-media.jpg");
    std::fs::write(&staged, b"obsolete image bytes").unwrap();
    let chat = "1@s.whatsapp.net";
    let mut message = crate::archive::tests::message(chat, "message", 100, false);
    message.content = Content::Image {
        caption: None,
        media: crate::model::Media {
            mime: "image/jpeg".into(),
            size: 10,
            width: Some(2),
            height: Some(2),
            path: None,
            state: Default::default(),
        },
    };
    worker.archive.ensure_chat(chat, "A").unwrap();
    worker
        .archive
        .insert_message(&message, Some(b"replacement raw"))
        .unwrap();

    worker
        .handle_command(Command::Downloaded {
            chat: chat.to_owned(),
            id: "message".to_owned(),
            session_generation: worker.session_generation,
            raw_fingerprint: message_raw_fingerprint(b"original raw"),
            destination: destination.clone(),
            result: Ok(staged.clone()),
        })
        .await;

    let stored = worker
        .archive
        .message(chat, "message")
        .unwrap()
        .expect("message exists");
    assert_eq!(
        stored.content.media().and_then(|media| media.path.clone()),
        None
    );
    assert!(!staged.exists());
    assert!(!destination.exists());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn stale_download_does_not_release_replacement_picker_request() {
    for (stale_success, current_success) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let (mut worker, events, _inbox, _wa) = worker();
        let directory = tempfile::tempdir().unwrap();
        let chat = "fixture@s.whatsapp.net";
        let id = "sticker";
        let raw = b"replacement sticker raw";
        let current_fingerprint = message_raw_fingerprint(raw);
        let old_fingerprint = message_raw_fingerprint(b"obsolete sticker raw");
        let mut replacement = crate::archive::tests::message(chat, id, 100, true);
        replacement.content = Content::Sticker {
            media: crate::model::Media {
                mime: "image/webp".into(),
                size: 10,
                width: Some(2),
                height: Some(2),
                path: None,
                state: Default::default(),
            },
            animated: false,
        };
        worker.archive.ensure_chat(chat, "Fixture").unwrap();
        worker
            .archive
            .insert_message(&replacement, Some(raw))
            .unwrap();
        let current_key = (chat.to_owned(), id.to_owned(), current_fingerprint);
        let old_key = (chat.to_owned(), id.to_owned(), old_fingerprint);
        worker.sticker_downloads.insert(current_key.clone());
        worker.sticker_downloads.insert(old_key.clone());
        let destination = directory.path().join("current.webp");
        let old_staged = download_staging_path(directory.path());
        let old_result = if stale_success {
            std::fs::write(&old_staged, b"obsolete sticker bytes").unwrap();
            Ok(old_staged.clone())
        } else {
            Err("obsolete download failure".to_owned())
        };

        worker
            .handle_command(Command::Downloaded {
                chat: chat.into(),
                id: id.into(),
                session_generation: worker.session_generation,
                raw_fingerprint: old_fingerprint,
                destination: destination.clone(),
                result: old_result,
            })
            .await;

        assert!(
            events.try_recv().is_err(),
            "obsolete results must not reach the replacement"
        );
        assert!(worker.sticker_downloads.contains(&current_key));
        assert!(!worker.sticker_downloads.contains(&old_key));
        assert!(!old_staged.exists());
        assert!(!destination.exists());

        let current_staged = download_staging_path(directory.path());
        let current_result = if current_success {
            std::fs::write(&current_staged, b"current sticker bytes").unwrap();
            Ok(current_staged.clone())
        } else {
            Err("current download failure".to_owned())
        };
        worker
            .handle_command(Command::Downloaded {
                chat: chat.into(),
                id: id.into(),
                session_generation: worker.session_generation,
                raw_fingerprint: current_fingerprint,
                destination: destination.clone(),
                result: current_result,
            })
            .await;

        if current_success {
            assert!(
                matches!(events.try_recv(), Ok(Event::Media { result: Ok(path), .. }) if path == destination)
            );
            assert_eq!(
                std::fs::read(&destination).unwrap(),
                b"current sticker bytes"
            );
        } else {
            assert!(
                matches!(events.try_recv(), Ok(Event::Media { result: Err(error), .. }) if error == "current download failure")
            );
            assert!(!destination.exists());
        }
        assert!(
            matches!(events.try_recv(), Ok(Event::Stickers { recent, .. }) if recent.contains(&destination) == current_success)
        );
        assert!(!worker.sticker_downloads.contains(&current_key));
        assert!(!current_staged.exists());
    }
}

#[tokio::test]
async fn stalled_read_sync_future_times_out() {
    let outcome = bounded_read_sync(
        Duration::from_millis(1),
        std::future::pending::<std::result::Result<(), ()>>(),
    )
    .await;

    assert_eq!(outcome, ReadSyncOutcome::TimedOut);
}

#[tokio::test]
async fn stalled_bot_shutdown_fails_closed_before_logout_cleanup() {
    assert!(!wait_for_shutdown(Duration::from_millis(1), std::future::pending::<()>(),).await);
    assert!(wait_for_shutdown(Duration::from_secs(1), async {}).await);
}

#[test]
fn picture_update_invalidates_only_that_contacts_avatar_requests() {
    let (mut worker, _, _, _) = worker();
    let first = worker.avatar_generation("first@s.whatsapp.net", false);
    let other = worker.avatar_generation("other@s.whatsapp.net", false);
    let first_before = first.load(Ordering::Acquire);
    let other_before = other.load(Ordering::Acquire);

    worker.invalidate_avatar_generations("first@s.whatsapp.net");

    assert_eq!(first.load(Ordering::Acquire), first_before + 1);
    assert_eq!(other.load(Ordering::Acquire), other_before);
}

#[test]
fn logout_clears_history_sync_deadlines_requests_and_warnings() {
    let (mut worker, events, _inbox, _wa) = worker();
    let chat = "1@s.whatsapp.net".to_owned();
    worker.syncing = true;
    worker.sync_deadline = Some(Instant::now());
    worker.pending_older.insert(
        chat.clone(),
        PendingOlder {
            request_id: 1,
            asked: Instant::now(),
            before: (100, "boundary".to_owned()),
            protocol_id: None,
            received_count: 0,
            more_on_phone: None,
            early_responses: HashMap::new(),
        },
    );
    worker.older_warned.insert(chat);

    worker.clear_history_sync_state();

    assert!(!worker.syncing);
    assert!(worker.sync_deadline.is_none());
    assert!(worker.pending_older.is_empty());
    assert!(worker.older_warned.is_empty());
    assert!(matches!(events.try_recv(), Ok(Event::Syncing(false))));
}

pub(super) mod receipt_tests {
    use super::*;
    use crate::model::{Content, Delivery, Message};

    const ME: &str = "15550001111@s.whatsapp.net";
    const PEER: &str = "4917663430455@s.whatsapp.net";
    const PEER_LID: &str = "167650256810092@lid";

    #[test]
    fn group_questions_wait_in_line() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker
            .archive
            .ensure_chat("1-1@g.us", "Group")
            .expect("chat");
        worker
            .archive
            .ensure_chat("2-2@g.us", "Group")
            .expect("chat");
        worker.request_group_info("1-1@g.us", false);
        worker.request_group_info("2-2@g.us", false);
        worker.request_group_info("1-1@g.us", false);
        assert_eq!(worker.group_info_queue.len(), 2, "asked once each");
        // Forced requests go to the front.
        worker.request_group_info("1-1@g.us", true);
        assert_eq!(
            worker.group_info_queue.front().map(String::as_str),
            Some("1-1@g.us")
        );
        // Without a client, processing schedules a retry.
        worker.pump_group_info();
        assert!(worker.group_info_queue.is_empty() || worker.group_info_retry.len() >= 2);
        // Permanent failures are not requeued.
        worker.group_info_retry.clear();
        worker.handle_failed_group("gone@g.us".to_owned(), true);
        assert!(worker.group_info_retry.is_empty());
        // Retry transient failures after their delay.
        worker.handle_failed_group("busy@g.us".to_owned(), false);
        assert_eq!(worker.group_info_retry.len(), 1);
        assert_eq!(worker.group_info_tries.get("busy@g.us"), Some(&1));
    }

    #[test]
    fn phone_recents_and_saved_stickers_keep_their_sources() {
        let (mut worker, events, _, _) = worker();
        let root = tempfile::tempdir().expect("temporary sticker root");
        worker.dirs = AppDirs::under(root.path());
        let saved = worker.dirs.saved_sticker_dir().join("favorite.webp");
        let phone_recent = worker.dirs.sticker_cache_dir().join("phone-recent.webp");
        std::fs::create_dir_all(worker.dirs.saved_sticker_dir()).expect("saved directory");
        std::fs::create_dir_all(worker.dirs.sticker_cache_dir()).expect("cache directory");
        std::fs::write(&saved, b"favorite").expect("favorite fixture");
        std::fs::write(&phone_recent, b"recent").expect("recent fixture");
        worker
            .archive
            .upsert_phone_sticker("phone-recent", b"metadata", 42, 1.0)
            .expect("phone sticker");
        worker
            .archive
            .set_sticker_path("phone-recent", &phone_recent)
            .expect("cached sticker");

        worker.emit_stickers();
        let Event::Stickers {
            saved: favorites,
            recent,
            ..
        } = events.try_recv().expect("sticker event")
        else {
            panic!("expected sticker event");
        };
        assert_eq!(favorites, vec![saved]);
        assert_eq!(recent, vec![phone_recent]);
    }

    #[test]
    fn unavailable_attachment_batch_reports_every_staged_path_in_order() {
        let (mut worker, events, _inbox, _wa) = worker();
        let paths = vec![PathBuf::from("first.jpg"), PathBuf::from("second.jpg")];

        worker.send_files(
            PEER.into(),
            paths.clone(),
            Default::default(),
            Some("caption".into()),
            None,
            Vec::new(),
        );

        let completions: Vec<_> = events
            .try_iter()
            .filter_map(|event| match event {
                Event::AttachmentCompleted {
                    batch,
                    index,
                    total,
                    path,
                    success,
                    ..
                } => Some((batch, index, total, path, success)),
                _ => None,
            })
            .collect();
        assert_eq!(
            completions,
            vec![
                (0, 0, 2, PathBuf::from("first.jpg"), false),
                (0, 1, 2, PathBuf::from("second.jpg"), false),
            ]
        );
        assert_eq!(worker.next_attachment_batch, 1);
    }

    #[tokio::test]
    async fn edit_completion_updates_the_archive_only_after_success() {
        let (mut worker, events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(PEER, "R").expect("chat");
        worker
            .archive
            .insert_message(&own_message("edit", 100), None)
            .expect("message");

        worker
            .handle_command(Command::Edited {
                chat: PEER.into(),
                id: "edit".into(),
                session_generation: 0,
                success: false,
                content: Content::text("new"),
                mentions: Vec::new(),
            })
            .await;
        let message = worker
            .archive
            .message(PEER, "edit")
            .expect("read")
            .expect("message");
        assert_eq!(message.content, Content::text("hi"));
        assert!(!message.edited);
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Edited { success: false, .. })
        ));

        worker
            .handle_command(Command::Edited {
                chat: PEER.into(),
                id: "edit".into(),
                session_generation: 0,
                success: true,
                content: Content::text("new"),
                mentions: Vec::new(),
            })
            .await;
        let message = worker
            .archive
            .message(PEER, "edit")
            .expect("read")
            .expect("message");
        assert_eq!(message.content, Content::text("new"));
        assert!(message.edited);
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Edited { success: true, .. }))
        );
    }

    #[test]
    fn quoted_attachment_context_reuses_text_quote_metadata() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.store_message(
            own_message("quoted", 1),
            Some(wa::Message::text("quoted text").encode_to_vec()),
            None,
        );

        let (context, quoted) = worker.quote_context(&PEER.to_owned(), Some("quoted"));

        assert_eq!(
            context.and_then(|context| context.stanza_id),
            Some("quoted".to_owned())
        );
        assert_eq!(quoted.map(|quoted| quoted.id), Some("quoted".to_owned()));
    }

    fn own_message(id: &str, timestamp: i64) -> Message {
        Message {
            id: id.into(),
            chat: PEER.into(),
            sender: ME.into(),
            sender_name: None,
            from_me: true,
            timestamp,
            content: Content::text("hi"),
            status: Delivery::Sent,
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

    fn receipt(chat: &str, ids: &[&str], kind: ReceiptType) -> wa_events::Receipt {
        let chat: Jid = chat.parse().expect("jid");
        wa_events::Receipt::builder()
            .message_ids(ids.iter().map(|id| (*id).into()).collect())
            .source(MessageSource {
                chat: chat.clone(),
                sender: chat,
                ..Default::default()
            })
            .timestamp(whatsapp_rust::wacore::time::now_utc())
            .r#type(kind)
            .offline(false)
            .build()
    }

    #[test]
    fn group_checks_wait_for_every_recipient_and_do_not_read_earlier_messages() {
        let (mut worker, _events, _inbox, _wa) = worker();
        let group = "123-456@g.us";
        let other = "12025550123@s.whatsapp.net";
        worker.archive.ensure_chat(group, "Group").unwrap();
        worker
            .archive
            .set_group_info(
                group,
                None,
                &[ME.into(), PEER_LID.into(), other.into()],
                false,
            )
            .unwrap();
        for (id, timestamp) in [("old", 100), ("new", 200)] {
            worker.store_message(
                Message {
                    chat: group.into(),
                    ..own_message(id, timestamp)
                },
                None,
                None,
            );
            assert!(worker.save_group_recipients(
                group,
                id,
                &[ME.into(), PEER_LID.into(), other.into()]
            ));
        }
        let send = |worker: &mut Worker, sender: &str, kind| {
            let mut receipt = receipt(group, &["new"], kind);
            receipt.source.sender = sender.parse().unwrap();
            receipt.source.is_group = true;
            worker.on_receipt(&receipt);
        };
        let status =
            |worker: &Worker, id| worker.archive.message(group, id).unwrap().unwrap().status;
        send(&mut worker, PEER_LID, ReceiptType::Read);
        send(&mut worker, ME, ReceiptType::Read);
        send(&mut worker, "12025550999@s.whatsapp.net", ReceiptType::Read);
        assert_eq!(status(&worker, "new"), Delivery::Sent);
        // A new alias or device is not another reader. Learning a mapping after
        // the first receipt must also merge its saved audience entry.
        worker.learn_lid("167650256810092", "4917663430455");
        send(&mut worker, PEER, ReceiptType::Read);
        send(
            &mut worker,
            "4917663430455:2@s.whatsapp.net",
            ReceiptType::Read,
        );
        assert_eq!(status(&worker, "new"), Delivery::Sent);
        send(&mut worker, other, ReceiptType::Delivered);
        assert_eq!(status(&worker, "new"), Delivery::Delivered);
        // Departures and joins do not rewrite the message's original audience.
        worker
            .archive
            .set_group_info(group, None, &[ME.into(), PEER.into()], false)
            .unwrap();
        send(&mut worker, PEER, ReceiptType::Read);
        assert_eq!(status(&worker, "new"), Delivery::Delivered);
        send(&mut worker, other, ReceiptType::Read);
        assert_eq!(status(&worker, "new"), Delivery::Read);
        assert_eq!(status(&worker, "old"), Delivery::Sent);
        send(&mut worker, PEER, ReceiptType::Delivered);
        assert_eq!(status(&worker, "new"), Delivery::Read);
    }

    #[test]
    fn history_keeps_ephemeral_metadata() {
        let parsed = parse_conversation(wa::Conversation {
            id: PEER.into(),
            ephemeral_expiration: Some(7_776_000),
            ephemeral_setting_timestamp: Some(1_700_000_000),
            ..Default::default()
        });

        assert_eq!(parsed.ephemeral_expiration, Some(7_776_000));
        assert_eq!(parsed.ephemeral_setting_timestamp, Some(1_700_000_000));
    }

    fn history_entry(
        chat: &str,
        id: &str,
        from_me: bool,
        participant: Option<&str>,
        message: wa::Message,
        reactions: Vec<wa::Reaction>,
        secret: Option<Vec<u8>>,
    ) -> wa::HistorySyncMsg {
        wa::HistorySyncMsg {
            message: MessageField::some(wa::WebMessageInfo {
                key: MessageField::some(wa::MessageKey {
                    remote_jid: Some(chat.into()),
                    from_me: Some(from_me),
                    id: Some(id.into()),
                    participant: participant.map(str::to_owned),
                }),
                message: MessageField::some(message),
                message_timestamp: Some(100),
                reactions,
                message_secret: secret,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn reaction_emoji_prefers_text_then_grouping_key() {
        assert_eq!(
            reaction_emoji(Some("🏆"), Some("👍")).as_deref(),
            Some("🏆")
        );
        assert_eq!(reaction_emoji(Some(""), Some("🏆")).as_deref(), Some("🏆"));
        assert_eq!(reaction_emoji(None, Some("🏆")).as_deref(), Some("🏆"));
        assert_eq!(reaction_emoji(Some("  "), None), None);
    }

    #[test]
    fn history_applies_a_standalone_custom_reaction_from_another_sender() {
        let group = "123-456@g.us";
        let reactor = "12025550999@s.whatsapp.net";
        let parsed = parse_conversation(wa::Conversation {
            id: group.into(),
            messages: vec![
                history_entry(
                    group,
                    "photo",
                    false,
                    Some(PEER),
                    wa::Message {
                        conversation: Some("caption".into()),
                        ..Default::default()
                    },
                    Vec::new(),
                    None,
                ),
                history_entry(
                    group,
                    "react",
                    false,
                    Some(reactor),
                    wa::Message {
                        reaction_message: MessageField::some(wa::message::ReactionMessage {
                            key: MessageField::some(wa::MessageKey {
                                remote_jid: Some(group.into()),
                                from_me: Some(false),
                                id: Some("photo".into()),
                                participant: Some(PEER.into()),
                            }),
                            text: Some("🏆".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Vec::new(),
                    None,
                ),
            ],
            ..Default::default()
        });
        assert!(parsed.messages.iter().all(|message| message.id != "react"));
        assert_eq!(parsed.reactions.len(), 1);
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.apply_history(
            ParsedHistory {
                chats: vec![parsed],
                push_names: Vec::new(),
                lids: Vec::new(),
                stickers: Vec::new(),
            },
            true,
        );
        let stored = worker
            .archive
            .message(group, "photo")
            .unwrap()
            .expect("parent");
        assert_eq!(stored.reactions.len(), 1);
        assert_eq!(stored.reactions[0].emoji, "🏆");
        assert!(!stored.reactions[0].from_me);
        assert_eq!(stored.reactions[0].sender, reactor);
    }

    #[test]
    fn history_reads_aggregated_reactions_from_grouping_key() {
        let parsed = parse_conversation(wa::Conversation {
            id: PEER.into(),
            messages: vec![history_entry(
                PEER,
                "photo",
                false,
                None,
                wa::Message {
                    conversation: Some("caption".into()),
                    ..Default::default()
                },
                vec![wa::Reaction {
                    key: MessageField::some(wa::MessageKey {
                        from_me: Some(false),
                        participant: Some(PEER.into()),
                        ..Default::default()
                    }),
                    grouping_key: Some("🏆".into()),
                    ..Default::default()
                }],
                None,
            )],
            ..Default::default()
        });
        assert_eq!(parsed.messages[0].reactions.len(), 1);
        assert_eq!(parsed.messages[0].reactions[0].2, "🏆");
        assert!(!parsed.messages[0].reactions[0].1);
    }

    #[test]
    fn live_grouping_key_reaction_from_another_sender_is_stored() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(PEER, "Ada").unwrap();
        worker
            .archive
            .insert_message(&incoming("photo", 10), None)
            .unwrap();
        let raw = wa::Message {
            reaction_message: MessageField::some(wa::message::ReactionMessage {
                key: MessageField::some(wa::MessageKey {
                    remote_jid: Some(PEER.into()),
                    from_me: Some(false),
                    id: Some("photo".into()),
                    ..Default::default()
                }),
                grouping_key: Some("🏆".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let info = MessageInfo {
            source: MessageSource {
                chat: PEER.parse().unwrap(),
                sender: PEER.parse().unwrap(),
                ..Default::default()
            },
            timestamp: whatsapp_rust::wacore::time::from_secs(20).unwrap(),
            ..Default::default()
        };
        worker.ingest(&Arc::new(raw), &info);
        let stored = worker
            .archive
            .message(PEER, "photo")
            .unwrap()
            .expect("parent");
        assert_eq!(stored.reactions.len(), 1);
        assert_eq!(stored.reactions[0].emoji, "🏆");
        assert!(!stored.reactions[0].from_me);
        assert_eq!(stored.reactions[0].sender, PEER);
    }

    #[test]
    fn live_encrypted_custom_reaction_from_another_sender_is_stored() {
        let secret = [0x42u8; 32];
        let reactor = "12025550999@s.whatsapp.net";
        let (payload, iv) = whatsapp_rust::wacore::reaction::encrypt_reaction_with_secret(
            "🏆",
            1_700_000_000_123,
            &secret,
            "photo",
            PEER,
            reactor,
        )
        .expect("encrypt");
        let parent_raw = wa::Message {
            conversation: Some("caption".into()),
            message_context_info: MessageField::some(wa::MessageContextInfo {
                message_secret: Some(secret.to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(PEER, "Ada").unwrap();
        worker
            .archive
            .insert_message(&incoming("photo", 10), Some(&parent_raw.encode_to_vec()))
            .unwrap();
        let raw = wa::Message {
            enc_reaction_message: MessageField::some(wa::message::EncReactionMessage {
                target_message_key: MessageField::some(wa::MessageKey {
                    remote_jid: Some(PEER.into()),
                    from_me: Some(false),
                    id: Some("photo".into()),
                    participant: Some(PEER.into()),
                }),
                enc_payload: Some(payload),
                enc_iv: Some(iv.to_vec()),
            }),
            ..Default::default()
        };
        let info = MessageInfo {
            source: MessageSource {
                chat: PEER.parse().unwrap(),
                sender: reactor.parse().unwrap(),
                ..Default::default()
            },
            timestamp: whatsapp_rust::wacore::time::from_secs(20).unwrap(),
            ..Default::default()
        };
        worker.ingest(&Arc::new(raw), &info);
        let stored = worker
            .archive
            .message(PEER, "photo")
            .unwrap()
            .expect("parent");
        assert_eq!(stored.reactions.len(), 1);
        assert_eq!(stored.reactions[0].emoji, "🏆");
        assert_eq!(stored.reactions[0].sender, reactor);
        assert!(!stored.reactions[0].from_me);
    }

    #[tokio::test]
    async fn group_timer_updates_work_before_history_and_keep_disable_versions() {
        let (mut worker, _events, _inbox, _wa) = worker();
        let group = "123-456@g.us";
        for (expiration, timestamp, expected) in [
            (86_400, 200, Some(86_400)),
            (0, 300, None),
            (604_800, 250, None),
        ] {
            let update = wa_events::GroupUpdate::builder()
                .group_jid(group.parse().unwrap())
                .timestamp(whatsapp_rust::wacore::time::from_secs(timestamp).unwrap())
                .is_lid_addressing_mode(false)
                .action(Box::new(
                    whatsapp_rust::wacore::stanza::groups::GroupNotificationAction::Ephemeral {
                        expiration,
                        trigger: None,
                    },
                ))
                .build();
            worker
                .handle_wa_event(Arc::new(wa_events::Event::GroupUpdate(update)))
                .await;
            assert_eq!(
                worker
                    .archive
                    .chat(group)
                    .unwrap()
                    .unwrap()
                    .ephemeral_expiration,
                expected
            );
        }
    }

    #[tokio::test]
    async fn default_timer_notifications_never_rewrite_existing_chat_timers() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.ensure_chat(PEER, None);
        worker.archive.set_ephemeral(PEER, 604_800, 100).unwrap();
        for (from, duration, timestamp) in [
            (PEER, 86_400, 200),
            (ME, 86_400, 200),
            (ME, 0, 300),
            (ME, 604_800, 250),
        ] {
            let update = wa_events::DisappearingModeChanged::builder()
                .from(from.parse().unwrap())
                .duration(duration)
                .setting_timestamp(whatsapp_rust::wacore::time::from_secs(timestamp).unwrap())
                .build();
            worker
                .handle_wa_event(Arc::new(wa_events::Event::DisappearingModeChanged(update)))
                .await;
        }
        assert_eq!(worker.ephemeral_expiration(PEER), Some(604_800));
        assert!(worker.archive.chat(ME).unwrap().is_none());
    }

    #[tokio::test]
    async fn own_typing_is_hidden_in_self_direct_and_group_chats() {
        let (mut worker, events, _inbox, _wa) = worker();
        let device = ME.replacen('@', ":2@", 1);
        let own_lid = "9000001@lid";
        worker.me_lid = Some(own_lid.into());
        for (chat, sender) in [ME, PEER, "123-456@g.us"]
            .into_iter()
            .flat_map(|chat| [ME, device.as_str(), own_lid, PEER].map(|sender| (chat, sender)))
        {
            let presence = wa_events::ChatPresenceUpdate::builder()
                .source(MessageSource {
                    chat: chat.parse().unwrap(),
                    sender: sender.parse().unwrap(),
                    is_group: chat.ends_with("@g.us"),
                    ..Default::default()
                })
                .state(ChatPresence::Composing)
                .media(whatsapp_rust::types::presence::ChatPresenceMedia::Text)
                .build();
            worker
                .handle_wa_event(Arc::new(wa_events::Event::ChatPresence(presence)))
                .await;
        }
        let senders: Vec<_> = events
            .try_iter()
            .filter_map(|event| match event {
                Event::Typing { sender, .. } => Some(sender),
                _ => None,
            })
            .collect();
        assert_eq!(senders, [PEER, PEER, PEER]);
    }

    #[test]
    fn partial_group_history_receipts_do_not_override_the_phone_aggregate() {
        use wa::web_message_info::Status;
        let parsed = |chat: &str, status| {
            parse_conversation(wa::Conversation {
                id: chat.into(),
                messages: vec![wa::HistorySyncMsg {
                    message: MessageField::some(wa::WebMessageInfo {
                        key: MessageField::some(wa::MessageKey {
                            id: Some("history".into()),
                            from_me: Some(true),
                            ..Default::default()
                        }),
                        message: MessageField::some(wa::Message {
                            conversation: Some("hello".into()),
                            ..Default::default()
                        }),
                        status: Some(status),
                        user_receipt: vec![wa::UserReceipt {
                            user_jid: PEER.into(),
                            read_timestamp: Some(123),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            })
        };
        assert_eq!(
            parsed("123-456@g.us", Status::SERVER_ACK).messages[0].status,
            Delivery::Sent
        );
        assert_eq!(
            parsed("123-456@g.us", Status::DELIVERY_ACK).messages[0].status,
            Delivery::Delivered
        );
        assert_eq!(
            parsed("123-456@g.us", Status::READ).messages[0].status,
            Delivery::Read
        );
        assert_eq!(
            parsed(PEER, Status::SERVER_ACK).messages[0].status,
            Delivery::Read
        );
    }

    fn incoming(id: &str, timestamp: i64) -> Message {
        Message {
            from_me: false,
            sender: PEER.into(),
            status: Delivery::None,
            ..own_message(id, timestamp)
        }
    }

    #[test]
    fn saved_contact_name_replaces_push_name_on_messages() {
        let (mut worker, _events, _commands, _runtime) = worker();
        let mut message = Message {
            sender_name: Some("~pushed".into()),
            ..incoming("M1", 1)
        };
        worker.polish(&mut message);
        assert_eq!(message.sender_name.as_deref(), Some("~pushed"));
        worker.contacts.insert(
            PEER.into(),
            Contact {
                id: PEER.into(),
                full_name: Some("Ada Saved".into()),
                push_name: Some("pushed".into()),
            },
        );
        worker.polish(&mut message);
        assert_eq!(message.sender_name.as_deref(), Some("Ada Saved"));
    }

    #[test]
    fn unknown_or_disabled_account_privacy_never_permits_receipts() {
        use whatsapp_rust::wacore::iq::privacy::{
            PrivacyCategory, PrivacySetting, PrivacySettingsResponse, PrivacyValue,
        };
        let mut settings = PrivacySettingsResponse {
            settings: Vec::new(),
        };
        assert!(!account_allows_receipts(&settings));
        settings.settings.push(PrivacySetting {
            category: PrivacyCategory::ReadReceipts,
            value: PrivacyValue::None,
        });
        assert!(!account_allows_receipts(&settings));
        settings.settings[0].value = PrivacyValue::All;
        assert!(account_allows_receipts(&settings));
        settings.settings[0].value = PrivacyValue::None;
        assert!(
            !account_allows_receipts(&settings),
            "a phone privacy change takes effect without reconnecting"
        );
    }

    fn unread(worker: &Worker) -> u32 {
        worker.archive.chat(PEER).unwrap().unwrap().unread
    }

    fn history(unread: u32) -> ParsedHistory {
        ParsedHistory {
            chats: vec![parse_conversation(wa::Conversation {
                id: PEER.into(),
                unread_count: Some(unread),
                conversation_timestamp: Some(200),
                ..Default::default()
            })],
            push_names: Vec::new(),
            lids: Vec::new(),
            stickers: Vec::new(),
        }
    }

    #[test]
    fn history_preserves_pin_time_and_distinguishes_missing_mute_metadata() {
        let chat = parse_conversation(wa::Conversation {
            id: PEER.into(),
            pinned: Some(1_700_000_000),
            mute_end_time: Some(1_800_000_000),
            ..Default::default()
        });
        assert_eq!(chat.pinned_at, Some(1_700_000_000_000));
        assert_eq!(chat.muted_until, Some(Some(1_800_000_000)));
        assert_eq!(chat.locked, None, "absence must preserve existing state");
        let chat = parse_conversation(wa::Conversation {
            id: PEER.into(),
            locked: Some(true),
            ..Default::default()
        });
        assert_eq!(chat.locked, Some(true));
        for (end, expected) in [
            (None, None),
            (Some(0), Some(None)),
            (Some(u64::MAX), Some(Some(0))),
        ] {
            let chat = parse_conversation(wa::Conversation {
                id: PEER.into(),
                mute_end_time: end,
                ..Default::default()
            });
            assert_eq!(chat.muted_until, expected);
        }
    }

    #[tokio::test]
    async fn mute_and_pin_sync_before_history_survive_replays_and_unsetting() {
        let (mut worker, _events, _inbox, _wa) = worker();
        let time = whatsapp_rust::wacore::time::now_utc();
        for enabled in [true, false] {
            let mute = wa_events::MuteUpdate::builder()
                .jid(PEER.parse().unwrap())
                .timestamp(time)
                .from_full_sync(true)
                .action(Box::new(wa::sync_action_value::MuteAction {
                    muted: Some(enabled),
                    mute_end_timestamp: Some(-1),
                    ..Default::default()
                }))
                .build();
            let pin = wa_events::PinUpdate::builder()
                .jid(PEER.parse().unwrap())
                .timestamp(time)
                .from_full_sync(true)
                .action(Box::new(wa::sync_action_value::PinAction {
                    pinned: Some(enabled),
                }))
                .build();
            worker
                .handle_wa_event(Arc::new(wa_events::Event::MuteUpdate(mute)))
                .await;
            worker
                .handle_wa_event(Arc::new(wa_events::Event::PinUpdate(pin)))
                .await;
            let before = worker
                .archive
                .chat(PEER)
                .unwrap()
                .expect("sync creates the chat");
            assert_eq!(before.muted_until, enabled.then_some(0));
            assert_eq!(before.pinned, enabled);
            assert_eq!(
                before.pinned_at,
                if enabled { time.timestamp_millis() } else { 0 }
            );

            let mut stale = history(0);
            stale.chats[0].pinned_at = Some(if enabled { 0 } else { 123_000 });
            stale.chats[0].muted_until = Some(if enabled { None } else { Some(0) });
            worker.apply_history(stale, true);
            let after = worker.archive.chat(PEER).unwrap().unwrap();
            assert_eq!(after.muted_until, before.muted_until);
            assert_eq!(after.pinned, before.pinned);
            assert_eq!(after.pinned_at, before.pinned_at);
        }
    }

    #[tokio::test]
    async fn lock_sync_survives_stale_history_replay() {
        let (mut worker, _events, _inbox, _wa) = worker();
        let time = whatsapp_rust::wacore::time::now_utc();
        let lock = wa_events::LockChatUpdate::builder()
            .jid(PEER.parse().unwrap())
            .timestamp(time)
            .from_full_sync(true)
            .action(Box::new(wa::sync_action_value::LockChatAction {
                locked: Some(true),
            }))
            .build();
        worker
            .handle_wa_event(Arc::new(wa_events::Event::LockChatUpdate(lock)))
            .await;
        let before = worker
            .archive
            .chat(PEER)
            .unwrap()
            .expect("sync creates the chat");
        assert!(before.locked);

        // A history chunk cannot supersede a timestamped app-state update.
        worker.apply_history(history(0), true);
        assert!(worker.archive.chat(PEER).unwrap().unwrap().locked);
        let mut locked_history = history(0);
        locked_history.chats[0].locked = Some(false);
        worker.apply_history(locked_history, true);
        assert!(worker.archive.chat(PEER).unwrap().unwrap().locked);
    }

    #[test]
    fn early_privacy_id_mute_reaches_the_canonical_chat_without_a_duplicate() {
        let (mut worker, events, _inbox, _wa) = worker();
        worker.ensure_chat(PEER_LID, None);
        worker.archive.set_muted_at(PEER_LID, Some(0), 200).unwrap();
        worker.learn_lid("167650256810092", "4917663430455");
        let mut snapshot = history(0);
        snapshot.chats[0].pinned_at = Some(123_000);
        worker.apply_history(snapshot, true);
        let chat = worker.archive.chat(PEER).unwrap().unwrap();
        assert_eq!(chat.muted_until, Some(0));
        assert!(chat.pinned, "missing pin sync must not block history's pin");
        worker.emit_chats();
        let chats = events
            .try_iter()
            .filter_map(|event| match event {
                Event::Chats(chats) => Some(chats),
                _ => None,
            })
            .last()
            .unwrap();
        assert!(chats.iter().any(|chat| chat.id == PEER));
        assert!(!chats.iter().any(|chat| chat.id == PEER_LID));
        worker.archive.set_muted_at(PEER, None, 300).unwrap();
        worker
            .archive
            .put_lid("167650256810092", "4917663430455")
            .unwrap();
        assert_eq!(
            worker.archive.chat(PEER).unwrap().unwrap().muted_until,
            None
        );
    }

    #[test]
    fn history_without_mute_metadata_preserves_the_existing_history_value() {
        let (mut worker, _events, _inbox, _wa) = worker();
        let mut first = history(0);
        first.chats[0].muted_until = Some(Some(0));
        worker.apply_history(first, true);
        worker.apply_history(history(0), true);
        assert_eq!(
            worker.archive.chat(PEER).unwrap().unwrap().muted_until,
            Some(0)
        );
        let mut unmuted = history(0);
        unmuted.chats[0].muted_until = Some(None);
        worker.apply_history(unmuted, true);
        assert_eq!(
            worker.archive.chat(PEER).unwrap().unwrap().muted_until,
            None
        );
    }

    #[test]
    fn reading_without_blue_ticks_still_queues_private_sync_and_survives_history() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.store_message(incoming("a", 100), None, None);
        worker.store_message(incoming("b", 200), None, None);
        assert_eq!(unread(&worker), 2);
        worker.mark_read(PEER.into(), false);
        assert_eq!(unread(&worker), 0);
        assert_eq!(
            worker.archive.pending_reads().unwrap(),
            vec![(PEER.into(), 200)]
        );
        worker.apply_history(history(2), true);
        assert_eq!(
            unread(&worker),
            0,
            "stale history must not resurrect badges"
        );
        worker.store_message(incoming("late", 150), None, None);
        assert_eq!(unread(&worker), 0, "a delayed read message stays read");
        worker.store_message(incoming("new", 300), None, None);
        worker.apply_history(history(2), false);
        assert_eq!(
            unread(&worker),
            1,
            "paging old history preserves a new unread message"
        );
    }

    #[tokio::test]
    async fn a_failed_read_sync_stays_queued_until_it_succeeds() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.store_message(incoming("a", 100), None, None);
        worker.mark_read(PEER.into(), false);
        let now = Instant::now();
        let first_attempt = worker.read_sync.start(PEER, 100, now).unwrap();
        worker
            .handle_command(Command::ReadSyncFinished {
                session_generation: worker.session_generation,
                attempt_id: first_attempt,
                chat: PEER.into(),
                through: 100,
                success: false,
            })
            .await;
        assert_eq!(
            worker.archive.pending_reads().unwrap(),
            vec![(PEER.into(), 100)]
        );
        assert!(!worker.read_sync.ready(Instant::now()));
        assert!(
            worker
                .read_sync
                .start("another-chat", 200, Instant::now())
                .is_none()
        );
        // A new local read stays queued while the shared collection backs off.
        worker.store_message(incoming("b", 200), None, None);
        worker.mark_read(PEER.into(), false);
        let retry_attempt = worker
            .read_sync
            .start(PEER, 100, now + Duration::from_secs(31))
            .unwrap();
        worker
            .handle_command(Command::ReadSyncFinished {
                session_generation: worker.session_generation,
                attempt_id: retry_attempt,
                chat: PEER.into(),
                through: 100,
                success: true,
            })
            .await;
        assert_eq!(
            worker.archive.pending_reads().unwrap(),
            vec![(PEER.into(), 200)]
        );
        let final_attempt = worker.read_sync.start(PEER, 200, Instant::now()).unwrap();
        worker
            .handle_command(Command::ReadSyncFinished {
                session_generation: worker.session_generation,
                attempt_id: final_attempt,
                chat: PEER.into(),
                through: 200,
                success: true,
            })
            .await;
        assert!(worker.archive.pending_reads().unwrap().is_empty());
        assert!(worker.read_sync.ready(Instant::now()));
    }

    #[tokio::test]
    async fn stale_read_sync_attempt_cannot_acknowledge_same_position_retry() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.store_message(incoming("same-position", 100), None, None);
        worker.mark_read(PEER.into(), false);
        let now = Instant::now();
        let old_attempt = worker.read_sync.start(PEER, 100, now).unwrap();
        assert!(worker.read_sync.finish(old_attempt, PEER, 100, false, now));
        let new_attempt = worker
            .read_sync
            .start(PEER, 100, now + Duration::from_secs(31))
            .unwrap();
        assert_ne!(old_attempt, new_attempt);

        worker
            .handle_command(Command::ReadSyncFinished {
                session_generation: worker.session_generation,
                attempt_id: old_attempt,
                chat: PEER.into(),
                through: 100,
                success: true,
            })
            .await;

        assert_eq!(
            worker.archive.pending_reads().unwrap(),
            vec![(PEER.into(), 100)]
        );
        assert!(!worker.read_sync.ready(now + Duration::from_secs(31)));
    }

    #[tokio::test]
    async fn stale_read_sync_result_cannot_acknowledge_current_archive_position() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.store_message(incoming("read-sync", 100), None, None);
        worker.mark_read(PEER.into(), false);
        let attempt_id = worker.read_sync.start(PEER, 100, Instant::now()).unwrap();
        worker.session_generation = 1;

        worker
            .handle_command(Command::ReadSyncFinished {
                session_generation: 0,
                attempt_id,
                chat: PEER.into(),
                through: 100,
                success: true,
            })
            .await;

        assert_eq!(
            worker.archive.pending_reads().unwrap(),
            vec![(PEER.into(), 100)]
        );
        assert!(!worker.read_sync.ready(Instant::now()));
    }

    #[tokio::test]
    async fn stale_contact_results_cannot_write_or_emit_for_new_session() {
        let (mut worker, events, _inbox, _wa) = worker();
        worker.session_generation = 1;

        worker
            .handle_command(Command::ContactChecked {
                session_generation: 0,
                phone: "15550002222".into(),
                full_name: Some("Synthetic Contact".into()),
                first_name: None,
                to_phone: false,
                registered: true,
            })
            .await;
        worker
            .handle_command(Command::ContactSaved {
                session_generation: 0,
                id: "15550002222@s.whatsapp.net".into(),
                name: "Synthetic Contact".into(),
                error: None,
            })
            .await;

        assert!(
            worker
                .archive
                .contact("15550002222@s.whatsapp.net")
                .unwrap()
                .is_none()
        );
        assert!(events.try_iter().next().is_none());
    }

    #[tokio::test]
    async fn stale_group_results_cannot_mutate_archive_or_retry_state() {
        let (mut worker, _events, _inbox, _wa) = worker();
        let group = "123-456@g.us";
        worker.archive.ensure_chat(group, "Original group").unwrap();
        worker.group_info_requested.insert(group.into());
        worker.session_generation = 1;

        worker
            .handle_command(Command::GroupInfo {
                session_generation: 0,
                chat: group.into(),
                name: Some("Stale group name".into()),
                participants: vec![ME.into()],
                read_only: true,
                ephemeral_expiration: Some(3600),
                ephemeral_setting_timestamp: Some(10),
            })
            .await;
        worker
            .handle_command(Command::GroupInfoFailed {
                session_generation: 0,
                chat: group.into(),
                permanent: true,
            })
            .await;

        assert_eq!(
            worker.archive.chat(group).unwrap().unwrap().name,
            "Original group"
        );
        assert!(worker.group_info_requested.contains(group));
    }

    #[test]
    fn replying_on_the_phone_reads_only_preceding_messages() {
        let (mut worker, events, _inbox, _wa) = worker();
        worker.store_message(incoming("old", 100), None, None);
        worker.store_message(incoming("new", 300), None, None);
        worker.store_message(
            Message {
                status: Delivery::Failed,
                ..own_message("failed", 400)
            },
            None,
            None,
        );
        assert_eq!(unread(&worker), 2, "a failed send does not read the chat");
        worker.store_message(own_message("reply", 200), None, None);
        assert_eq!(unread(&worker), 1);
        worker.store_message(own_message("reply2", 400), None, None);
        assert_eq!(unread(&worker), 0);
        worker.store_message(own_message("reply", 200), None, None);
        assert_eq!(worker.archive.read_through(PEER).unwrap(), Some(400));
        while events.try_recv().is_ok() {}
        worker.store_message(incoming("late", 150), None, None);
        assert_eq!(unread(&worker), 0);
        assert!(
            !events
                .try_iter()
                .any(|event| matches!(event, Event::Incoming { .. }))
        );
    }

    #[test]
    fn delayed_phone_receipts_preserve_newer_unread_messages() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.learn_lid("167650256810092", "4917663430455");
        worker.store_message(incoming("old", 100), None, None);
        worker.store_message(incoming("new", 300), None, None);
        worker.on_receipt(&receipt(PEER_LID, &["old"], ReceiptType::ReadSelf));
        assert_eq!(unread(&worker), 1);
        worker.on_receipt(&receipt(PEER_LID, &["unknown"], ReceiptType::ReadSelf));
        assert_eq!(
            unread(&worker),
            1,
            "an unknown receipt has no known read position"
        );
        worker.on_receipt(&receipt(PEER_LID, &["new"], ReceiptType::ReadSelf));
        assert_eq!(unread(&worker), 0);
    }

    #[test]
    fn a_phone_read_addressed_to_an_unmapped_privacy_id_still_reads_the_chat() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.store_message(incoming("seen", 100), None, None);
        assert_eq!(unread(&worker), 1);
        worker.on_receipt(&receipt(PEER_LID, &["seen"], ReceiptType::ReadSelf));
        assert_eq!(unread(&worker), 0);
    }

    #[test]
    fn rapid_messages_keep_distinct_read_positions_within_the_same_second() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.store_message(incoming("first", 100), None, None);
        worker.mark_read(PEER.into(), false);
        worker.store_message(incoming("second", 100), None, None);
        worker.store_message(incoming("third", 100), None, None);
        assert_eq!(unread(&worker), 2);
        worker.on_receipt(&receipt(PEER, &["first"], ReceiptType::ReadSelf));
        assert_eq!(unread(&worker), 2);
        worker.on_receipt(&receipt(PEER, &["second"], ReceiptType::ReadSelf));
        assert_eq!(unread(&worker), 1);
        assert_eq!(
            worker.archive.unread_incoming(PEER, 1).unwrap(),
            vec![("third".into(), PEER.into())]
        );
        worker.on_receipt(&receipt(PEER, &["third"], ReceiptType::ReadSelf));
        assert_eq!(unread(&worker), 0);
    }

    #[test]
    fn a_phone_history_snapshot_can_clear_stale_unread_counts() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.store_message(incoming("a", 100), None, None);
        worker.store_message(incoming("b", 200), None, None);
        worker.store_message(incoming("new", 300), None, None);
        worker.apply_history(history(0), true);
        assert_eq!(
            unread(&worker),
            1,
            "a read snapshot preserves later arrivals"
        );
        worker.apply_history(history(2), true);
        assert_eq!(
            unread(&worker),
            1,
            "older unread history cannot undo a read snapshot"
        );
    }

    #[tokio::test]
    async fn phone_read_updates_cover_their_range_even_before_history_arrives() {
        let (mut worker, _events, _inbox, _wa) = worker();
        let event = wa_events::MarkChatAsReadUpdate::builder()
            .jid(PEER.parse().unwrap())
            .timestamp(whatsapp_rust::wacore::time::now_utc())
            .from_full_sync(false)
            .action(Box::new(wa::sync_action_value::MarkChatAsReadAction {
                read: Some(true),
                message_range: MessageField::some(whatsapp_rust::message_range(
                    200,
                    None,
                    Vec::new(),
                )),
            }))
            .build();
        worker
            .handle_wa_event(Arc::new(wa_events::Event::MarkChatAsReadUpdate(event)))
            .await;
        worker.apply_history(history(2), true);
        worker.store_message(incoming("late", 100), None, None);
        worker.store_message(incoming("new", 300), None, None);
        assert_eq!(unread(&worker), 1);
    }

    #[test]
    fn a_read_receipt_from_the_peers_privacy_id_moves_our_messages() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(PEER, "R").expect("chat");
        for (id, when) in [("A1", 100), ("A2", 200), ("A3", 300)] {
            worker
                .archive
                .insert_message(&own_message(id, when), None)
                .expect("stored");
        }
        worker.learn_lid("167650256810092", "4917663430455");
        worker.on_receipt(&receipt(PEER_LID, &["A2"], ReceiptType::Read));
        let status = |id: &str| {
            worker
                .archive
                .message(PEER, id)
                .expect("read")
                .expect("row")
                .status
        };
        assert_eq!(status("A2"), Delivery::Read, "the named message");
        assert_eq!(status("A1"), Delivery::Read, "and everything before it");
        assert_eq!(status("A3"), Delivery::Sent, "not what came after");
    }

    #[test]
    fn inactive_counts_as_delivered_and_sender_only_in_the_chat_with_ourselves() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(PEER, "R").expect("chat");
        worker.archive.ensure_chat(ME, "Me").expect("chat");
        worker
            .archive
            .insert_message(&own_message("C1", 100), None)
            .expect("stored");
        let mut to_self = own_message("S1", 100);
        to_self.chat = ME.into();
        worker
            .archive
            .insert_message(&to_self, None)
            .expect("stored");
        worker.on_receipt(&receipt(PEER, &["C1"], ReceiptType::Inactive));
        assert_eq!(
            worker
                .archive
                .message(PEER, "C1")
                .expect("read")
                .expect("row")
                .status,
            Delivery::Delivered,
            "an inactive device still received it"
        );
        worker.on_receipt(&receipt(PEER, &["C1"], ReceiptType::Sender));
        assert_eq!(
            worker
                .archive
                .message(PEER, "C1")
                .expect("read")
                .expect("row")
                .status,
            Delivery::Delivered,
            "our own other device says nothing about the peer"
        );
        worker.on_receipt(&receipt(ME, &["S1"], ReceiptType::Sender));
        assert_eq!(
            worker
                .archive
                .message(ME, "S1")
                .expect("read")
                .expect("row")
                .status,
            Delivery::Read,
            "a message to ourselves is read once the phone has it"
        );
    }

    #[test]
    fn a_delivery_receipt_from_the_phone_number_moves_only_the_named_message() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.archive.ensure_chat(PEER, "R").expect("chat");
        for (id, when) in [("B1", 100), ("B2", 200)] {
            worker
                .archive
                .insert_message(&own_message(id, when), None)
                .expect("stored");
        }
        worker.on_receipt(&receipt(PEER, &["B2"], ReceiptType::Delivered));
        let status = |id: &str| {
            worker
                .archive
                .message(PEER, id)
                .expect("read")
                .expect("row")
                .status
        };
        assert_eq!(status("B2"), Delivery::Delivered);
        assert_eq!(status("B1"), Delivery::Sent);
    }
}
