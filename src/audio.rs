//! Audio playback and voice-message recording.
//!
//! Input and output devices are opened on demand and released when idle.

use std::collections::HashMap;
use std::num::NonZero;
use std::path::Path;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

use rodio::buffer::SamplesBuffer;

use crate::voice;

mod decode;
mod recorder;

use decode::{
    DecodePermit, Decoded, LONGEST_RECORDING, decode_file_with, decode_file_with_waveform,
    publish_decode,
};
#[cfg(test)]
use decode::{
    MAX_AUDIO_BYTES, MAX_AUDIO_SAMPLES, MAX_DECODE_JOBS, audio_limit_error,
    collect_samples_limited, collect_samples_with_limits, decode_file, decode_opus,
    input_frame_limit, read_audio, try_acquire_decode_job,
};

#[cfg(test)]
use recorder::record_with;
pub use recorder::{Recorder, recording_path};

fn mono() -> NonZero<u16> {
    NonZero::<u16>::MIN
}

fn rate() -> NonZero<u32> {
    NonZero::new(voice::RATE).expect("48 kHz is not zero")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Loading,
    Playing,
    Paused,
}

/// Playback state for one message.
#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub state: State,
    pub position: Duration,
    pub total: Duration,
}

impl Status {
    const IDLE: Self = Self {
        state: State::Idle,
        position: Duration::ZERO,
        total: Duration::ZERO,
    };
}

/// Playback speeds, in the order the speed button cycles them.
pub const SPEEDS: [f32; 3] = [1.0, 1.5, 2.0];

/// Label for a playback speed, like `1x` or `1.5x`.
pub fn speed_label(speed: f32) -> String {
    if speed.fract() == 0.0 {
        format!("{}x", speed as i32)
    } else {
        format!("{speed:.1}x")
    }
}

/// Plays one clip at a time through the default output device.
pub struct Player {
    output: Option<(rodio::MixerDeviceSink, rodio::Player)>,
    loaded: Option<Loaded>,
    decoding: Option<Decoding>,
    /// Playback speed applied to the current clip and to later ones.
    speed: f32,
    /// Time-compressed copies of the loaded clip, one per speed already
    /// built, dropped when the clip changes.
    stretches: Vec<(f32, Arc<Vec<f32>>)>,
    /// Compression being built for the loaded message.
    stretching: Option<Stretching>,
    /// Generated waveforms for clips that did not include one.
    bars: HashMap<String, Vec<u8>>,
}

struct Loaded {
    message: String,
    samples: Arc<Vec<f32>>,
    /// Samples queued in the sink: the clip itself or its compression.
    buffer: Arc<Vec<f32>>,
    /// Speed the queued buffer represents; 1 plays the clip as recorded.
    factor: f32,
    /// Restart position in the clip's own timeline.
    base: Duration,
    paused: bool,
    done: bool,
}

struct Stretching {
    factor: f32,
    slot: StretchedSlot,
    /// Set when this job is replaced or the clip changes, so the worker
    /// stops instead of piling up behind the next one.
    cancelled: Arc<AtomicBool>,
}

impl Drop for Stretching {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

type StretchedSlot = Arc<Mutex<Option<Arc<Vec<f32>>>>>;

struct Decoding {
    message: String,
    /// Requested start position after decoding, from 0 to 1.
    start: f32,
    slot: Decoded,
    cancelled: Arc<AtomicBool>,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            output: None,
            loaded: None,
            decoding: None,
            speed: SPEEDS[0],
            stretches: Vec::new(),
            stretching: None,
            bars: HashMap::new(),
        }
    }
}

impl Player {
    /// Current playback speed multiplier.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Sets the playback speed for the clip playing now and for later ones.
    ///
    /// Speeds above 1x play a time-compressed copy of the clip, once it has
    /// been built, so the voice keeps its pitch. Until then playback
    /// continues at the speed already queued. Speeds outside 1x to 2x, such
    /// as a hand-edited setting, are clamped, and non-finite ones play at 1x.
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = if speed.is_finite() {
            speed.clamp(SPEEDS[0], SPEEDS[SPEEDS.len() - 1])
        } else {
            SPEEDS[0]
        };
        self.apply_speed();
        self.ensure_stretch();
    }

    /// Cycles 1x, 1.5x, and 2x, wrapping back to 1x.
    pub fn cycle_speed(&mut self) -> f32 {
        let next = SPEEDS
            .iter()
            .copied()
            .find(|&candidate| candidate > self.speed)
            .unwrap_or(SPEEDS[0]);
        self.set_speed(next);
        self.speed
    }

    /// The samples that play at `speed` and the speed they represent: the
    /// clip itself at 1x, its compression once built, and otherwise whatever
    /// is queued, so a speed still building does not drop playback to 1x.
    fn buffer_for(
        loaded: &Loaded,
        stretches: &[(f32, Arc<Vec<f32>>)],
        speed: f32,
    ) -> (Arc<Vec<f32>>, f32) {
        if speed <= 1.0 {
            return (Arc::clone(&loaded.samples), 1.0);
        }
        match stretches.iter().find(|(factor, _)| *factor == speed) {
            Some((factor, compressed)) => (Arc::clone(compressed), *factor),
            None => (Arc::clone(&loaded.buffer), loaded.factor),
        }
    }

    /// Restarts playback on the buffer for the current speed, keeping the
    /// position, when it differs from what is queued.
    fn apply_speed(&mut self) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let (wanted, _) = Self::buffer_for(loaded, &self.stretches, self.speed);
        if self.output.is_none() || Arc::ptr_eq(&wanted, &loaded.buffer) {
            return;
        }
        let total = clip_length(loaded.samples.len());
        let fraction = if total > Duration::ZERO {
            (self.status(&loaded.message).position.as_secs_f64() / total.as_secs_f64()) as f32
        } else {
            0.0
        }
        .clamp(0.0, 1.0);
        let paused = loaded.paused;
        if self.restart(fraction).is_ok() && paused {
            if let Some((_, sink)) = &self.output {
                sink.pause();
            }
            if let Some(loaded) = self.loaded.as_mut() {
                loaded.paused = true;
            }
        }
    }

    /// Builds the compression for the current speed in the background, if it
    /// is still missing. Replacing an outstanding job cancels it, and so does
    /// going back to 1x, which needs none.
    fn ensure_stretch(&mut self) {
        let factor = self.speed;
        if factor <= 1.0 {
            self.stretching = None;
            return;
        }
        if self.stretches.iter().any(|(built, _)| *built == factor)
            || self
                .stretching
                .as_ref()
                .is_some_and(|job| job.factor == factor)
        {
            return;
        }
        let Some(loaded) = &self.loaded else {
            return;
        };
        let samples = Arc::clone(&loaded.samples);
        let slot: StretchedSlot = Default::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let thread_slot = Arc::clone(&slot);
        let thread_cancelled = Arc::clone(&cancelled);
        let spawned = std::thread::Builder::new()
            .name("voice-stretch".to_owned())
            .spawn(move || {
                let Some(compressed) =
                    crate::timestretch::speed_up_unless(&samples, factor, &thread_cancelled)
                else {
                    return;
                };
                *thread_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(compressed));
            });
        if spawned.is_ok() {
            self.stretching = Some(Stretching {
                factor,
                slot,
                cancelled,
            });
        }
    }

    /// Plays or pauses a message. Finished clips restart; new clips decode first.
    pub fn toggle(&mut self, message: &str, path: &Path) -> Result<(), String> {
        match self.loaded.as_mut() {
            Some(loaded) if loaded.message == message => {
                if loaded.done {
                    return self.restart(0.0);
                }
                if let Some((_, sink)) = &self.output {
                    if loaded.paused {
                        sink.play();
                    } else {
                        sink.pause();
                    }
                    loaded.paused = !loaded.paused;
                }
                Ok(())
            }
            _ => self.load(message, path, 0.0),
        }
    }

    /// Seeks to a fraction from 0 to 1 and starts playback.
    pub fn seek(&mut self, message: &str, path: &Path, fraction: f32) -> Result<(), String> {
        match &self.loaded {
            Some(loaded) if loaded.message == message => self.restart(fraction),
            _ => self.load(message, path, fraction),
        }
    }

    /// Clears the loaded clip and releases the output device.
    pub fn stop(&mut self) {
        self.output = None;
        self.loaded = None;
        if let Some(decoding) = &self.decoding {
            decoding.cancelled.store(true, Ordering::Relaxed);
        }
        self.decoding = None;
        self.stretches.clear();
        self.stretching = None;
    }

    pub fn is_playing(&self) -> bool {
        self.decoding.is_some()
            || self
                .loaded
                .as_ref()
                .is_some_and(|loaded| !loaded.paused && !loaded.done)
    }

    pub fn status(&self, message: &str) -> Status {
        if let Some(decoding) = &self.decoding
            && decoding.message == message
        {
            return Status {
                state: State::Loading,
                ..Status::IDLE
            };
        }
        match &self.loaded {
            Some(loaded) if loaded.message == message => {
                let total = clip_length(loaded.samples.len());
                if loaded.done {
                    return Status {
                        state: State::Idle,
                        position: Duration::ZERO,
                        total,
                    };
                }
                let position = self
                    .output
                    .as_ref()
                    .map(|(_, sink)| loaded.base + sink.get_pos().mul_f32(loaded.factor))
                    .unwrap_or(loaded.base)
                    .min(total);
                Status {
                    state: if loaded.paused {
                        State::Paused
                    } else {
                        State::Playing
                    },
                    position,
                    total,
                }
            }
            _ => Status::IDLE,
        }
    }

    /// Generated waveform for a decoded clip.
    pub fn bars(&self, message: &str) -> Option<&[u8]> {
        self.bars.get(message).map(Vec::as_slice)
    }

    /// Handles completed decodes and finished playback; call periodically.
    pub fn poll(&mut self) -> Result<(), String> {
        let decoded = self.decoding.as_ref().and_then(|decoding| {
            decoding
                .slot
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
        });
        if let Some(result) = decoded {
            let Decoding { message, start, .. } = self.decoding.take().expect("just seen");
            let (samples, bars) = result?;
            if samples.is_empty() {
                return Err("The clip is empty".to_owned());
            }
            let samples = Arc::new(samples);
            self.bars.entry(message.clone()).or_insert(bars);
            self.loaded = Some(Loaded {
                message,
                buffer: Arc::clone(&samples),
                factor: 1.0,
                samples,
                base: Duration::ZERO,
                paused: false,
                done: false,
            });
            self.restart(start)?;
            self.ensure_stretch();
        }
        let compressed = self
            .stretching
            .as_ref()
            .and_then(|job| job.slot.lock().unwrap_or_else(|p| p.into_inner()).take());
        if let Some(samples) = compressed {
            let factor = self.stretching.take().expect("just seen").factor;
            self.stretches.push((factor, samples));
            self.apply_speed();
            // The speed may have moved on while this compression built.
            self.ensure_stretch();
        }
        let ended = self
            .loaded
            .as_mut()
            .zip(self.output.as_ref())
            .is_some_and(|(loaded, (_, sink))| playback_ended(loaded, sink.empty()));
        if ended {
            self.output = None;
        }
        Ok(())
    }

    fn load(&mut self, message: &str, path: &Path, start: f32) -> Result<(), String> {
        self.stop();
        let permit = DecodePermit::acquire()
            .ok_or_else(|| "Too many audio clips are decoding".to_owned())?;
        let slot: Decoded = Default::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        let path = path.to_owned();
        let thread_slot = Arc::clone(&slot);
        let thread_cancelled = Arc::clone(&cancelled);
        let spawned = std::thread::Builder::new()
            .name("voice-decode".to_owned())
            .spawn(move || {
                let _permit = permit;
                let result = decode_file_with_waveform(&path, &thread_cancelled);
                drop(_permit);
                if publish_decode(&thread_slot, &thread_cancelled, result) {}
            });
        if let Err(error) = spawned {
            return Err(format!("Could not decode audio: {error}"));
        }
        self.decoding = Some(Decoding {
            message: message.to_owned(),
            start,
            slot,
            cancelled,
        });
        Ok(())
    }

    /// Plays the loaded clip from a fraction from 0 to 1.
    fn restart(&mut self, fraction: f32) -> Result<(), String> {
        let Some(loaded) = self.loaded.as_mut() else {
            return Ok(());
        };
        // The sink always plays at 1x: running it faster sharpens the voice,
        // so speeds above 1x queue a time-compressed copy of the clip.
        let (buffer, factor) = Self::buffer_for(loaded, &self.stretches, self.speed);
        let total = clip_length(loaded.samples.len());
        let offset = ((fraction.clamp(0.0, 1.0) * buffer.len() as f32) as usize).min(buffer.len());
        if self.output.is_none() {
            let mut device = open_device(
                "No sound output",
                rodio::DeviceSinkBuilder::open_default_sink,
            )?;
            // The player deliberately releases or replaces its output device.
            // Rodio otherwise prints a warning on every intentional drop.
            device.log_on_drop(false);
            let sink = rodio::Player::connect_new(device.mixer());
            self.output = Some((device, sink));
        }
        let (_, sink) = self.output.as_ref().expect("just opened");
        sink.clear();
        sink.append(SamplesBuffer::new(
            mono(),
            rate(),
            buffer[offset..].to_vec(),
        ));
        sink.play();
        loaded.buffer = buffer;
        loaded.factor = factor;
        loaded.base =
            Duration::from_secs_f64(fraction.clamp(0.0, 1.0) as f64 * total.as_secs_f64());
        loaded.paused = false;
        loaded.done = false;
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Some(decoding) = &self.decoding {
            decoding.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

fn clip_length(samples: usize) -> Duration {
    Duration::from_secs_f64(samples as f64 / f64::from(voice::RATE))
}

fn open_device<T, E: std::fmt::Display>(
    description: &str,
    open: impl FnOnce() -> Result<T, E>,
) -> Result<T, String> {
    open().map_err(|error| format!("{description}: {error}"))
}

fn playback_ended(loaded: &mut Loaded, sink_empty: bool) -> bool {
    if loaded.done || loaded.paused || !sink_empty {
        return false;
    }
    loaded.done = true;
    true
}

/// Generate bounded waveform data off the GTK thread for an attachment.
pub fn waveform_file(path: &Path) -> Result<Vec<u8>, String> {
    waveform_file_cancellable(path, &AtomicBool::new(false))
}

pub(crate) fn waveform_file_cancellable(
    path: &Path,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let _permit =
        DecodePermit::acquire().ok_or_else(|| "Too many audio clips are decoding".to_owned())?;
    waveform_cancellable(&decode_file_with(path, cancelled)?, cancelled)
}

fn waveform_cancellable(samples: &[f32], cancelled: &AtomicBool) -> Result<Vec<u8>, String> {
    waveform_with_cancel_check(samples, || cancelled.load(Ordering::Acquire))
}

fn waveform_with_cancel_check(
    samples: &[f32],
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<Vec<u8>, String> {
    const CANCEL_CHECK_INTERVAL: usize = 4_096;
    if is_cancelled() {
        return Err("Audio decoding cancelled".to_owned());
    }
    if samples.is_empty() {
        return Ok(vec![0; voice::BARS]);
    }

    let slice = samples.len().div_ceil(voice::BARS);
    let mut loudness = Vec::with_capacity(voice::BARS);
    for chunk in samples.chunks(slice) {
        let mut sum = 0.0f32;
        for (index, sample) in chunk.iter().enumerate() {
            if index % CANCEL_CHECK_INTERVAL == 0 && is_cancelled() {
                return Err("Audio decoding cancelled".to_owned());
            }
            sum += sample * sample;
        }
        loudness.push((sum / chunk.len() as f32).sqrt());
    }
    if is_cancelled() {
        return Err("Audio decoding cancelled".to_owned());
    }

    let loudest = loudness.iter().copied().fold(0.0f32, f32::max);
    let mut bars: Vec<u8> = loudness
        .iter()
        .map(|value| {
            if loudest > 0.0 {
                (value / loudest * 100.0).round() as u8
            } else {
                0
            }
        })
        .collect();
    bars.resize(voice::BARS, 0);
    if is_cancelled() {
        return Err("Audio decoding cancelled".to_owned());
    }
    Ok(bars)
}

#[cfg(test)]
mod tests;
