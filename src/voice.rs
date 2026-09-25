//! WhatsApp voice-message encoding and decoding: mono 48 kHz Opus in OGG.
//!
//! libopus is built into the app.

/// Opus sample rate used for every mono clip.
pub const RATE: u32 = 48_000;
/// One Opus frame: 20 ms.
const FRAME: usize = 960;
/// Number of bars in a WhatsApp voice-message waveform.
pub const BARS: usize = 64;
/// Target bitrate for speech, similar to the phone.
const BITRATE: i32 = 32_000;

/// An OGG/Opus file from mono samples at `RATE`.
pub fn encode(samples: &[f32]) -> Result<Vec<u8>, String> {
    use ogg::PacketWriteEndInfo::{EndPage, EndStream, NormalPacket};
    let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)
        .map_err(|error| error.to_string())?;
    encoder
        .set_bitrate(opus::Bitrate::Bits(BITRATE))
        .map_err(|error| error.to_string())?;
    let pre_skip = encoder
        .get_lookahead()
        .map_err(|error| error.to_string())?
        .max(0) as usize;
    let serial = 0x5641_5341;
    let io = |error: std::io::Error| error.to_string();
    let mut writer = ogg::PacketWriter::new(Vec::new());
    writer
        .write_packet(opus_head(pre_skip as u16), serial, EndPage, 0)
        .map_err(io)?;
    writer
        .write_packet(opus_tags(), serial, EndPage, 0)
        .map_err(io)?;
    // Append silence to flush encoder lookahead. The final granule trims it.
    let frames = (samples.len() + pre_skip).div_ceil(FRAME).max(1);
    let mut frame = vec![0f32; FRAME];
    let mut packet = vec![0u8; 4000];
    for index in 0..frames {
        frame.fill(0.0);
        let start = index * FRAME;
        if start < samples.len() {
            let end = (start + FRAME).min(samples.len());
            frame[..end - start].copy_from_slice(&samples[start..end]);
        }
        let written = encoder
            .encode_float(&frame, &mut packet)
            .map_err(|error| error.to_string())?;
        let last = index + 1 == frames;
        let granule = if last {
            (pre_skip + samples.len()) as u64
        } else {
            ((index + 1) * FRAME) as u64
        };
        let end = if last {
            EndStream
        } else if (index + 1) % 50 == 0 {
            EndPage
        } else {
            NormalPacket
        };
        writer
            .write_packet(packet[..written].to_vec(), serial, end, granule)
            .map_err(io)?;
    }
    Ok(writer.into_inner())
}

fn opus_head(pre_skip: u16) -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1);
    head.push(1);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&RATE.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    head
}

fn opus_tags() -> Vec<u8> {
    let vendor = b"ZapTide";
    let mut tags = Vec::with_capacity(20 + vendor.len());
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());
    tags
}

/// Normalizes a quiet recording to just below full scale, with capped gain.
pub fn normalize(samples: &mut [f32]) {
    let peak = samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if peak <= 0.0 {
        return;
    }
    let gain = (0.89 / peak).clamp(1.0, 10.0);
    if gain > 1.0 {
        for sample in samples {
            *sample *= gain;
        }
    }
}

/// Builds WhatsApp's 0–100 waveform bars from clip loudness.
pub fn waveform(samples: &[f32]) -> Vec<u8> {
    if samples.is_empty() {
        return vec![0; BARS];
    }
    let slice = samples.len().div_ceil(BARS);
    let loudness: Vec<f32> = samples
        .chunks(slice)
        .map(|chunk| (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt())
        .collect();
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
    bars.resize(BARS, 0);
    bars
}

/// Mixes interleaved channels to mono and resamples to `RATE`.
pub fn mono_at_rate(interleaved: &[f32], channels: u16, rate: u32) -> Vec<f32> {
    let channels = usize::from(channels.max(1));
    let mono: Vec<f32> = interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();
    if rate == RATE || rate == 0 || mono.is_empty() {
        return mono;
    }
    let ratio = f64::from(rate) / f64::from(RATE);
    let count = (mono.len() as f64 / ratio).floor() as usize;
    (0..count)
        .map(|index| {
            let position = index as f64 * ratio;
            let left = position.floor() as usize;
            let t = (position - left as f64) as f32;
            let a = mono[left.min(mono.len() - 1)];
            let b = mono.get(left + 1).copied().unwrap_or(a);
            a + (b - a) * t
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f32) -> Vec<f32> {
        (0..(RATE as f32 * seconds) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn the_waveform_follows_the_loudness() {
        let mut samples = tone(1.0);
        let count = samples.len() as f32;
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample *= index as f32 / count;
        }
        let bars = waveform(&samples);
        assert_eq!(bars.len(), BARS);
        assert_eq!(bars[BARS - 1], 100);
        assert!(bars[0] < 10, "{}", bars[0]);
        assert!(bars.windows(2).all(|pair| pair[0] <= pair[1] + 2));
        assert_eq!(waveform(&[]), vec![0; BARS]);
    }

    #[test]
    fn any_input_becomes_mono_at_48k() {
        let stereo: Vec<f32> = (0..44_100 * 2)
            .map(|i| if i % 2 == 0 { 1.0 } else { 0.0 })
            .collect();
        let mono = mono_at_rate(&stereo, 2, 44_100);
        assert!((mono.len() as i64 - 48_000).abs() <= 2, "{}", mono.len());
        assert!(mono.iter().all(|v| (v - 0.5).abs() < 1e-6));
        assert_eq!(mono_at_rate(&[0.25; 10], 1, RATE), vec![0.25; 10]);
    }

    #[test]
    fn a_quiet_recording_is_brought_up_but_not_blasted() {
        let mut quiet: Vec<f32> = tone(0.1).iter().map(|sample| sample * 0.2).collect();
        normalize(&mut quiet);
        let peak = quiet.iter().fold(0.0f32, |a, s| a.max(s.abs()));
        assert!((peak - 0.89).abs() < 0.01, "{peak}");
        // Cap gain at 20 dB.
        let mut faint = vec![0.001f32, -0.002];
        normalize(&mut faint);
        assert!((faint[1] + 0.02).abs() < 1e-6, "{}", faint[1]);
        // Do not reduce already loud recordings.
        let mut loud = vec![0.95f32];
        normalize(&mut loud);
        assert_eq!(loud, vec![0.95]);
        normalize(&mut []);
    }
}
