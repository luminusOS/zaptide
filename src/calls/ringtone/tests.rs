use super::*;
use std::cell::Cell;

#[derive(Default)]
struct Log {
    started: Cell<usize>,
    playing: Cell<usize>,
    refuse: Cell<bool>,
}

struct Fake<'a>(&'a Log);

struct Sound<'a>(&'a Log);

impl Drop for Sound<'_> {
    fn drop(&mut self) {
        self.0.playing.set(self.0.playing.get() - 1);
    }
}

impl<'a> Output for Fake<'a> {
    type Playing = Sound<'a>;

    fn play(&self, _: Ring) -> Option<Sound<'a>> {
        self.0.started.set(self.0.started.get() + 1);
        if self.0.refuse.get() {
            return None;
        }
        self.0.playing.set(self.0.playing.get() + 1);
        Some(Sound(self.0))
    }
}

#[test]
fn asking_for_the_ring_that_is_going_does_not_restart_it() {
    let log = Log::default();
    let mut ringer = Ringer::with_output(Fake(&log));
    ringer.set(Some(Ring::Incoming));
    ringer.set(Some(Ring::Incoming));
    assert_eq!((log.started.get(), log.playing.get()), (1, 1));
    assert_eq!(ringer.tone(), Some(Ring::Incoming));
}

#[test]
fn changing_the_tone_stops_the_old_one_first() {
    let log = Log::default();
    let mut ringer = Ringer::with_output(Fake(&log));
    ringer.set(Some(Ring::Outgoing));
    ringer.set(Some(Ring::Incoming));
    assert_eq!((log.started.get(), log.playing.get()), (2, 1));
    assert_eq!(ringer.tone(), Some(Ring::Incoming));
}

#[test]
fn stopping_is_idempotent_and_silences_at_once() {
    let log = Log::default();
    let mut ringer = Ringer::with_output(Fake(&log));
    ringer.stop();
    ringer.set(Some(Ring::Incoming));
    ringer.stop();
    ringer.stop();
    ringer.set(None);
    assert_eq!((log.started.get(), log.playing.get()), (1, 0));
    assert_eq!(ringer.tone(), None);
}

#[test]
fn dropping_the_ringer_stops_the_sound() {
    let log = Log::default();
    {
        let mut ringer = Ringer::with_output(Fake(&log));
        ringer.set(Some(Ring::Outgoing));
        assert_eq!(log.playing.get(), 1);
    }
    assert_eq!(log.playing.get(), 0);
}

#[test]
fn a_ring_that_cannot_start_is_not_retried_until_asked_again_after_a_stop() {
    let log = Log::default();
    log.refuse.set(true);
    let mut ringer = Ringer::with_output(Fake(&log));
    ringer.set(Some(Ring::Incoming));
    ringer.set(Some(Ring::Incoming));
    assert_eq!(log.started.get(), 1);
    log.refuse.set(false);
    ringer.stop();
    ringer.set(Some(Ring::Incoming));
    assert_eq!((log.started.get(), log.playing.get()), (2, 1));
}

#[test]
fn the_made_tones_stay_quiet_and_fade_in_and_out() {
    for tone in [Ring::Incoming, Ring::Outgoing] {
        let samples = cadence(tone);
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 1000, "{tone:?} is audible");
        assert!(
            f32::from(peak) <= LEVEL * f32::from(i16::MAX) + 1.0,
            "{tone:?} is not loud"
        );
        assert_eq!(samples[0], 0, "no click at the start");
        assert_eq!(*samples.last().unwrap(), 0, "a pause ends each repeat");
    }
    assert_eq!(cadence(Ring::Incoming).len(), 3 * SAMPLE_RATE);
    assert_eq!(cadence(Ring::Outgoing).len(), 4 * SAMPLE_RATE);
}

#[test]
fn a_sound_is_found_in_the_first_data_directory_that_has_it() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let place = |root: &Path, file: &str| {
        let directory = root.join("sounds").join(SOUND_THEME).join("stereo");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(file), b"x").unwrap();
    };
    place(second.path(), "phone-incoming-call.oga");
    place(first.path(), "phone-incoming-call.wav");
    let directories = [first.path().to_path_buf(), second.path().to_path_buf()];
    let found = sound_in(&directories, "phone-incoming-call").unwrap();
    assert!(found.starts_with(first.path()));
    assert_eq!(sound_in(&directories, "phone-outgoing-calling"), None);
}

fn listen_for_two_seconds(context: &gst::glib::MainContext) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < until {
        context.iteration(false);
    }
}

#[test]
#[ignore = "needs an audio server"]
fn the_speaker_plays_and_stops_both_tones() {
    gst::init().unwrap();
    let context = gst::glib::MainContext::new();
    context
        .with_thread_default(|| {
            for tone in [Ring::Incoming, Ring::Outgoing] {
                let mut ringer = Ringer::new();
                ringer.set(Some(tone));
                let Some((_, Some(playback))) = ringer.current.as_ref() else {
                    panic!("{tone:?} did not start");
                };
                listen_for_two_seconds(&context);
                assert_eq!(playback.state(), Some(gst::State::Playing), "{tone:?}");
                ringer.stop();
                assert_eq!(ringer.tone(), None);
            }
        })
        .unwrap();
}

#[test]
#[ignore = "needs an audio server"]
fn the_made_tone_plays_without_a_sound_theme() {
    gst::init().unwrap();
    let context = gst::glib::MainContext::new();
    context
        .with_thread_default(|| {
            let slot = Rc::new(RefCell::new(Slot::default()));
            assert!(launch(&slot, Ring::Incoming, false));
            listen_for_two_seconds(&context);
            let pipeline = slot.borrow().pipeline.clone().expect("running");
            assert_eq!(
                pipeline.state(gst::ClockTime::from_seconds(5)).1,
                gst::State::Playing
            );
        })
        .unwrap();
}
