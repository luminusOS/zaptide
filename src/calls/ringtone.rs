//! The sound a call makes while it rings: the desktop's ringtone for an
//! incoming call and its ring-back tone for one being placed, or a tone made
//! here when the sound theme or its codec is missing.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;

use super::pipeline_from;

#[cfg(test)]
mod tests;

const SAMPLE_RATE: usize = 8000;
const LEVEL: f32 = 0.2;
const FADE: usize = SAMPLE_RATE / 100;
const SOUND_THEME: &str = "freedesktop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ring {
    Incoming,
    Outgoing,
}

impl Ring {
    fn theme_name(self) -> &'static str {
        match self {
            Self::Incoming => "phone-incoming-call",
            Self::Outgoing => "phone-outgoing-calling",
        }
    }
}

/// Where a ring is heard. Dropping what `play` returns stops the sound.
pub trait Output {
    type Playing;

    /// `None` when the sound cannot be started at all.
    fn play(&self, tone: Ring) -> Option<Self::Playing>;
}

/// Plays at most one ring at a time and keeps it matching what the call needs.
pub struct Ringer<O: Output = Speaker> {
    output: O,
    current: Option<(Ring, Option<O::Playing>)>,
}

impl Ringer {
    pub fn new() -> Self {
        Self::with_output(Speaker)
    }
}

impl Default for Ringer {
    fn default() -> Self {
        Self::new()
    }
}

impl<O: Output> Ringer<O> {
    pub fn with_output(output: O) -> Self {
        Self {
            output,
            current: None,
        }
    }

    /// Starts, changes or stops the ring to match `wanted`. Asking again for the
    /// ring that is already going, or that already failed to start, does nothing.
    pub fn set(&mut self, wanted: Option<Ring>) {
        if self.tone() == wanted {
            return;
        }
        self.current = None;
        if let Some(tone) = wanted {
            self.current = Some((tone, self.output.play(tone)));
        }
    }

    pub fn stop(&mut self) {
        self.current = None;
    }

    pub fn tone(&self) -> Option<Ring> {
        self.current.as_ref().map(|(tone, _)| *tone)
    }
}

/// The default audio output of the desktop.
pub struct Speaker;

impl Output for Speaker {
    type Playing = Playback;

    fn play(&self, tone: Ring) -> Option<Playback> {
        Playback::start(tone)
    }
}

/// One ring in progress. It repeats until dropped.
pub struct Playback {
    _slot: Rc<RefCell<Slot>>,
}

#[derive(Default)]
struct Slot {
    pipeline: Option<gst::Pipeline>,
    watch: Option<gst::bus::BusWatchGuard>,
}

impl Slot {
    fn stop(&mut self) {
        self.watch = None;
        if let Some(pipeline) = self.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Playback {
    fn start(tone: Ring) -> Option<Self> {
        gst::init().ok()?;
        let slot = Rc::new(RefCell::new(Slot::default()));
        (launch(&slot, tone, true) || launch(&slot, tone, false)).then_some(Self { _slot: slot })
    }

    #[cfg(test)]
    fn state(&self) -> Option<gst::State> {
        let pipeline = self._slot.borrow().pipeline.clone()?;
        Some(pipeline.state(gst::ClockTime::from_seconds(5)).1)
    }
}

/// Starts the pipeline for `tone` into `slot`. A themed sound that fails later,
/// for want of a decoder or an output, is replaced by the made tone.
fn launch(slot: &Rc<RefCell<Slot>>, tone: Ring, themed: bool) -> bool {
    let pipeline = if themed {
        themed_pipeline(tone)
    } else {
        made_pipeline(tone)
    };
    let Some(pipeline) = pipeline else {
        return false;
    };
    let Some(bus) = pipeline.bus() else {
        return false;
    };
    let weak = Rc::downgrade(slot);
    let Ok(watch) = bus.add_watch_local(move |_, message| {
        if matches!(message.view(), gst::MessageView::Error(_)) {
            let weak = weak.clone();
            gst::glib::idle_add_local_once(move || {
                let Some(slot) = weak.upgrade() else {
                    return;
                };
                slot.borrow_mut().stop();
                if themed {
                    launch(&slot, tone, false);
                }
            });
        }
        gst::glib::ControlFlow::Continue
    }) else {
        return false;
    };
    if pipeline.set_state(gst::State::Playing).is_err() {
        let _ = pipeline.set_state(gst::State::Null);
        return false;
    }
    let mut slot = slot.borrow_mut();
    slot.stop();
    slot.pipeline = Some(pipeline);
    slot.watch = Some(watch);
    true
}

fn themed_pipeline(tone: Ring) -> Option<gst::Pipeline> {
    let file = sound_file(tone.theme_name())?;
    let uri = gst::glib::filename_to_uri(&file, None).ok()?.to_string();
    let playbin = gst::ElementFactory::make("playbin")
        .property("uri", &uri)
        .build()
        .ok()?;
    playbin.set_property_from_str("flags", "audio");
    playbin.connect("about-to-finish", false, move |values| {
        let playbin = values.first()?.get::<gst::Element>().ok()?;
        playbin.set_property("uri", &uri);
        None
    });
    playbin.downcast::<gst::Pipeline>().ok()
}

fn made_pipeline(tone: Ring) -> Option<gst::Pipeline> {
    let pipeline = pipeline_from(&format!(
        "appsrc name=tone format=time block=false \
         caps=audio/x-raw,format=S16LE,rate={SAMPLE_RATE},channels=1,layout=interleaved ! \
         audioconvert ! audioresample ! autoaudiosink"
    ))
    .ok()?;
    let source = pipeline
        .by_name("tone")?
        .downcast::<gst_app::AppSrc>()
        .ok()?;
    let cycle: Vec<u8> = cadence(tone)
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    let length = gst::ClockTime::from_nseconds(
        (cycle.len() / 2) as u64 * 1_000_000_000 / SAMPLE_RATE as u64,
    );
    let mut cycles = 0_u64;
    source.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |source, _| {
                let mut buffer = gst::Buffer::from_slice(cycle.clone());
                if let Some(buffer) = buffer.get_mut() {
                    buffer.set_pts(length * cycles);
                    buffer.set_duration(length);
                }
                cycles += 1;
                let _ = source.push_buffer(buffer);
            })
            .build(),
    );
    Some(pipeline)
}

/// One repeat of the tone made here, as 8 kHz mono samples.
fn cadence(tone: Ring) -> Vec<i16> {
    let samples = |seconds: f32| (seconds * SAMPLE_RATE as f32) as usize;
    let mut out = Vec::new();
    let burst = |out: &mut Vec<i16>, seconds: f32, frequencies: &[f32]| {
        let length = samples(seconds);
        for index in 0..length {
            let time = index as f32 / SAMPLE_RATE as f32;
            let wave: f32 = frequencies
                .iter()
                .map(|frequency| (std::f32::consts::TAU * frequency * time).sin())
                .sum::<f32>()
                / frequencies.len() as f32;
            let edge = index.min(length - 1 - index).min(FADE) as f32 / FADE as f32;
            out.push((wave * edge * LEVEL * f32::from(i16::MAX)) as i16);
        }
    };
    let silence = |out: &mut Vec<i16>, seconds: f32| out.resize(out.len() + samples(seconds), 0);
    match tone {
        Ring::Incoming => {
            burst(&mut out, 0.4, &[440.0, 480.0]);
            silence(&mut out, 0.2);
            burst(&mut out, 0.4, &[440.0, 480.0]);
            silence(&mut out, 2.0);
        }
        Ring::Outgoing => {
            burst(&mut out, 1.0, &[425.0]);
            silence(&mut out, 3.0);
        }
    }
    out
}

/// The sound `name` of the freedesktop sound theme, if some data directory has it.
fn sound_file(name: &str) -> Option<PathBuf> {
    let mut directories = vec![gst::glib::user_data_dir()];
    directories.extend(gst::glib::system_data_dirs());
    sound_in(&directories, name)
}

fn sound_in(directories: &[PathBuf], name: &str) -> Option<PathBuf> {
    directories.iter().find_map(|directory| {
        ["oga", "ogg", "wav"].iter().find_map(|extension| {
            let file = directory
                .join("sounds")
                .join(SOUND_THEME)
                .join("stereo")
                .join(format!("{name}.{extension}"));
            Path::is_file(&file).then_some(file)
        })
    })
}
