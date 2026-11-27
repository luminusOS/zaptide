//! View-once photos, videos and voice messages: shown, marked, never dropped.

use super::worker;
use super::*;
use crate::model::unsupported;
use whatsapp_rust::prelude::MessageField;

fn wrapper(inner: wa::Message) -> wa::message::FutureProofMessage {
    from_default!(wa::message::FutureProofMessage {
        message: MessageField::some(inner),
    })
}

fn legacy_v1(inner: wa::Message) -> wa::Message {
    from_default!(wa::Message {
        view_once_message: MessageField::some(wrapper(inner)),
    })
}

fn legacy_v2(inner: wa::Message) -> wa::Message {
    from_default!(wa::Message {
        view_once_message_v2: MessageField::some(wrapper(inner)),
    })
}

fn legacy_v2_extension(inner: wa::Message) -> wa::Message {
    from_default!(wa::Message {
        view_once_message_v2_extension: MessageField::some(wrapper(inner)),
    })
}

fn image(flag: Option<bool>) -> wa::Message {
    from_default!(wa::Message {
        image_message: MessageField::some(from_default!(wa::message::ImageMessage {
            mimetype: Some("image/jpeg".into()),
            caption: Some("synthetic caption".into()),
            view_once: flag,
        })),
    })
}

fn video(flag: Option<bool>) -> wa::Message {
    from_default!(wa::Message {
        video_message: MessageField::some(from_default!(wa::message::VideoMessage {
            mimetype: Some("video/mp4".into()),
            seconds: Some(3),
            view_once: flag,
        })),
    })
}

fn voice(flag: Option<bool>) -> wa::Message {
    from_default!(wa::Message {
        audio_message: MessageField::some(from_default!(wa::message::AudioMessage {
            mimetype: Some("audio/ogg; codecs=opus".into()),
            seconds: Some(4),
            ptt: Some(true),
            view_once: flag,
        })),
    })
}

fn marked(message: &wa::Message) -> bool {
    classify(message).is_some_and(|content| content.view_once())
}

#[test]
fn every_wrapper_generation_marks_a_photo_and_keeps_it() {
    for wrap in [legacy_v1, legacy_v2, legacy_v2_extension] {
        let message = wrap(image(None));
        let Some(Content::Image { caption, media }) = classify(&message) else {
            panic!("a view-once photo must stay a photo");
        };
        assert!(media.view_once);
        assert_eq!(media.mime, "image/jpeg");
        assert_eq!(caption.as_deref(), Some("synthetic caption"));
    }
}

#[test]
fn wrappers_nest_in_any_order() {
    assert!(marked(&legacy_v2(legacy_v1(image(None)))));
    assert!(marked(&legacy_v2_extension(legacy_v2(video(None)))));
    let ephemeral = from_default!(wa::Message {
        ephemeral_message: MessageField::some(wrapper(legacy_v2(voice(None)))),
    });
    assert!(marked(&ephemeral));
}

#[test]
fn view_once_markers_survive_zaptide_wrappers() {
    let group_mentioned = from_default!(wa::Message {
        group_mentioned_message: MessageField::some(wrapper(legacy_v2(image(None)))),
    });
    let associated_child = from_default!(wa::Message {
        associated_child_message: MessageField::some(wrapper(voice(Some(true)))),
    });
    assert!(matches!(
        classify(&group_mentioned),
        Some(Content::Image { media, .. }) if media.view_once
    ));
    assert!(matches!(
        classify(&associated_child),
        Some(Content::Audio { media, .. }) if media.view_once
    ));
}

#[test]
fn only_the_selected_media_inline_flag_marks_the_message() {
    let mut message = image(Some(false));
    message.audio_message = voice(Some(true)).audio_message;
    assert!(matches!(
        classify(&message),
        Some(Content::Image { media, .. }) if !media.view_once
    ));
}

#[test]
fn view_once_inline_flag_marks_video_notes() {
    let ptv = from_default!(wa::Message {
        ptv_message: video(Some(true)).video_message,
    });
    assert!(matches!(
        classify(&ptv),
        Some(Content::Video { media, .. }) if media.view_once
    ));
}

#[test]
fn the_inline_flag_marks_photos_videos_and_voice_messages() {
    for message in [image(Some(true)), video(Some(true)), voice(Some(true))] {
        assert!(marked(&message));
    }
    for message in [image(Some(false)), video(None), voice(None)] {
        assert!(!marked(&message));
    }
}

#[test]
fn each_kind_keeps_its_own_details_when_marked() {
    assert!(matches!(
        classify(&legacy_v2(video(None))),
        Some(Content::Video { seconds: Some(3), gif: false, media, .. }) if media.view_once
    ));
    assert!(matches!(
        classify(&legacy_v2(voice(None))),
        Some(Content::Audio { seconds: Some(4), voice_note: true, media, .. }) if media.view_once
    ));
}

#[test]
fn ordinary_media_and_text_are_not_marked() {
    assert!(!marked(&image(None)));
    assert!(!marked(&wa_text("synthetic")));
    // A wrapped message that is not media has nothing to mark.
    assert!(!marked(&legacy_v2(wa_text("synthetic"))));
}

#[test]
fn summaries_read_like_ordinary_media() {
    for (plain, once) in [
        (image(None), legacy_v2(image(None))),
        (video(None), legacy_v2(video(None))),
        (voice(None), legacy_v2(voice(None))),
    ] {
        assert_eq!(
            classify(&plain).unwrap().summary(),
            classify(&once).unwrap().summary()
        );
    }
}

#[test]
fn a_live_view_once_photo_is_archived_marked() {
    let (mut worker, _events, _, _) = worker();
    let chat = "123-456@g.us";
    let info = group_message_id_info("111@s.whatsapp.net");
    worker.ingest(&Arc::new(legacy_v2(image(None))), &info);
    let row = stored(&worker, chat, "collision");
    assert!(row.content.view_once());
    assert!(matches!(row.content, Content::Image { .. }));
    // The raw message still carries its wrapper and the download keys.
    assert!(worker.archive.raw(chat, "collision").unwrap().is_some());
}

#[test]
fn a_view_once_message_without_a_body_leaves_a_placeholder() {
    let (mut worker, _events, _, _) = worker();
    let info = group_message_id_info("111@s.whatsapp.net");
    worker.ingest_unavailable(&info, unsupported::VIEW_ONCE);
    let row = stored(&worker, "123-456@g.us", "collision");
    assert_eq!(
        row.content,
        Content::Unsupported {
            what: unsupported::VIEW_ONCE.to_owned()
        }
    );
    assert!(row.summary().contains("View once"));
}

#[test]
fn an_edited_caption_keeps_the_mark() {
    let (mut worker, _events, _, _) = worker();
    let chat = "123-456@g.us";
    let info = group_message_id_info("111@s.whatsapp.net");
    worker.ingest(&Arc::new(legacy_v2(image(None))), &info);

    let mut edit_info = group_message_id_info("111@s.whatsapp.net");
    edit_info.id = "edit".into();
    let edit = from_default!(wa::Message {
        protocol_message: MessageField::some(from_default!(wa::message::ProtocolMessage {
            r#type: Some(wa::message::protocol_message::Type::MESSAGE_EDIT),
            key: MessageField::some(from_default!(wa::MessageKey {
                id: Some("collision".into()),
            })),
            edited_message: MessageField::some(image(None)),
        })),
    });
    worker.ingest(&Arc::new(edit), &edit_info);
    assert!(stored(&worker, chat, "collision").content.view_once());
}

#[test]
fn wrapped_media_still_resolves_its_download_keys() {
    let keyed_image = from_default!(wa::Message {
        image_message: MessageField::some(from_default!(wa::message::ImageMessage {
            media_key: Some(vec![1]),
            view_once: Some(true),
        })),
    });
    let keyed_video = from_default!(wa::Message {
        video_message: MessageField::some(from_default!(wa::message::VideoMessage {
            media_key: Some(vec![2]),
        })),
    });
    let keyed_voice = from_default!(wa::Message {
        audio_message: MessageField::some(from_default!(wa::message::AudioMessage {
            media_key: Some(vec![3]),
        })),
    });
    let key_of = |message: &wa::Message| {
        let base = protocol::base_message(message);
        base.image_message
            .as_option()
            .and_then(|media| media.media_key.clone())
            .or_else(|| {
                base.video_message
                    .as_option()
                    .and_then(|media| media.media_key.clone())
            })
            .or_else(|| {
                base.audio_message
                    .as_option()
                    .and_then(|media| media.media_key.clone())
            })
    };
    assert_eq!(key_of(&legacy_v2(keyed_image)), Some(vec![1]));
    assert_eq!(key_of(&legacy_v1(keyed_video)), Some(vec![2]));
    assert_eq!(key_of(&legacy_v2_extension(keyed_voice)), Some(vec![3]));
}
