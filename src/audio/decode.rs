//! Bounded, cancellable decoding for clips and generated waveforms.

use std::io::{Read, Seek};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;

use crate::voice;

/// Maximum recording length. The phone uses a shorter limit.
pub(super) const LONGEST_RECORDING: std::time::Duration = std::time::Duration::from_secs(15 * 60);
pub(super) const MAX_DECODE_JOBS: usize = 2;
pub(super) const MAX_AUDIO_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const MAX_AUDIO_SAMPLES: usize =
    voice::RATE as usize * LONGEST_RECORDING.as_secs() as usize;
const SAMPLE_CAPACITY_CHUNK: usize = (4 * 1024 * 1024) / std::mem::size_of::<f32>();
static ACTIVE_DECODE_JOBS: AtomicUsize = AtomicUsize::new(0);

pub(super) struct DecodePermit {
    active: &'static AtomicUsize,
}

impl DecodePermit {
    pub(super) fn acquire() -> Option<Self> {
        Self::acquire_from(&ACTIVE_DECODE_JOBS, MAX_DECODE_JOBS)
    }

    fn acquire_from(active: &'static AtomicUsize, limit: usize) -> Option<Self> {
        try_acquire_decode_job(active, limit).then(|| Self { active })
    }
}

pub(super) fn try_acquire_decode_job(active: &AtomicUsize, limit: usize) -> bool {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < limit).then(|| count + 1)
        })
        .is_ok()
}

impl Drop for DecodePermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Decodes a file to mono 48 kHz samples. OGG/Opus uses the voice codec; other
/// formats use GStreamer.
#[cfg(test)]
pub(super) fn decode_file(path: &std::path::Path) -> Result<Vec<f32>, String> {
    decode_file_with(path, &AtomicBool::new(false))
}

pub(super) fn decode_file_with(
    path: &std::path::Path,
    cancelled: &AtomicBool,
) -> Result<Vec<f32>, String> {
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
        match decode_opus(&mut file, cancelled) {
            Ok(samples) => {
                if is_cancelled(cancelled) {
                    return Err("Audio decoding cancelled".to_owned());
                }
                if samples.len() > MAX_AUDIO_SAMPLES {
                    return Err(audio_limit_error());
                }
                return Ok(samples);
            }
            Err(error) if error == audio_limit_error() => return Err(error),
            Err(_) => {}
        }
    }
    if is_cancelled(cancelled) {
        return Err("Audio decoding cancelled".to_owned());
    }
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|error| format!("Could not read the audio: {error}"))?;
    decode_with_gstreamer(&file, cancelled)
}

/// A decode that gets no new samples for this long has stalled.
const DECODE_STALL: std::time::Duration = std::time::Duration::from_secs(10);
/// A whole decode may take this long; a clip within the size cap takes a
/// fraction of it.
const DECODE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_SOURCE_RATE: u32 = 384_000;
const MAX_SOURCE_CHANNELS: u16 = 8;

/// Element kinds `decodebin` must not plug for a received file. Waveforms
/// decode without a click, so a file that is really a streaming manifest,
/// a playlist or a video must not make the decoder open URLs, read other
/// files, or run video, image and subtitle parsers.
fn refused_for_waveforms(klass: &str) -> bool {
    klass
        .split('/')
        .any(|kind| matches!(kind, "Adaptive" | "Source" | "Video" | "Image" | "Subtitle"))
}

/// Decodes any format GStreamer can play, keeping the stream's own rate and
/// channels so `collect_samples` downmixes and resamples it exactly as it
/// does every other decoded clip.
fn decode_with_gstreamer(file: &std::fs::File, cancelled: &AtomicBool) -> Result<Vec<f32>, String> {
    let failed = |error: &dyn std::fmt::Display| format!("Could not decode the audio: {error}");
    gst::init().map_err(|error| failed(&error))?;
    let make = |factory: &str| {
        gst::ElementFactory::make(factory)
            .build()
            .map_err(|error| failed(&error))
    };
    // Reopens the file already opened and size-checked through its
    // descriptor, so a rename or swap of the path cannot change what is
    // decoded. fdsrc would read the descriptor directly, but cut Vorbis and
    // chained Ogg streams short; `file` outlives the pipeline.
    let source = gst::ElementFactory::make("filesrc")
        .property(
            "location",
            format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(file)),
        )
        .build()
        .map_err(|error| failed(&error))?;
    let decoder = make("decodebin")?;
    let select_result = gst::glib::Type::from_name("GstAutoplugSelectResult")
        .ok_or_else(|| failed(&"decodebin has no autoplug-select"))?;
    let select = move |skip: bool| {
        gst::glib::EnumClass::with_type(select_result)
            .and_then(|class| class.to_value_by_nick(if skip { "skip" } else { "try" }))
    };
    if select(true).is_none() || select(false).is_none() {
        return Err(failed(&"decodebin has no autoplug-select"));
    }
    decoder.connect("autoplug-select", false, move |args| {
        let skip = args
            .get(3)
            .and_then(|factory| factory.get::<gst::ElementFactory>().ok())
            .is_none_or(|factory| refused_for_waveforms(factory.klass()));
        select(skip)
    });
    let convert = make("audioconvert")?;
    let sink = gst_app::AppSink::builder()
        .caps(
            &gst::Caps::builder("audio/x-raw")
                .field("format", "F32LE")
                .field("layout", "interleaved")
                .build(),
        )
        .sync(false)
        .max_buffers(8)
        .build();
    let pipeline = gst::Pipeline::new();
    pipeline
        .add_many([&source, &decoder, &convert, sink.upcast_ref()])
        .map_err(|error| failed(&error))?;
    source.link(&decoder).map_err(|error| failed(&error))?;
    convert.link(&sink).map_err(|error| failed(&error))?;
    let convert_input = convert.downgrade();
    let implausible = std::sync::Arc::new(AtomicBool::new(false));
    let seen_implausible = std::sync::Arc::clone(&implausible);
    decoder.connect_pad_added(move |_, pad| {
        let Some(input) = convert_input
            .upgrade()
            .and_then(|convert| convert.static_pad("sink"))
        else {
            return;
        };
        let caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
        let Some(structure) = caps.structure(0) else {
            return;
        };
        if !structure.name().starts_with("audio/") || input.is_linked() {
            return;
        }
        let within = |field: &str, limit: u32| {
            structure.get::<i32>(field).map_or(true, |value| {
                u32::try_from(value).is_ok_and(|value| (1..=limit).contains(&value))
            })
        };
        if !within("channels", u32::from(MAX_SOURCE_CHANNELS)) || !within("rate", MAX_SOURCE_RATE) {
            seen_implausible.store(true, Ordering::Release);
            return;
        }
        let _ = pad.link(&input);
    });
    let started = std::time::Instant::now();
    let mut samples = super::pulled::PulledSamples::start(pipeline, sink, |idle| {
        if is_cancelled(cancelled) {
            Ok(false)
        } else if idle >= DECODE_STALL {
            Err("the decoder stopped producing audio".to_owned())
        } else {
            Ok(true)
        }
    })
    .map_err(|error| failed(&error))?
    .with_deadline(started + DECODE_DEADLINE, audio_limit_error());
    let Some((channels, rate)) = samples.format() else {
        if is_cancelled(cancelled) {
            return Err("Audio decoding cancelled".to_owned());
        }
        if implausible.load(Ordering::Acquire) {
            return Err(audio_limit_error());
        }
        return Err(decode_failure(
            samples.failure().unwrap_or("no audio stream"),
        ));
    };
    if !(1..=MAX_SOURCE_CHANNELS).contains(&channels) || !(1..=MAX_SOURCE_RATE).contains(&rate) {
        return Err(audio_limit_error());
    }
    let decoded = collect_samples(&mut samples, channels, rate, cancelled)?;
    match samples.failure() {
        Some(error) => Err(decode_failure(error)),
        None => Ok(decoded),
    }
}

fn decode_failure(error: &str) -> String {
    if error == audio_limit_error() {
        error.to_owned()
    } else {
        format!("Could not decode the audio: {error}")
    }
}

pub(super) fn is_cancelled(cancelled: &AtomicBool) -> bool {
    cancelled.load(Ordering::Acquire)
}

pub(super) fn audio_limit_error() -> String {
    "The audio exceeds the supported size or duration".to_owned()
}

#[cfg(test)]
pub(super) fn read_audio(
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

pub(super) fn decode_opus(
    input: impl Read + Seek,
    cancelled: &AtomicBool,
) -> Result<Vec<f32>, String> {
    let mut reader = ogg::PacketReader::new(input);
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
        let decoded_frames = frames;
        let skipped = skip.min(decoded_frames);
        skip -= skipped;
        let additional = decoded_frames - skipped;
        let new_len = out.len().saturating_add(additional);
        if new_len > MAX_AUDIO_SAMPLES {
            return Err(audio_limit_error());
        }
        ensure_sample_capacity(&mut out, new_len, MAX_AUDIO_SAMPLES)?;
        if channels == 2 {
            for pair in decoded[skipped * 2..].as_chunks::<2>().0 {
                out.push((pair[0] + pair[1]) * 0.5);
            }
        } else {
            out.extend_from_slice(&decoded[skipped..]);
        }
    }
    if decoder.is_none() {
        return Err("Could not decode the audio: not an OGG stream".to_owned());
    }
    Ok(out)
}

pub(super) fn collect_samples<I: Iterator<Item = f32>>(
    decoder: I,
    channels: u16,
    sample_rate: u32,
    cancelled: &AtomicBool,
) -> Result<Vec<f32>, String> {
    collect_samples_limited(decoder, channels, sample_rate, cancelled, MAX_AUDIO_SAMPLES)
}

pub(super) fn collect_samples_limited<I: Iterator<Item = f32>>(
    decoder: I,
    channels: u16,
    sample_rate: u32,
    cancelled: &AtomicBool,
    max_samples: usize,
) -> Result<Vec<f32>, String> {
    collect_samples_with_limits(
        decoder,
        channels,
        sample_rate,
        cancelled,
        max_samples,
        input_frame_limit(sample_rate),
    )
}

pub(super) fn input_frame_limit(sample_rate: u32) -> usize {
    u64::from(sample_rate.max(1))
        .saturating_mul(LONGEST_RECORDING.as_secs())
        .min(usize::MAX as u64) as usize
}

fn ensure_sample_capacity(
    samples: &mut Vec<f32>,
    required_len: usize,
    max_samples: usize,
) -> Result<(), String> {
    if required_len > max_samples {
        return Err(audio_limit_error());
    }
    if required_len > samples.capacity() {
        let chunks = required_len.div_ceil(SAMPLE_CAPACITY_CHUNK);
        let target_capacity = chunks
            .saturating_mul(SAMPLE_CAPACITY_CHUNK)
            .min(max_samples);
        samples
            .try_reserve_exact(target_capacity - samples.len())
            .map_err(|error| format!("Could not decode the audio: {error}"))?;
    }
    Ok(())
}

pub(super) fn collect_samples_with_limits<I: Iterator<Item = f32>>(
    mut decoder: I,
    channels: u16,
    sample_rate: u32,
    cancelled: &AtomicBool,
    max_samples: usize,
    max_input_frames: usize,
) -> Result<Vec<f32>, String> {
    let channels = usize::from(channels.max(1));
    let rate = sample_rate.max(1);
    let ratio = if sample_rate == 0 {
        1.0
    } else {
        f64::from(rate) / f64::from(voice::RATE)
    };
    let mut output = Vec::with_capacity(max_samples.min(8_192));
    let mut frame = Vec::with_capacity(channels.min(8_192));
    let mut previous = None;
    let mut input_frames = 0usize;
    let mut output_index = 0usize;
    loop {
        if is_cancelled(cancelled) {
            return Err("Audio decoding cancelled".to_owned());
        }
        let Some(sample) = decoder.next() else {
            break;
        };
        frame.push(sample);
        if frame.len() != channels {
            continue;
        }
        if input_frames == max_input_frames {
            return Err(audio_limit_error());
        }
        let mono = frame.iter().sum::<f32>() / channels as f32;
        frame.clear();
        if let Some(left) = previous {
            while (output_index as f64 * ratio).floor() < input_frames as f64 {
                if output_index >= max_samples {
                    return Err(audio_limit_error());
                }
                if output_index.is_multiple_of(4_096) && is_cancelled(cancelled) {
                    return Err("Audio decoding cancelled".to_owned());
                }
                let position = output_index as f64 * ratio;
                let fraction = (position - position.floor()) as f32;
                ensure_sample_capacity(&mut output, output_index + 1, max_samples)?;
                output.push(left + (mono - left) * fraction);
                output_index += 1;
            }
        }
        previous = Some(mono);
        input_frames += 1;
    }

    // Existing linear interpolation holds the final source sample when the
    // last output position has no following input frame.
    if let Some(last) = previous {
        let count = (input_frames as f64 / ratio).floor() as usize;
        while output_index < count {
            if output_index >= max_samples {
                return Err(audio_limit_error());
            }
            if output_index.is_multiple_of(4_096) && is_cancelled(cancelled) {
                return Err("Audio decoding cancelled".to_owned());
            }
            ensure_sample_capacity(&mut output, output_index + 1, max_samples)?;
            output.push(last);
            output_index += 1;
        }
    }
    if is_cancelled(cancelled) {
        return Err("Audio decoding cancelled".to_owned());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_ACTIVE_DECODE_JOBS: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn denied_decode_permit_does_not_release_unowned_slot() {
        TEST_ACTIVE_DECODE_JOBS.store(1, Ordering::Release);

        assert!(DecodePermit::acquire_from(&TEST_ACTIVE_DECODE_JOBS, 1).is_none());

        assert_eq!(TEST_ACTIVE_DECODE_JOBS.load(Ordering::Acquire), 1);
        TEST_ACTIVE_DECODE_JOBS.store(0, Ordering::Release);
    }

    #[test]
    fn two_frames_at_36khz_produce_floor_resampled_count() {
        let samples = collect_samples_limited(
            [0.0, 1.0].into_iter(),
            1,
            36_000,
            &AtomicBool::new(false),
            2,
        )
        .expect("two input frames resample to two output frames");

        assert_eq!(samples, vec![0.0, 0.75]);
    }

    #[test]
    fn sample_capacity_grows_in_bounded_chunks() {
        let max_samples = SAMPLE_CAPACITY_CHUNK + 123;
        let mut samples = Vec::new();

        ensure_sample_capacity(&mut samples, 1, max_samples).unwrap();
        assert!(samples.capacity() >= SAMPLE_CAPACITY_CHUNK);
        ensure_sample_capacity(&mut samples, SAMPLE_CAPACITY_CHUNK + 1, max_samples).unwrap();
        assert!(samples.capacity() >= max_samples);
        assert!(samples.capacity() <= max_samples + SAMPLE_CAPACITY_CHUNK);
        assert!(ensure_sample_capacity(&mut samples, max_samples + 1, max_samples).is_err());
    }

    #[test]
    fn opus_file_decodes_from_stream() {
        let tone: Vec<f32> = (0..voice::RATE / 10)
            .map(|index| {
                (index as f32 * 330.0 * std::f32::consts::TAU / voice::RATE as f32).sin() * 0.3
            })
            .collect();
        let bytes = voice::encode(&tone).expect("encode Opus fixture");
        let path = std::env::temp_dir().join(format!(
            "zaptide-audio-stream-{}-{}.ogg",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, bytes).expect("write Opus fixture");

        let result = decode_file_with(&path, &AtomicBool::new(false));
        let _ = std::fs::remove_file(path);
        let samples = result.expect("decode Opus fixture through seeked File");
        assert!(!samples.is_empty());
        assert!(samples.iter().any(|sample| sample.abs() > 0.01));
    }
}
