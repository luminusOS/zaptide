//! H.264 camera capture for the outgoing picture and decoding for the incoming one.

use std::os::fd::OwnedFd;
use std::sync::Arc;

use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use whatsapp_rust::voip::VideoFrame;

use super::{
    Fault, FaultHandler, MediaError, RgbaFrame, VideoSlots, camera_portal, pipeline_from, watch,
};

/// The outgoing picture is 15 frames per second, the cadence the VoIP runtime
/// assumes for a plain channel source.
const FRAMES_PER_SECOND: i32 = 15;
const KEYFRAME_INTERVAL: i32 = 2 * FRAMES_PER_SECOND;
/// Encoded pictures waiting for the VoIP runtime; a full queue drops the newest.
const FRAMES_QUEUED: usize = 8;
const LOCAL_PREVIEW_WIDTH: i32 = 480;
const REMOTE_MAX_SIDE: i32 = 960;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum H264Encoder {
    X264,
    OpenH264,
}

impl H264Encoder {
    fn element(self) -> &'static str {
        match self {
            Self::X264 => "x264enc",
            Self::OpenH264 => "openh264enc",
        }
    }

    /// Constrained Baseline with no B-frames and a keyframe every two seconds,
    /// which is what WhatsApp video calls use.
    fn description(self) -> String {
        match self {
            Self::X264 => format!(
                "x264enc tune=zerolatency speed-preset=ultrafast bitrate=900 \
                 key-int-max={KEYFRAME_INTERVAL} bframes=0 ! video/x-h264,profile=baseline"
            ),
            Self::OpenH264 => format!(
                "openh264enc usage-type=camera rate-control=bitrate bitrate=900000 \
                 gop-size={KEYFRAME_INTERVAL} complexity=low ! \
                 video/x-h264,profile=constrained-baseline"
            ),
        }
    }
}

fn has_element(name: &str) -> bool {
    gst::init().is_ok() && gst::ElementFactory::find(name).is_some()
}

/// The first H.264 encoder this system has.
pub fn available_encoder() -> Option<H264Encoder> {
    [H264Encoder::X264, H264Encoder::OpenH264]
        .into_iter()
        .find(|encoder| has_element(encoder.element()))
}

/// Whether an H.264 decoder is installed.
pub fn decoder_available() -> bool {
    ["avdec_h264", "openh264dec", "vah264dec", "vaapih264dec"]
        .into_iter()
        .any(has_element)
        && has_element("h264parse")
}

fn app_sink(pipeline: &gst::Pipeline, name: &str) -> Result<gst_app::AppSink, MediaError> {
    pipeline
        .by_name(name)
        .and_then(|element| element.downcast::<gst_app::AppSink>().ok())
        .ok_or(MediaError::Unavailable)
}

fn rgba_of(sample: &gst::Sample) -> Option<RgbaFrame> {
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(sample.buffer()?, &info).ok()?;
    let plane = frame.plane_data(0).ok()?;
    let stride = usize::try_from(frame.plane_stride()[0]).ok()?;
    let (width, height) = (info.width() as usize, info.height() as usize);
    let row = width * 4;
    // Rows are padded to `stride`; refuse a buffer shorter than it claims
    // rather than read past the plane.
    if stride < row || plane.len() < stride * (height.saturating_sub(1)) + row {
        return None;
    }
    let mut data = Vec::with_capacity(row * height);
    for line in 0..height {
        data.extend_from_slice(&plane[line * stride..line * stride + row]);
    }
    Some(RgbaFrame {
        width: info.width(),
        height: info.height(),
        data,
    })
}

/// Whether a source factory draws a picture instead of capturing one.
fn is_test_source(factory: &str) -> bool {
    matches!(factory, "videotestsrc" | "gltestsrc")
}

fn factory_name(element: &gst::Element) -> Option<String> {
    element.factory().map(|factory| factory.name().to_string())
}

/// Whether an `autovideosrc` in `pipeline` settled on a test pattern. It does
/// so without an error when no camera can be opened, so the call would send
/// colour bars as if they were the user's picture.
fn auto_source_fell_back(pipeline: &gst::Pipeline) -> bool {
    pipeline
        .iterate_recurse()
        .into_iter()
        .filter_map(Result::ok)
        .any(|element| {
            factory_name(&element).is_some_and(|name| is_test_source(&name))
                && element
                    .parent()
                    .and_then(|parent| parent.downcast::<gst::Element>().ok())
                    .and_then(|parent| factory_name(&parent))
                    .is_some_and(|name| name == "autovideosrc")
        })
}

/// The camera, encoded for the call and mirrored for the local preview.
pub struct Camera {
    pipeline: gst::Pipeline,
    frames: async_channel::Receiver<Vec<u8>>,
    /// Declared after the pipeline so it closes once the pipeline has let go
    /// of its own copy.
    _remote: Option<OwnedFd>,
}

impl Camera {
    pub(super) fn start(
        encoder: H264Encoder,
        source: &str,
        slots: Arc<VideoSlots>,
        faults: FaultHandler,
    ) -> Result<Self, MediaError> {
        Self::launch(encoder, source, None, slots, faults)
    }

    /// Reads the camera through the PipeWire connection the portal gave, which
    /// the returned camera keeps open until it is dropped.
    pub(super) fn start_remote(
        encoder: H264Encoder,
        remote: camera_portal::Remote,
        slots: Arc<VideoSlots>,
        faults: FaultHandler,
    ) -> Result<Self, MediaError> {
        let source = remote.source();
        Self::launch(encoder, &source, Some(remote.into_fd()), slots, faults)
    }

    fn launch(
        encoder: H264Encoder,
        source: &str,
        remote: Option<OwnedFd>,
        slots: Arc<VideoSlots>,
        faults: FaultHandler,
    ) -> Result<Self, MediaError> {
        // 640 px wide keeps the bitrate and the encoder load low enough for a
        // relayed call. `config-interval=-1` repeats the stream parameters
        // with every keyframe, so a decoder that joins late can start.
        let pipeline = pipeline_from(&format!(
            "{source} ! videoconvert ! videoscale ! videorate ! \
             video/x-raw,width=640,pixel-aspect-ratio=1/1,framerate={FRAMES_PER_SECOND}/1 ! \
             tee name=split \
             split. ! queue max-size-buffers=2 leaky=downstream ! videoconvert ! {encoder} ! \
             h264parse config-interval=-1 ! \
             video/x-h264,stream-format=byte-stream,alignment=au ! \
             appsink name=encoded sync=false max-buffers=4 drop=true \
             split. ! queue max-size-buffers=1 leaky=downstream ! videoflip method=horizontal-flip ! \
             videoconvert ! videoscale ! \
             video/x-raw,format=RGBA,width=[1,{LOCAL_PREVIEW_WIDTH}] ! \
             appsink name=preview sync=false max-buffers=1 drop=true",
            encoder = encoder.description(),
        ))?;
        watch(&pipeline, &faults, Fault::Camera);
        let encoded = app_sink(&pipeline, "encoded")?;
        let preview = app_sink(&pipeline, "preview")?;
        let (sender, frames) = async_channel::bounded(FRAMES_QUEUED);
        encoded.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let _ = sender.try_send(map.as_slice().to_vec());
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        preview.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(frame) = rgba_of(&sample) {
                        slots.put_local(frame);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        if pipeline.set_state(gst::State::Playing).is_err() {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(MediaError::NoCamera);
        }
        // A camera that has not settled in 3 s is treated as missing.
        let (result, _, _) = pipeline.state(gst::ClockTime::from_seconds(3));
        if result.is_err() || auto_source_fell_back(&pipeline) {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(MediaError::NoCamera);
        }
        Ok(Self {
            pipeline,
            frames,
            _remote: remote,
        })
    }

    pub fn frames(&self) -> async_channel::Receiver<Vec<u8>> {
        self.frames.clone()
    }

    pub(super) fn pipeline(&self) -> gst::Pipeline {
        self.pipeline.clone()
    }
}

/// Asks a camera pipeline's encoder for a keyframe with fresh parameter sets.
pub(super) fn request_keyframe(pipeline: &gst::Pipeline) {
    let event = gst_video::UpstreamForceKeyUnitEvent::builder()
        .all_headers(true)
        .build();
    pipeline.send_event(event);
}

impl Drop for Camera {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        self.frames.close();
    }
}

/// Decodes the other side's access units into pictures for the window.
pub struct RemoteDecoder {
    pipeline: gst::Pipeline,
    frames: async_channel::Sender<VideoFrame>,
}

impl RemoteDecoder {
    pub(super) fn start(slots: Arc<VideoSlots>, faults: FaultHandler) -> Result<Self, MediaError> {
        let pipeline = pipeline_from(&format!(
            "appsrc name=in is-live=true format=time do-timestamp=true \
             caps=video/x-h264,stream-format=byte-stream,alignment=au ! \
             h264parse ! decodebin ! videoconvert ! videoscale ! \
             video/x-raw,format=RGBA,width=[1,{REMOTE_MAX_SIDE}],height=[1,{REMOTE_MAX_SIDE}],\
             pixel-aspect-ratio=1/1 ! \
             appsink name=out sync=false max-buffers=1 drop=true"
        ))?;
        watch(&pipeline, &faults, Fault::Decoder);
        let source = pipeline
            .by_name("in")
            .and_then(|element| element.downcast::<gst_app::AppSrc>().ok())
            .ok_or(MediaError::NoVideoDecoder)?;
        let output = app_sink(&pipeline, "out")?;
        output.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(frame) = rgba_of(&sample) {
                        slots.put_remote(frame);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        if pipeline.set_state(gst::State::Playing).is_err() {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(MediaError::NoVideoDecoder);
        }
        let (frames, incoming) = async_channel::bounded::<VideoFrame>(FRAMES_QUEUED);
        std::thread::Builder::new()
            .name("zaptide-call-video".to_owned())
            .spawn(move || {
                while let Ok(frame) = incoming.recv_blocking() {
                    if source
                        .push_buffer(gst::Buffer::from_slice(frame.data))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|_| MediaError::Unavailable)?;
        Ok(Self { pipeline, frames })
    }

    pub fn sender(&self) -> async_channel::Sender<VideoFrame> {
        self.frames.clone()
    }
}

impl Drop for RemoteDecoder {
    fn drop(&mut self) {
        self.frames.close();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn start_with(source: &str) -> Result<Camera, MediaError> {
        let encoder = available_encoder().expect("a caller checked for an encoder");
        let slots = Arc::new(VideoSlots::new(|| {}));
        Camera::start(encoder, source, slots, crate::calls::ignore_faults())
    }

    fn camera_test_ready() -> bool {
        let Some(encoder) = available_encoder() else {
            eprintln!("skipped: no H.264 encoder is installed");
            assert!(
                std::env::var_os("ZAPTIDE_REQUIRE_GST").is_none(),
                "an H.264 encoder is required"
            );
            return false;
        };
        crate::calls::gst_ready(&[
            "autovideosrc",
            "videotestsrc",
            "videoconvert",
            "videoflip",
            "videorate",
            encoder.element(),
        ])
    }

    #[test]
    fn only_test_patterns_count_as_test_sources() {
        assert!(is_test_source("videotestsrc"));
        assert!(is_test_source("gltestsrc"));
        for real in ["v4l2src", "pipewiresrc", "libcamerasrc", "fakesrc"] {
            assert!(!is_test_source(real), "{real}");
        }
    }

    #[test]
    fn auto_source_with_no_candidate_is_refused() {
        if !camera_test_ready() {
            return;
        }
        // No source accepts these caps, so autovideosrc takes its fallback
        // without opening any device, as it does on a machine with no camera.
        // pipewiresrc takes any caps, so it is left out of the choice.
        if let Some(pipewire) = gst::ElementFactory::find("pipewiresrc") {
            pipewire.set_rank(gst::Rank::NONE);
        }
        let started = start_with("autovideosrc filter-caps=video/x-nonexistent");
        assert_eq!(started.err(), Some(MediaError::NoCamera));
    }

    #[test]
    fn the_remote_descriptor_lives_and_dies_with_the_camera() {
        let Some(encoder) = available_encoder() else {
            eprintln!("skipped: no H.264 encoder is installed");
            return;
        };
        let elements = [
            "videotestsrc",
            "videoconvert",
            "videoflip",
            "videorate",
            encoder.element(),
        ];
        if !crate::calls::gst_ready(&elements) {
            return;
        }
        let marker = tempfile::NamedTempFile::new().expect("a marker file");
        let copy = || OwnedFd::from(marker.as_file().try_clone().expect("a copy"));
        let held = || crate::calls::open_descriptors_of(marker.path());
        assert_eq!(held(), 1);
        for _ in 0..20 {
            let slots = Arc::new(VideoSlots::new(|| {}));
            let camera = Camera::launch(
                encoder,
                "videotestsrc is-live=true",
                Some(copy()),
                slots,
                crate::calls::ignore_faults(),
            )
            .expect("the test camera starts");
            assert_eq!(held(), 2, "the camera keeps the connection open");
            drop(camera);
            assert_eq!(held(), 1, "dropping the camera closes it");
        }
    }

    #[test]
    fn a_camera_that_fails_to_start_closes_the_remote_descriptor() {
        let Some(encoder) = available_encoder() else {
            eprintln!("skipped: no H.264 encoder is installed");
            return;
        };
        let marker = tempfile::NamedTempFile::new().expect("a marker file");
        let copy = OwnedFd::from(marker.as_file().try_clone().expect("a copy"));
        let slots = Arc::new(VideoSlots::new(|| {}));
        let started = Camera::launch(
            encoder,
            "no-such-element-zaptide",
            Some(copy),
            slots,
            crate::calls::ignore_faults(),
        );
        assert!(started.is_err());
        assert_eq!(crate::calls::open_descriptors_of(marker.path()), 1);
    }

    #[test]
    #[ignore = "opens the real camera when the machine has one"]
    fn auto_source_is_a_camera_or_nothing() {
        if !camera_test_ready() {
            return;
        }
        match start_with("autovideosrc") {
            Ok(camera) => assert!(!auto_source_fell_back(&camera.pipeline())),
            Err(error) => assert_eq!(error, MediaError::NoCamera),
        }
    }

    #[test]
    fn camera_output_decodes_back_into_pictures() {
        let Some(encoder) = available_encoder() else {
            eprintln!("skipped: no H.264 encoder is installed");
            assert!(
                std::env::var_os("ZAPTIDE_REQUIRE_GST").is_none(),
                "an H.264 encoder is required"
            );
            return;
        };
        let decoder_elements = ["h264parse", "decodebin", "videoconvert", "videoscale"];
        let source_elements = ["videotestsrc", "videoconvert", "videoflip", "videorate"];
        if !decoder_available()
            || !crate::calls::gst_ready(&decoder_elements)
            || !crate::calls::gst_ready(&source_elements)
            || !crate::calls::gst_ready(&[encoder.element()])
        {
            return;
        }
        let pictures = Arc::new(AtomicUsize::new(0));
        let counted = pictures.clone();
        let slots = Arc::new(VideoSlots::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        let camera = Camera::start(
            encoder,
            "videotestsrc is-live=true pattern=ball",
            slots.clone(),
            crate::calls::ignore_faults(),
        )
        .expect("the test camera starts");
        let decoder = RemoteDecoder::start(slots.clone(), crate::calls::ignore_faults())
            .expect("the decoder starts");
        let frames = camera.frames();
        let sender = decoder.sender();

        let first = crate::calls::recv_within(&frames, Duration::from_secs(10))
            .expect("an access unit arrives");
        assert!(
            first.starts_with(&[0, 0, 0, 1]) || first.starts_with(&[0, 0, 1]),
            "access units are Annex-B"
        );
        assert!(
            whatsapp_rust::voip::VideoFrame::new(first.clone()).keyframe,
            "the first access unit starts the stream"
        );
        sender.try_send(VideoFrame::new(first)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut remote = None;
        while remote.is_none() && std::time::Instant::now() < deadline {
            if let Some(next) = crate::calls::recv_within(&frames, Duration::from_millis(500)) {
                let _ = sender.try_send(VideoFrame::new(next));
            }
            remote = slots.take().remote.flatten();
        }
        let remote = remote.expect("a decoded picture reaches the window");
        assert_eq!(remote.width, 640);
        assert_eq!(
            remote.data.len(),
            remote.width as usize * remote.height as usize * 4
        );
    }
}
