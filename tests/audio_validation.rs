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
//! | `player_handles_missing_device_gracefully`     | No (tests absence) | No    | No      |
//! | `recorder_handles_permission_denied`           | No            | No (tests denial) | No |
//! | `player_respects_seek_within_bounds`           | Optional      | No         | No      |
//! | `recorder_level_metering_produces_50ms_windows`| No            | Optional   | No      |
//! | `alsa_playback_reaches_completion_within_budget`| ALSA         | No         | No      |
//! | `alsa_capture_produces_at_least_one_second_of_audio` | ALSA  | ALSA       | No      |
//! | `player_handles_device_unplug_during_playback` | Deferred      | No         | No      |
//! | `recorder_handles_device_unplug_during_capture`| No            | Deferred   | No      |
//! | `flatpak_microphone_denial_*`                  | No            | Sandbox    | Yes     |
//! | `flatpak_microphone_revocation_*`              | No            | Sandbox    | Yes     |

use std::time::{Duration, Instant};

use tempfile::NamedTempFile;

use zaptide::audio::{Player, Recorder, State};
use zaptide::voice;

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

fn poll_until_loaded(player: &mut Player, message: &str, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        match player.poll() {
            Ok(()) => {
                if player.status(message).state != State::Loading {
                    return Ok(());
                }
            }
            Err(error) => return Err(error),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("decode timed out".to_owned())
}

// ─── Mock audio backend tests (no hardware required) ──────────────────────────

#[test]
fn player_handles_missing_device_gracefully() {
    let file = write_ogg_temp(&synthetic_tone(0.5, 440.0, 0.3));
    let mut player = Player::default();

    player
        .toggle("missing-device", file.path())
        .expect("toggle spawns decode thread");

    // Poll picks up the decoded clip and attempts to open the output device.
    // Without a device, restart() returns a recoverable error; with one,
    // playback begins. Neither path panics.
    match poll_until_loaded(&mut player, "missing-device", Duration::from_secs(3)) {
        Ok(()) => {
            // Device was available; stop playback.
            player.stop();
        }
        Err(error) => {
            // Device was missing or denied; verify the error is a clean string.
            assert!(
                error.contains("No sound output") || error.contains("device"),
                "unexpected error: {error}"
            );
            player.stop();
        }
    }
}

#[test]
fn recorder_handles_permission_denied() {
    // In CI without a microphone, Recorder::start() fails gracefully.
    // With a microphone present, recording proceeds. Either way, no panic.
    let recorder = Recorder::start();
    std::thread::sleep(Duration::from_millis(500));

    if let Some(error) = recorder.failure() {
        assert!(!error.is_empty(), "error message must be non-empty");
        assert!(
            error.contains("microphone")
                || error.contains("Microphone")
                || error.contains("device")
                || error.contains("permission")
                || error.contains("audio"),
            "error should describe the denial: {error}"
        );
    }
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
fn player_respects_seek_within_bounds() {
    let file = write_ogg_temp(&synthetic_tone(2.0, 440.0, 0.3));
    let mut player = Player::default();

    player
        .seek("seek-msg", file.path(), 0.0)
        .expect("seek spawns decode thread");

    let device_available =
        poll_until_loaded(&mut player, "seek-msg", Duration::from_secs(3)).is_ok();

    if !device_available {
        // No output device: subsequent seeks still return cleanly, no panic.
        let _ = player.seek("seek-msg", file.path(), 0.5);
        let _ = player.poll();
        let _ = player.seek("seek-msg", file.path(), 1.0);
        let _ = player.poll();
        player.stop();
        return;
    }

    // Device available: verify seek to middle lands near 50%.
    player
        .seek("seek-msg", file.path(), 0.5)
        .expect("seek to 0.5");
    std::thread::sleep(Duration::from_millis(100));
    let _ = player.poll();
    let status = player.status("seek-msg");
    let total = status.total.as_secs_f32();
    if total > 0.0 {
        let fraction = status.position.as_secs_f32() / total;
        assert!(
            (0.35..=0.75).contains(&fraction),
            "seek(0.5) landed at {fraction:.2} of total"
        );
    }

    // Seek to end does not panic.
    let _ = player.seek("seek-msg", file.path(), 1.0);
    std::thread::sleep(Duration::from_millis(100));
    let _ = player.poll();

    player.stop();
}

#[test]
fn recorder_level_metering_produces_50ms_windows() {
    let recorder = Recorder::start();
    std::thread::sleep(Duration::from_millis(2_000));

    if let Some(error) = recorder.failure() {
        // No microphone available: clean error, not a panic.
        assert!(!error.is_empty());
        return;
    }

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
    //   ALSA fallback: read /dev/snd/pcmC*D*p
    //
    // Capture (voice recording):
    //   PulseAudio/PipeWire-Pulse: same socket handles both directions
    //   ALSA fallback: read /dev/snd/pcmC*D*c
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

// ─── ALSA-specific tests (ignored: require live audio devices) ─────────────────

#[test]
#[ignore = "requires ALSA output device; run with: cargo test --test audio_validation -- --ignored"]
fn alsa_playback_reaches_completion_within_budget() {
    let file = write_ogg_temp(&synthetic_tone(1.0, 330.0, 0.3));
    let mut player = Player::default();

    player
        .toggle("alsa-playback", file.path())
        .expect("starts decoding");

    let budget = Duration::from_millis(1_500);
    let deadline = Duration::from_secs(3);
    let started = Instant::now();
    let mut seen_playing = false;

    while started.elapsed() < deadline {
        player.poll().expect("poll does not error during playback");
        let status = player.status("alsa-playback");
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
        player.status("alsa-playback").state,
        State::Idle,
        "playback should reach completion"
    );
}

#[test]
#[ignore = "requires ALSA capture device; run with: cargo test --test audio_validation -- --ignored"]
fn alsa_capture_produces_at_least_one_second_of_audio() {
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

// ─── Deferred: device hotplug simulation ──────────────────────────────────────
//
// These tests require physical device removal events that cannot be reliably
// reproduced in CI without hardware. The underlying logic is covered by unit
// tests in src/audio.rs:
//
//   player unplug  → audio::tests::an_unplugged_playback_sink_marks_clip_complete
//   recorder unplug → audio::tests::recorder_handles_unplug_and_cancellation_with_synthetic_samples

#[test]
#[ignore = "requires physical device hotplug; covered by unit tests in src/audio.rs"]
fn player_handles_device_unplug_during_playback() {
    unimplemented!(
        "start playback on a real device, remove it mid-stream, \
         verify playback_ended() fires and state returns to Idle"
    );
}

#[test]
#[ignore = "requires physical device hotplug; covered by unit tests in src/audio.rs"]
fn recorder_handles_device_unplug_during_capture() {
    unimplemented!(
        "start recording, remove the microphone mid-capture, \
         verify record_with() breaks and returns partial samples"
    );
}

// ─── Flatpak permission simulation ────────────────────────────────────────────
//
// Flatpak enforces audio access through the sandbox, not through ZapTide code.
// These skeletons document the expected behavior and the exact commands needed
// to reproduce the denial or revocation scenarios.
//
// Required Flatpak finish-args for audio:
//   --socket=pulseaudio     (PulseAudio or PipeWire-Pulse socket; covers both
//                            playback and capture)
//
// To reproduce microphone denial:
//   flatpak run --nosocket=pulseaudio --noflatpak-spawn com.luminus.ZapTide
//
// To revoke microphone permission at runtime:
//   flatpak permission-remove com.luminus.ZapTide   (then restart the app)
//
// Expected behavior on denial:
//   Recorder::start() → thread fails → failure() returns
//   "No microphone available: ..." and the app shows a recoverable toast.

#[test]
#[ignore = "requires Flatpak sandbox; run: flatpak run --nosocket=pulseaudio ..."]
fn flatpak_microphone_denial_produces_recoverable_error() {
    // Under Flatpak with --nosocket=pulseaudio:
    //
    // let recorder = Recorder::start();
    // std::thread::sleep(Duration::from_millis(500));
    // let error = recorder.failure().expect("microphone must be denied");
    // assert!(
    //     error.contains("microphone") || error.contains("device"),
    //     "error should describe the denial: {error}"
    // );
    unimplemented!("run inside Flatpak with --nosocket=pulseaudio");
}

#[test]
#[ignore = "requires Flatpak sandbox with runtime permission revocation"]
fn flatpak_microphone_revocation_stops_capture() {
    // Under Flatpak, after `flatpak permission-remove com.luminus.ZapTide`:
    //
    // 1. Recorder::start() succeeds initially
    // 2. The PulseAudio socket is revoked mid-capture
    // 3. record_with() detects the stream error and returns partial samples
    // 4. Subsequent Recorder::start() calls fail with a clean error
    unimplemented!("requires Flatpak Portal runtime interaction");
}
