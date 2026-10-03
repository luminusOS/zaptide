//! Reusable GTK widgets for message media and decorations.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use relm4::adw::{self, prelude::AdwDialogExt};
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

mod audio;
mod cards;
mod contacts;
mod photo;
mod sticker;
mod widget;

pub use audio::{AudioControls, RecordingMeter};
pub(crate) use contacts::contact_preview;
pub use photo::{build_album_widget, show_in_folder};
pub(crate) use sticker::{StickerAnimation, decode_sticker_file};
pub use widget::build_media_widget_with_action;

use cards::{
    ListChoiceSection, add_formatted_label, add_label, decode_preview_async, link_button,
    link_card, location_card, reply_button, show_list_choices,
};
use photo::{append_photo, append_video, document_card};
use sticker::append_sticker;
use widget::playback_projection;

#[cfg(test)]
mod tests {
    fn gif(frames: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for index in 0..frames {
                let pixel = image::Rgba([(index * 7) as u8, 0, 0, 255]);
                let frame = image::Frame::from_parts(
                    image::RgbaImage::from_pixel(320, 320, pixel),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(50, 1),
                );
                encoder.encode_frame(frame).expect("frame");
            }
        }
        bytes
    }

    #[test]
    fn animated_stickers_keep_their_frames_within_the_cap() {
        let frames = super::sticker::decode_sticker(&gif(3), || true).expect("decoded");
        assert_eq!(frames.len(), 3);
        assert!(
            frames
                .iter()
                .all(|frame| frame.width == super::sticker::STICKER_SIZE)
        );
        assert_eq!(frames[0].delay_ms, 50);

        let frames = super::sticker::decode_sticker(&gif(200), || true).expect("decoded");
        assert!(frames.len() <= super::sticker::MAX_STICKER_FRAMES);
        let total: u32 = frames.iter().map(|frame| frame.delay_ms).sum();
        assert_eq!(total, 200 * 50, "merged frames keep the clip's length");
    }

    #[test]
    fn oversized_or_cancelled_stickers_are_not_decoded() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for _ in 0..2 {
                let frame = image::Frame::new(image::RgbaImage::new(9_000, 1));
                encoder.encode_frame(frame).expect("frame");
            }
        }
        assert!(super::sticker::decode_sticker(&bytes, || true).is_none());
        assert!(super::sticker::decode_sticker(&gif(3), || false).is_none());
    }

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

    /// Fictional contact-card rendering and action checks, no account/backend access.
    #[test]
    #[ignore = "needs a display"]
    fn render_contact_cards() {
        use std::{cell::RefCell, rc::Rc};
        gtk::init().unwrap();
        adw::init().unwrap();
        let dir = PathBuf::from(std::env::var("RENDER_DIR").unwrap());
        let one = crate::contact_cards::ContactCard::from_saved(
            "15555550123@s.whatsapp.net",
            "Ada Example",
        )
        .unwrap();
        let many = format!(
            "{}BEGIN:VCARD\nVERSION:4.0\nFN:Éva Example with a long contact name that should wrap on narrow windows\nTEL;VALUE=uri:tel:+15555550124\nTEL;TYPE=HOME:555-0125\nEND:VCARD\n",
            one.vcard
        );
        let actions = Rc::new(RefCell::new(Vec::new()));
        let context = gtk::glib::MainContext::default();
        for (name, scheme, vcard) in [
            (
                "contact-light",
                adw::ColorScheme::ForceLight,
                one.vcard.clone(),
            ),
            ("contact-dark", adw::ColorScheme::ForceDark, many),
        ] {
            adw::StyleManager::default().set_color_scheme(scheme);
            let captured = actions.clone();
            let rendered = build_media_widget_with_action(
                &message(Content::Contact {
                    display_name: "Fixture contact".into(),
                    vcard,
                }),
                move |action| captured.borrow_mut().push(action),
            );
            let frame = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .margin_top(24)
                .margin_bottom(24)
                .margin_start(24)
                .margin_end(24)
                .build();
            frame.append(&rendered.widget);
            let window = adw::Window::builder()
                // Narrow phone width: card actions must wrap rather than widen the bubble.
                .default_width(300)
                .default_height(500)
                .content(&frame)
                .build();
            window.present();
            let end = std::time::Instant::now() + std::time::Duration::from_millis(400);
            while std::time::Instant::now() < end {
                context.iteration(false);
            }
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                &snapshot,
                f64::from(window.width()),
                f64::from(window.height()),
            );
            let node = snapshot.to_node().unwrap();
            let texture = window
                .native()
                .unwrap()
                .renderer()
                .unwrap()
                .render_texture(&node, None);
            let path = dir.join(format!("{name}.png"));
            std::fs::write(&path, texture.save_to_png_bytes()).unwrap();
            assert!(path.is_file(), "render artifact must exist");
            fn activate(widget: &gtk::Widget) {
                if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                    button.emit_clicked();
                }
                let mut child = widget.first_child();
                while let Some(widget) = child {
                    activate(&widget);
                    child = widget.next_sibling();
                }
            }
            activate(rendered.widget.upcast_ref());
            window.close();
        }
        let actions = actions.borrow();
        assert!(actions.iter().any(|action| matches!(action, NativeMediaAction::CopyContactPhone(number) if number == "+15555550123")));
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, NativeMediaAction::MessageContact { .. }))
                .count(),
            2,
            "Only Ada's explicit WhatsApp ID gets a Message action, once per rendering"
        );
        assert!(actions.iter().any(|action| matches!(action, NativeMediaAction::OpenContact(card) if card.name == "Ada Example")));
    }

    /// Renders an album of seven photos and a store template to PNGs under
    /// `$RENDER_DIR`. Run under a display:
    /// `RENDER_DIR=/tmp/zaptide xvfb-run -a cargo test render_album -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a display"]
    fn render_album_and_template() {
        gtk::init().expect("display");
        adw::init().expect("libadwaita");
        let dir = PathBuf::from(std::env::var("RENDER_DIR").expect("RENDER_DIR"));
        std::fs::create_dir_all(&dir).unwrap();
        let photos = |count: usize| -> Vec<Message> {
            (0..count)
                .map(|index| {
                    let mut photo = message(Content::Image {
                        caption: None,
                        media: media(None, MediaState::Idle),
                    });
                    photo.id = format!("photo-{index}");
                    photo
                })
                .collect()
        };
        let template = message(Content::Template {
            text: "Hello, *Customer*! Your *order* is scheduled for _01/01_.\n\
                   - ~cancelada~ não\n> citação"
                .into(),
            footer: Some("Store".into()),
            links: vec![crate::model::TemplateLink {
                label: "Assembly status".into(),
                url: "https://example.com/status".into(),
            }],
        });
        let widgets = [
            ("album-7", build_album_widget(&photos(7), |_| {}).widget),
            ("album-4", build_album_widget(&photos(4), |_| {}).widget),
            (
                "template",
                build_media_widget_with_action(&template, |_| {}).widget,
            ),
        ];
        let context = gtk::glib::MainContext::default();
        for (name, widget) in widgets {
            let window = gtk::Window::builder()
                .default_width(340)
                .default_height(360)
                .child(&widget)
                .build();
            window.present();
            let end = std::time::Instant::now() + std::time::Duration::from_millis(400);
            while std::time::Instant::now() < end {
                context.iteration(false);
            }
            let paintable = gtk::WidgetPaintable::new(Some(&window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(&snapshot, 340.0, 360.0);
            let node = snapshot.to_node().expect("rendered node");
            let renderer = window.native().and_then(|native| native.renderer());
            let texture = renderer.expect("renderer").render_texture(&node, None);
            texture
                .save_to_png(dir.join(format!("{name}.png")))
                .unwrap();
        }
    }

    #[test]
    fn photos_keep_their_shape_within_the_frame() {
        assert_eq!(super::photo::photo_size(Some(4000), Some(3000)), (300, 225));
        assert_eq!(super::photo::photo_size(Some(1080), Some(1920)), (169, 300));
        assert_eq!(super::photo::photo_size(Some(3000), Some(100)), (300, 100));
        assert_eq!(super::photo::photo_size(None, Some(10)), (300, 300));
    }

    #[test]
    fn attachment_activation_returns_only_typed_safe_actions() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let ready = message(Content::Image {
            media: media(Some(file.path().to_str().unwrap()), MediaState::Idle),
            caption: None,
        });
        assert!(matches!(
            attachment_action(&ready),
            Some(NativeMediaAction::Open(_))
        ));

        let removed = message(Content::Image {
            media: media(Some("/nonexistent/zaptide/photo.jpg"), MediaState::Idle),
            caption: None,
        });
        assert!(matches!(
            attachment_action(&removed),
            Some(NativeMediaAction::Download { .. })
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
