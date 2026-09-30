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
mod tests {
    use super::*;
    use std::io;
    use std::sync::Barrier;

    #[test]
    fn speed_labels_match_the_button() {
        assert_eq!(speed_label(SPEEDS[0]), "1x");
        assert_eq!(speed_label(1.5), "1.5x");
        assert_eq!(speed_label(SPEEDS[2]), "2x");
    }

    #[test]
    fn cycling_wraps_through_every_speed() {
        let mut player = Player::default();
        assert_eq!(player.speed(), SPEEDS[0]);
        assert_eq!(player.cycle_speed(), 1.5);
        assert_eq!(player.cycle_speed(), 2.0);
        assert_eq!(player.cycle_speed(), 1.0);
        // A speed set by hand still cycles up to the next known one.
        player.set_speed(1.75);
        assert_eq!(player.cycle_speed(), 2.0);
        assert_eq!(player.speed(), 2.0);
    }

    #[test]
    fn a_speed_still_building_keeps_the_queued_one() {
        let samples = Arc::new(vec![0.0; 12]);
        let one_and_a_half = Arc::new(vec![0.0; 8]);
        let double = Arc::new(vec![0.0; 6]);
        let loaded = Loaded {
            message: "clip".to_owned(),
            samples: Arc::clone(&samples),
            buffer: Arc::clone(&one_and_a_half),
            factor: 1.5,
            base: Duration::ZERO,
            paused: false,
            done: false,
        };
        let mut stretches = vec![(1.5, Arc::clone(&one_and_a_half))];

        let (buffer, factor) = Player::buffer_for(&loaded, &stretches, 2.0);
        assert!(Arc::ptr_eq(&buffer, &one_and_a_half));
        assert_eq!(factor, 1.5);

        stretches.push((2.0, Arc::clone(&double)));
        let (buffer, factor) = Player::buffer_for(&loaded, &stretches, 2.0);
        assert!(Arc::ptr_eq(&buffer, &double));
        assert_eq!(factor, 2.0);
        // Both built speeds stay available when cycling back.
        let (buffer, _) = Player::buffer_for(&loaded, &stretches, 1.5);
        assert!(Arc::ptr_eq(&buffer, &one_and_a_half));
        let (buffer, factor) = Player::buffer_for(&loaded, &stretches, 1.0);
        assert!(Arc::ptr_eq(&buffer, &samples));
        assert_eq!(factor, 1.0);
    }

    #[test]
    fn unusable_speeds_are_kept_in_range() {
        let mut player = Player::default();
        player.set_speed(f32::NAN);
        assert_eq!(player.speed(), 1.0);
        player.set_speed(f32::INFINITY);
        assert_eq!(player.speed(), 1.0);
        player.set_speed(50.0);
        assert_eq!(player.speed(), 2.0);
        player.set_speed(-3.0);
        assert_eq!(player.speed(), 1.0);
    }

    #[test]
    fn going_back_to_one_x_cancels_the_outstanding_compression() {
        let mut player = Player::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        player.set_speed(2.0);
        player.stretching = Some(Stretching {
            factor: 2.0,
            slot: Default::default(),
            cancelled: Arc::clone(&cancelled),
        });
        player.set_speed(1.0);
        assert!(player.stretching.is_none());
        assert!(cancelled.load(Ordering::Relaxed));
    }

    #[test]
    fn missing_or_revoked_output_is_a_recoverable_error() {
        let missing = open_device("No sound output", || Err::<(), _>("device missing"));
        assert_eq!(missing.unwrap_err(), "No sound output: device missing");
        let revoked = open_device("No sound output", || Err::<(), _>("permission revoked"));
        assert_eq!(revoked.unwrap_err(), "No sound output: permission revoked");
    }

    #[test]
    fn microphone_open_failure_is_returned_without_audio_hardware() {
        let levels = Mutex::new(Vec::new());
        let error = record_with(&AtomicBool::new(false), &levels, || {
            Err::<(std::iter::Empty<f32>, u16, u32), _>("permission denied".to_owned())
        })
        .unwrap_err();
        assert_eq!(error, "permission denied");
        assert!(levels.lock().unwrap().is_empty());
    }

    #[test]
    fn recorder_handles_unplug_and_cancellation_with_synthetic_samples() {
        let levels = Mutex::new(Vec::new());
        let samples = std::iter::repeat_n(0.25, 17);
        let recorded = record_with(&AtomicBool::new(false), &levels, || Ok((samples, 1, 1_000)))
            .expect("partial samples before unplug are usable");
        assert!(!recorded.is_empty());
        assert_eq!(levels.lock().unwrap().len(), 1);

        let stopped = AtomicBool::new(true);
        let error = record_with(&stopped, &Mutex::new(Vec::new()), || {
            Ok((std::iter::repeat_n(0.25, 100), 1, 1_000))
        })
        .unwrap_err();
        assert_eq!(error, "The microphone did not record any audio");
    }

    #[test]
    fn late_decode_completion_after_stop_is_discarded() {
        let slot: Decoded = Default::default();
        let mut player = Player::default();
        player.decoding = Some(Decoding {
            message: "old-message".to_owned(),
            start: 0.0,
            slot: Arc::clone(&slot),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        let cancelled = Arc::clone(&player.decoding.as_ref().unwrap().cancelled);
        player.stop();

        assert!(!publish_decode(&slot, &cancelled, Ok((vec![0.5], vec![1]))));
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn audio_read_checks_cancellation_between_chunks() {
        struct CancellingReader {
            cancelled: Arc<AtomicBool>,
            reads: usize,
        }

        impl std::io::Read for CancellingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.reads == 1 {
                    self.cancelled.store(true, Ordering::Release);
                }
                self.reads += 1;
                buffer[0] = 1;
                Ok(1)
            }
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let err = read_audio(
            CancellingReader {
                cancelled: Arc::clone(&cancelled),
                reads: 0,
            },
            2,
            &cancelled,
        )
        .unwrap_err();
        assert_eq!(err, "Audio decoding cancelled");
    }

    #[test]
    fn decoded_sample_limit_rejects_excess_output() {
        let err = collect_samples_limited(
            std::iter::repeat_n(0.1, 5),
            1,
            voice::RATE,
            &AtomicBool::new(false),
            4,
        )
        .unwrap_err();
        assert_eq!(err, audio_limit_error());
    }

    #[test]
    fn stereo_full_rate_duration_counts_frames_and_accepts_exact_limit() {
        let frames = input_frame_limit(voice::RATE);
        assert_eq!(frames, voice::RATE as usize * 15 * 60);
        assert_eq!(frames * 2, voice::RATE as usize * 15 * 60 * 2);

        let accepted = collect_samples_with_limits(
            [0.25, 0.75, -0.5, 0.5].into_iter(),
            2,
            voice::RATE,
            &AtomicBool::new(false),
            2,
            2,
        )
        .expect("two stereo frames meet exact input-frame limit");
        assert_eq!(accepted, vec![0.5, 0.0]);

        let rejected = collect_samples_with_limits(
            [0.25, 0.75, -0.5, 0.5, 0.0, 0.0].into_iter(),
            2,
            voice::RATE,
            &AtomicBool::new(false),
            3,
            2,
        )
        .unwrap_err();
        assert_eq!(rejected, audio_limit_error());
    }

    #[test]
    fn incremental_downmix_and_linear_resampling_cover_mono_and_stereo() {
        let mono = collect_samples_limited(
            [0.0, 1.0, 0.0, 1.0].into_iter(),
            1,
            voice::RATE / 2,
            &AtomicBool::new(false),
            8,
        )
        .expect("mono input resamples");
        assert_eq!(mono, vec![0.0, 0.5, 1.0, 0.5, 0.0, 0.5, 1.0, 1.0]);

        let stereo = collect_samples_limited(
            [1.0, -1.0, 1.0, 0.0, 0.0, 1.0].into_iter(),
            2,
            voice::RATE / 2,
            &AtomicBool::new(false),
            8,
        )
        .expect("stereo input is averaged and resampled");
        assert_eq!(stereo, vec![0.0, 0.25, 0.5, 0.5, 0.5, 0.5]);

        let unchanged = collect_samples_limited(
            [0.2, -0.4].into_iter(),
            1,
            voice::RATE,
            &AtomicBool::new(false),
            2,
        )
        .expect("mono full-rate input passes through");
        assert_eq!(unchanged, vec![0.2, -0.4]);
    }

    #[test]
    fn incremental_decode_stops_consuming_at_output_cap() {
        struct CountedSamples(Arc<AtomicUsize>);

        impl Iterator for CountedSamples {
            type Item = f32;

            fn next(&mut self) -> Option<Self::Item> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Some(0.25)
            }
        }

        let consumed = Arc::new(AtomicUsize::new(0));
        let result = collect_samples_limited(
            CountedSamples(Arc::clone(&consumed)),
            1,
            voice::RATE / 2,
            &AtomicBool::new(false),
            4,
        );
        assert_eq!(result.unwrap_err(), audio_limit_error());
        assert!(consumed.load(Ordering::Relaxed) <= 8);
    }

    #[test]
    fn waveform_scan_honors_cancellation_before_returning_bars() {
        let samples = vec![0.5; 20_000];
        let mut checks = 0;
        let error = waveform_with_cancel_check(&samples, || {
            checks += 1;
            checks == 3
        })
        .unwrap_err();
        assert_eq!(error, "Audio decoding cancelled");
        assert_eq!(checks, 3);
    }

    #[test]
    fn sample_decode_stops_when_cancelled() {
        struct CancellingSamples {
            cancelled: Arc<AtomicBool>,
            count: usize,
        }

        impl Iterator for CancellingSamples {
            type Item = f32;

            fn next(&mut self) -> Option<Self::Item> {
                self.count += 1;
                if self.count == 2 {
                    self.cancelled.store(true, Ordering::Release);
                }
                Some(0.1)
            }
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        let err = collect_samples_limited(
            CancellingSamples {
                cancelled: Arc::clone(&cancelled),
                count: 0,
            },
            1,
            voice::RATE,
            &cancelled,
            10,
        )
        .unwrap_err();
        assert_eq!(err, "Audio decoding cancelled");
    }

    #[test]
    fn decode_job_reservation_never_exceeds_limit() {
        let active = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let active = Arc::clone(&active);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    if !try_acquire_decode_job(&active, MAX_DECODE_JOBS) {
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    active.fetch_sub(1, Ordering::AcqRel);
                    true
                })
            })
            .collect();
        let acquired = threads
            .into_iter()
            .map(|thread| thread.join().expect("worker completes"))
            .filter(|acquired| *acquired)
            .count();
        assert!(acquired <= MAX_DECODE_JOBS);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn oversized_audio_input_is_rejected_before_allocation() {
        let err =
            read_audio(io::empty(), MAX_AUDIO_BYTES + 1, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(err, audio_limit_error());
    }

    #[test]
    fn an_unplugged_playback_sink_marks_clip_complete() {
        let samples = Arc::new(vec![0.25; 480]);
        let mut loaded = Loaded {
            message: "clip".to_owned(),
            buffer: Arc::clone(&samples),
            samples,
            factor: 1.0,
            base: Duration::ZERO,
            paused: false,
            done: false,
        };
        // A synthetic sink reports empty when its device disappears mid-clip.
        assert!(playback_ended(&mut loaded, true));
        assert!(loaded.done);
        assert_eq!(loaded.message, "clip");
        assert!(!playback_ended(&mut loaded, true));
    }

    #[test]
    fn unsupported_or_corrupt_audio_returns_codec_error() {
        let path = std::env::temp_dir().join(format!(
            "zaptide-audio-invalid-{}-{}.bin",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::write(&path, b"not an audio stream").expect("write synthetic fixture");
        let error = decode_file(&path).unwrap_err();
        let _ = std::fs::remove_file(path);
        assert!(error.starts_with("Could not decode the audio:"), "{error}");
    }

    #[test]
    fn opus_decode_preserves_voice_clip_samples() {
        let tone: Vec<f32> = (0..voice::RATE / 10)
            .map(|i| (i as f32 * 330.0 * std::f32::consts::TAU / voice::RATE as f32).sin() * 0.3)
            .collect();
        let bytes = voice::encode(&tone).expect("encodes synthetic tone");
        let decoded = decode_opus(std::io::Cursor::new(bytes), &AtomicBool::new(false))
            .expect("decodes tone");
        assert!(!decoded.is_empty());
        assert!(decoded.len() <= MAX_AUDIO_SAMPLES);
        assert!(decoded.iter().any(|sample| sample.abs() > 0.01));
    }

    /// Plays a one-second test tone:
    /// `cargo test audio::tests::plays -- --ignored --nocapture`.
    #[test]
    #[ignore = "makes a sound on this machine"]
    fn plays_a_clip_on_this_machine() {
        let dir = std::env::temp_dir();
        let path = dir.join("zaptide-audio-test.ogg");
        let tone: Vec<f32> = (0..voice::RATE)
            .map(|i| (i as f32 * 330.0 * std::f32::consts::TAU / voice::RATE as f32).sin() * 0.3)
            .collect();
        std::fs::write(&path, voice::encode(&tone).expect("encodes")).expect("written");
        let mut player = Player::default();
        player.toggle("clip", &path).expect("starts decoding");
        assert_eq!(player.status("clip").state, State::Loading);
        let started = Instant::now();
        let mut seen_playing = false;
        while started.elapsed() < Duration::from_secs(3) {
            player.poll().expect("plays");
            let status = player.status("clip");
            if status.state == State::Playing && status.position > Duration::from_millis(300) {
                seen_playing = true;
                eprintln!("playing at {:?} of {:?}", status.position, status.total);
            }
            if seen_playing && status.state == State::Idle {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(seen_playing, "never heard it playing");
        assert_eq!(player.status("clip").state, State::Idle, "ends on its own");
        assert_eq!(player.bars("clip").map(<[u8]>::len), Some(voice::BARS));
        let _ = std::fs::remove_file(path);
    }

    /// Plays a two-second tone at double speed and checks the position
    /// outruns the clock:
    /// `cargo test audio::tests::doubles -- --ignored --nocapture`.
    #[test]
    #[ignore = "makes a sound on this machine"]
    fn doubles_the_position_rate_on_this_machine() {
        let dir = std::env::temp_dir();
        let path = dir.join("zaptide-audio-speed-test.ogg");
        let tone: Vec<f32> = (0..voice::RATE * 2)
            .map(|i| (i as f32 * 330.0 * std::f32::consts::TAU / voice::RATE as f32).sin() * 0.3)
            .collect();
        std::fs::write(&path, voice::encode(&tone).expect("encodes")).expect("written");
        let mut player = Player::default();
        player.set_speed(2.0);
        player.toggle("clip", &path).expect("starts decoding");
        let started = Instant::now();
        let mut seen: Vec<(Duration, Duration)> = Vec::new();
        while started.elapsed() < Duration::from_secs(6) {
            player.poll().expect("plays");
            let status = player.status("clip");
            // The clip starts at 1x while its compression builds; measure
            // only after the compressed buffer has taken over.
            if status.state == State::Playing && status.position > Duration::from_millis(600) {
                seen.push((started.elapsed(), status.position));
            }
            if status.state == State::Idle && !seen.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let (first_wall, first_position) = seen.first().expect("played");
        let (last_wall, last_position) = seen.last().expect("played");
        let wall = *last_wall - *first_wall;
        let advanced = *last_position - *first_position;
        assert!(wall > Duration::from_millis(200), "played for {wall:?}");
        assert!(
            advanced.as_secs_f32() >= 1.5 * wall.as_secs_f32(),
            "position advanced {advanced:?} over {wall:?} of wall time"
        );
        let _ = std::fs::remove_file(path);
    }

    /// Records one second from the default microphone:
    /// `cargo test audio::tests::records -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a microphone"]
    fn records_a_second_on_this_machine() {
        let recorder = Recorder::start();
        std::thread::sleep(Duration::from_millis(1_000));
        assert!(recorder.failure().is_none(), "{:?}", recorder.failure());
        let levels = recorder.levels();
        let heard = recorder.finish().expect("something was heard");
        eprintln!("{} samples, {} level readings", heard.len(), levels.len());
        assert!(
            heard.len() > voice::RATE as usize * 8 / 10,
            "{}",
            heard.len()
        );
        assert!(levels.len() >= 15, "{}", levels.len());
    }
}
