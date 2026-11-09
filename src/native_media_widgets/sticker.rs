use super::*;
use crate::i18n::gettext;

/// Stickers draw without a card, as on the phone. Until the file downloads,
/// a dim label keeps the row from collapsing.
pub(super) fn append_sticker(
    parent: &gtk::Box,
    path: Option<std::path::PathBuf>,
    token: &DecodeToken,
) {
    let Some(path) = path else {
        let label = gtk::Label::new(Some(&gettext("Sticker")));
        label.set_xalign(0.0);
        label.add_css_class("dim-label");
        parent.append(&label);
        return;
    };
    let sticker = gtk::Image::new();
    sticker.set_pixel_size(160);
    sticker.set_halign(gtk::Align::Start);
    sticker.update_property(&[gtk::accessible::Property::Label(&gettext("Sticker"))]);
    // Keep the space reserved while decoding so the transcript does not jump.
    sticker.set_tooltip_text(Some(&gettext("Sticker")));
    parent.append(&sticker);
    start_sticker_decode(sticker.downgrade(), path, token.issue());
}

/// One decoded sticker frame, scaled for the transcript.
pub(crate) struct StickerFrame {
    pub(super) texture_rgba: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) delay_ms: u32,
}

pub(super) const STICKER_SIZE: u32 = 160;
/// Caps retained frames per visible sticker.
pub(super) const MAX_STICKER_FRAMES: usize = 64;

/// Frames read from one sticker, merged or not; later frames are dropped.
const MAX_STICKER_DECODED_FRAMES: usize = 1_024;

/// Decodes every frame of an animated WebP or GIF sticker, or its single
/// frame, keeping at most `MAX_STICKER_FRAMES` by merging neighbours. Stops
/// early when `current` turns false, as the row was recycled.
pub(super) fn decode_sticker(
    bytes: &[u8],
    current: impl Fn() -> bool,
) -> Option<Vec<StickerFrame>> {
    if bytes.starts_with(b"PK\x03\x04") {
        return decode_lottie_sticker(bytes, current);
    }
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

fn decode_lottie_sticker(bytes: &[u8], current: impl Fn() -> bool) -> Option<Vec<StickerFrame>> {
    use std::io::Read;

    const MAX_JSON_BYTES: u64 = 1024 * 1024;
    const MAX_ARCHIVE_ENTRIES: usize = 32;
    const MAX_DURATION_SECONDS: f32 = 30.0;
    if !current() {
        return None;
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return None;
    }
    let mut candidate = None;
    for index in 0..archive.len() {
        let file = archive.by_index(index).ok()?;
        let name = file.name().ok()?;
        if !file.is_dir() && name.ends_with(".json") {
            if name.ends_with("animation.json") {
                candidate = Some(index);
                break;
            }
            candidate.get_or_insert(index);
        }
    }
    let file = archive.by_index(candidate?).ok()?;
    if file.size() > MAX_JSON_BYTES {
        return None;
    }
    let mut json = Vec::new();
    file.take(MAX_JSON_BYTES + 1).read_to_end(&mut json).ok()?;
    if json.len() as u64 > MAX_JSON_BYTES || !current() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&json).ok()?;
    if !safe_lottie_shapes(&value) {
        return None;
    }
    let animation =
        rasterlottie::Animation::from_json_str(std::str::from_utf8(&json).ok()?).ok()?;
    let frames = animation.out_point - animation.in_point;
    if animation.width == 0
        || animation.height == 0
        || animation.width > 1024
        || animation.height > 1024
        || !animation.frame_rate.is_finite()
        || !(1.0..=60.0).contains(&animation.frame_rate)
        || !animation.in_point.is_finite()
        || !frames.is_finite()
        || frames <= 0.0
        || frames / animation.frame_rate > MAX_DURATION_SECONDS
        || !safe_lottie_layers(&animation)
    {
        return None;
    }
    let count = (frames.ceil() as usize).min(MAX_STICKER_FRAMES);
    let duration_ms = (frames * 1000.0 / animation.frame_rate).round() as u32;
    let mut config = rasterlottie::RenderConfig::default();
    config.scale = STICKER_SIZE as f32 / animation.width.max(animation.height) as f32;
    let prepared = rasterlottie::Renderer::default().prepare(&animation).ok()?;
    let mut output = Vec::with_capacity(count);
    for index in 0..count {
        if !current() {
            return None;
        }
        let frame = prepared
            .render_frame(
                animation.in_point + frames * index as f32 / count as f32,
                config,
            )
            .ok()?;
        let mut rgba = frame.pixels;
        for pixel in rgba.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            for channel in &mut pixel[..3] {
                *channel = (u32::from(*channel) * 255)
                    .checked_div(alpha)
                    .unwrap_or(0)
                    .min(255) as u8;
            }
        }
        let start = duration_ms as u64 * index as u64 / count as u64;
        let end = duration_ms as u64 * (index + 1) as u64 / count as u64;
        output.push(StickerFrame {
            texture_rgba: rgba,
            width: frame.width,
            height: frame.height,
            delay_ms: (end - start) as u32,
        });
    }
    Some(output)
}

fn safe_lottie_shapes(root: &serde_json::Value) -> bool {
    let mut pending = vec![root];
    let mut visited = 0;
    while let Some(value) = pending.pop() {
        visited += 1;
        if visited > 20_000 {
            return false;
        }
        match value {
            serde_json::Value::Object(object) => {
                if matches!(object.get("ty").and_then(|v| v.as_str()), Some("rp" | "sr")) {
                    return false;
                }
                pending.extend(object.values());
            }
            serde_json::Value::Array(array) => pending.extend(array),
            _ => {}
        }
    }
    true
}

fn safe_lottie_layers(animation: &rasterlottie::Animation) -> bool {
    fn visit(
        layers: &[rasterlottie::Layer],
        animation: &rasterlottie::Animation,
        depth: usize,
        budget: &mut usize,
    ) -> bool {
        if depth > 8 || layers.len() > *budget {
            return false;
        }
        *budget -= layers.len();
        layers.iter().all(|layer| {
            if layer.layer_type.0 != 0 {
                return true;
            }
            layer
                .ref_id
                .as_ref()
                .and_then(|id| animation.assets.iter().find(|asset| &asset.id == id))
                .is_none_or(|asset| visit(&asset.layers, animation, depth + 1, budget))
        })
    }
    visit(&animation.layers, animation, 0, &mut 512)
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
                        let label = gtk::Label::new(Some(&gettext("Sticker")));
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
