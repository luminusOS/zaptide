//! Audio playback and voice-message recording.
//!
//! Input and output devices are opened on demand and released when idle.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rodio::Source;
use rodio::buffer::SamplesBuffer;

use crate::voice;

/// Maximum recording length. The phone uses a shorter limit.
const LONGEST_RECORDING: Duration = Duration::from_secs(15 * 60);
const MAX_DECODE_JOBS: usize = 2;
const MAX_AUDIO_BYTES: u64 = 64 * 1024 * 1024;
const MAX_AUDIO_SAMPLES: usize = voice::RATE as usize * LONGEST_RECORDING.as_secs() as usize;
static ACTIVE_DECODE_JOBS: AtomicUsize = AtomicUsize::new(0);

struct DecodePermit;

impl DecodePermit {
    fn acquire() -> Option<Self> {
        try_acquire_decode_job(&ACTIVE_DECODE_JOBS, MAX_DECODE_JOBS).then_some(Self)
    }
}

fn try_acquire_decode_job(active: &AtomicUsize, limit: usize) -> bool {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < limit).then_some(count + 1)
        })
        .is_ok()
}

impl Drop for DecodePermit {
    fn drop(&mut self) {
        ACTIVE_DECODE_JOBS.fetch_sub(1, Ordering::AcqRel);
    }
}

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

type Decoded = Arc<Mutex<Option<Result<Vec<f32>, String>>>>;

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
            let samples = result?;
            if samples.is_empty() {
                return Err("The clip is empty".to_owned());
            }
            let samples = Arc::new(samples);
            self.bars
                .entry(message.clone())
                .or_insert_with(|| voice::waveform(&samples));
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
            // Release the device after playback ends.
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
                let result = decode_file_with(&path, &thread_cancelled);
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

fn publish_decode(
    slot: &Decoded,
    cancelled: &AtomicBool,
    result: Result<Vec<f32>, String>,
) -> bool {
    if cancelled.load(Ordering::Relaxed) {
        return false;
    }
    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
    true
}

fn playback_ended(loaded: &mut Loaded, sink_empty: bool) -> bool {
    if loaded.done || loaded.paused || !sink_empty {
        return false;
    }
    loaded.done = true;
    true
}

/// Decodes a file to mono 48 kHz samples. OGG/Opus uses the voice codec; other
/// supported formats use rodio.
#[cfg(test)]
fn decode_file(path: &Path) -> Result<Vec<f32>, String> {
    decode_file_with(path, &AtomicBool::new(false))
}

fn decode_file_with(path: &Path, cancelled: &AtomicBool) -> Result<Vec<f32>, String> {
    if is_cancelled(cancelled) {
        return Err("Audio decoding cancelled".to_owned());
    }
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("Could not read the audio: {error}"))?;
    let size = file
        .metadata()
        .map_err(|error| format!("Could not read the audio: {error}"))?
        .len();
    if size > MAX_AUDIO_BYTES {
        return Err(audio_limit_error());
    }
    let mut signature = [0; 4];
    let read = file
        .read(&mut signature)
        .map_err(|error| format!("Could not read the audio: {error}"))?;
    if read == signature.len() && &signature == b"OggS" {
        file.seek(std::io::SeekFrom::Start(0))
            .map_err(|error| format!("Could not read the audio: {error}"))?;
        let bytes = read_audio(file, size, cancelled)?;
        if is_cancelled(cancelled) {
            return Err("Audio decoding cancelled".to_owned());
        }
        if let Ok(samples) = decode_opus(&bytes, cancelled) {
            if is_cancelled(cancelled) {
                return Err("Audio decoding cancelled".to_owned());
            }
            if samples.len() > MAX_AUDIO_SAMPLES {
                return Err(audio_limit_error());
            }
            return Ok(samples);
        }
    }
    if is_cancelled(cancelled) {
        return Err("Audio decoding cancelled".to_owned());
    }
    let file =
        std::fs::File::open(path).map_err(|error| format!("Could not read the audio: {error}"))?;
    let decoder = rodio::Decoder::new(std::io::BufReader::new(file))
        .map_err(|error| format!("Could not decode the audio: {error}"))?;
    let channels = decoder.channels().get();
    let rate = decoder.sample_rate().get();
    collect_samples(decoder, channels, rate, cancelled)
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
    decode_file_with(path, cancelled).map(|samples| voice::waveform(&samples))
}

fn is_cancelled(cancelled: &AtomicBool) -> bool {
    cancelled.load(Ordering::Acquire)
}

fn audio_limit_error() -> String {
    "The audio exceeds the supported size or duration".to_owned()
}

fn read_audio(
    mut reader: impl std::io::Read,
    size: u64,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, String> {
    if size > MAX_AUDIO_BYTES {
        return Err(audio_limit_error());
    }
    let mut bytes = Vec::with_capacity(size as usize);
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if is_cancelled(cancelled) {
            return Err("Audio decoding cancelled".to_owned());
        }
        let count = reader
            .read(&mut chunk)
            .map_err(|error| format!("Could not read the audio: {error}"))?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > MAX_AUDIO_BYTES as usize {
            return Err(audio_limit_error());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(bytes)
}

fn decode_opus(bytes: &[u8], cancelled: &AtomicBool) -> Result<Vec<f32>, String> {
    let mut reader = ogg::PacketReader::new(std::io::Cursor::new(bytes));
    let mut decoder = None;
    let mut channels = 0usize;
    let mut skip = 0usize;
    let mut tagged = false;
    let mut out = Vec::new();
    let mut scratch = vec![0.0f32; 11_520];
    loop {
        if is_cancelled(cancelled) {
            return Err("Audio decoding cancelled".to_owned());
        }
        let packet = match reader.read_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(error) => return Err(format!("Could not decode the audio: {error}")),
        };
        if decoder.is_none() {
            let head = packet
                .data
                .strip_prefix(b"OpusHead")
                .ok_or_else(|| "Could not decode the audio: not an Opus stream".to_owned())?;
            if head.len() < 11 {
                return Err("Could not decode the audio: truncated Opus header".to_owned());
            }
            channels = usize::from(head[1]);
            let layout = match channels {
                1 => opus::Channels::Mono,
                2 => opus::Channels::Stereo,
                other => return Err(format!("Could not decode the audio: {other} channels")),
            };
            skip = usize::from(u16::from_le_bytes([head[2], head[3]]));
            decoder = Some(
                opus::Decoder::new(voice::RATE, layout)
                    .map_err(|error| format!("Could not decode the audio: {error}"))?,
            );
            continue;
        }
        if !tagged {
            tagged = true;
            continue;
        }
        let opus_decoder = decoder.as_mut().expect("Opus header initialized decoder");
        let frames = opus_decoder
            .decode_float(&packet.data, &mut scratch, false)
            .map_err(|error| format!("Could not decode the audio: bad Opus packet: {error}"))?;
        let decoded = &scratch[..frames * channels];
        let mono = if channels == 2 {
            decoded
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[left, right]| (left + right) * 0.5)
                .collect::<Vec<_>>()
        } else {
            decoded.to_vec()
        };
        let skipped = skip.min(mono.len());
        skip -= skipped;
        if out.len().saturating_add(mono.len() - skipped) > MAX_AUDIO_SAMPLES {
            return Err(audio_limit_error());
        }
        out.extend_from_slice(&mono[skipped..]);
    }
    if decoder.is_none() {
        return Err("Could not decode the audio: not an OGG stream".to_owned());
    }
    Ok(out)
}

fn collect_samples<I: Iterator<Item = f32>>(
    decoder: I,
    channels: u16,
    sample_rate: u32,
    cancelled: &AtomicBool,
) -> Result<Vec<f32>, String> {
    collect_samples_limited(decoder, channels, sample_rate, cancelled, MAX_AUDIO_SAMPLES)
}

fn collect_samples_limited<I: Iterator<Item = f32>>(
    mut decoder: I,
    channels: u16,
    sample_rate: u32,
    cancelled: &AtomicBool,
    max_samples: usize,
) -> Result<Vec<f32>, String> {
    let input_channels = usize::from(channels.max(1));
    let max_input = (u64::from(sample_rate.max(1))
        .saturating_mul(LONGEST_RECORDING.as_secs())
        .saturating_mul(input_channels as u64))
    .min(max_samples as u64) as usize;
    let mut interleaved = Vec::with_capacity(max_input.min(8_192));
    loop {
        if is_cancelled(cancelled) {
            return Err("Audio decoding cancelled".to_owned());
        }
        let Some(sample) = decoder.next() else {
            break;
        };
        if interleaved.len() == max_input {
            return Err(audio_limit_error());
        }
        interleaved.push(sample);
    }
    mono_at_rate_cancellable(&interleaved, channels, sample_rate, cancelled, max_samples)
}

fn mono_at_rate_cancellable(
    interleaved: &[f32],
    channels: u16,
    sample_rate: u32,
    cancelled: &AtomicBool,
    max_samples: usize,
) -> Result<Vec<f32>, String> {
    let channels = usize::from(channels.max(1));
    let mut mono = Vec::with_capacity((interleaved.len() / channels).min(8_192));
    for (index, frame) in interleaved.chunks_exact(channels).enumerate() {
        if index % 4_096 == 0 && is_cancelled(cancelled) {
            return Err("Audio decoding cancelled".to_owned());
        }
        mono.push(frame.iter().sum::<f32>() / channels as f32);
    }
    if sample_rate == voice::RATE || sample_rate == 0 || mono.is_empty() {
        return Ok(mono);
    }
    let ratio = f64::from(sample_rate) / f64::from(voice::RATE);
    let count = (mono.len() as f64 / ratio).floor() as usize;
    if count > max_samples {
        return Err(audio_limit_error());
    }
    let mut output = Vec::with_capacity(count.min(8_192));
    for index in 0..count {
        if index % 4_096 == 0 && is_cancelled(cancelled) {
            return Err("Audio decoding cancelled".to_owned());
        }
        let position = index as f64 * ratio;
        let left = position.floor() as usize;
        let fraction = (position - left as f64) as f32;
        let a = mono[left.min(mono.len() - 1)];
        let b = mono.get(left + 1).copied().unwrap_or(a);
        output.push(a + (b - a) * fraction);
    }
    Ok(output)
}

type Outcome = Arc<Mutex<Option<Result<Vec<f32>, String>>>>;

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
                    *outcome.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
                })
        };
        let thread = match spawned {
            Ok(thread) => Some(thread),
            Err(error) => {
                *outcome.lock().unwrap_or_else(|p| p.into_inner()) = Some(Err(error.to_string()));
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
        self.levels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The newest `count` readings, without copying the whole recording.
    pub fn recent_levels(&self, count: usize) -> Vec<f32> {
        let levels = self.levels.lock().unwrap_or_else(|p| p.into_inner());
        levels[levels.len().saturating_sub(count)..].to_vec()
    }

    /// Error that stopped recording early.
    pub fn failure(&self) -> Option<String> {
        match self
            .outcome
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
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
        self.outcome
            .lock()
            .unwrap_or_else(|p| p.into_inner())
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

fn record_with<I: Iterator<Item = f32>>(
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
        levels
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(loudness);
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
pub fn recording_path(dir: &Path) -> PathBuf {
    dir.join(format!("voice-{}.ogg", crate::util::now()))
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

        assert!(!publish_decode(&slot, &cancelled, Ok(vec![0.5])));
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
        let decoded = decode_opus(&bytes, &AtomicBool::new(false)).expect("decodes tone");
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
