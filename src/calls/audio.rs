//! Microphone capture and speaker playout at the 16 kHz mono the VoIP runtime uses.

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;

use super::{Fault, FaultHandler, MediaError, watch};

/// Samples in one 60 ms frame at 16 kHz.
pub const FRAME_SAMPLES: usize = 960;
const FRAMES_QUEUED: usize = 8;

/// Cuts a byte stream of little-endian 16-bit samples into whole frames.
#[derive(Default)]
pub(super) struct Framer {
    pending: Vec<i16>,
    odd_byte: Option<u8>,
}

impl Framer {
    pub(super) fn push(&mut self, bytes: &[u8], mut emit: impl FnMut(Vec<i16>)) {
        let mut bytes = bytes;
        if let Some(low) = self.odd_byte.take() {
            let Some((&high, rest)) = bytes.split_first() else {
                self.odd_byte = Some(low);
                return;
            };
            self.pending.push(i16::from_le_bytes([low, high]));
            bytes = rest;
        }
        let (pairs, remainder) = bytes.as_chunks::<2>();
        self.odd_byte = remainder.first().copied();
        self.pending
            .extend(pairs.iter().map(|pair| i16::from_le_bytes(*pair)));
        while self.pending.len() >= FRAME_SAMPLES {
            let rest = self.pending.split_off(FRAME_SAMPLES);
            emit(std::mem::replace(&mut self.pending, rest));
        }
    }
}

fn pipeline_from(description: &str) -> Result<gst::Pipeline, MediaError> {
    gst::init().map_err(|error| MediaError::Unavailable(error.to_string()))?;
    gst::parse::launch(description)
        .map_err(|error| MediaError::Unavailable(error.to_string()))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| MediaError::Unavailable("not a pipeline".to_owned()))
}

fn settle(pipeline: &gst::Pipeline, missing: MediaError) -> Result<(), MediaError> {
    if pipeline.set_state(gst::State::Playing).is_err() {
        let _ = pipeline.set_state(gst::State::Null);
        return Err(missing);
    }
    let (result, _, _) = pipeline.state(gst::ClockTime::from_seconds(3));
    if result.is_err() {
        let _ = pipeline.set_state(gst::State::Null);
        return Err(missing);
    }
    Ok(())
}

/// The default microphone, delivering 60 ms frames.
pub struct Capture {
    pipeline: gst::Pipeline,
    frames: async_channel::Receiver<Vec<i16>>,
}

impl Capture {
    pub fn start(faults: FaultHandler) -> Result<Self, MediaError> {
        Self::with_source("autoaudiosrc", faults)
    }

    pub(super) fn with_source(source: &str, faults: FaultHandler) -> Result<Self, MediaError> {
        let pipeline = pipeline_from(&format!(
            "{source} ! audioconvert ! audioresample ! \
             audio/x-raw,format=S16LE,rate=16000,channels=1,layout=interleaved ! \
             appsink name=out sync=false max-buffers=8 drop=true"
        ))?;
        let sink = pipeline
            .by_name("out")
            .and_then(|element| element.downcast::<gst_app::AppSink>().ok())
            .ok_or(MediaError::NoMicrophone)?;
        let (sender, frames) = async_channel::bounded(FRAMES_QUEUED);
        let mut framer = Framer::default();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    framer.push(map.as_slice(), |frame| {
                        let _ = sender.try_send(frame);
                    });
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        watch(&pipeline, &faults, Fault::Audio);
        settle(&pipeline, MediaError::NoMicrophone)?;
        Ok(Self { pipeline, frames })
    }

    pub fn frames(&self) -> async_channel::Receiver<Vec<i16>> {
        self.frames.clone()
    }

    pub(super) fn pipeline(&self) -> gst::Pipeline {
        self.pipeline.clone()
    }
}

/// Stops capturing while inactive, so a muted call listens to nothing.
pub(super) fn set_pipeline_active(pipeline: &gst::Pipeline, active: bool) {
    let state = if active {
        gst::State::Playing
    } else {
        gst::State::Paused
    };
    let _ = pipeline.set_state(state);
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        self.frames.close();
    }
}

/// The default audio output, playing frames pushed into [`Playout::sender`].
pub struct Playout {
    pipeline: gst::Pipeline,
    frames: async_channel::Sender<Vec<i16>>,
}

impl Playout {
    pub fn start(faults: FaultHandler) -> Result<Self, MediaError> {
        Self::with_sink("autoaudiosink", faults)
    }

    pub(super) fn with_sink(sink: &str, faults: FaultHandler) -> Result<Self, MediaError> {
        let pipeline = pipeline_from(&format!(
            "appsrc name=in is-live=true format=time do-timestamp=true max-bytes=64000 \
             leaky-type=downstream \
             caps=audio/x-raw,format=S16LE,rate=16000,channels=1,layout=interleaved ! \
             audioconvert ! audioresample ! {sink}"
        ))?;
        let source = pipeline
            .by_name("in")
            .and_then(|element| element.downcast::<gst_app::AppSrc>().ok())
            .ok_or(MediaError::NoSpeaker)?;
        watch(&pipeline, &faults, Fault::Audio);
        settle(&pipeline, MediaError::NoSpeaker)?;
        let (frames, incoming) = async_channel::bounded::<Vec<i16>>(FRAMES_QUEUED);
        std::thread::Builder::new()
            .name("zaptide-call-playout".to_owned())
            .spawn(move || {
                while let Ok(frame) = incoming.recv_blocking() {
                    let bytes: Vec<u8> = frame
                        .iter()
                        .flat_map(|sample| sample.to_le_bytes())
                        .collect();
                    if source.push_buffer(gst::Buffer::from_slice(bytes)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| MediaError::Unavailable(error.to_string()))?;
        Ok(Self { pipeline, frames })
    }

    pub fn sender(&self) -> async_channel::Sender<Vec<i16>> {
        self.frames.clone()
    }
}

impl Drop for Playout {
    fn drop(&mut self) {
        self.frames.close();
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(samples: &[i16]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    #[test]
    fn framer_emits_whole_sixty_millisecond_frames_across_uneven_buffers() {
        let samples: Vec<i16> = (0..2500).map(|n| n as i16).collect();
        let encoded = bytes(&samples);
        let mut framer = Framer::default();
        let mut frames = Vec::new();
        for chunk in encoded.chunks(1001) {
            framer.push(chunk, |frame| frames.push(frame));
        }
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|frame| frame.len() == FRAME_SAMPLES));
        let flat: Vec<i16> = frames.concat();
        assert_eq!(flat, samples[..2 * FRAME_SAMPLES]);
    }

    #[test]
    fn framer_waits_for_the_second_byte_of_a_split_sample() {
        let mut framer = Framer::default();
        let mut frames = Vec::new();
        let encoded = bytes(&vec![7; FRAME_SAMPLES]);
        framer.push(&encoded[..encoded.len() - 1], |frame| frames.push(frame));
        assert!(frames.is_empty());
        framer.push(&encoded[encoded.len() - 1..], |frame| frames.push(frame));
        assert_eq!(frames, [vec![7; FRAME_SAMPLES]]);
    }

    #[test]
    fn test_tone_goes_through_capture_and_playout_pipelines() {
        if !crate::calls::gst_ready(&["audiotestsrc", "fakesink", "audioconvert", "audioresample"])
        {
            return;
        }
        let capture = Capture::with_source(
            "audiotestsrc is-live=true freq=440 samplesperbuffer=480",
            crate::calls::ignore_faults(),
        )
        .expect("test source starts");
        let frame =
            crate::calls::recv_within(&capture.frames(), std::time::Duration::from_secs(10))
                .expect("a captured frame arrives");
        assert_eq!(frame.len(), FRAME_SAMPLES);
        assert!(frame.iter().any(|sample| *sample != 0));

        let playout = Playout::with_sink("fakesink sync=false", crate::calls::ignore_faults())
            .expect("test sink starts");
        playout.sender().try_send(frame).expect("accepts audio");
    }

    #[test]
    fn muting_pauses_the_microphone_and_unmuting_resumes_it() {
        if !crate::calls::gst_ready(&["audiotestsrc", "fakesink", "audioconvert", "audioresample"])
        {
            return;
        }
        let capture = Capture::with_source(
            "audiotestsrc is-live=true freq=440 samplesperbuffer=480",
            crate::calls::ignore_faults(),
        )
        .expect("test source starts");
        let pipeline = capture.pipeline();
        set_pipeline_active(&pipeline, false);
        let settled = |pipeline: &gst::Pipeline| pipeline.state(gst::ClockTime::from_seconds(3)).1;
        assert_eq!(settled(&pipeline), gst::State::Paused);
        set_pipeline_active(&pipeline, true);
        assert_eq!(settled(&pipeline), gst::State::Playing);
    }
}
