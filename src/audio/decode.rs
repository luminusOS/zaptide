//! Bounded, cancellable decoding for clips and generated waveforms.

use std::io::{Read, Seek};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rodio::Source;

use crate::voice;

/// Maximum recording length. The phone uses a shorter limit.
pub(super) const LONGEST_RECORDING: std::time::Duration = std::time::Duration::from_secs(15 * 60);
pub(super) const MAX_DECODE_JOBS: usize = 2;
pub(super) const MAX_AUDIO_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const MAX_AUDIO_SAMPLES: usize =
    voice::RATE as usize * LONGEST_RECORDING.as_secs() as usize;
const SAMPLE_CAPACITY_CHUNK: usize = (4 * 1024 * 1024) / std::mem::size_of::<f32>();
static ACTIVE_DECODE_JOBS: AtomicUsize = AtomicUsize::new(0);

pub(super) struct DecodePermit;

impl DecodePermit {
    pub(super) fn acquire() -> Option<Self> {
        try_acquire_decode_job(&ACTIVE_DECODE_JOBS, MAX_DECODE_JOBS).then_some(Self)
    }
}

pub(super) fn try_acquire_decode_job(active: &AtomicUsize, limit: usize) -> bool {
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

pub(super) type Decoded =
    std::sync::Arc<std::sync::Mutex<Option<Result<(Vec<f32>, Vec<u8>), String>>>>;

pub(super) fn publish_decode(
    slot: &Decoded,
    cancelled: &AtomicBool,
    result: Result<(Vec<f32>, Vec<u8>), String>,
) -> bool {
    if cancelled.load(Ordering::Relaxed) {
        return false;
    }
    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
    true
}

pub(super) fn decode_file_with_waveform(
    path: &std::path::Path,
    cancelled: &AtomicBool,
) -> Result<(Vec<f32>, Vec<u8>), String> {
    let samples = decode_file_with(path, cancelled)?;
    samples_with_waveform(samples, cancelled)
}

fn samples_with_waveform(
    samples: Vec<f32>,
    cancelled: &AtomicBool,
) -> Result<(Vec<f32>, Vec<u8>), String> {
    let bars = super::waveform_cancellable(&samples, cancelled)?;
    Ok((samples, bars))
}

/// Decodes a file to mono 48 kHz samples. OGG/Opus uses the voice codec; other
/// supported formats use rodio.
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
        match decode_opus(file, cancelled) {
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
    let file =
        std::fs::File::open(path).map_err(|error| format!("Could not read the audio: {error}"))?;
    let decoder = rodio::Decoder::new(std::io::BufReader::new(file))
        .map_err(|error| format!("Could not decode the audio: {error}"))?;
    let channels = decoder.channels().get();
    let rate = decoder.sample_rate().get();
    collect_samples(decoder, channels, rate, cancelled)
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
    use std::sync::{Arc, Mutex};

    #[test]
    fn decode_completion_publishes_samples_and_waveform_together() {
        let slot: Decoded = Arc::new(Mutex::new(None));
        let cancelled = AtomicBool::new(false);
        let samples = vec![0.5; voice::BARS];
        let (projected_samples, bars) = samples_with_waveform(samples.clone(), &cancelled).unwrap();

        assert!(publish_decode(
            &slot,
            &cancelled,
            Ok((projected_samples, bars.clone()))
        ));

        let (published_samples, published_bars) = slot.lock().unwrap().take().unwrap().unwrap();
        assert_eq!(published_samples, samples);
        assert_eq!(published_bars, bars);
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
