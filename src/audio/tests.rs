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

mod formats {
    use super::*;
    use gst::prelude::*;
    use gstreamer as gst;
    use gstreamer_app as gst_app;

    pub(super) const SOURCE_RATE: u32 = 44_100;
    const SECONDS: f32 = 3.0;

    /// A stereo clip whose loudness rises and falls, so its bars differ.
    pub(super) fn source() -> Vec<f32> {
        let frames = (SOURCE_RATE as f32 * SECONDS) as usize;
        let mut interleaved = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let t = frame as f32 / SOURCE_RATE as f32;
            let phase = t / SECONDS;
            let envelope = 0.1
                + 0.6
                    * (std::f32::consts::PI * phase).sin().powi(2)
                    * (0.7 + 0.3 * (std::f32::consts::TAU * 1.5 * phase).sin());
            interleaved.push((std::f32::consts::TAU * 440.0 * t).sin() * envelope);
            interleaved.push((std::f32::consts::TAU * 660.0 * t).sin() * envelope);
        }
        interleaved
    }

    /// Encodes `source()` with `encoder` into a temporary file, or `None`
    /// when this machine lacks one of its elements.
    pub(super) fn fixture(encoder: &str) -> Option<tempfile::NamedTempFile> {
        gst::init().ok()?;
        let file = tempfile::NamedTempFile::new().ok()?;
        let pipeline = gst::parse::launch(&format!(
            "appsrc name=in format=time \
             caps=audio/x-raw,format=F32LE,rate={SOURCE_RATE},channels=2,layout=interleaved ! \
             audioconvert ! audioresample ! {encoder} ! filesink name=out"
        ))
        .ok()?
        .downcast::<gst::Pipeline>()
        .ok()?;
        pipeline
            .by_name("out")?
            .set_property("location", file.path().to_string_lossy().as_ref());
        let input = pipeline.by_name("in")?.downcast::<gst_app::AppSrc>().ok()?;
        pipeline.set_state(gst::State::Playing).ok()?;
        const FRAMES: usize = 1_024;
        for (index, chunk) in source().chunks(FRAMES * 2).enumerate() {
            let bytes: Vec<u8> = chunk
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect();
            let mut buffer = gst::Buffer::from_slice(bytes);
            let at = |frame: usize| {
                gst::ClockTime::from_nseconds(frame as u64 * 1_000_000_000 / u64::from(SOURCE_RATE))
            };
            let start = index * FRAMES;
            let buffer_mut = buffer.get_mut()?;
            buffer_mut.set_pts(at(start));
            buffer_mut.set_duration(at(start + chunk.len() / 2) - at(start));
            input.push_buffer(buffer).ok()?;
        }
        input.end_of_stream().ok()?;
        let finished = pipeline.bus()?.timed_pop_filtered(
            gst::ClockTime::from_seconds(20),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        let _ = pipeline.set_state(gst::State::Null);
        matches!(finished?.view(), gst::MessageView::Eos(..)).then_some(file)
    }

    pub(super) const ENCODERS: [(&str, &str); 5] = [
        ("WAV", "audio/x-raw,format=F32LE ! wavenc"),
        ("Ogg Vorbis", "vorbisenc ! oggmux"),
        ("Ogg Opus", "opusenc ! oggmux"),
        ("MP3", "lamemp3enc"),
        ("AAC in MP4", "fdkaacenc ! mp4mux"),
    ];

    pub(super) fn bars(samples: &[f32]) -> Vec<u8> {
        waveform_cancellable(samples, &AtomicBool::new(false)).expect("bars")
    }

    pub(super) fn largest_difference(left: &[u8], right: &[u8]) -> u8 {
        left.iter()
            .zip(right)
            .map(|(left, right)| left.abs_diff(*right))
            .max()
            .unwrap_or(0)
    }
}

/// Received clips in every supported container decode to the length and
/// bars of the clip that was encoded, downmixed and resampled the way every
/// decoded clip is. Lossless float WAV must match exactly. The other
/// tolerances come from lossy encoders, not from the decoder: MP3 and AAC
/// add encoder delay at the start (about 30 ms and 65 ms), which shifts the
/// bars by up to a bar and a half on a clip whose loudness changes. The
/// enforced maximum bar differences are WAV 0, Vorbis 5, Opus 6, MP3 10,
/// and AAC 22 points out of 100.
#[test]
fn received_formats_decode_close_to_their_source() {
    const TOLERANCES: [(u8, u32); 5] = [(0, 0), (5, 10), (6, 20), (10, 50), (22, 100)];
    let expected = collect_samples_limited(
        formats::source().into_iter(),
        2,
        formats::SOURCE_RATE,
        &AtomicBool::new(false),
        MAX_AUDIO_SAMPLES,
    )
    .expect("the source resamples");
    let expected_samples = expected.len();
    let expected_bars = formats::bars(&expected);
    for ((name, encoder), (bars, milliseconds)) in formats::ENCODERS.into_iter().zip(TOLERANCES) {
        let Some(file) = formats::fixture(encoder) else {
            // wavenc ships with gst-plugins-good, which playback needs anyway.
            assert_ne!(name, "WAV", "the lossless WAV check must run");
            assert!(
                std::env::var_os("ZAPTIDE_REQUIRE_GST").is_none(),
                "{name} fixture unavailable: {encoder}"
            );
            eprintln!("skipping {name}: `{encoder}` is not available here");
            continue;
        };
        let decoded = decode_file(file.path()).unwrap_or_else(|error| panic!("{name}: {error}"));
        let difference = decoded.len().abs_diff(expected_samples);
        assert!(
            difference <= voice::RATE as usize * milliseconds as usize / 1_000,
            "{name}: {} samples, expected about {expected_samples}",
            decoded.len()
        );
        let largest = formats::largest_difference(&formats::bars(&decoded), &expected_bars);
        assert!(largest <= bars, "{name}: bars differ by up to {largest}");
    }
}

fn have_test_source() -> bool {
    crate::calls::gst_ready(&["audiotestsrc"])
}

#[test]
fn recording_a_test_source_makes_a_voice_note_of_its_length() {
    if !have_test_source() {
        eprintln!("skipping: audiotestsrc is not available here");
        return;
    }
    let recorder = Recorder::with_source("audiotestsrc is-live=true freq=440 volume=0.3");
    std::thread::sleep(Duration::from_millis(1_000));
    assert!(recorder.failure().is_none(), "{:?}", recorder.failure());
    let levels = recorder.levels();
    let samples = recorder.finish().expect("the test tone was recorded");
    let seconds = samples.len() as f32 / voice::RATE as f32;
    // Wide bounds: the thread may start late or stop late on a busy machine.
    assert!((0.5..2.5).contains(&seconds), "recorded {seconds} s");
    assert!(levels.len() >= 8, "{} level readings", levels.len());
    assert!(levels.iter().all(|level| *level > 0.1 && *level < 0.4));

    let note = voice::encode(&samples).expect("encodes the recording");
    assert!(note.starts_with(b"OggS"));
    let decoded = decode_opus(std::io::Cursor::new(note), &AtomicBool::new(false))
        .expect("the voice note is valid Ogg Opus");
    // The encoder pads the last 20 ms Opus frame.
    assert!(
        decoded.len().abs_diff(samples.len()) <= voice::RATE as usize / 50,
        "{} decoded samples for {} recorded",
        decoded.len(),
        samples.len()
    );
}

#[test]
fn a_source_that_ends_keeps_what_it_recorded() {
    if !have_test_source() {
        eprintln!("skipping: audiotestsrc is not available here");
        return;
    }
    // Pin the source rate so resampling cannot alter the exact sample count.
    let recorder = Recorder::with_source(
        "audiotestsrc is-live=true num-buffers=30 samplesperbuffer=1024 volume=0.3 ! audio/x-raw,rate=48000,channels=1",
    );
    std::thread::sleep(Duration::from_millis(1_200));
    let samples = recorder.finish().expect("partial samples are usable");
    assert_eq!(samples.len(), 30 * 1_024);
}

#[test]
fn a_missing_or_refused_microphone_is_a_readable_error() {
    if !have_test_source() {
        eprintln!("skipping: audiotestsrc is not available here");
        return;
    }
    let missing = Recorder::with_source("zaptidenosuchsrc")
        .finish()
        .unwrap_err();
    assert!(missing.starts_with("No microphone available:"), "{missing}");

    // A source that exists but cannot open its device, as when the sound
    // server refuses the microphone.
    let refused = Recorder::with_source("filesrc location=/nonexistent/zaptide-microphone")
        .finish()
        .unwrap_err();
    assert!(
        refused.starts_with("Could not open the microphone:"),
        "{refused}"
    );
}

#[test]
fn a_streaming_manifest_posing_as_audio_is_not_followed() {
    if !crate::calls::gst_ready(&["hlsdemux"]) {
        eprintln!("skipping: no HLS demuxer here, so nothing could follow the manifest");
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("local listener");
    listener
        .set_nonblocking(true)
        .expect("non-blocking listener");
    let port = listener.local_addr().expect("listener address").port();
    let manifest = tempfile::NamedTempFile::new().expect("temporary manifest");
    std::fs::write(
        manifest.path(),
        format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:1.0,\nhttp://127.0.0.1:{port}/segment.aac\n#EXT-X-ENDLIST\n"
        ),
    )
    .expect("write manifest");

    assert!(decode_file(manifest.path()).is_err());
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
        "the decoder connected to the address in the manifest"
    );
}

fn wav_header(channels: u16, rate: u32, data: &[u8]) -> Vec<u8> {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    wav.extend_from_slice(&(channels * 2).to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(data);
    wav
}

#[test]
fn implausible_rates_and_channel_counts_are_refused() {
    if !crate::calls::gst_ready(&["wavparse"]) {
        return;
    }
    let data = vec![0x10; 4_096 * 18];
    for (channels, rate) in [(9, 48_000), (2, 768_000)] {
        let file = tempfile::NamedTempFile::new().expect("temporary clip");
        std::fs::write(file.path(), wav_header(channels, rate, &data)).expect("write clip");
        assert_eq!(
            decode_file(file.path()),
            Err(audio_limit_error()),
            "{channels} channels at {rate} Hz"
        );
    }
}

#[test]
fn a_format_change_mid_stream_is_refused() {
    use gstreamer::prelude::*;
    if !crate::calls::gst_ready(&["vorbisenc"]) {
        eprintln!("skipping: vorbisenc is not available here");
        return;
    }
    // A chained Ogg file: a second stream at another rate follows the first.
    let mut chained = Vec::new();
    for rate in [44_100, 22_050] {
        let file = tempfile::NamedTempFile::new().expect("temporary link");
        let pipeline = gstreamer::parse::launch(&format!(
            "audiotestsrc num-buffers=20 ! audio/x-raw,rate={rate},channels=1 ! audioconvert ! \
             vorbisenc ! oggmux ! filesink location={}",
            file.path().display()
        ))
        .expect("encoder pipeline");
        pipeline
            .set_state(gstreamer::State::Playing)
            .expect("encode");
        let _ = pipeline.bus().expect("bus").timed_pop_filtered(
            gstreamer::ClockTime::from_seconds(10),
            &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
        );
        let _ = pipeline.set_state(gstreamer::State::Null);
        chained.extend(std::fs::read(file.path()).expect("read link"));
    }
    let file = tempfile::NamedTempFile::new().expect("temporary clip");
    std::fs::write(file.path(), chained).expect("write chained clip");
    let error = decode_file(file.path()).unwrap_err();
    assert!(error.contains("changed mid-stream"), "{error}");
}

/// Runs the recorder's real microphone path with every audio source ranked
/// out, in a child process so the environment cannot leak into other tests,
/// and so no real microphone can be opened.
#[test]
fn without_a_sound_server_recording_reports_no_microphone() {
    const CHILD: &str = "ZAPTIDE_TEST_NO_AUDIO_SOURCES";
    if std::env::var_os(CHILD).is_some() {
        let recorder = Recorder::start();
        // Long enough that a silent fallback would have recorded something.
        std::thread::sleep(Duration::from_millis(300));
        match recorder.finish() {
            Ok(samples) => panic!("recorded {} samples of silence", samples.len()),
            Err(error) => assert!(error.starts_with("No microphone available:"), "{error}"),
        }
        return;
    }
    if !crate::calls::gst_ready(&["autoaudiosrc", "audiotestsrc"]) {
        eprintln!("skipping: autoaudiosrc is not available here");
        return;
    }
    let ranked_out: Vec<String> = gstreamer::ElementFactory::factories_with_type(
        gstreamer::ElementFactoryType::SRC,
        gstreamer::Rank::NONE,
    )
    .into_iter()
    .filter(|factory| {
        use gstreamer::prelude::*;
        factory.name() != "autoaudiosrc"
    })
    .map(|factory| {
        use gstreamer::prelude::*;
        format!("{}:NONE", factory.name())
    })
    .collect();
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "audio::tests::without_a_sound_server_recording_reports_no_microphone",
            "--exact",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("GST_PLUGIN_FEATURE_RANK", ranked_out.join(","))
        .env("PULSE_SERVER", "unix:/nonexistent/zaptide-pulse")
        .env("PIPEWIRE_REMOTE", "/nonexistent/zaptide-pipewire")
        .output()
        .expect("child test runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
