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
            let n = values.0.len();
            if n == 0 || width <= 0 {
                return;
            }
            let color = area.color();
            // Leave room for the progress dot at both ends.
            let dot = 5.0;
            let bar_width = (width as f64 - 2.0 * dot) / n as f64;
            for (index, value) in values.0.iter().enumerate() {
                let h = (f64::from(*value.min(&100)) / 100.0 * (height - 4) as f64).max(3.0);
                cr.set_source_rgba(
                    color.red() as f64,
                    color.green() as f64,
                    color.blue() as f64,
                    if (index as f32) < values.1 * n as f32 {
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
            let played = f64::from(values.1.clamp(0.0, 1.0));
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                1.0,
            );
            cr.arc(
                dot + played * (width as f64 - 2.0 * dot),
                height as f64 / 2.0,
                dot,
                0.0,
                std::f64::consts::TAU,
            );
            let _ = cr.fill();
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

/// A photo in its own rounded frame: the small inline thumbnail at once,
/// the file itself once downloaded, and a full view on click.
fn append_photo(
    parent: &gtk::Box,
    message: &Message,
    media: &crate::model::Media,
    token: &DecodeToken,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let (width, height) = photo_size(media.width, media.height);
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .width_request(width)
        .height_request(height)
        .build();
    picture.update_property(&[gtk::accessible::Property::Label("Photo")]);
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
            button.connect_clicked(move |button| show_photo(button, &path, on_action.clone()));
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
        None => {
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

/// Opens `path` in a dialog over the window, fitted to it.
fn show_photo(
    parent: &impl IsA<gtk::Widget>,
    path: &std::path::Path,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .vexpand(true)
        .build();
    picture.update_property(&[gtk::accessible::Property::Label("Photo")]);
    let open = gtk::Button::with_label("Open With…");
    let header = adw::HeaderBar::new();
    header.pack_start(&open);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&picture));
    let dialog = adw::Dialog::builder()
        .title("Photo")
        .content_width(900)
        .content_height(700)
        .child(&view)
        .build();
    {
        let (path, dialog) = (path.to_path_buf(), dialog.downgrade());
        open.connect_clicked(move |_| {
            on_action(NativeMediaAction::Open(path.clone()));
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        });
    }
    // Full size up to a large screen; the view fits it to the dialog.
    load_photo(
        &picture,
        path.to_path_buf(),
        3840,
        3840,
        &DecodeToken::default(),
    );
    dialog.present(Some(parent));
}

#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn photo_error(error: &impl std::fmt::Display) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!("photo could not be loaded: {error}");
    }
}

/// Without glycin, photos keep their inline thumbnail.
#[cfg(not(target_os = "linux"))]
fn load_photo(_: &gtk::Picture, _: std::path::PathBuf, _: u32, _: u32, _: &DecodeToken) {}

/// A shared place: the sender's map snapshot, name, address, and
/// coordinates. Clicking opens the default maps app, else OpenStreetMap.
fn location_card(
    latitude: f64,
    longitude: f64,
    name: Option<&str>,
    address: Option<&str>,
    thumbnail: Option<Vec<u8>>,
    decode_token: &DecodeToken,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.add_css_class("card");
    card.add_css_class("zaptide-media-card");
    card.add_css_class("zaptide-link-card");
    card.set_tooltip_text(Some("Open in Maps"));
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
    let click = gtk::GestureClick::new();
    click.connect_released(move |gesture, _, _, _| {
        let window = gesture
            .widget()
            .and_then(|widget| widget.root())
            .and_downcast::<gtk::Window>();
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
    });
    card.add_controller(click);
    card.set_cursor_from_name(Some("pointer"));
    card
}

/// Link preview as WhatsApp sends it: the sender's thumbnail, title,
/// description, and site. Clicking it opens the link.
fn link_card(
    preview: &crate::model::LinkPreview,
    thumbnail: Option<Vec<u8>>,
    decode_token: &DecodeToken,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    card.add_css_class("card");
    card.add_css_class("zaptide-media-card");
    card.add_css_class("zaptide-link-card");
    card.set_tooltip_text(Some(&preview.url));
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
    let click = gtk::GestureClick::new();
    click.connect_released(move |gesture, _, _, _| {
        let window = gesture
            .widget()
            .and_then(|widget| widget.root())
            .and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(&url).launch(window.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
    });
    card.add_controller(click);
    card.set_cursor_from_name(Some("pointer"));
    card
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
