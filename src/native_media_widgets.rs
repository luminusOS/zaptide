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
            root.append(&link_card(
                preview,
                message.thumbnail.clone(),
                &decode_token,
            ));
        }
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if let Content::Location {
        latitude,
        longitude,
        name,
        address,
    } = &message.content
    {
        root.append(&location_card(
            *latitude,
            *longitude,
            name.as_deref(),
            address.as_deref(),
            message.thumbnail.clone(),
            &decode_token,
        ));
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
    if let Content::Video {
        media,
        seconds,
        gif,
        ..
    } = &message.content
    {
        append_video(
            &root,
            message,
            media,
            *seconds,
            *gif,
            std::rc::Rc::new(on_action),
        );
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if let Content::Document { media, .. } = &message.content {
        root.append(&document_card(message, media, std::rc::Rc::new(on_action)));
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if let Content::Image { media, .. } = &message.content {
        append_photo(
            &root,
            message,
            media,
            &decode_token,
            std::rc::Rc::new(on_action),
        );
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    if matches!(&message.content, Content::Audio { .. })
        && (!message.from_me
            || matches!(
                &message.content,
                Content::Audio {
                    voice_note: true,
                    ..
                }
            ))
    {
        return NativeMediaWidget {
            widget: root,
            decode_token,
        };
    }
    let on_action = std::rc::Rc::new(on_action);
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
        NativeMediaContent::Buttons {
            text,
            footer,
            labels,
            answered,
        } => {
            if !text.is_empty() {
                add_label(&root, text);
            }
            if let Some(footer) = footer {
                let label = add_label(&root, footer);
                label.add_css_class("dim-label");
                label.add_css_class("caption");
            }
            let ids: Vec<&str> = match &message.content {
                Content::Buttons { buttons, .. } => {
                    buttons.iter().map(|button| button.id.as_str()).collect()
                }
                _ => Vec::new(),
            };
            // Only messages from others can be answered, and only once.
            let open = message.content.answer().is_none() && !message.from_me;
            for (index, label) in labels.iter().enumerate() {
                let chosen = *answered == Some(index);
                // A text marker as well as the style: an insensitive
                // accent button is faint, and styles are not announced.
                let button = reply_button(&if chosen {
                    format!("✓ {label}")
                } else {
                    label.clone()
                });
                button.set_sensitive(open && ids.get(index).is_some());
                if chosen {
                    button.add_css_class("suggested-action");
                    button.update_property(&[gtk::accessible::Property::Description(
                        "Selected reply",
                    )]);
                } else if !open {
                    let reason = if message.from_me {
                        "Only the recipient can choose from these buttons"
                    } else {
                        "Already answered"
                    };
                    button.set_tooltip_text(Some(reason));
                    button.update_property(&[gtk::accessible::Property::Description(reason)]);
                }
                if let (true, Some(id)) = (open, ids.get(index)) {
                    let (chat, message_id, button_id) =
                        (message.chat.clone(), message.id.clone(), (*id).to_owned());
                    let on_action = on_action.clone();
                    // The button stays enabled until the stored state changes:
                    // the worker ignores a second click, and a failed one
                    // leaves the row exactly as it was.
                    button.connect_clicked(move |_| {
                        on_action(NativeMediaAction::AnswerButton {
                            chat: chat.clone(),
                            message: message_id.clone(),
                            button: button_id.clone(),
                        });
                    });
                }
                root.append(&button);
            }
            ("", "")
        }
        NativeMediaContent::List {
            title,
            description,
            button,
            footer,
            sections,
            answered,
        } => {
            if !title.is_empty() {
                add_label(&root, title).add_css_class("heading");
            }
            if let Some(description) = description {
                add_label(&root, description);
            }
            // Row ids come from the message itself, in the projection's order.
            let ids: Vec<&str> = match &message.content {
                Content::List { sections, .. } => sections
                    .iter()
                    .flat_map(|section| &section.rows)
                    .map(|row| row.id.as_str())
                    .collect(),
                _ => Vec::new(),
            };
            let chosen_id = match &message.content {
                Content::List { answered, .. } => answered.as_deref(),
                _ => None,
            };
            let mut index = 0;
            for section in sections {
                if let Some(heading) = &section.title {
                    let label = add_label(&root, heading);
                    label.add_css_class("caption-heading");
                }
                for (row, detail) in &section.rows {
                    let mark = if chosen_id.is_some() && ids.get(index).copied() == chosen_id {
                        "✓"
                    } else {
                        "•"
                    };
                    index += 1;
                    add_label(&root, &format!("{mark} {row}"));
                    if let Some(detail) = detail {
                        let label = add_label(&root, detail);
                        label.set_margin_start(12);
                        label.add_css_class("dim-label");
                        label.add_css_class("caption");
                    }
                }
            }
            if let Some(footer) = footer {
                let label = add_label(&root, footer);
                label.add_css_class("dim-label");
                label.add_css_class("caption");
            }
            let label = if button.is_empty() { "Choose" } else { button };
            // From the message itself: the projection's `answered` is `None`
            // for an answer naming a row that no longer exists.
            let open = message.content.answer().is_none() && !message.from_me;
            let picker = reply_button(label);
            picker.set_sensitive(open);
            if open {
                let choices: Vec<ListChoiceSection> = match &message.content {
                    Content::List { sections, .. } => sections
                        .iter()
                        .map(|section| {
                            (
                                section.title.clone(),
                                section
                                    .rows
                                    .iter()
                                    .map(|row| {
                                        (row.id.clone(), row.title.clone(), row.description.clone())
                                    })
                                    .collect(),
                            )
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let (chat, message_id) = (message.chat.clone(), message.id.clone());
                let (heading, on_action) = (title.clone(), on_action.clone());
                picker.connect_clicked(move |button| {
                    let (chat, message_id, on_action) =
                        (chat.clone(), message_id.clone(), on_action.clone());
                    show_list_choices(button, &heading, &choices, move |row| {
                        on_action(NativeMediaAction::AnswerListRow {
                            chat: chat.clone(),
                            message: message_id.clone(),
                            row,
                        });
                    });
                });
            } else {
                let reason = match answered {
                    Some(chosen) => format!("Already answered: {chosen}"),
                    None if message.from_me => {
                        "Only the recipient can choose from this list".into()
                    }
                    None => "This list can no longer be answered".into(),
                };
                picker.set_tooltip_text(Some(&reason));
                picker.update_property(&[gtk::accessible::Property::Description(&reason)]);
            }
            root.append(&picker);
            ("", "")
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

/// Draws voice-message bars (0-100) across the area; with `progress`, the played
/// part is brighter and a dot marks the position. Shared by playback and recording.
fn draw_waveform(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    bars: &[u8],
    progress: Option<f32>,
) {
    let n = bars.len();
    if n == 0 || width <= 0 {
        return;
    }
    let color = area.color();
    // Leave room for the progress dot at both ends.
    let dot = 5.0;
    let bar_width = (width as f64 - 2.0 * dot) / n as f64;
    let played = progress.map_or(1.0, |p| p.clamp(0.0, 1.0));
    for (index, value) in bars.iter().enumerate() {
        let h = (f64::from(*value.min(&100)) / 100.0 * (height - 4) as f64).max(3.0);
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            if (index as f32) < played * n as f32 {
                0.9
            } else {
                0.35
            },
        );
        cr.rectangle(
            dot + index as f64 * bar_width,
            (height as f64 - h) / 2.0,
            (bar_width - 2.0).max(1.0),
            h,
        );
        let _ = cr.fill();
    }
    if progress.is_some() {
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            1.0,
        );
        cr.arc(
            dot + f64::from(played) * (width as f64 - 2.0 * dot),
            height as f64 / 2.0,
            dot,
            0.0,
            std::f64::consts::TAU,
        );
        let _ = cr.fill();
    }
}

/// Live microphone meter: the most recent loudness readings, drawn like a
/// voice-message waveform so both look the same.
#[derive(Clone)]
pub struct RecordingMeter {
    pub area: gtk::DrawingArea,
    bars: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
}

impl RecordingMeter {
    pub fn new() -> Self {
        let bars = std::rc::Rc::new(std::cell::RefCell::new(vec![0; crate::voice::BARS]));
        let area = gtk::DrawingArea::builder()
            .content_width(160)
            .content_height(32)
            .hexpand(true)
            .build();
        area.update_property(&[gtk::accessible::Property::Label("Microphone level")]);
        let paint = bars.clone();
        area.set_draw_func(move |area, cr, width, height| {
            draw_waveform(area, cr, width, height, &paint.borrow(), None);
        });
        Self { area, bars }
    }

    /// Shows the latest readings, entering from the right of the message-sized row.
    pub fn set_levels(&self, levels: &[f32]) {
        let recent = &levels[levels.len().saturating_sub(crate::voice::BARS)..];
        let peak = recent
            .iter()
            .copied()
            .filter(|level| level.is_finite())
            .fold(0.02_f32, f32::max);
        let mut bars = vec![0; crate::voice::BARS - recent.len()];
        bars.extend(recent.iter().map(|level| {
            if level.is_finite() {
                ((level / peak).clamp(0.0, 1.0) * 100.0).round() as u8
            } else {
                0
            }
        }));
        *self.bars.borrow_mut() = bars;
        self.area.queue_draw();
    }
}

impl Default for RecordingMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// Row-local GTK controls. Updating these avoids stealing focus during playback.
#[derive(Clone)]
pub struct AudioControls {
    pub widget: gtk::Box,
    button: gtk::Button,
    seek: gtk::Scale,
    time: gtk::Label,
    speed: gtk::Button,
    error: gtk::Label,
    waveform: gtk::DrawingArea,
    bars: std::rc::Rc<std::cell::RefCell<(Vec<u8>, f32)>>,
}

impl AudioControls {
    pub fn new(
        voice: &crate::native_voice::VoiceMessage,
        voice_note: bool,
        on_action: impl Fn(crate::native_voice::VoiceIntent) + Clone + 'static,
    ) -> Self {
        let widget = gtk::Box::builder().spacing(8).build();
        widget.add_css_class("zaptide-audio-controls");
        let button = gtk::Button::builder()
            .css_classes(["circular", "flat"])
            .build();
        let action = on_action.clone();
        button.connect_clicked(move |_| action(crate::native_voice::VoiceIntent::Activate));
        widget.append(&button);

        let overlay = gtk::Overlay::new();
        overlay.set_hexpand(true);
        let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        seek.set_draw_value(false);
        // The waveform shows progress; the scale only takes clicks and drags.
        seek.add_css_class("zaptide-audio-seek");
        seek.update_property(&[gtk::accessible::Property::Label("Seek audio")]);
        let action = on_action.clone();
        seek.connect_change_value(move |_, _, value| {
            action(crate::native_voice::VoiceIntent::Seek(
                value.clamp(0.0, 1.0) as f32,
            ));
            gtk::glib::Propagation::Stop
        });

        let bars = std::rc::Rc::new(std::cell::RefCell::new((Vec::<u8>::new(), 0.0_f32)));
        let waveform = gtk::DrawingArea::builder()
            .content_width(160)
            .content_height(32)
            .build();
        waveform.set_hexpand(true);
        let paint = bars.clone();
        waveform.set_draw_func(move |area, cr, width, height| {
            let values = paint.borrow();
            draw_waveform(area, cr, width, height, &values.0, Some(values.1));
        });
        overlay.set_child(Some(&waveform));
        overlay.add_overlay(&seek);
        widget.append(&overlay);
        let time = gtk::Label::builder()
            .css_classes(["numeric", "dim-label"])
            .build();
        widget.append(&time);
        let speed = gtk::Button::builder()
            .css_classes(["flat", "zaptide-audio-speed"])
            .width_request(58)
            .build();
        speed.set_visible(voice_note);
        let action = on_action;
        speed.connect_clicked(move |_| action(crate::native_voice::VoiceIntent::CycleSpeed));
        widget.append(&speed);
        let error = gtk::Label::builder()
            .css_classes(["error", "caption"])
            .wrap(true)
            .visible(false)
            .build();
        // Keep error alongside the controls instead of relegating it to a toast.
        let container = gtk::Box::new(gtk::Orientation::Vertical, 2);
        container.append(&widget);
        container.append(&error);
        let controls = Self {
            widget: container,
            button,
            seek,
            time,
            speed,
            error,
            waveform,
            bars,
        };
        controls.update(voice);
        controls
    }

    pub fn update(&self, voice: &crate::native_voice::VoiceMessage) {
        use crate::native_voice::VoiceControl;
        let (icon, label) = match voice.control {
            VoiceControl::Download => ("folder-download-symbolic", "Download audio"),
            VoiceControl::Loading => ("content-loading-symbolic", "Loading audio"),
            VoiceControl::Play => ("media-playback-start-symbolic", "Play audio"),
            VoiceControl::Pause => ("media-playback-pause-symbolic", "Pause audio"),
        };
        self.button.set_icon_name(icon);
        self.button.set_tooltip_text(Some(label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.button
            .set_sensitive(voice.control != VoiceControl::Loading);
        self.seek
            .set_sensitive(voice.speed.is_some() && voice.control != VoiceControl::Loading);
        self.seek.set_value(f64::from(voice.progress));
        self.time.set_label(&voice.time);
        let speed_label = voice.speed.as_deref().unwrap_or("1x");
        self.speed.set_label(speed_label);
        if speed_label == "1.5x" {
            self.speed.add_css_class("compact");
        } else {
            self.speed.remove_css_class("compact");
        }
        *self.bars.borrow_mut() = (voice.waveform.clone(), voice.progress);
        self.waveform.queue_draw();
        if let Some(error) = &voice.error {
            self.button.set_tooltip_text(Some(error));
        }
        self.error
            .set_label(voice.error.as_deref().unwrap_or_default());
        self.error.set_visible(voice.error.is_some());
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
    // Keep the space reserved while decoding so the transcript does not jump.
    sticker.set_tooltip_text(Some("Sticker"));
    parent.append(&sticker);
    start_sticker_decode(sticker.downgrade(), path, token.issue());
}

/// One decoded sticker frame, scaled for the transcript.
pub(crate) struct StickerFrame {
    texture_rgba: Vec<u8>,
    width: u32,
    height: u32,
    delay_ms: u32,
}

const STICKER_SIZE: u32 = 160;
// ponytail: frames kept in memory per visible sticker (~100 KB each);
// decimated past this cap. Stream frames from disk if long stickers matter.
const MAX_STICKER_FRAMES: usize = 64;

/// Frames read from one sticker, merged or not; later frames are dropped.
const MAX_STICKER_DECODED_FRAMES: usize = 1_024;

/// Decodes every frame of an animated WebP or GIF sticker, or its single
/// frame, keeping at most `MAX_STICKER_FRAMES` by merging neighbours. Stops
/// early when `current` turns false, as the row was recycled.
fn decode_sticker(bytes: &[u8], current: impl Fn() -> bool) -> Option<Vec<StickerFrame>> {
    use crate::native_media::{MAX_THUMBNAIL_DIMENSION, MAX_THUMBNAIL_PIXELS, image_limits};
    use image::{AnimationDecoder, ImageDecoder};
    let fits = |(width, height): (u32, u32)| {
        width > 0
            && height > 0
            && width <= MAX_THUMBNAIL_DIMENSION
            && height <= MAX_THUMBNAIL_DIMENSION
            && u64::from(width) * u64::from(height) <= MAX_THUMBNAIL_PIXELS
    };
    let cursor = std::io::Cursor::new(bytes);
    let frames: Box<dyn Iterator<Item = image::ImageResult<image::Frame>>> =
        match image::codecs::webp::WebPDecoder::new(cursor.clone()) {
            Ok(mut decoder) if decoder.has_animation() => {
                decoder.set_limits(image_limits()).ok()?;
                fits(decoder.dimensions()).then_some(())?;
                Box::new(decoder.into_frames())
            }
            _ if bytes.starts_with(b"GIF8") => {
                let mut decoder = image::codecs::gif::GifDecoder::new(cursor).ok()?;
                decoder.set_limits(image_limits()).ok()?;
                fits(decoder.dimensions()).then_some(())?;
                Box::new(decoder.into_frames())
            }
            _ => {
                let mut reader = image::ImageReader::new(cursor).with_guessed_format().ok()?;
                reader.limits(image_limits());
                let image = reader.decode().ok()?;
                Box::new(std::iter::once(Ok(image::Frame::new(image.to_rgba8()))))
            }
        };
    let mut kept: Vec<StickerFrame> = Vec::new();
    let mut step = 1;
    for (index, frame) in frames.take(MAX_STICKER_DECODED_FRAMES).enumerate() {
        if !current() {
            return None;
        }
        // A corrupt frame ends the animation; the frames before it still play.
        let Ok(frame) = frame else { break };
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        // Browsers treat delays under 20 ms as 100 ms; do the same before
        // merging, so merged frames keep the real duration.
        let delay_ms = match numerator / denominator.max(1) {
            delay if delay < 20 => 100,
            delay => delay,
        };
        if index % step != 0 {
            if let Some(last) = kept.last_mut() {
                last.delay_ms += delay_ms;
            }
            continue;
        }
        let image = image::DynamicImage::ImageRgba8(frame.into_buffer())
            .thumbnail(STICKER_SIZE, STICKER_SIZE)
            .to_rgba8();
        kept.push(StickerFrame {
            width: image.width(),
            height: image.height(),
            texture_rgba: image.into_raw(),
            delay_ms,
        });
        if kept.len() > MAX_STICKER_FRAMES {
            let mut pairs = std::mem::take(&mut kept).into_iter();
            while let Some(mut first) = pairs.next() {
                if let Some(second) = pairs.next() {
                    first.delay_ms += second.delay_ms;
                }
                kept.push(first);
            }
            step *= 2;
        }
    }
    (!kept.is_empty()).then_some(kept)
}

fn start_sticker_decode(
    image: glib::WeakRef<gtk::Image>,
    path: std::path::PathBuf,
    ticket: crate::native_media::DecodeTicket,
) {
    if !ticket.is_current() || image.upgrade().is_none() {
        return;
    }
    let Some(permit) = ThumbnailDecodePermit::acquire() else {
        glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
            start_sticker_decode(image, path, ticket);
        });
        return;
    };
    let image = glib::SendWeakRef::from(image);
    let main_context = glib::MainContext::default();
    thread::Builder::new()
        .name("zaptide-sticker".into())
        .spawn(move || {
            let _permit = permit;
            let frames = decode_sticker_file(&path, || ticket.is_current());
            main_context.invoke(move || {
                let Some(image) = image.upgrade() else {
                    return;
                };
                if !ticket.is_current() {
                    return;
                }
                let Some(frames) = frames else {
                    // Same fallback as a sticker without a file.
                    image.set_visible(false);
                    if let Some(parent) = image.parent().and_downcast::<gtk::Box>() {
                        let label = gtk::Label::new(Some("Sticker"));
                        label.set_xalign(0.0);
                        label.add_css_class("dim-label");
                        parent.append(&label);
                    }
                    return;
                };
                let animation = StickerAnimation::new(&image, frames);
                if animation.is_animated() {
                    animation.play(Some(STICKER_PLAY_LIMIT));
                    // Pointing at a sticker that has stopped plays it again.
                    let hover = gtk::EventControllerMotion::new();
                    hover.connect_enter(move |_, _, _| animation.play(Some(STICKER_PLAY_LIMIT)));
                    image.add_controller(hover);
                }
            });
        })
        .ok();
}

/// How long a sticker in the conversation animates before it rests on its
/// first frame; it finishes the loop it is in first.
const STICKER_PLAY_LIMIT: std::time::Duration = std::time::Duration::from_secs(8);

/// Decodes a sticker file for animation, or `None` when it is unreadable,
/// too large, or `current` turns false first.
pub(crate) fn decode_sticker_file(
    path: &std::path::Path,
    current: impl Fn() -> bool,
) -> Option<Vec<StickerFrame>> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > crate::native_media::MAX_THUMBNAIL_INPUT_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    decode_sticker(&bytes, current)
}

/// Decoded sticker frames shown on an image, played on demand.
#[derive(Clone)]
pub(crate) struct StickerAnimation {
    image: glib::WeakRef<gtk::Image>,
    frames: std::rc::Rc<Vec<(gdk::MemoryTexture, u32)>>,
    /// Bumped by every play and stop, so timers of an older run end.
    generation: std::rc::Rc<std::cell::Cell<u64>>,
    playing: std::rc::Rc<std::cell::Cell<bool>>,
}

impl StickerAnimation {
    /// Uploads the frames and shows the first one on `image`.
    pub(crate) fn new(image: &gtk::Image, frames: Vec<StickerFrame>) -> Self {
        let frames: Vec<(gdk::MemoryTexture, u32)> = frames
            .into_iter()
            .map(|frame| {
                let texture = gdk::MemoryTexture::new(
                    frame.width as i32,
                    frame.height as i32,
                    gdk::MemoryFormat::R8g8b8a8,
                    &glib::Bytes::from_owned(frame.texture_rgba),
                    frame.width as usize * 4,
                );
                (texture, frame.delay_ms)
            })
            .collect();
        if let Some((first, _)) = frames.first() {
            image.set_paintable(Some(first));
        }
        Self {
            image: image.downgrade(),
            frames: std::rc::Rc::new(frames),
            generation: Default::default(),
            playing: Default::default(),
        }
    }

    pub(crate) fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// Plays from the first frame, unless already playing. With a `limit`,
    /// stops on the first frame at the end of the loop that reaches it.
    pub(crate) fn play(&self, limit: Option<std::time::Duration>) {
        if !self.is_animated() || self.playing.replace(true) {
            return;
        }
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let budget = limit.map(|limit| limit.as_millis() as u64);
        self.schedule(generation, 0, budget);
    }

    /// Stops and shows the first frame again.
    pub(crate) fn stop(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.playing.set(false);
        if let (Some(image), Some((first, _))) = (self.image.upgrade(), self.frames.first()) {
            image.set_paintable(Some(first));
        }
    }

    /// Shows the frame after `index` once its delay passes. Hidden stickers
    /// wait without advancing or spending their budget.
    fn schedule(&self, generation: u64, index: usize, budget: Option<u64>) {
        let delay = self.frames[index].1;
        let animation = self.clone();
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(u64::from(delay)),
            move || {
                if animation.generation.get() != generation {
                    return;
                }
                let Some(image) = animation.image.upgrade() else {
                    return;
                };
                if !image.is_mapped() {
                    animation.schedule(generation, index, budget);
                    return;
                }
                let next = (index + 1) % animation.frames.len();
                let budget = budget.map(|budget| budget.saturating_sub(u64::from(delay)));
                if next == 0 && budget == Some(0) {
                    animation.stop();
                    return;
                }
                image.set_paintable(Some(&animation.frames[next].0));
                animation.schedule(generation, next, budget);
            },
        );
    }
}

/// Largest side of a photo in the transcript, in logical pixels.
const PHOTO_EDGE: u32 = 300;

/// Photo size in the transcript: its aspect ratio within `PHOTO_EDGE`, and
/// not so thin that it vanishes. Unknown sizes show square.
fn photo_size(width: Option<u32>, height: Option<u32>) -> (i32, i32) {
    let (Some(width), Some(height)) = (width.filter(|w| *w > 0), height.filter(|h| *h > 0)) else {
        return (PHOTO_EDGE as i32, PHOTO_EDGE as i32);
    };
    let scale = f64::from(PHOTO_EDGE) / f64::from(width.max(height));
    let side = |value: u32| ((f64::from(value) * scale).round() as i32).max(PHOTO_EDGE as i32 / 3);
    (side(width), side(height))
}

/// A rounded frame of `width` by `height` showing the message thumbnail,
/// with room for overlays such as play or download buttons.
fn media_frame(
    message: &Message,
    width: i32,
    height: i32,
    label: &str,
) -> (gtk::Picture, gtk::Overlay) {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .width_request(width)
        .height_request(height)
        .build();
    picture.update_property(&[gtk::accessible::Property::Label(label)]);
    // A picture's natural size is its texture's, loaded at twice the frame
    // for dense screens; the clamp keeps the frame at its logical size.
    let clamp = adw::Clamp::builder()
        .maximum_size(width)
        .tightening_threshold(width)
        .child(&picture)
        .build();
    // Pictures do not clip their own drawing; a frame with hidden overflow
    // rounds the corners.
    let frame = gtk::Overlay::builder()
        .child(&clamp)
        .halign(gtk::Align::Start)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["zaptide-photo"])
        .build();
    if let Some(texture) = message
        .thumbnail
        .as_deref()
        .and_then(|bytes| gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok())
    {
        picture.set_paintable(Some(&texture));
    }
    (picture, frame)
}

/// A photo in its own rounded frame: the small inline thumbnail at once,
/// the file itself once downloaded, and a full view on click.
fn append_photo(
    parent: &gtk::Box,
    message: &Message,
    media: &crate::model::Media,
    token: &DecodeToken,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let size = photo_size(media.width, media.height);
    append_photo_sized(parent, message, size, token, on_action, None);
}

/// Side of one album tile and the tiles per row: two large tiles for up to
/// four photos, three smaller ones beyond that.
fn album_layout(count: usize) -> (i32, i32) {
    if count <= 4 { (2, 150) } else { (3, 100) }
}

/// Several photos sent together, as one grid of square tiles. Each tile keeps
/// the single photo's behaviour: thumbnail, download, and full view.
pub fn build_album_widget(
    messages: &[Message],
    on_action: impl Fn(NativeMediaAction) + 'static,
) -> NativeMediaWidget {
    let mut decode_token = DecodeToken::default();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let grid = gtk::Grid::builder()
        .row_spacing(3)
        .column_spacing(3)
        .halign(gtk::Align::Start)
        .build();
    let (columns, side) = album_layout(messages.len());
    let on_action: std::rc::Rc<dyn Fn(NativeMediaAction)> = std::rc::Rc::new(on_action);
    // The viewer steps through the photos already on disk.
    let items: std::rc::Rc<Vec<PhotoItem>> = std::rc::Rc::new(
        messages
            .iter()
            .filter_map(|message| match attachment_action(message) {
                Some(NativeMediaAction::Open(path)) => Some(PhotoItem {
                    path,
                    details: ViewerDetails::of(message),
                }),
                _ => None,
            })
            .collect(),
    );
    let mut opened = 0;
    for (index, message) in messages.iter().enumerate() {
        let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let tile_token = DecodeToken::default();
        let viewer =
            matches!(attachment_action(message), Some(NativeMediaAction::Open(_))).then(|| {
                opened += 1;
                (items.clone(), opened - 1)
            });
        append_photo_sized(
            &tile,
            message,
            (side, side),
            &tile_token,
            on_action.clone(),
            viewer,
        );
        decode_token.adopt(tile_token);
        grid.attach(&tile, index as i32 % columns, index as i32 / columns, 1, 1);
    }
    root.append(&grid);
    NativeMediaWidget {
        widget: root,
        decode_token,
    }
}

fn append_photo_sized(
    parent: &gtk::Box,
    message: &Message,
    (width, height): (i32, i32),
    token: &DecodeToken,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
    viewer: Option<(std::rc::Rc<Vec<PhotoItem>>, usize)>,
) {
    let (picture, frame) = media_frame(message, width, height, "Photo");
    match attachment_action(message) {
        Some(NativeMediaAction::Open(path)) => {
            // Twice the frame, for high-density screens.
            load_photo(
                &picture,
                path.clone(),
                2 * width as u32,
                2 * height as u32,
                token,
            );
            // A button, so the full view opens from the keyboard too.
            let button = gtk::Button::builder()
                .child(&frame)
                .halign(gtk::Align::Start)
                .tooltip_text("View photo")
                .css_classes(["flat", "zaptide-photo-button"])
                .build();
            button.update_property(&[gtk::accessible::Property::Label("View photo")]);
            let (items, start) = viewer.unwrap_or_else(|| {
                let item = PhotoItem {
                    path,
                    details: ViewerDetails::of(message),
                };
                (std::rc::Rc::new(vec![item]), 0)
            });
            button.connect_clicked(move |button| {
                show_photo(button, items.clone(), start, on_action.clone());
            });
            parent.append(&button);
        }
        Some(action @ NativeMediaAction::Download { .. }) => {
            let button = gtk::Button::builder()
                .icon_name("folder-download-symbolic")
                .tooltip_text("Download photo")
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .css_classes(["osd", "circular"])
                .build();
            button.connect_clicked(move |_| on_action(action.clone()));
            frame.add_overlay(&button);
            parent.append(&frame);
        }
        Some(NativeMediaAction::AnswerButton { .. } | NativeMediaAction::AnswerListRow { .. })
        | None => {
            let spinner = adw::Spinner::builder()
                .width_request(32)
                .height_request(32)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build();
            frame.add_overlay(&spinner);
            parent.append(&frame);
        }
    }
}

/// A video as a framed thumbnail with a play button and its length. GIFs
/// loop silently in place; other videos open in the viewer with sound.
fn append_video(
    parent: &gtk::Box,
    message: &Message,
    media: &crate::model::Media,
    seconds: Option<u32>,
    gif: bool,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let (width, height) = photo_size(media.width, media.height);
    let (picture, frame) = media_frame(message, width, height, if gif { "GIF" } else { "Video" });
    let length = if gif {
        Some("GIF".to_owned())
    } else {
        seconds.map(crate::util::duration)
    };
    let action = attachment_action(message);
    let badge = match (&action, length) {
        (Some(NativeMediaAction::Download { .. }), Some(length)) => {
            Some(format!("{length} · {}", crate::util::bytes(media.size)))
        }
        (Some(NativeMediaAction::Download { .. }), None) => Some(crate::util::bytes(media.size)),
        (_, length) => length,
    };
    if let Some(badge) = badge {
        frame.add_overlay(
            &gtk::Label::builder()
                .label(badge)
                .halign(gtk::Align::Start)
                .valign(gtk::Align::End)
                .margin_start(8)
                .margin_bottom(8)
                .css_classes(["caption", "zaptide-media-badge"])
                .build(),
        );
    }
    let centered = |icon: &str, tooltip: &str| {
        gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tooltip)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .css_classes(["osd", "circular", "zaptide-play"])
            .build()
    };
    match action {
        Some(NativeMediaAction::Open(path)) => {
            let projection = playback_projection(message);
            if projection == (PlaybackProjection::Native { looping: true }) {
                let clip = gtk::MediaFile::for_filename(&path);
                clip.set_loop(true);
                clip.set_muted(true);
                clip.play();
                picture.set_paintable(Some(&clip));
                parent.append(&frame);
                return;
            }
            // Only a cue: the whole frame is the button.
            let play = gtk::Image::builder()
                .icon_name("media-playback-start-symbolic")
                .pixel_size(24)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .css_classes(["osd", "zaptide-play"])
                .build();
            frame.add_overlay(&play);
            let playable = matches!(projection, PlaybackProjection::Native { .. });
            let button = gtk::Button::builder()
                .child(&frame)
                .halign(gtk::Align::Start)
                .tooltip_text(if playable { "Play video" } else { "Open video" })
                .css_classes(["flat", "zaptide-photo-button"])
                .build();
            button.update_property(&[gtk::accessible::Property::Label("Play video")]);
            let details = ViewerDetails::of(message);
            button.connect_clicked(move |button| {
                if playable {
                    show_video(button, &path, &details, on_action.clone());
                } else {
                    on_action(NativeMediaAction::Open(path.clone()));
                }
            });
            parent.append(&button);
        }
        Some(action @ NativeMediaAction::Download { .. }) => {
            let button = centered("folder-download-symbolic", "Download video");
            button.connect_clicked(move |_| on_action(action.clone()));
            frame.add_overlay(&button);
            parent.append(&frame);
        }
        Some(NativeMediaAction::AnswerButton { .. } | NativeMediaAction::AnswerListRow { .. })
        | None => {
            frame.add_overlay(
                &adw::Spinner::builder()
                    .width_request(32)
                    .height_request(32)
                    .halign(gtk::Align::Center)
                    .valign(gtk::Align::Center)
                    .build(),
            );
            parent.append(&frame);
        }
    }
}

/// Plays `path` in the viewer with sound; stops when the viewer closes and
/// offers another app when GStreamer cannot play it.
fn show_video(
    parent: &impl IsA<gtk::Widget>,
    path: &std::path::Path,
    details: &ViewerDetails,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let video = gtk::Video::builder()
        .file(&gtk::gio::File::for_path(path))
        .autoplay(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    let failed = adw::StatusPage::builder()
        .icon_name("video-x-generic-symbolic")
        .title("Can't Play This Video")
        .description("Open it with another app instead")
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&video, Some("video"));
    stack.add_named(&failed, Some("failed"));
    if let Some(stream) = video.media_stream() {
        let stack = stack.downgrade();
        let failed = failed.downgrade();
        stream.connect_error_notify(move |stream| {
            if let (Some(error), Some(stack), Some(failed)) =
                (stream.error(), stack.upgrade(), failed.upgrade())
            {
                // GStreamer names what is missing, such as a decoder.
                log::warn!("video could not be played: {error}");
                failed.set_description(Some(&format!(
                    "{error}\n\nOpen it with another app instead."
                )));
                stack.set_visible_child_name("failed");
            }
        });
    }
    let (open, target) = open_with_button(path, on_action);
    let dialog = media_viewer(
        parent,
        details,
        &stack,
        &[open.upcast(), show_in_folder_button(path).upcast()],
    )
    .dialog;
    *target.borrow_mut() = dialog.downgrade();
    dialog.connect_closed(move |_| {
        if let Some(stream) = video.media_stream() {
            stream.pause();
        }
    });
    dialog.present(Some(parent));
}

/// Opens the file manager at `path` with the file selected, through the
/// portal inside Flatpak.
pub fn show_in_folder(widget: &impl IsA<gtk::Widget>, path: &std::path::Path) {
    let window = widget.as_ref().root().and_downcast::<gtk::Window>();
    gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(path))).open_containing_folder(
        window.as_ref(),
        gtk::gio::Cancellable::NONE,
        |result| {
            if let Err(error) = result {
                log::warn!("could not show the attachment in its folder: {error}");
            }
        },
    );
}

fn show_in_folder_button(path: &std::path::Path) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Show in Folder")
        .valign(gtk::Align::Center)
        .build();
    let path = path.to_path_buf();
    button.connect_clicked(move |button| show_in_folder(button, &path));
    button
}

/// A document as a file row: its type icon, name, size and pages, and the
/// actions its download state allows.
fn document_card(
    message: &Message,
    media: &crate::model::Media,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) -> gtk::Box {
    let (file_name, detail) = match project_content(message).content {
        NativeMediaContent::Document { file_name, detail } => (file_name, detail),
        _ => ("Document".to_owned(), String::new()),
    };
    let card = gtk::Box::builder()
        .spacing(12)
        .css_classes(["card", "zaptide-media-card", "zaptide-document"])
        .build();
    let (content_type, _) = gtk::gio::content_type_guess(Some(file_name.as_str()), None);
    let icon = gtk::Image::builder()
        .gicon(&gtk::gio::content_type_get_icon(&content_type))
        .pixel_size(40)
        .valign(gtk::Align::Center)
        .build();
    card.append(&icon);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(&file_name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(32)
            .tooltip_text(&file_name)
            .css_classes(["heading"])
            .build(),
    );
    let kind = gtk::gio::content_type_get_description(&content_type);
    let detail = [kind.as_str(), detail.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    text.append(
        &gtk::Label::builder()
            .label(&detail)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption", "dim-label"])
            .build(),
    );
    card.append(&text);
    let actions = gtk::Box::builder().css_classes(["linked"]).build();
    match attachment_action(message) {
        Some(NativeMediaAction::Open(path)) => {
            let open = gtk::Button::builder()
                .icon_name("adw-external-link-symbolic")
                .tooltip_text("Open")
                .valign(gtk::Align::Center)
                .build();
            let target = path.clone();
            open.connect_clicked(move |_| on_action(NativeMediaAction::Open(target.clone())));
            actions.append(&open);
            actions.append(&show_in_folder_button(&path));
        }
        Some(action @ NativeMediaAction::Download { .. }) => {
            let download = gtk::Button::builder()
                .icon_name("folder-download-symbolic")
                .tooltip_text(format!("Download ({})", crate::util::bytes(media.size)))
                .valign(gtk::Align::Center)
                .build();
            download.connect_clicked(move |_| on_action(action.clone()));
            actions.append(&download);
        }
        Some(NativeMediaAction::AnswerButton { .. } | NativeMediaAction::AnswerListRow { .. })
        | None => actions.append(
            &adw::Spinner::builder()
                .width_request(24)
                .height_request(24)
                .valign(gtk::Align::Center)
                .tooltip_text("Downloading")
                .build(),
        ),
    }
    card.append(&actions);
    card
}

/// Who sent a photo or video, when, and its caption, for the viewer.
#[derive(Clone)]
struct ViewerDetails {
    title: String,
    subtitle: String,
    caption: Option<String>,
    thumbnail: Option<Vec<u8>>,
}

impl ViewerDetails {
    fn of(message: &Message) -> Self {
        let title = if message.from_me {
            "You".to_owned()
        } else {
            message
                .sender_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| {
                    crate::model::phone_of(&message.sender)
                        .map(crate::util::phone)
                        .unwrap_or_default()
                })
        };
        let caption = match &message.content {
            Content::Image { caption, .. } | Content::Video { caption, .. } => caption.clone(),
            _ => None,
        };
        Self {
            title,
            subtitle: crate::util::clock(message.timestamp),
            caption: caption.filter(|caption| !caption.is_empty()),
            thumbnail: message.thumbnail.clone(),
        }
    }
}

/// A dark, near-full-window dialog around `content`, with the sender and
/// time on top, the caption below, and `actions` in the header.
fn media_viewer(
    parent: &impl IsA<gtk::Widget>,
    details: &ViewerDetails,
    content: &impl IsA<gtk::Widget>,
    actions: &[gtk::Widget],
) -> Viewer {
    let title = adw::WindowTitle::new(&details.title, &details.subtitle);
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    for action in actions {
        header.pack_end(action);
    }
    let view = adw::ToolbarView::builder()
        .content(content)
        .top_bar_style(adw::ToolbarStyle::Raised)
        .css_classes(["zaptide-viewer"])
        .build();
    view.add_top_bar(&header);
    let caption = gtk::Label::builder()
        .label(details.caption.as_deref().unwrap_or_default())
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(80)
        .justify(gtk::Justification::Center)
        .selectable(true)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(16)
        .margin_end(16)
        .build();
    view.add_bottom_bar(&caption);
    view.set_bottom_bar_style(adw::ToolbarStyle::Raised);
    view.set_reveal_bottom_bars(details.caption.is_some());
    // Most of the window, as a photo viewer would take.
    let (width, height) = parent
        .as_ref()
        .root()
        .map_or((900, 700), |root| (root.width(), root.height()));
    let dialog = adw::Dialog::builder()
        .title(&details.title)
        .content_width((width * 9 / 10).max(360))
        .content_height((height * 9 / 10).max(360))
        .child(&view)
        .build();
    Viewer {
        dialog,
        title,
        caption,
        view,
    }
}

/// A viewer dialog and the parts that change when it shows another item.
struct Viewer {
    dialog: adw::Dialog,
    title: adw::WindowTitle,
    caption: gtk::Label,
    view: adw::ToolbarView,
}

/// One photo the viewer can step to.
#[derive(Clone)]
struct PhotoItem {
    path: std::path::PathBuf,
    details: ViewerDetails,
}

/// A header button that closes `dialog` after opening the file elsewhere.
fn open_with_button(
    path: &std::path::Path,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) -> (
    gtk::Button,
    std::rc::Rc<std::cell::RefCell<glib::WeakRef<adw::Dialog>>>,
) {
    let button = gtk::Button::builder()
        .icon_name("adw-external-link-symbolic")
        .tooltip_text("Open With Another App")
        .build();
    let dialog: std::rc::Rc<std::cell::RefCell<glib::WeakRef<adw::Dialog>>> =
        std::rc::Rc::default();
    let (path, target) = (path.to_path_buf(), dialog.clone());
    button.connect_clicked(move |_| {
        on_action(NativeMediaAction::Open(path.clone()));
        if let Some(dialog) = target.borrow().upgrade() {
            dialog.close();
        }
    });
    (button, dialog)
}

/// Opens `path` in the viewer: fitted to the window, or at its real size
/// with double-click or the zoom button, and dragged around when larger.
fn show_photo(
    parent: &impl IsA<gtk::Widget>,
    items: std::rc::Rc<Vec<PhotoItem>>,
    start: usize,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let start = start.min(items.len().saturating_sub(1));
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    picture.update_property(&[gtk::accessible::Property::Label("Photo")]);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&picture)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let zoom = gtk::ToggleButton::builder()
        .icon_name("zoom-original-symbolic")
        .tooltip_text("Actual Size")
        .build();
    {
        let (picture, scroller) = (picture.clone(), scroller.clone());
        zoom.connect_toggled(move |zoom| {
            let actual = zoom.is_active();
            picture.set_can_shrink(!actual);
            let policy = if actual {
                gtk::PolicyType::Automatic
            } else {
                gtk::PolicyType::Never
            };
            scroller.set_policy(policy, policy);
            zoom.set_icon_name(if actual {
                "zoom-fit-best-symbolic"
            } else {
                "zoom-original-symbolic"
            });
            zoom.set_tooltip_text(Some(if actual {
                "Fit to Window"
            } else {
                "Actual Size"
            }));
        });
    }
    let double_click = gtk::GestureClick::new();
    {
        let zoom = zoom.clone();
        double_click.connect_pressed(move |_, presses, _, _| {
            if presses == 2 {
                zoom.set_active(!zoom.is_active());
            }
        });
    }
    scroller.add_controller(double_click);
    let pan = gtk::GestureDrag::new();
    let origin = std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0)));
    {
        let (scroller, origin) = (scroller.clone(), origin.clone());
        pan.connect_drag_begin(move |_, _, _| {
            origin.set((
                scroller.hadjustment().value(),
                scroller.vadjustment().value(),
            ));
        });
    }
    {
        let scroller = scroller.clone();
        pan.connect_drag_update(move |_, x, y| {
            let (h, v) = origin.get();
            scroller.hadjustment().set_value(h - x);
            scroller.vadjustment().set_value(v - y);
        });
    }
    scroller.add_controller(pan);
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy Image")
        .build();
    {
        let picture = picture.clone();
        copy.connect_clicked(move |button| {
            if let Some(texture) = picture.paintable().and_downcast::<gdk::Texture>() {
                button.clipboard().set_texture(&texture);
            }
        });
    }
    // The header actions follow whichever photo is showing.
    let current = std::rc::Rc::new(std::cell::Cell::new(start));
    let dialog_ref: std::rc::Rc<std::cell::RefCell<glib::WeakRef<adw::Dialog>>> =
        std::rc::Rc::default();
    let open = gtk::Button::builder()
        .icon_name("adw-external-link-symbolic")
        .tooltip_text("Open With Another App")
        .build();
    {
        let (items, current, dialog_ref) = (items.clone(), current.clone(), dialog_ref.clone());
        open.connect_clicked(move |_| {
            on_action(NativeMediaAction::Open(items[current.get()].path.clone()));
            if let Some(dialog) = dialog_ref.borrow().upgrade() {
                dialog.close();
            }
        });
    }
    let folder = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Show in Folder")
        .valign(gtk::Align::Center)
        .build();
    {
        let (items, current) = (items.clone(), current.clone());
        folder.connect_clicked(move |button| show_in_folder(button, &items[current.get()].path));
    }
    let previous = photo_step_button("go-previous-symbolic", "Previous Photo", gtk::Align::Start);
    let next = photo_step_button("go-next-symbolic", "Next Photo", gtk::Align::End);
    let overlay = gtk::Overlay::builder().child(&scroller).build();
    if items.len() > 1 {
        overlay.add_overlay(&previous);
        overlay.add_overlay(&next);
    }
    let viewer = media_viewer(
        parent,
        &items[start].details,
        &overlay,
        &[
            open.upcast(),
            folder.upcast(),
            copy.upcast(),
            zoom.clone().upcast(),
        ],
    );
    let dialog = viewer.dialog.clone();
    *dialog_ref.borrow_mut() = dialog.downgrade();
    // One token: showing another photo drops the previous one's pending load.
    let token = DecodeToken::default();
    let shown = current.clone();
    let show: std::rc::Rc<dyn Fn(usize)> = {
        let items = items.clone();
        let (picture, previous, next) = (picture.clone(), previous.clone(), next.clone());
        std::rc::Rc::new(move |index: usize| {
            current.set(index);
            let item = &items[index];
            zoom.set_active(false);
            // The sender's thumbnail shows at once, until the file loads.
            picture.set_paintable(
                item.details
                    .thumbnail
                    .as_deref()
                    .and_then(|bytes| gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok())
                    .as_ref(),
            );
            // Full size up to a large screen; the view fits it to the dialog.
            load_photo(&picture, item.path.clone(), 3840, 3840, &token);
            viewer.title.set_title(&item.details.title);
            viewer.title.set_subtitle(&if items.len() > 1 {
                format!(
                    "{} · {} of {}",
                    item.details.subtitle,
                    index + 1,
                    items.len()
                )
            } else {
                item.details.subtitle.clone()
            });
            viewer
                .caption
                .set_label(item.details.caption.as_deref().unwrap_or_default());
            viewer
                .view
                .set_reveal_bottom_bars(item.details.caption.is_some());
            previous.set_sensitive(index > 0);
            next.set_sensitive(index + 1 < items.len());
        })
    };
    let step: std::rc::Rc<dyn Fn(isize)> = {
        let (show, items, shown) = (show.clone(), items.clone(), shown.clone());
        std::rc::Rc::new(move |delta: isize| {
            let target = shown.get().saturating_add_signed(delta);
            if delta != 0 && target < items.len() && target != shown.get() {
                show(target);
            }
        })
    };
    for (button, delta) in [(&previous, -1), (&next, 1)] {
        let step = step.clone();
        button.connect_clicked(move |_| step(delta));
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(move |_, key, _, _| match key {
        gdk::Key::Left => {
            step(-1);
            glib::Propagation::Stop
        }
        gdk::Key::Right => {
            step(1);
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    });
    dialog.add_controller(keys);
    show(start);
    dialog.present(Some(parent));
}

/// A round button over the viewer's edge that steps to a neighbouring photo.
fn photo_step_button(icon: &str, tooltip: &str, side: gtk::Align) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .halign(side)
        .valign(gtk::Align::Center)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["osd", "circular"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
}

fn photo_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("zaptide-photo")
            .enable_all()
            .build()
            .expect("photo runtime")
    });
    &RUNTIME
}

/// Loads `path` through glycin's sandboxed loaders, scaled to fit
/// `width` by `height`, into `picture`, unless its row moved on first.
fn load_photo(
    picture: &gtk::Picture,
    path: std::path::PathBuf,
    width: u32,
    height: u32,
    token: &DecodeToken,
) {
    let ticket = token.issue();
    let picture = glib::SendWeakRef::from(picture.downgrade());
    let main_context = glib::MainContext::default();
    let current = ticket.clone();
    photo_runtime().spawn(async move {
        let texture = async {
            // Rows recycled while scrolling queue loads; skip the stale ones.
            if !current.is_current() {
                return None;
            }
            let mut loader = glycin::Loader::new(gtk::gio::File::for_path(&path));
            loader.accepted_memory_formats(glycin::MemoryFormatSelection::R8g8b8a8);
            let mut image = loader.load().await.inspect_err(photo_error).ok()?;
            if !current.is_current() {
                return None;
            }
            let frame = image
                .specific_frame(glycin::FrameRequest::new().scale(width, height))
                .await
                .inspect_err(photo_error)
                .ok()?;
            Some(fit_frame(&frame, width, height))
        }
        .await;
        main_context.invoke(move || {
            if let (true, Some(picture), Some(texture)) =
                (ticket.is_current(), picture.upgrade(), texture)
            {
                picture.set_paintable(Some(&texture));
            }
        });
    });
}

/// Loaders may ignore the requested scale and return the full image, tens
/// of megabytes for a phone photo; shrink it to fit `width` by `height`.
fn fit_frame(frame: &glycin::Frame, width: u32, height: u32) -> gdk::Texture {
    let (frame_width, frame_height) = (frame.width(), frame.height());
    let fits = frame_width <= width && frame_height <= height;
    let rgba = frame.memory_format() == glycin::MemoryFormat::R8g8b8a8;
    let pixels = (!fits && rgba)
        .then(|| {
            let stride = frame.stride() as usize;
            let row = frame_width as usize * 4;
            let packed = frame
                .buf_slice()
                .chunks(stride)
                .take(frame_height as usize)
                .flat_map(|line| line.get(..row).unwrap_or_default().iter().copied())
                .collect::<Vec<u8>>();
            image::RgbaImage::from_raw(frame_width, frame_height, packed)
        })
        .flatten();
    let Some(pixels) = pixels else {
        return frame.texture();
    };
    let scale = f64::min(
        f64::from(width) / f64::from(frame_width),
        f64::from(height) / f64::from(frame_height),
    );
    let fitted = image::imageops::thumbnail(
        &pixels,
        ((f64::from(frame_width) * scale).round() as u32).max(1),
        ((f64::from(frame_height) * scale).round() as u32).max(1),
    );
    let (fitted_width, fitted_height) = fitted.dimensions();
    gdk::MemoryTexture::new(
        fitted_width as i32,
        fitted_height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(fitted.into_raw()),
        fitted_width as usize * 4,
    )
    .upcast()
}

/// Logs the first failure only: without glycin's loaders or bubblewrap,
/// every photo fails the same way and keeps its thumbnail.
fn photo_error(error: &impl std::fmt::Display) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!("photo could not be loaded: {error}");
    }
}

/// A shared place: the sender's map snapshot, name, address, and
/// coordinates. Clicking opens the default maps app, else OpenStreetMap.
fn location_card(
    latitude: f64,
    longitude: f64,
    name: Option<&str>,
    address: Option<&str>,
    thumbnail: Option<Vec<u8>>,
    decode_token: &DecodeToken,
) -> gtk::Button {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    if let Some(bytes) = thumbnail {
        let image = gtk::Image::new();
        image.set_pixel_size(160);
        image.set_halign(gtk::Align::Start);
        image.add_css_class("zaptide-link-thumbnail");
        image.set_overflow(gtk::Overflow::Hidden);
        image.set_visible(false);
        card.append(&image);
        decode_preview_async(&image, move || Some(bytes), decode_token);
    }
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    heading.append(&gtk::Image::from_icon_name("mark-location-symbolic"));
    let title = gtk::Label::builder()
        .label(name.unwrap_or("Location"))
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(44)
        .css_classes(["heading"])
        .build();
    heading.append(&title);
    card.append(&heading);
    let coordinates = format!("{latitude:.5}, {longitude:.5}");
    for (text, classes) in [
        (address, &["caption"][..]),
        (
            Some(coordinates.as_str()),
            &["caption", "dim-label", "numeric"][..],
        ),
    ] {
        if let Some(text) = text {
            card.append(
                &gtk::Label::builder()
                    .label(text)
                    .xalign(0.0)
                    .wrap(true)
                    .max_width_chars(44)
                    .selectable(false)
                    .css_classes(classes)
                    .build(),
            );
        }
    }
    clickable_card(&card, "Open in Maps", move |button| {
        let window = button.root().and_downcast::<gtk::Window>();
        let fallback = format!(
            "https://www.openstreetmap.org/?mlat={latitude}&mlon={longitude}#map=16/{latitude}/{longitude}"
        );
        let retry = window.clone();
        gtk::UriLauncher::new(&format!("geo:{latitude},{longitude}")).launch(
            window.as_ref(),
            gtk::gio::Cancellable::NONE,
            move |result| {
                if result.is_err() {
                    gtk::UriLauncher::new(&fallback).launch(
                        retry.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        |_| {},
                    );
                }
            },
        );
    })
}

/// Wraps card content in a flat button, so it takes focus and opens with
/// Enter or Space as well as a click.
fn clickable_card(
    content: &gtk::Box,
    tooltip: &str,
    on_click: impl Fn(&gtk::Button) + 'static,
) -> gtk::Button {
    let button = gtk::Button::builder()
        .child(content)
        .tooltip_text(tooltip)
        .css_classes(["flat", "card", "zaptide-media-card", "zaptide-link-card"])
        .build();
    button.set_cursor_from_name(Some("pointer"));
    button.connect_clicked(on_click);
    button
}

/// Link preview as WhatsApp sends it: the sender's thumbnail, title,
/// description, and site. Clicking it opens the link.
fn link_card(
    preview: &crate::model::LinkPreview,
    thumbnail: Option<Vec<u8>>,
    decode_token: &DecodeToken,
) -> gtk::Button {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    if let Some(bytes) = thumbnail {
        let image = gtk::Image::new();
        image.set_pixel_size(72);
        image.set_valign(gtk::Align::Start);
        image.add_css_class("zaptide-link-thumbnail");
        image.set_overflow(gtk::Overflow::Hidden);
        image.set_visible(false);
        card.append(&image);
        decode_preview_async(&image, move || Some(bytes), decode_token);
    }
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let line = |value: &str, lines: i32, classes: &[&str]| {
        let label = gtk::Label::builder()
            .label(value)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(lines)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(44)
            .css_classes(classes)
            .build();
        text.append(&label);
    };
    let host = url::Url::parse(&preview.url).ok().and_then(|url| {
        url.host_str()
            .map(|host| host.trim_start_matches("www.").to_owned())
    });
    line(
        preview.title.as_deref().unwrap_or(&preview.url),
        2,
        &["heading"],
    );
    if let Some(description) = &preview.description {
        line(description, 3, &["caption"]);
    }
    if let Some(host) = &host {
        line(host, 1, &["caption", "dim-label"]);
    }
    card.append(&text);
    let url = preview.url.clone();
    clickable_card(&card, &preview.url, move |button| {
        let window = button.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(&url).launch(window.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
    })
}

fn add_label(parent: &gtk::Box, text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(52);
    parent.append(&label);
    label
}

/// One list section for the chooser: optional heading and rows as (id, title, detail).
type ListChoiceSection = (Option<String>, Vec<(String, String, Option<String>)>);

/// Dialog listing a message's rows; picking one calls `on_pick` with its id
/// and closes. The dialog is held weakly by its own rows.
fn show_list_choices(
    anchor: &gtk::Button,
    title: &str,
    sections: &[ListChoiceSection],
    on_pick: impl Fn(String) + 'static,
) {
    use adw::prelude::*;
    let dialog = adw::Dialog::builder()
        .title(if title.is_empty() { "Choose" } else { title })
        .content_width(380)
        .content_height(480)
        .build();
    if !title.is_empty() {
        dialog.set_tooltip_text(Some(title));
    }
    let page = adw::PreferencesPage::new();
    let on_pick = std::rc::Rc::new(on_pick);
    let fired = std::rc::Rc::new(std::cell::Cell::new(false));
    let sections = sections.to_vec();
    let (mut section_index, mut row_index) = (0, 0);
    let mut group: Option<adw::PreferencesGroup> = None;
    let weak_dialog = dialog.downgrade();
    let pending_page = page.clone();
    gtk::glib::idle_add_local(move || {
        if weak_dialog.upgrade().is_none() {
            return gtk::glib::ControlFlow::Break;
        }
        let mut added = 0;
        while added < 12 && section_index < sections.len() {
            let (heading, rows) = &sections[section_index];
            if group.is_none() {
                let next = adw::PreferencesGroup::new();
                if let Some(heading) = heading {
                    next.set_title(&gtk::glib::markup_escape_text(heading));
                    next.set_tooltip_text(Some(heading));
                }
                pending_page.add(&next);
                group = Some(next);
            }
            if let Some((id, row_title, detail)) = rows.get(row_index) {
                // Sender text: markup off before any text is set.
                let row = adw::ActionRow::builder()
                    .use_markup(false)
                    .title_lines(2)
                    .activatable(true)
                    .build();
                row.set_title(row_title);
                if let Some(detail) = detail {
                    row.set_subtitle(detail);
                    row.set_subtitle_lines(3);
                }
                let (id, on_pick, dialog) = (id.clone(), on_pick.clone(), weak_dialog.clone());
                let fired = fired.clone();
                row.connect_activated(move |_| {
                    // The dialog closes with an animation; a second pick during
                    // it must not send a second answer.
                    if fired.replace(true) {
                        return;
                    }
                    on_pick(id.clone());
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                });
                if let Some(group) = &group {
                    group.add(&row);
                }
                row_index += 1;
                added += 1;
            } else {
                group = None;
                section_index += 1;
                row_index = 0;
            }
        }
        if section_index < sections.len() {
            gtk::glib::ControlFlow::Continue
        } else {
            gtk::glib::ControlFlow::Break
        }
    });
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));
    dialog.present(Some(anchor));
}

/// Button with a wrapping label for an interactive message.
fn reply_button(text: &str) -> gtk::Button {
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(40);
    label.set_justify(gtk::Justification::Center);
    let button = gtk::Button::new();
    button.set_child(Some(&label));
    button
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
    start_preview_decode(image.downgrade(), load, token.issue());
}

/// Waits for a decode slot instead of dropping the preview, until the row is
/// recycled or destroyed.
fn start_preview_decode(
    image: glib::WeakRef<gtk::Image>,
    load: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ticket: crate::native_media::DecodeTicket,
) {
    if !ticket.is_current() || image.upgrade().is_none() {
        return;
    }
    let Some(permit) = ThumbnailDecodePermit::acquire() else {
        glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
            start_preview_decode(image, load, ticket);
        });
        return;
    };
    let image = glib::SendWeakRef::from(image);
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
        let frames = super::decode_sticker(&gif(3), || true).expect("decoded");
        assert_eq!(frames.len(), 3);
        assert!(
            frames
                .iter()
                .all(|frame| frame.width == super::STICKER_SIZE)
        );
        assert_eq!(frames[0].delay_ms, 50);

        let frames = super::decode_sticker(&gif(200), || true).expect("decoded");
        assert!(frames.len() <= super::MAX_STICKER_FRAMES);
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
        assert!(super::decode_sticker(&bytes, || true).is_none());
        assert!(super::decode_sticker(&gif(3), || false).is_none());
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

    #[test]
    fn photos_keep_their_shape_within_the_frame() {
        assert_eq!(super::photo_size(Some(4000), Some(3000)), (300, 225));
        assert_eq!(super::photo_size(Some(1080), Some(1920)), (169, 300));
        assert_eq!(super::photo_size(Some(3000), Some(100)), (300, 100));
        assert_eq!(super::photo_size(None, Some(10)), (300, 300));
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
