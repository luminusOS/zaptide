use super::worker;
use super::*;
use crate::model::unsupported;
use protocol::MAX_WRAPPER_DEPTH;
use whatsapp_rust::prelude::MessageField;

type Wrap = fn(wa::Message) -> wa::Message;

fn future_proof(inner: wa::Message) -> wa::message::FutureProofMessage {
    wa::message::FutureProofMessage {
        message: MessageField::some(inner),
    }
}

fn group_mentioned(inner: wa::Message) -> wa::Message {
    wa::Message {
        group_mentioned_message: MessageField::some(future_proof(inner)),
        ..Default::default()
    }
}

fn associated_child(inner: wa::Message) -> wa::Message {
    wa::Message {
        associated_child_message: MessageField::some(future_proof(inner)),
        ..Default::default()
    }
}

fn spoiler(inner: wa::Message) -> wa::Message {
    wa::Message {
        spoiler_message: MessageField::some(future_proof(inner)),
        ..Default::default()
    }
}

fn image() -> wa::Message {
    wa::Message {
        image_message: MessageField::some(wa::message::ImageMessage {
            caption: Some("synthetic caption".into()),
            mimetype: Some("image/jpeg".into()),
            file_length: Some(10),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn sticker() -> wa::Message {
    wa::Message {
        sticker_message: MessageField::some(wa::message::StickerMessage {
            mimetype: Some("image/webp".into()),
            file_length: Some(10),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn lottie_sticker() -> wa::Message {
    wa::Message {
        lottie_sticker_message: MessageField::some(future_proof(wa::Message {
            sticker_message: MessageField::some(wa::message::StickerMessage {
                mimetype: Some("application/was".into()),
                ..Default::default()
            }),
            ..Default::default()
        })),
        ..Default::default()
    }
}

#[test]
fn wrapped_messages_show_their_inner_content() {
    let wrappers: [(&str, Wrap); 3] = [
        ("group mention", group_mentioned),
        ("associated child", associated_child),
        ("spoiler", spoiler),
    ];
    for (name, wrap) in wrappers {
        assert_eq!(
            classify(&wrap(wa::Message::text("synthetic text"))),
            Some(Content::text("synthetic text")),
            "{name} around text"
        );
        assert!(
            matches!(
                classify(&wrap(image())),
                Some(Content::Image { caption: Some(caption), .. }) if caption == "synthetic caption"
            ),
            "{name} around an image"
        );
        assert!(
            matches!(
                classify(&wrap(sticker())),
                Some(Content::Sticker {
                    animated: false,
                    ..
                })
            ),
            "{name} around a sticker"
        );
    }
}

#[test]
fn wrappers_nest_inside_the_transport_wrappers() {
    let ephemeral = wa::Message {
        ephemeral_message: MessageField::some(future_proof(group_mentioned(sticker()))),
        ..Default::default()
    };
    assert!(matches!(
        classify(&ephemeral),
        Some(Content::Sticker { .. })
    ));
    let edited = wa::Message {
        edited_message: MessageField::some(future_proof(spoiler(wa::Message::text("again")))),
        ..Default::default()
    };
    assert_eq!(classify(&edited), Some(Content::text("again")));
}

fn nested(layers: usize) -> wa::Message {
    let mut message = wa::Message::text("synthetic text");
    for _ in 0..layers {
        message = group_mentioned(message);
    }
    message
}

#[test]
fn wrapper_chains_stop_at_the_depth_limit() {
    assert_eq!(
        classify(&nested(MAX_WRAPPER_DEPTH)),
        Some(Content::text("synthetic text"))
    );
    for layers in [MAX_WRAPPER_DEPTH + 1, MAX_WRAPPER_DEPTH * 8] {
        assert_eq!(
            classify(&nested(layers)),
            Some(Content::Unsupported {
                what: unsupported::GENERIC.into()
            })
        );
    }
}

#[test]
fn a_wrapper_around_nothing_is_internal_traffic() {
    assert_eq!(classify(&group_mentioned(wa::Message::default())), None);
}

#[test]
fn animated_stickers_are_labelled_and_not_claimed_as_supported() {
    let content = classify(&lottie_sticker()).expect("labelled content");
    assert_eq!(
        content,
        Content::Unsupported {
            what: unsupported::ANIMATED_STICKER.into()
        }
    );
    assert_eq!(content.summary(), "Animated sticker");
    assert!(content.media().is_none());
}

#[test]
fn a_quote_without_readable_content_still_has_a_summary() {
    let (worker, _events, _commands, _runtime) = worker();
    let quoting = |quoted: Option<wa::Message>| wa::Message {
        extended_text_message: MessageField::some(wa::message::ExtendedTextMessage {
            text: Some("synthetic reply".into()),
            context_info: MessageField::some(wa::ContextInfo {
                stanza_id: Some("QUOTED".into()),
                participant: Some("fixture@s.whatsapp.net".into()),
                quoted_message: quoted.map_or_else(MessageField::none, MessageField::some),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let summary = |quoted| worker.quoted_of(&quoting(quoted)).expect("quote").summary;
    assert_eq!(summary(Some(wa::Message::default())), "Message");
    assert_eq!(summary(None), "Message");
    assert_eq!(summary(Some(group_mentioned(sticker()))), "Sticker");
    assert_eq!(summary(Some(lottie_sticker())), "Animated sticker");
}
