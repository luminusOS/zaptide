use super::*;
use std::io;
use std::sync::Barrier;
use std::time::Instant;

#[test]
fn speed_labels_match_the_button() {
    assert_eq!(speed_label(SPEEDS[0]), "1x");
    assert_eq!(speed_label(1.5), "1.5x");
    assert_eq!(speed_label(SPEEDS[2]), "2x");
}

#[test]
fn unusable_speeds_are_kept_in_range() {
    assert_eq!(playable_speed(f32::NAN), 1.0);
    assert_eq!(playable_speed(f32::INFINITY), 1.0);
    assert_eq!(playable_speed(50.0), 2.0);
    assert_eq!(playable_speed(-3.0), 1.0);
    assert_eq!(playable_speed(1.75), 1.75);
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
    let err = read_audio(io::empty(), MAX_AUDIO_BYTES + 1, &AtomicBool::new(false)).unwrap_err();
    assert_eq!(err, audio_limit_error());
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
    let decoded =
        decode_opus(std::io::Cursor::new(bytes), &AtomicBool::new(false)).expect("decodes tone");
    assert!(!decoded.is_empty());
    assert!(decoded.len() <= MAX_AUDIO_SAMPLES);
    assert!(decoded.iter().any(|sample| sample.abs() > 0.01));
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
