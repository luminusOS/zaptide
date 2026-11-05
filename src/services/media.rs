//! GTK-main-context facade for playback and recording.

use std::path::Path;

use crate::audio;
use crate::i18n::gettext;

use gst::prelude::*;
use gstreamer as gst;
use gtk4::gio::prelude::FileExt;

struct NativePlayback {
    message: String,
    path: std::path::PathBuf,
    pipeline: gst::Element,
    _hold: crate::media_retention::Hold,
    state: audio::State,
    ended: bool,
    rate_applied: bool,
    pending_seek: Option<f32>,
    /// Without `scaletempo` a rate other than 1x would change the pitch, so
    /// such a pipeline only plays at 1x.
    scaletempo: bool,
}

fn missing_scaletempo() -> String {
    gettext("Playing faster needs the GStreamer scaletempo plugin (gst-plugins-good)")
}

fn check_rate(scaletempo: bool, speed: f32) -> Result<(), String> {
    if scaletempo || speed <= audio::SPEEDS[0] {
        Ok(())
    } else {
        Err(missing_scaletempo())
    }
}

fn apply_rate(pipeline: &gst::Element, speed: f32) -> bool {
    let Some(position) = pipeline.query_position::<gst::ClockTime>() else {
        return false;
    };
    pipeline
        .seek(
            speed as f64,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            position,
            gst::SeekType::None,
            None::<gst::ClockTime>,
        )
        .is_ok()
}

pub struct MediaService {
    speed: f32,
    native: Option<NativePlayback>,
    recorder: Option<audio::Recorder>,
}

impl Default for MediaService {
    fn default() -> Self {
        Self {
            speed: audio::SPEEDS[0],
            native: None,
            recorder: None,
        }
    }
}

impl MediaService {
    /// Sets the playback speed for the clip playing now and for later ones.
    ///
    /// A clip playing without `scaletempo` keeps its 1x rate; the next
    /// `poll` stops it and reports the missing plugin.
    pub fn set_speed(&mut self, speed: f32) {
        let speed = audio::playable_speed(speed);
        if speed == self.speed {
            return;
        }
        self.speed = speed;
        if let Some(native) = &mut self.native
            && native.scaletempo
        {
            native.rate_applied = apply_rate(&native.pipeline, speed);
        }
    }

    pub fn speed(&self) -> f32 {
        self.speed
    }

    pub fn cycle_speed(&mut self) -> f32 {
        let next = audio::SPEEDS
            .iter()
            .copied()
            .find(|speed| *speed > self.speed())
            .unwrap_or(audio::SPEEDS[0]);
        self.set_speed(next);
        self.speed()
    }

    pub fn playback_status(&self, message: &str) -> audio::Status {
        if let Some(native) = &self.native
            && native.message == message
        {
            let position = native
                .pipeline
                .query_position::<gst::ClockTime>()
                .unwrap_or(gst::ClockTime::ZERO);
            let duration = native
                .pipeline
                .query_duration::<gst::ClockTime>()
                .unwrap_or(gst::ClockTime::ZERO);
            return audio::Status {
                state: if native.ended {
                    audio::State::Idle
                } else {
                    native.state
                },
                position: std::time::Duration::from_nanos(position.nseconds()),
                total: std::time::Duration::from_nanos(duration.nseconds()),
            };
        }
        audio::Status::IDLE
    }

    pub fn toggle_playback(&mut self, message: &str, path: &Path) -> Result<(), String> {
        {
            let speed = self.speed() as f64;
            if let Some(native) = &mut self.native
                && native.message == message
                && native.path == path
            {
                if native.state != audio::State::Playing {
                    check_rate(native.scaletempo, speed as f32)?;
                }
                if native.ended {
                    native
                        .pipeline
                        .seek(
                            speed,
                            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                            gst::SeekType::Set,
                            gst::ClockTime::ZERO,
                            gst::SeekType::None,
                            None::<gst::ClockTime>,
                        )
                        .map_err(|error| error.to_string())?;
                    native.ended = false;
                }
                let playing = native.state != audio::State::Playing;
                native
                    .pipeline
                    .set_state(if playing {
                        gst::State::Playing
                    } else {
                        gst::State::Paused
                    })
                    .map_err(|error| error.to_string())?;
                native.state = if playing {
                    audio::State::Playing
                } else {
                    audio::State::Paused
                };
                return Ok(());
            }
            self.stop_native();
            if !path.is_file() {
                return Err("Audio file is unavailable".into());
            }
            gst::init().map_err(|error| error.to_string())?;
            let pipeline = gst::ElementFactory::make("playbin")
                .build()
                .map_err(|error| error.to_string())?;
            if let Ok(video_sink) = gst::ElementFactory::make("fakesink").build() {
                pipeline.set_property("video-sink", video_sink);
            }
            let scaletempo = match gst::ElementFactory::make("scaletempo").build() {
                Ok(filter) => {
                    pipeline.set_property("audio-filter", filter);
                    true
                }
                Err(_) => false,
            };
            check_rate(scaletempo, self.speed())?;
            let uri = gtk4::gio::File::for_path(path).uri();
            pipeline.set_property("uri", uri.as_str());
            pipeline
                .set_state(gst::State::Playing)
                .map_err(|error| error.to_string())?;
            self.native = Some(NativePlayback {
                message: message.to_owned(),
                path: path.to_owned(),
                pipeline,
                _hold: crate::media_retention::hold([path.to_owned()]),
                state: audio::State::Loading,
                ended: false,
                rate_applied: self.speed() <= audio::SPEEDS[0],
                pending_seek: None,
                scaletempo,
            });
            Ok(())
        }
    }

    pub fn seek(&mut self, message: &str, path: &Path, fraction: f32) -> Result<(), String> {
        {
            let speed = self.speed() as f64;
            if self
                .native
                .as_ref()
                .is_none_or(|native| native.message != message || native.path != path)
            {
                self.toggle_playback(message, path)?;
            }
            if let Some(native) = &mut self.native {
                check_rate(native.scaletempo, speed as f32)?;
                let duration = native
                    .pipeline
                    .query_duration::<gst::ClockTime>()
                    .unwrap_or(gst::ClockTime::ZERO);
                if duration.is_zero() {
                    native.pending_seek = Some(fraction.clamp(0.0, 1.0));
                } else {
                    let target =
                        (duration.nseconds() as f64 * f64::from(fraction.clamp(0.0, 1.0))) as u64;
                    native
                        .pipeline
                        .seek(
                            speed,
                            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                            gst::SeekType::Set,
                            gst::ClockTime::from_nseconds(target),
                            gst::SeekType::None,
                            None::<gst::ClockTime>,
                        )
                        .map_err(|error| error.to_string())?;
                    native.pending_seek = None;
                    native.rate_applied = true;
                }
                native.ended = false;
                let loading = native.state == audio::State::Loading;
                native
                    .pipeline
                    .set_state(gst::State::Playing)
                    .map_err(|error| error.to_string())?;
                native.state = if loading {
                    audio::State::Loading
                } else {
                    audio::State::Playing
                };
            }
            Ok(())
        }
    }

    pub fn poll(&mut self) -> Result<(), String> {
        let speed = self.speed();
        if let Some(native) = &self.native
            && !native.ended
            && matches!(native.state, audio::State::Loading | audio::State::Playing)
            && let Err(error) = check_rate(native.scaletempo, speed)
        {
            self.stop_native();
            return Err(error);
        }
        if let Some(native) = &mut self.native
            && let Some(bus) = native.pipeline.bus()
        {
            while let Some(event) = bus.pop_filtered(&[
                gst::MessageType::Error,
                gst::MessageType::Eos,
                gst::MessageType::StateChanged,
                gst::MessageType::AsyncDone,
            ]) {
                match event.view() {
                    gst::MessageView::Error(error) => {
                        let text = error.error().to_string();
                        self.stop_native();
                        return Err(text);
                    }
                    gst::MessageView::Eos(..) => {
                        native.ended = true;
                        native.state = audio::State::Idle;
                        let _ = native.pipeline.set_state(gst::State::Paused);
                    }
                    gst::MessageView::StateChanged(state)
                        if event.src().is_some_and(|source| {
                            source == native.pipeline.upcast_ref::<gst::Object>()
                        }) && state.current() == gst::State::Playing
                            && native.state == audio::State::Loading =>
                    {
                        native.state = audio::State::Playing;
                    }
                    _ => {}
                }
            }
            if let Some(fraction) = native.pending_seek
                && let Some(duration) = native.pipeline.query_duration::<gst::ClockTime>()
                && !duration.is_zero()
            {
                let target = (duration.nseconds() as f64 * f64::from(fraction)) as u64;
                if native
                    .pipeline
                    .seek(
                        speed as f64,
                        gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                        gst::SeekType::Set,
                        gst::ClockTime::from_nseconds(target),
                        gst::SeekType::None,
                        None::<gst::ClockTime>,
                    )
                    .is_ok()
                {
                    native.pending_seek = None;
                    native.rate_applied = true;
                }
            }
            if native.state == audio::State::Playing
                && !native.rate_applied
                && native.pending_seek.is_none()
            {
                native.rate_applied = apply_rate(&native.pipeline, speed);
            }
        }
        Ok(())
    }

    pub fn is_playing(&self) -> bool {
        if let Some(native) = &self.native {
            return !native.ended
                && matches!(native.state, audio::State::Loading | audio::State::Playing);
        }
        false
    }

    pub fn actually_playing(&self, message: &str) -> bool {
        self.playback_status(message).state == audio::State::Playing
    }

    pub fn stop_playback(&mut self) {
        self.stop_native();
    }

    fn stop_native(&mut self) {
        if let Some(native) = self.native.take() {
            let _ = native.pipeline.set_state(gst::State::Null);
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    pub fn start_recording(&mut self) -> bool {
        if self.recorder.is_some() {
            return false;
        }
        self.recorder = Some(audio::Recorder::start());
        true
    }

    pub fn cancel_recording(&mut self) -> bool {
        self.recorder.take().is_some()
    }

    pub fn recording_elapsed(&self) -> Option<std::time::Duration> {
        self.recorder.as_ref().map(audio::Recorder::elapsed)
    }

    pub fn recording_levels(&self) -> Vec<f32> {
        self.recorder
            .as_ref()
            .map_or_else(Vec::new, audio::Recorder::levels)
    }

    /// Newest `count` microphone readings; empty when not recording.
    pub fn recent_recording_levels(&self, count: usize) -> Vec<f32> {
        self.recorder
            .as_ref()
            .map_or_else(Vec::new, |recorder| recorder.recent_levels(count))
    }

    pub fn finish_recording(&mut self) -> Option<Result<Vec<f32>, String>> {
        self.recorder.take().map(audio::Recorder::finish)
    }
}

impl Drop for MediaService {
    fn drop(&mut self) {
        self.stop_native();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gstreamer_plays_and_seeks_received_opus_without_audio_device() {
        gst::init().expect("GStreamer initialized");
        let file = tempfile::NamedTempFile::new().expect("temporary clip");
        let samples = vec![0.1; crate::voice::RATE as usize * 2];
        std::fs::write(
            file.path(),
            crate::voice::encode(&samples).expect("encode opus"),
        )
        .expect("write clip");

        let pipeline = gst::ElementFactory::make("playbin")
            .build()
            .expect("playbin plugin");
        let sink = gst::ElementFactory::make("fakesink")
            .build()
            .expect("test audio sink");
        sink.set_property("sync", true);
        pipeline.set_property("audio-sink", sink);
        pipeline.set_property("uri", gtk4::gio::File::for_path(file.path()).uri().as_str());
        pipeline.set_state(gst::State::Playing).expect("start clip");
        let mut media = MediaService::default();
        media.native = Some(NativePlayback {
            message: "synthetic-audio".into(),
            path: file.path().to_owned(),
            pipeline,
            _hold: crate::media_retention::hold([file.path().to_owned()]),
            state: audio::State::Loading,
            ended: false,
            rate_applied: true,
            pending_seek: None,
            scaletempo: false,
        });
        let start = std::time::Instant::now();
        while !media.actually_playing("synthetic-audio") && start.elapsed().as_secs() < 4 {
            media.poll().expect("decode and playback");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(media.actually_playing("synthetic-audio"));
        media
            .seek("synthetic-audio", file.path(), 0.5)
            .expect("seek");
        assert!(media.playback_status("synthetic-audio").total.as_millis() > 0);
        if let Some(native) = media.native.as_mut() {
            native.ended = true;
            native.state = audio::State::Idle;
        }
        media
            .toggle_playback("synthetic-audio", file.path())
            .expect("replay completed clip");
        assert_eq!(
            media.playback_status("synthetic-audio").state,
            audio::State::Playing
        );
        media.stop_playback();
        assert_eq!(
            media.playback_status("synthetic-audio").state,
            audio::State::Idle
        );
    }

    fn synthetic_clip(seconds: u32) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temporary clip");
        let tone: Vec<f32> = (0..crate::voice::RATE * seconds)
            .map(|i| {
                (i as f32 * 330.0 * std::f32::consts::TAU / crate::voice::RATE as f32).sin() * 0.3
            })
            .collect();
        std::fs::write(
            file.path(),
            crate::voice::encode(&tone).expect("encode opus"),
        )
        .expect("write clip");
        file
    }

    fn wait_until_playing(media: &mut MediaService, message: &str) {
        let start = std::time::Instant::now();
        while !media.actually_playing(message) && start.elapsed().as_secs() < 4 {
            media.poll().expect("decode and playback");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(media.actually_playing(message));
    }

    #[test]
    fn cycling_wraps_through_every_speed() {
        let mut media = MediaService::default();
        assert_eq!(media.speed(), audio::SPEEDS[0]);
        assert_eq!(media.cycle_speed(), 1.5);
        assert_eq!(media.cycle_speed(), 2.0);
        assert_eq!(media.cycle_speed(), 1.0);
        // A speed set by hand still cycles up to the next known one.
        media.set_speed(1.75);
        assert_eq!(media.cycle_speed(), 2.0);
        media.set_speed(f32::NAN);
        assert_eq!(media.speed(), 1.0);
        media.set_speed(50.0);
        assert_eq!(media.speed(), 2.0);
    }

    #[test]
    fn without_scaletempo_only_one_x_plays() {
        gst::init().expect("GStreamer initialized");
        let file = synthetic_clip(2);
        let pipeline = gst::ElementFactory::make("playbin")
            .build()
            .expect("playbin plugin");
        let sink = gst::ElementFactory::make("fakesink")
            .build()
            .expect("test audio sink");
        sink.set_property("sync", true);
        pipeline.set_property("audio-sink", sink);
        pipeline.set_property("uri", gtk4::gio::File::for_path(file.path()).uri().as_str());
        pipeline.set_state(gst::State::Playing).expect("start clip");
        let mut media = MediaService::default();
        media.native = Some(NativePlayback {
            message: "synthetic-audio".into(),
            path: file.path().to_owned(),
            pipeline,
            _hold: crate::media_retention::hold([file.path().to_owned()]),
            state: audio::State::Loading,
            ended: false,
            rate_applied: true,
            pending_seek: None,
            scaletempo: false,
        });
        wait_until_playing(&mut media, "synthetic-audio");

        media
            .toggle_playback("synthetic-audio", file.path())
            .expect("pause at 1x");
        media.set_speed(2.0);
        assert_eq!(
            media.toggle_playback("synthetic-audio", file.path()),
            Err(missing_scaletempo())
        );
        assert_eq!(
            media.seek("synthetic-audio", file.path(), 0.5),
            Err(missing_scaletempo())
        );
        assert_eq!(
            media.playback_status("synthetic-audio").state,
            audio::State::Paused
        );

        media.set_speed(1.0);
        media
            .toggle_playback("synthetic-audio", file.path())
            .expect("resume at 1x");
        wait_until_playing(&mut media, "synthetic-audio");
        media.set_speed(1.5);
        assert_eq!(media.poll(), Err(missing_scaletempo()));
        assert!(!media.is_playing());
        assert_eq!(
            media.playback_status("synthetic-audio").state,
            audio::State::Idle
        );
    }

    /// Plays a one-second tone on the default output device:
    /// `cargo test services::media::tests::plays -- --ignored --nocapture`.
    #[test]
    #[ignore = "makes a sound on this machine"]
    fn plays_a_clip_on_this_machine() {
        let file = synthetic_clip(1);
        let mut media = MediaService::default();
        media
            .toggle_playback("clip", file.path())
            .expect("starts playing");
        let started = std::time::Instant::now();
        let mut seen_playing = false;
        while started.elapsed() < std::time::Duration::from_secs(3) {
            media.poll().expect("plays");
            let status = media.playback_status("clip");
            if status.state == audio::State::Playing
                && status.position > std::time::Duration::from_millis(300)
            {
                seen_playing = true;
                eprintln!("playing at {:?} of {:?}", status.position, status.total);
            }
            if seen_playing && status.state == audio::State::Idle {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        assert!(seen_playing, "never heard it playing");
        assert_eq!(
            media.playback_status("clip").state,
            audio::State::Idle,
            "ends on its own"
        );
    }

    /// Plays a two-second tone at double speed and checks the position
    /// outruns the clock:
    /// `cargo test services::media::tests::doubles -- --ignored --nocapture`.
    #[test]
    #[ignore = "makes a sound on this machine"]
    fn doubles_the_position_rate_on_this_machine() {
        let file = synthetic_clip(2);
        let mut media = MediaService::default();
        media.set_speed(2.0);
        media
            .toggle_playback("clip", file.path())
            .expect("starts playing");
        let started = std::time::Instant::now();
        let mut seen = Vec::new();
        while started.elapsed() < std::time::Duration::from_secs(4) {
            media.poll().expect("plays");
            let status = media.playback_status("clip");
            if status.state == audio::State::Playing
                && status.position > std::time::Duration::from_millis(200)
            {
                seen.push((started.elapsed(), status.position));
            }
            if status.state == audio::State::Idle && !seen.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        let (first_wall, first_position) = seen.first().expect("played");
        let (last_wall, last_position) = seen.last().expect("played");
        let wall = *last_wall - *first_wall;
        let advanced = *last_position - *first_position;
        assert!(
            wall > std::time::Duration::from_millis(200),
            "played for {wall:?}"
        );
        assert!(
            advanced.as_secs_f32() >= 1.5 * wall.as_secs_f32(),
            "position advanced {advanced:?} over {wall:?} of wall time"
        );
    }
}
