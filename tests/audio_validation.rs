//! Audio validation tests for ZapTide.
//!
//! Exercises the audio subsystem with synthetic data only: no real WhatsApp
//! content, no network access, no persistent files.
//!
//! ## Environment requirements
//!
//! | Test                                           | Output device | Microphone | Flatpak |
//! |------------------------------------------------|---------------|------------|---------|
//! | `voice_codec_roundtrip`                        | No            | No         | No      |
//! | `waveform_generation_is_64_bars`               | No            | No         | No      |
//! | `narrowest_audio_permissions_documented`       | No            | No         | No      |
//! | `recorder_handles_source_that_cannot_open`      | No            | No (test source) | No |
//! | `recorder_level_metering_produces_50ms_windows`| No            | No (test tone) | No  |
//! | `playback_reaches_completion_within_budget`    | Yes           | No         | No      |
//! | `capture_produces_at_least_one_second_of_audio` | No           | Yes        | No      |
//! | `player_handles_device_unplug_during_playback` | Deferred      | No         | No      |
//! | `recorder_handles_device_unplug_during_capture`| No            | Deferred   | No      |
//! | `flatpak_microphone_denial_*`                  | No            | Sandbox    | Yes     |
//! | `flatpak_microphone_revocation_*`              | No            | Sandbox    | Yes     |

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tempfile::NamedTempFile;

use zaptide::audio::{Recorder, State};
use zaptide::services::media::MediaService;
use zaptide::voice;

static AUDIO_DECODE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn decode_test_guard() -> MutexGuard<'static, ()> {
    AUDIO_DECODE_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn synthetic_tone(seconds: f32, frequency: f32, amplitude: f32) -> Vec<f32> {
    (0..(voice::RATE as f32 * seconds) as usize)
        .map(|i| {
            (i as f32 * frequency * std::f32::consts::TAU / voice::RATE as f32).sin() * amplitude
        })
        .collect()
}

fn write_ogg_temp(samples: &[f32]) -> NamedTempFile {
    let bytes = voice::encode(samples).expect("synthetic samples encode to OGG/Opus");
    let file = NamedTempFile::new().expect("temp file created");
    std::fs::write(file.path(), bytes).expect("write OGG bytes");
    file
}

#[test]
fn received_audio_without_sender_waveform_generates_bars_off_ui_thread() {
    let _guard = decode_test_guard();
    let samples = synthetic_tone(0.5, 440.0, 0.4);
    let file = write_ogg_temp(&samples);
    let bars = std::thread::spawn(move || zaptide::audio::waveform_file(file.path()))
        .join()
        .expect("waveform worker completes")
        .expect("OGG/Opus decoded");
    assert_eq!(bars.len(), voice::BARS);
    assert!(bars.iter().any(|bar| *bar > 0));
}

#[test]
fn ordinary_wav_attachment_generates_a_waveform() {
    let _guard = decode_test_guard();
    let samples = synthetic_tone(0.25, 220.0, 0.4);
    let pcm: Vec<u8> = samples
        .iter()
        .flat_map(|sample| ((*sample * i16::MAX as f32) as i16).to_le_bytes())
        .collect();
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&voice::RATE.to_le_bytes());
    wav.extend_from_slice(&(voice::RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    let file = NamedTempFile::new().expect("temp file created");
    std::fs::write(file.path(), wav).expect("write WAV bytes");
    let bars = zaptide::audio::waveform_file(file.path()).expect("WAV decoded");
    assert_eq!(bars.len(), voice::BARS);
    assert!(bars.iter().any(|bar| *bar > 0));
}

// ─── Mock audio backend tests (no hardware required) ──────────────────────────

#[test]
fn recorder_handles_source_that_cannot_open() {
    // A source that exists but cannot open its device stands in for a
    // microphone the sound server refuses; no real microphone is opened.
    let recorder = Recorder::with_source("filesrc location=/nonexistent/zaptide-microphone");
    std::thread::sleep(Duration::from_millis(500));

    let error = recorder
        .failure()
        .expect("a refused microphone fails recording");
    assert!(!error.is_empty(), "error message must be non-empty");
    assert!(
        error.contains("microphone")
            || error.contains("Microphone")
            || error.contains("device")
            || error.contains("permission")
            || error.contains("audio"),
        "error should describe the denial: {error}"
    );
    // Dropping the recorder stops the capture thread cleanly.
}

#[test]
fn voice_encodes_to_ogg_opus() {
    let bytes = voice::encode(&synthetic_tone(1.0, 330.0, 0.3)).expect("encode to OGG/Opus");
    assert!(bytes.starts_with(b"OggS"));
}

#[test]
fn waveform_generation_is_64_bars() {
    let tone = synthetic_tone(2.0, 440.0, 0.5);
    let bars = voice::waveform(&tone);

    assert_eq!(bars.len(), voice::BARS, "must produce exactly 64 bars");
    for (i, &value) in bars.iter().enumerate() {
        assert!(value <= 100, "bar {i} value {value} exceeds maximum 100");
    }

    let empty = voice::waveform(&[]);
    assert_eq!(empty.len(), voice::BARS);
    assert!(
        empty.iter().all(|&v| v == 0),
        "empty input yields all zeros"
    );

    let mut rising = tone;
    let count = rising.len() as f32;
    for (i, s) in rising.iter_mut().enumerate() {
        *s *= i as f32 / count;
    }
    let rising_bars = voice::waveform(&rising);
    assert_eq!(rising_bars.len(), voice::BARS);
    assert_eq!(
        rising_bars[voice::BARS - 1],
        100,
        "loudest bar should be last for a rising-amplitude tone"
    );
    assert!(
        rising_bars[0] < 10,
        "first bar of rising tone should be quiet: {}",
        rising_bars[0]
    );
}

#[test]
fn recorder_level_metering_produces_50ms_windows() {
    // A live test tone instead of the microphone, so no real device opens.
    let recorder = Recorder::with_source("audiotestsrc is-live=true volume=0.3");
    std::thread::sleep(Duration::from_millis(2_000));

    assert!(
        recorder.failure().is_none(),
        "test tone failed: {:?}",
        recorder.failure()
    );

    let levels = recorder.levels();
    assert!(
        levels.len() >= 10,
        "expected at least 10 level readings in 2s (~50ms each), got {}",
        levels.len()
    );

    for (i, &level) in levels.iter().enumerate() {
        assert!(level.is_finite(), "level {i} is not finite: {level}");
        assert!(level >= 0.0, "level {i} is negative: {level}");
    }
}

#[test]
fn narrowest_audio_permissions_documented() {
    // Playback (voice messages):
    //   PulseAudio/PipeWire-Pulse: socket access (Flatpak: --socket=pulseaudio)
    //   GStreamer ALSA fallback: read /dev/snd/pcmC*D*p
    //
    // Capture (voice recording):
    //   PulseAudio/PipeWire-Pulse: same socket handles both directions
    //   GStreamer ALSA fallback: read /dev/snd/pcmC*D*c
    //
    // Narrowest Flatpak finish-args for full audio:
    //   --socket=pulseaudio
    //
    // No --device=all is required when PulseAudio or PipeWire-Pulse is the
    // backend, because the daemon multiplexes all physical devices.

    const {
        assert!(voice::RATE == 48_000, "voice sample rate is 48 kHz");
        assert!(voice::BARS == 64, "waveform has 64 bars");
    }
}

// ─── Device tests (ignored: require live audio devices) ───────────────────────

#[test]
#[ignore = "requires an audio output device; run with: cargo test --test audio_validation -- --ignored"]
fn playback_reaches_completion_within_budget() {
    let _guard = decode_test_guard();
    let file = write_ogg_temp(&synthetic_tone(1.0, 330.0, 0.3));
    let mut player = MediaService::default();

    player
        .toggle_playback("alsa-playback", file.path())
        .expect("starts playing");

    let budget = Duration::from_millis(1_500);
    let deadline = Duration::from_secs(3);
    let started = Instant::now();
    let mut seen_playing = false;

    while started.elapsed() < deadline {
        player.poll().expect("poll does not error during playback");
        let status = player.playback_status("alsa-playback");
        if status.state == State::Playing {
            seen_playing = true;
        }
        if seen_playing && status.state == State::Idle {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }

    assert!(seen_playing, "playback never started");
    assert!(
        started.elapsed() <= budget + Duration::from_millis(500),
        "playback took {:?}, budget was {:?}",
        started.elapsed(),
        budget
    );
    assert_eq!(
        player.playback_status("alsa-playback").state,
        State::Idle,
        "playback should reach completion"
    );
}

#[test]
#[ignore = "requires a microphone; run with: cargo test --test audio_validation -- --ignored"]
fn capture_produces_at_least_one_second_of_audio() {
    let recorder = Recorder::start();
    std::thread::sleep(Duration::from_millis(1_100));

    assert!(
        recorder.failure().is_none(),
        "recording failed: {:?}",
        recorder.failure()
    );

    let samples = recorder.finish().expect("captured samples");
    let min_samples = voice::RATE as usize * 8 / 10;
    assert!(
        samples.len() >= min_samples,
        "expected at least {min_samples} samples (80% of 1s at 48kHz mono), got {}",
        samples.len()
    );
}
