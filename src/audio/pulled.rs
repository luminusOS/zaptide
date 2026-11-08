//! Float samples pulled from a GStreamer pipeline that ends in an appsink.

use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;

const PULL_TIMEOUT: gst::ClockTime = gst::ClockTime::from_mseconds(100);

/// Interleaved `F32LE` samples from `sink`, in order, until end of stream,
/// a pipeline error, or `wait` gives up.
///
/// `wait` runs whenever no buffer arrived within 100 ms, with the time since
/// the last one: `Ok(true)` keeps waiting, `Ok(false)` ends the samples, and
/// `Err` ends them with that failure.
pub(super) struct PulledSamples<W> {
    pipeline: gst::Pipeline,
    sink: gst_app::AppSink,
    buffer: Vec<f32>,
    index: usize,
    format: Option<(u16, u32)>,
    last_data: Instant,
    wait: W,
    deadline: Option<(Instant, String)>,
    failure: Option<String>,
}

impl<W: FnMut(Duration) -> Result<bool, String>> PulledSamples<W> {
    /// Starts `pipeline`; the error is the pipeline's own message when it
    /// posted one.
    pub(super) fn start(
        pipeline: gst::Pipeline,
        sink: gst_app::AppSink,
        wait: W,
    ) -> Result<Self, String> {
        let samples = Self {
            pipeline,
            sink,
            buffer: Vec::new(),
            index: 0,
            format: None,
            last_data: Instant::now(),
            wait,
            deadline: None,
            failure: None,
        };
        if let Err(error) = samples.pipeline.set_state(gst::State::Playing) {
            return Err(samples.bus_error().unwrap_or_else(|| error.to_string()));
        }
        Ok(samples)
    }

    /// Ends the samples with `error` once `at` has passed, however steadily
    /// they arrive.
    pub(super) fn with_deadline(mut self, at: Instant, error: String) -> Self {
        self.deadline = Some((at, error));
        self
    }

    /// Channels and rate of the stream, read from its first buffer.
    pub(super) fn format(&mut self) -> Option<(u16, u32)> {
        if self.format.is_none() && self.index >= self.buffer.len() {
            self.fill();
        }
        self.format
    }

    /// The error that ended the samples early, if any.
    pub(super) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    fn bus_error(&self) -> Option<String> {
        let message = self
            .pipeline
            .bus()?
            .pop_filtered(&[gst::MessageType::Error])?;
        match message.view() {
            gst::MessageView::Error(error) => Some(error.error().to_string()),
            _ => None,
        }
    }

    fn fill(&mut self) -> bool {
        loop {
            if self.failure.is_some() {
                return false;
            }
            if let Some((at, error)) = &self.deadline
                && Instant::now() >= *at
            {
                self.failure = Some(error.clone());
                return false;
            }
            if let Some(sample) = self.sink.try_pull_sample(PULL_TIMEOUT) {
                self.last_data = Instant::now();
                let format = sample.caps().and_then(caps_format);
                match self.format {
                    None => self.format = format,
                    // The samples are read at the first format's rate and
                    // channels; anything else would be misread.
                    Some(known) if format != Some(known) => {
                        self.failure = Some("the audio format changed mid-stream".to_owned());
                        return false;
                    }
                    Some(_) => {}
                }
                let Some(buffer) = sample.buffer() else {
                    continue;
                };
                let Ok(map) = buffer.map_readable() else {
                    self.failure = Some("unreadable audio buffer".to_owned());
                    return false;
                };
                let (bytes, _) = map.as_slice().as_chunks::<4>();
                self.buffer.clear();
                self.buffer
                    .extend(bytes.iter().map(|bytes| f32::from_le_bytes(*bytes)));
                self.index = 0;
                return true;
            }
            if let Some(error) = self.bus_error() {
                self.failure = Some(error);
                return false;
            }
            if self.sink.is_eos() {
                return false;
            }
            match (self.wait)(self.last_data.elapsed()) {
                Ok(true) => {}
                Ok(false) => return false,
                Err(error) => {
                    self.failure = Some(error);
                    return false;
                }
            }
        }
    }
}

fn caps_format(caps: &gst::CapsRef) -> Option<(u16, u32)> {
    let structure = caps.structure(0)?;
    let channels = u16::try_from(structure.get::<i32>("channels").ok()?).ok()?;
    let rate = u32::try_from(structure.get::<i32>("rate").ok()?).ok()?;
    Some((channels, rate))
}

impl<W: FnMut(Duration) -> Result<bool, String>> Iterator for PulledSamples<W> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        loop {
            if let Some(&sample) = self.buffer.get(self.index) {
                self.index += 1;
                return Some(sample);
            }
            if !self.fill() {
                return None;
            }
        }
    }
}

impl<W> Drop for PulledSamples<W> {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
