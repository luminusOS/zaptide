//! Live call media: microphone and speaker through GStreamer, and H.264 video
//! capture and decoding. The VoIP runtime moves the packets; this module only
//! supplies and consumes raw audio and encoded video.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gstreamer as gst;
use gstreamer::prelude::*;

pub mod audio;
pub mod video;

/// Why call media could not start, in words fit for the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaError {
    NoMicrophone,
    NoSpeaker,
    NoCamera,
    NoVideoEncoder,
    NoVideoDecoder,
    /// The cause is dropped on purpose: GStreamer messages can name devices.
    Unavailable,
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::i18n::gettext;
        match self {
            Self::NoMicrophone => formatter.write_str(&gettext("No microphone is available")),
            Self::NoSpeaker => formatter.write_str(&gettext("No audio output is available")),
            Self::NoCamera => formatter.write_str(&gettext("No camera is available")),
            Self::NoVideoEncoder => formatter.write_str(&gettext(
                "Video calls need an H.264 encoder (the GStreamer x264 or OpenH264 plugin)",
            )),
            Self::NoVideoDecoder => formatter.write_str(&gettext(
                "Video calls need an H.264 decoder (the GStreamer libav or OpenH264 plugin)",
            )),
            Self::Unavailable => formatter.write_str(&gettext("Call audio is unavailable")),
        }
    }
}

impl std::error::Error for MediaError {}

/// Builds a pipeline from a `gst-launch` description.
pub(crate) fn pipeline_from(description: &str) -> Result<gst::Pipeline, MediaError> {
    gst::init().map_err(|_| MediaError::Unavailable)?;
    gst::parse::launch(description)
        .map_err(|_| MediaError::Unavailable)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| MediaError::Unavailable)
}

/// A media pipeline that stopped working.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The microphone or the speaker.
    Audio,
    Camera,
    /// The incoming picture could not be decoded.
    Decoder,
}

/// Told, from a GStreamer thread, when a pipeline reports an error.
pub type FaultHandler = Arc<dyn Fn(Fault) + Send + Sync>;

#[cfg(test)]
pub(crate) fn ignore_faults() -> FaultHandler {
    Arc::new(|_| {})
}

/// Reports pipeline errors to `faults`; messages are dropped after that, so an
/// unwatched bus does not pile them up.
pub(crate) fn watch(pipeline: &gst::Pipeline, faults: &FaultHandler, fault: Fault) {
    let Some(bus) = pipeline.bus() else {
        return;
    };
    let faults = faults.clone();
    bus.set_sync_handler(move |_, message| {
        if matches!(message.view(), gst::MessageView::Error(_)) {
            faults(fault);
        }
        gst::BusSyncReply::Drop
    });
}

/// Whether the named GStreamer elements can really be created here. A test that
/// needs them skips loudly when they are missing, and fails instead when
/// `ZAPTIDE_REQUIRE_GST` is set, as a CI image that should have them can ask.
#[cfg(test)]
pub(crate) fn gst_ready(elements: &[&str]) -> bool {
    let missing: Vec<&str> = if gst::init().is_ok() {
        elements
            .iter()
            .copied()
            .filter(|name| gst::ElementFactory::make(name).build().is_err())
            .collect()
    } else {
        elements.to_vec()
    };
    if missing.is_empty() {
        return true;
    }
    assert!(
        std::env::var_os("ZAPTIDE_REQUIRE_GST").is_none(),
        "GStreamer elements are required but missing: {missing:?}"
    );
    eprintln!("skipped: GStreamer elements missing: {missing:?}");
    false
}

/// Waits for an item without blocking forever.
#[cfg(test)]
pub(crate) fn recv_within<T>(
    receiver: &async_channel::Receiver<T>,
    limit: std::time::Duration,
) -> Option<T> {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if let Ok(item) = receiver.try_recv() {
            return Some(item);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}

/// A decoded picture ready for the window: tightly packed RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// The newest remote and local picture. GStreamer threads overwrite them and
/// the window takes them when it is woken, so a slow window drops frames
/// instead of queueing them.
pub struct VideoSlots {
    remote: Mutex<Option<Option<RgbaFrame>>>,
    local: Mutex<Option<Option<RgbaFrame>>>,
    pending: AtomicBool,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for VideoSlots {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("VideoSlots").finish_non_exhaustive()
    }
}

impl VideoSlots {
    /// `notify` runs when the first picture after a `take` arrives.
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            remote: Mutex::new(None),
            local: Mutex::new(None),
            pending: AtomicBool::new(false),
            notify: Box::new(notify),
        }
    }

    pub fn put_remote(&self, frame: RgbaFrame) {
        *lock(&self.remote) = Some(Some(frame));
        self.announce();
    }

    pub fn put_local(&self, frame: RgbaFrame) {
        *lock(&self.local) = Some(Some(frame));
        self.announce();
    }

    pub fn clear_local(&self) {
        *lock(&self.local) = Some(None);
        self.announce();
    }

    pub fn clear_remote(&self) {
        *lock(&self.remote) = Some(None);
        self.announce();
    }

    /// Takes the newest pictures and re-arms the notification. The outer
    /// `Option` says whether a picture changed; the inner one is the picture,
    /// `None` when it was cleared.
    pub fn take(&self) -> VideoUpdate {
        self.pending.store(false, Ordering::Release);
        VideoUpdate {
            remote: lock(&self.remote).take(),
            local: lock(&self.local).take(),
        }
    }

    fn announce(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            (self.notify)();
        }
    }
}

/// Pictures delivered by [`VideoSlots::take`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VideoUpdate {
    pub remote: Option<Option<RgbaFrame>>,
    pub local: Option<Option<RgbaFrame>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The microphone, speaker, and optional video of one call.
pub struct CallMedia {
    capture: audio::Capture,
    playout: audio::Playout,
    slots: Arc<VideoSlots>,
    faults: FaultHandler,
    camera: Option<video::Camera>,
    camera_pipeline: Arc<Mutex<Option<gst::Pipeline>>>,
    decoder: Option<video::RemoteDecoder>,
}

/// Channel ends the VoIP runtime reads and writes.
pub struct VideoEndpoints {
    pub source: async_channel::Receiver<Vec<u8>>,
    pub sink: async_channel::Sender<whatsapp_rust::voip::VideoFrame>,
    /// Whether `source` carries a camera picture.
    pub camera: bool,
}

/// Whether a call can go ahead without a camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraNeed {
    Required,
    Optional,
}

/// A source that never yields, for a video channel with no camera behind it.
pub fn no_camera() -> async_channel::Receiver<Vec<u8>> {
    async_channel::bounded(1).1
}

/// What opening the camera or the decoder needs, so it can be done without
/// holding the call's media while devices open.
#[derive(Clone)]
pub struct VideoPlan {
    encoder: video::H264Encoder,
    slots: Arc<VideoSlots>,
    faults: FaultHandler,
}

impl VideoPlan {
    /// Opens the camera, or gives `None` when `need` allows going without it.
    /// Blocks while the device starts.
    pub fn open_camera(&self, need: CameraNeed) -> Result<Option<video::Camera>, MediaError> {
        match video::Camera::start(
            self.encoder,
            "autovideosrc",
            self.slots.clone(),
            self.faults.clone(),
        ) {
            Ok(camera) => Ok(Some(camera)),
            Err(_) if need == CameraNeed::Optional => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn open_decoder(&self) -> Result<video::RemoteDecoder, MediaError> {
        video::RemoteDecoder::start(self.slots.clone(), self.faults.clone())
    }
}

/// Cheap handles on a running call's devices. They never wait for the call's
/// media, which can be busy opening a camera.
#[derive(Clone)]
pub struct CallControls {
    microphone: gst::Pipeline,
    microphone_frames: async_channel::Receiver<Vec<i16>>,
    microphone_wanted: Arc<AtomicBool>,
    camera: Arc<Mutex<Option<gst::Pipeline>>>,
}

impl CallControls {
    /// Pauses or resumes the microphone. Blocks while the device changes state.
    pub fn set_muted(&self, muted: bool) {
        self.microphone_wanted.store(!muted, Ordering::SeqCst);
        audio::set_pipeline_active(
            &self.microphone,
            self.microphone_wanted.load(Ordering::SeqCst),
        );
    }

    /// Drops sound captured while the call was still being set up.
    pub fn drain_microphone(&self) {
        while self.microphone_frames.try_recv().is_ok() {}
    }

    /// Asks the encoder for a picture the other side can start decoding from.
    pub fn request_keyframe(&self) {
        let pipeline = lock(&self.camera).clone();
        if let Some(pipeline) = pipeline {
            video::request_keyframe(&pipeline);
        }
    }
}

impl CallMedia {
    /// Opens the microphone and the speaker. Blocks while devices open, so run
    /// it off the async runtime.
    pub fn start(slots: Arc<VideoSlots>, faults: FaultHandler) -> Result<Self, MediaError> {
        let capture = audio::Capture::start(faults.clone())?;
        let playout = audio::Playout::start(faults.clone())?;
        Ok(Self {
            capture,
            playout,
            slots,
            faults,
            camera: None,
            camera_pipeline: Arc::default(),
            decoder: None,
        })
    }

    pub fn microphone(&self) -> async_channel::Receiver<Vec<i16>> {
        self.capture.frames()
    }

    pub fn speaker(&self) -> async_channel::Sender<Vec<i16>> {
        self.playout.sender()
    }

    pub fn controls(&self) -> CallControls {
        CallControls {
            microphone: self.capture.pipeline(),
            microphone_frames: self.capture.frames(),
            microphone_wanted: Arc::new(AtomicBool::new(true)),
            camera: self.camera_pipeline.clone(),
        }
    }

    /// Checks that this system can encode and decode H.264 before a call
    /// promises video.
    pub fn check_video() -> Result<video::H264Encoder, MediaError> {
        if !video::decoder_available() {
            return Err(MediaError::NoVideoDecoder);
        }
        video::available_encoder().ok_or(MediaError::NoVideoEncoder)
    }

    pub fn plan_video(&self, encoder: video::H264Encoder) -> VideoPlan {
        VideoPlan {
            encoder,
            slots: self.slots.clone(),
            faults: self.faults.clone(),
        }
    }

    pub fn decoder_sender(&self) -> Option<async_channel::Sender<whatsapp_rust::voip::VideoFrame>> {
        self.decoder.as_ref().map(video::RemoteDecoder::sender)
    }

    /// Keeps the decoder, unless one is already running.
    pub fn attach_decoder(
        &mut self,
        decoder: video::RemoteDecoder,
    ) -> async_channel::Sender<whatsapp_rust::voip::VideoFrame> {
        self.decoder.get_or_insert(decoder).sender()
    }

    /// Keeps the camera, replacing an earlier one, and returns its access units.
    pub fn attach_camera(&mut self, camera: video::Camera) -> async_channel::Receiver<Vec<u8>> {
        let frames = camera.frames();
        *lock(&self.camera_pipeline) = Some(camera.pipeline());
        self.camera = Some(camera);
        frames
    }

    /// Releases the camera so its light goes out.
    pub fn stop_camera(&mut self) {
        *lock(&self.camera_pipeline) = None;
        self.camera = None;
        self.slots.clear_local();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32) -> RgbaFrame {
        RgbaFrame {
            width,
            height: 1,
            data: vec![0; width as usize * 4],
        }
    }

    #[test]
    fn slots_notify_once_until_the_window_takes_the_pictures() {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = wakes.clone();
        let slots = VideoSlots::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        slots.put_remote(frame(1));
        slots.put_remote(frame(2));
        slots.put_local(frame(3));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);

        let update = slots.take();
        assert_eq!(update.remote, Some(Some(frame(2))));
        assert_eq!(update.local, Some(Some(frame(3))));
        assert_eq!(slots.take(), VideoUpdate::default());

        slots.put_remote(frame(4));
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_cleared_picture_is_reported_as_gone() {
        let slots = VideoSlots::new(|| {});
        assert_eq!(slots.take().local, None);
        slots.put_local(frame(1));
        slots.clear_local();
        assert_eq!(slots.take().local, Some(None));
        assert_eq!(slots.take().local, None);
    }
}
