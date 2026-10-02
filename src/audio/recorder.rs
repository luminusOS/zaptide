//! Microphone capture and live recording levels.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rodio::Source;

use super::LONGEST_RECORDING;
use crate::voice;

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
                    let result = record(&stop, &levels);
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

fn record(stop: &AtomicBool, levels: &Mutex<Vec<f32>>) -> Result<Vec<f32>, String> {
    record_with(stop, levels, || {
        let microphone = rodio::microphone::MicrophoneBuilder::new()
            .default_device()
            .map_err(|error| format!("No microphone available: {error}"))?
            .default_config()
            .map_err(|error| format!("The microphone has no supported format: {error}"))?
            .open_stream()
            .map_err(|error| format!("Could not open the microphone: {error}"))?;
        let channels = microphone.channels().get();
        let rate = microphone.sample_rate().get();
        Ok((microphone, channels, rate))
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

/// Temporary recording path used before sending and archiving.
#[allow(dead_code)]
pub fn recording_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(format!("voice-{}.ogg", crate::util::now()))
}
