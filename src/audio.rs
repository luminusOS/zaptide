//! Audio playback state and voice-message recording.
//!
//! Input devices are opened on demand and released when idle.

use std::path::Path;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::voice;

mod decode;
mod recorder;

use decode::{DecodePermit, LONGEST_RECORDING, decode_file_with};
#[cfg(test)]
use decode::{
    MAX_AUDIO_BYTES, MAX_AUDIO_SAMPLES, MAX_DECODE_JOBS, audio_limit_error,
    collect_samples_limited, collect_samples_with_limits, decode_file, decode_opus,
    input_frame_limit, read_audio, try_acquire_decode_job,
};

#[cfg(test)]
use recorder::record_with;
pub use recorder::{Recorder, recording_path};

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
    pub const IDLE: Self = Self {
        state: State::Idle,
        position: Duration::ZERO,
        total: Duration::ZERO,
    };
}

/// Playback speeds, in the order the speed button cycles them.
pub const SPEEDS: [f32; 3] = [1.0, 1.5, 2.0];

/// The speed playback actually uses for a requested one: speeds outside 1x
/// to 2x, such as a hand-edited setting, are clamped, and non-finite ones
/// play at 1x.
pub fn playable_speed(speed: f32) -> f32 {
    if speed.is_finite() {
        speed.clamp(SPEEDS[0], SPEEDS[SPEEDS.len() - 1])
    } else {
        SPEEDS[0]
    }
}

/// Label for a playback speed, like `1x` or `1.5x`.
pub fn speed_label(speed: f32) -> String {
    if speed.fract() == 0.0 {
        format!("{}x", speed as i32)
    } else {
        format!("{speed:.1}x")
    }
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
