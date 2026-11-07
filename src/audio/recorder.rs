//! Microphone capture and live recording levels.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;

use super::LONGEST_RECORDING;
use super::pulled::PulledSamples;
use crate::voice;

/// The same source the calls' microphone uses: GStreamer picks PipeWire or
/// PulseAudio, which reach the host through the Flatpak pulseaudio socket.
const MICROPHONE: &str = "autoaudiosrc";

type Outcome = Arc<Mutex<Option<Result<Vec<f32>, String>>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Records from the default microphone until told to stop.
pub struct Recorder {
    started: Instant,
    stop: Arc<AtomicBool>,
    /// Loudness for each recorded 50 ms segment.
    levels: Arc<Mutex<Vec<f32>>>,
    outcome: Outcome,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Recorder {
    pub fn start() -> Self {
        Self::with_source(MICROPHONE)
    }

    /// Test-only hook to record from a GStreamer source description instead of
    /// the microphone. Never pass untrusted input as the source description.
    #[doc(hidden)]
    pub fn with_source(source: &str) -> Self {
        let source = source.to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let levels: Arc<Mutex<Vec<f32>>> = Default::default();
        let outcome: Outcome = Default::default();
        let spawned = {
            let stop = Arc::clone(&stop);
            let levels = Arc::clone(&levels);
            let outcome = Arc::clone(&outcome);
            std::thread::Builder::new()
                .name("voice-record".to_owned())
                .spawn(move || {
                    let result = record(&source, &stop, &levels);
                    *lock(&outcome) = Some(result);
                })
        };
        let thread = match spawned {
            Ok(thread) => Some(thread),
            Err(error) => {
                *lock(&outcome) = Some(Err(error.to_string()));
                None
            }
        };
        Self {
            started: Instant::now(),
            stop,
            levels,
            outcome,
            thread,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn levels(&self) -> Vec<f32> {
        lock(&self.levels).clone()
    }

    /// The newest `count` readings, without copying the whole recording.
    pub fn recent_levels(&self, count: usize) -> Vec<f32> {
        let levels = lock(&self.levels);
        levels[levels.len().saturating_sub(count)..].to_vec()
    }

    /// Error that stopped recording early.
    pub fn failure(&self) -> Option<String> {
        match lock(&self.outcome).as_ref() {
            Some(Err(error)) => Some(error.clone()),
            _ => None,
        }
    }

    /// Stops and returns mono 48 kHz samples.
    pub fn finish(mut self) -> Result<Vec<f32>, String> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        lock(&self.outcome)
            .take()
            .unwrap_or_else(|| Err("No audio was recorded".to_owned()))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn record(source: &str, stop: &AtomicBool, levels: &Mutex<Vec<f32>>) -> Result<Vec<f32>, String> {
    let mut microphone = open_microphone(source, stop)?;
    let recorded = record_with(stop, levels, || Ok((&mut microphone, 1, voice::RATE)));
    match (recorded, microphone.failure()) {
        // A device the sound server refuses only fails once it starts.
        (Err(_), Some(error)) => Err(format!("Could not open the microphone: {error}")),
        (recorded, _) => recorded,
    }
}

/// Mono 48 kHz samples from `source` until it ends, fails, or `stop` is set
/// while it is quiet.
fn open_microphone<'a>(
    source: &str,
    stop: &'a AtomicBool,
) -> Result<PulledSamples<impl FnMut(Duration) -> Result<bool, String> + 'a>, String> {
    let unavailable = |error: &dyn std::fmt::Display| format!("No microphone available: {error}");
    gst::init().map_err(|error| unavailable(&error))?;
    let pipeline = gst::parse::launch(&format!(
        "{source} ! audioconvert ! audioresample ! \
         audio/x-raw,format=F32LE,rate={rate},channels=1,layout=interleaved ! \
         appsink name=out sync=false",
        rate = voice::RATE
    ))
    .map_err(|error| unavailable(&error))?
    .downcast::<gst::Pipeline>()
    .map_err(|_| unavailable(&"not a pipeline"))?;
    let sink = pipeline
        .by_name("out")
        .and_then(|element| element.downcast::<gst_app::AppSink>().ok())
        .ok_or_else(|| unavailable(&"no audio sink"))?;
    let samples = PulledSamples::start(pipeline.clone(), sink, move |_| {
        Ok(!stop.load(Ordering::Relaxed))
    })
    .map_err(|error| format!("Could not open the microphone: {error}"))?;
    if plays_silence(&pipeline) {
        return Err(unavailable(&"no sound server or microphone was found"));
    }
    Ok(samples)
}

/// Whether `autoaudiosrc` found no usable source and fell back to its
/// silent test source, which would record silence instead of failing.
fn plays_silence(pipeline: &gst::Pipeline) -> bool {
    pipeline.iterate_recurse().into_iter().any(|element| {
        element.is_ok_and(|element| {
            element.name().starts_with("fake-auto-")
                && element
                    .factory()
                    .is_some_and(|factory| factory.name() == "audiotestsrc")
        })
    })
}

pub(super) fn record_with<I: Iterator<Item = f32>>(
    stop: &AtomicBool,
    levels: &Mutex<Vec<f32>>,
    open: impl FnOnce() -> Result<(I, u16, u32), String>,
) -> Result<Vec<f32>, String> {
    let (mut microphone, channels, rate) = open()?;
    let chunk = (rate as usize * usize::from(channels) / 20).max(1);
    let started = Instant::now();
    let mut heard = Vec::new();
    while !stop.load(Ordering::Relaxed) && started.elapsed() < LONGEST_RECORDING {
        let before = heard.len();
        heard.extend(microphone.by_ref().take(chunk));
        let taken = &heard[before..];
        if taken.is_empty() {
            break;
        }
        let loudness = (taken.iter().map(|s| s * s).sum::<f32>() / taken.len() as f32).sqrt();
        lock(levels).push(loudness);
        if taken.len() < chunk {
            // The device disappeared before recording stopped.
            break;
        }
    }
    if heard.is_empty() {
        return Err("The microphone did not record any audio".to_owned());
    }
    Ok(voice::mono_at_rate(&heard, channels, rate))
}
