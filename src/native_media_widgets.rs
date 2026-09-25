//! Reusable GTK widgets for message media and decorations.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use relm4::gtk::{self, gdk, glib, prelude::*};

use crate::{
    model::{Content, Message},
    native_media::{
        DecodeToken, NativeMediaAction, NativeMediaContent, ThumbnailResult, attachment_action,
        project_content,
    },
};

const MAX_ACTIVE_THUMBNAIL_DECODES: usize = 2;
static ACTIVE_THUMBNAIL_DECODES: AtomicUsize = AtomicUsize::new(0);

struct ThumbnailDecodePermit;

impl ThumbnailDecodePermit {
    fn acquire() -> Option<Self> {
        ACTIVE_THUMBNAIL_DECODES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_ACTIVE_THUMBNAIL_DECODES).then_some(active + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for ThumbnailDecodePermit {
    fn drop(&mut self) {
        ACTIVE_THUMBNAIL_DECODES.fetch_sub(1, Ordering::AcqRel);
    }
}

/// GTK media row. Attachment actions are forwarded by button callbacks.
pub struct NativeMediaWidget {
    pub widget: gtk::Box,
    pub decode_token: DecodeToken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlaybackProjection {
    Native { looping: bool },
    Fallback,
    None,
}

/// Build message media widget and forward attachment actions to its owner.
pub fn build_media_widget_with_action(
    message: &Message,
    on_action: impl Fn(NativeMediaAction) + 'static,
) -> NativeMediaWidget {
    let projected = project_content(message);
    let decode_token = DecodeToken::default();
    // The message row draws sender, quote, body, reactions, and forwarding;
    // this widget only adds what the content itself needs.
    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    if let Content::Text { preview, .. } = &message.content {
        if let Some(preview) = preview {
            root.add_css_class("card");
            root.add_css_class("zaptide-media-card");
            add_label(&root, preview.title.as_deref().unwrap_or(&preview.url));
            if let Some(description) = &preview.description {
                add_label(&root, description);
            }
        }
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if let Content::Sticker { media, .. } = &message.content {
        append_sticker(&root, media.path.clone(), &decode_token);
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    root.add_css_class("card");
    root.add_css_class("zaptide-media-card");

    let (title, detail) = match &projected.content {
        NativeMediaContent::Document { file_name, detail } => {
            add_label(&root, "Document");
            (file_name.as_str(), detail.as_str())
        }
        NativeMediaContent::Contact { display_name } => ("Contact", display_name.as_str()),
        NativeMediaContent::Location { label } => ("Location", label.as_str()),
        NativeMediaContent::Poll { question, options } => {
            add_label(&root, question);
            for option in options {
                let selected = if option.selected { " · selected" } else { "" };
                add_label(
                    &root,
                    &format!("{} · {} votes{selected}", option.text, option.votes),
                );
            }
            ("Poll", "")
        }
        NativeMediaContent::VideoPlaceholder => ("Video", "Video preview unavailable"),
        NativeMediaContent::UnsupportedPlaceholder => (
            "Unsupported message",
            "This message type cannot be displayed",
        ),
        NativeMediaContent::Other => match &message.content {
            Content::Image { .. } if message.thumbnail.is_some() => ("", ""),
            Content::Image { .. } => ("Photo", "Image attachment"),
            Content::Audio { .. } => ("Audio", "Audio attachment"),
            Content::Revoked => ("Deleted message", "This message was deleted"),
            Content::Text { text, .. } => ("Message", text.as_str()),
            _ => ("Message", ""),
        },
    };
    if !title.is_empty() {
        add_label(&root, title);
    }
    if !detail.is_empty() {
        add_label(&root, detail);
    }

    match playback_projection(message) {
        PlaybackProjection::Native { looping } => {
            if let Some(path) = message
                .content
                .media()
                .and_then(|media| media.path.as_ref())
            {
                append_native_playback(&root, path, looping);
            }
        }
        PlaybackProjection::Fallback => {
            add_label(
                &root,
                "Preview unavailable for this media format. Open attachment to view it.",
            );
        }
        PlaybackProjection::None => {}
    }

    if matches!(&message.content, Content::Image { .. }) && message.thumbnail.is_some() {
        let preview = gtk::Image::new();
        preview.set_pixel_size(256);
        preview.set_halign(gtk::Align::Start);
        preview.set_tooltip_text(Some("Image preview"));
        // An empty gtk::Image still reserves its pixel size, stretching the
        // row with a blank card until the thumbnail decodes, if it ever does.
        preview.set_visible(false);
        root.append(&preview);
        if let Some(bytes) = message.thumbnail.clone() {
            decode_preview_async(&preview, move || Some(bytes), &decode_token);
        }
    }

    if let Some(action) = attachment_action(message) {
        let button = gtk::Button::with_label(attachment_button_label(message));
        button.set_tooltip_text(Some("Open or download attachment"));
        button.connect_clicked(move |_| {
            on_action(action.clone());
        });
        root.append(&button);
    }

    NativeMediaWidget {
        widget: root,
        decode_token,
    }
}

/// Stickers draw without a card, as on the phone. Until the file downloads,
/// a dim label keeps the row from collapsing.
fn append_sticker(parent: &gtk::Box, path: Option<std::path::PathBuf>, token: &DecodeToken) {
    let Some(path) = path else {
        let label = gtk::Label::new(Some("Sticker"));
        label.set_xalign(0.0);
        label.add_css_class("dim-label");
        parent.append(&label);
        return;
    };
    let sticker = gtk::Image::new();
    sticker.set_pixel_size(160);
    sticker.set_halign(gtk::Align::Start);
    sticker.set_tooltip_text(Some("Sticker"));
    sticker.set_visible(false);
    parent.append(&sticker);
    decode_preview_async(
        &sticker,
        move || {
            let size = std::fs::metadata(&path).ok()?.len();
            (size <= crate::native_media::MAX_THUMBNAIL_INPUT_BYTES as u64)
                .then(|| std::fs::read(&path).ok())
                .flatten()
        },
        token,
    );
}

fn add_label(parent: &gtk::Box, text: &str) {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_max_width_chars(52);
    parent.append(&label);
}

/// Choose only known local formats for GTK/GStreamer playback. The media
/// stream reports missing plugins and unsupported codecs after opening, which
/// switches presentation to the explicit open-attachment fallback.
fn playback_projection(message: &Message) -> PlaybackProjection {
    let Some(media) = message.content.media() else {
        return PlaybackProjection::None;
    };
    let Some(path) = media.path.as_ref() else {
        return match &message.content {
            Content::Video { .. } => PlaybackProjection::Fallback,
            _ => PlaybackProjection::None,
        };
    };

    let looping = match &message.content {
        Content::Video { gif, .. } => *gif,
        _ => return PlaybackProjection::None,
    };
    if !path.is_file() {
        return PlaybackProjection::Fallback;
    }

    let mime = media.mime.to_ascii_lowercase();
    let supported = matches!(mime.as_str(), "video/mp4" | "video/webm" | "video/ogg")
        || (looping && mime == "image/gif");
    if supported {
        PlaybackProjection::Native { looping }
    } else {
        PlaybackProjection::Fallback
    }
}

fn append_native_playback(parent: &gtk::Box, path: &std::path::Path, looping: bool) {
    let video = gtk::Video::for_filename(Some(path));
    video.set_autoplay(false);
    video.set_loop(looping);
    video.set_size_request(320, 180);
    video.set_tooltip_text(Some("Media preview"));
    if let Some(stream) = video.media_stream() {
        stream.set_muted(true);
    }

    let fallback = gtk::Label::new(Some(
        "This media format cannot be played here. Open attachment to view it.",
    ));
    fallback.set_xalign(0.0);
    fallback.set_wrap(true);
    fallback.set_visible(false);
    let fallback_weak = fallback.downgrade();
    let reveal_fallback = move || {
        if let Some(label) = fallback_weak.upgrade() {
            label.set_visible(true);
        }
    };
    if let Some(stream) = video.media_stream() {
        stream.connect_error_notify(move |stream| {
            if stream.error().is_some() {
                reveal_fallback();
            }
        });
        if stream.error().is_some() {
            fallback.set_visible(true);
        }
    }
    parent.append(&video);
    parent.append(&fallback);

    let control = gtk::Button::with_label("Play preview");
    control.set_tooltip_text(Some("Play or pause media preview"));
    let video_weak = video.downgrade();
    control.connect_clicked(move |button| {
        let Some(video) = video_weak.upgrade() else {
            return;
        };
        let Some(stream) = video.media_stream() else {
            return;
        };
        let playing = !stream.is_playing();
        stream.set_playing(playing);
        button.set_label(if playing {
            "Pause preview"
        } else {
            "Play preview"
        });
    });
    parent.append(&control);
}

fn attachment_button_label(message: &Message) -> &'static str {
    match &message.content {
        Content::Image { media, .. }
        | Content::Video { media, .. }
        | Content::Audio { media, .. }
        | Content::Document { media, .. } => {
            if media.path.is_some() {
                "Open attachment"
            } else {
                "Download attachment"
            }
        }
        _ => "Open attachment",
    }
}

fn decode_preview_async(
    image: &gtk::Image,
    load: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    token: &DecodeToken,
) {
    let Some(permit) = ThumbnailDecodePermit::acquire() else {
        image.set_tooltip_text(Some(
            "Too many previews are decoding; open attachment to view",
        ));
        return;
    };
    let ticket = token.issue();
    let image = glib::SendWeakRef::from(image.downgrade());
    let main_context = glib::MainContext::default();
    thread::Builder::new()
        .name("zaptide-thumbnail".into())
        .spawn(move || {
            let _permit = permit;
            let decoded = load().and_then(|bytes| ticket.decode_thumbnail(&bytes));
            main_context.invoke(move || {
                let Some(image) = image.upgrade() else {
                    return;
                };
                match decoded {
                    Some(ThumbnailResult::Image(thumbnail)) => {
                        let pixels = glib::Bytes::from_owned(thumbnail.rgba);
                        let texture = gdk::MemoryTexture::new(
                            thumbnail.width as i32,
                            thumbnail.height as i32,
                            gdk::MemoryFormat::R8g8b8a8,
                            &pixels,
                            thumbnail.width as usize * 4,
                        );
                        image.set_paintable(Some(&texture));
                        image.set_visible(true);
                    }
                    Some(ThumbnailResult::Placeholder(_)) | None => {
                        image.set_tooltip_text(Some("Image preview unavailable; open attachment"));
                        image.set_visible(false);
                    }
                }
            });
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Delivery, Media, MediaState, Reaction};

    fn message(content: Content) -> Message {
        Message {
            id: "message-id".into(),
            chat: "chat-id".into(),
            sender: "sender-id".into(),
            sender_name: None,
            from_me: false,
            timestamp: 0,
            content,
            status: Delivery::None,
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

    fn media(path: Option<&str>, state: MediaState) -> Media {
        Media {
            mime: "image/jpeg".into(),
            size: 12,
            width: Some(2),
            height: Some(2),
            path: path.map(PathBuf::from),
            state,
        }
    }

    #[test]
    fn widget_projection_covers_safe_content_categories() {
        let examples = [
            message(Content::Video {
                media: media(None, MediaState::Idle),
                seconds: None,
                gif: false,
                caption: None,
            }),
            message(Content::Sticker {
                media: media(None, MediaState::Idle),
                animated: true,
            }),
            message(Content::Contact {
                display_name: "Contact name".into(),
                vcard: "private vcard".into(),
            }),
            message(Content::Location {
                latitude: 1.0,
                longitude: 2.0,
                name: Some("Place".into()),
                address: None,
            }),
            message(Content::Poll {
                question: "Question".into(),
                options: vec!["Option".into()],
                state: Default::default(),
            }),
            message(Content::Unsupported {
                what: "private protocol data".into(),
            }),
        ];
        for message in examples {
            let projection = project_content(&message);
            assert!(!format!("{projection:?}").contains("private"));
        }
    }

    #[test]
    fn attachment_activation_returns_only_typed_safe_actions() {
        let ready = message(Content::Image {
            media: media(Some("/tmp/photo.jpg"), MediaState::Idle),
            caption: None,
        });
        assert!(matches!(
            attachment_action(&ready),
            Some(NativeMediaAction::Open(_))
        ));

        let downloading = message(Content::Image {
            media: media(None, MediaState::Downloading),
            caption: None,
        });
        assert_eq!(attachment_action(&downloading), None);

        let idle = message(Content::Image {
            media: media(None, MediaState::Idle),
            caption: None,
        });
        assert!(matches!(
            attachment_action(&idle),
            Some(NativeMediaAction::Download { .. })
        ));
    }

    #[test]
    fn playback_projection_selects_controls_only_for_local_supported_media() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut playable = media(Some(file.path().to_str().unwrap()), MediaState::Idle);
        playable.mime = "video/mp4".into();
        let video = message(Content::Video {
            media: playable,
            seconds: Some(3),
            gif: false,
            caption: None,
        });
        assert_eq!(
            playback_projection(&video),
            PlaybackProjection::Native { looping: false }
        );

        let mut gif_media = media(Some(file.path().to_str().unwrap()), MediaState::Idle);
        gif_media.mime = "image/gif".into();
        let gif = message(Content::Video {
            media: gif_media,
            seconds: None,
            gif: true,
            caption: None,
        });
        assert_eq!(
            playback_projection(&gif),
            PlaybackProjection::Native { looping: true }
        );

        let mut unsupported = media(Some(file.path().to_str().unwrap()), MediaState::Idle);
        unsupported.mime = "video/x-unsupported".into();
        let video = message(Content::Video {
            media: unsupported,
            seconds: None,
            gif: false,
            caption: None,
        });
        assert_eq!(playback_projection(&video), PlaybackProjection::Fallback);
    }

    #[test]
    fn videos_without_a_local_file_fall_back_and_stickers_never_play_as_video() {
        let video = message(Content::Video {
            media: media(None, MediaState::Idle),
            seconds: None,
            gif: true,
            caption: None,
        });
        assert_eq!(playback_projection(&video), PlaybackProjection::Fallback);

        let sticker = message(Content::Sticker {
            media: media(None, MediaState::Idle),
            animated: true,
        });
        assert_eq!(playback_projection(&sticker), PlaybackProjection::None);
    }

    #[test]
    fn reactions_are_projected_with_count_and_selection() {
        let mut message = message(Content::text("hello"));
        message.reactions = vec![
            Reaction {
                sender: "a".into(),
                from_me: false,
                emoji: "👍".into(),
            },
            Reaction {
                sender: "b".into(),
                from_me: true,
                emoji: "👍".into(),
            },
        ];
        let projected = project_content(&message);
        assert_eq!(projected.reactions[0].count, 2);
        assert!(projected.reactions[0].selected);
    }
}
