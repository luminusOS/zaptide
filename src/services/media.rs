//! GTK-main-context facade for playback and recording.

use std::path::Path;

use crate::audio;

#[cfg(target_os = "linux")]
use gst::prelude::*;
#[cfg(target_os = "linux")]
use gstreamer as gst;
#[cfg(target_os = "linux")]
use gtk4::gio::prelude::FileExt;

#[cfg(target_os = "linux")]
struct NativePlayback {
    message: String,
    path: std::path::PathBuf,
    pipeline: gst::Element,
    state: audio::State,
    ended: bool,
    rate_applied: bool,
    pending_seek: Option<f32>,
}

#[cfg(target_os = "linux")]
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

#[derive(Default)]
pub struct MediaService {
    player: audio::Player,
    #[cfg(target_os = "linux")]
    native: Option<NativePlayback>,
    recorder: Option<audio::Recorder>,
}

impl MediaService {
    pub fn set_speed(&mut self, speed: f32) {
        let previous = self.player.speed();
        self.player.set_speed(speed);
        if self.player.speed() == previous {
            return;
        }
        #[cfg(target_os = "linux")]
        let speed = self.player.speed();
        #[cfg(target_os = "linux")]
        if let Some(native) = &mut self.native {
            let position = native
                .pipeline
                .query_position::<gst::ClockTime>()
                .unwrap_or(gst::ClockTime::ZERO);
            // Preserve the clip's pitch when speeding up; keep the existing
            // Rust engine as fallback when the GStreamer plugin is missing.
            if speed > 1.0 && gst::ElementFactory::make("scaletempo").build().is_err() {
                let message = native.message.clone();
                let path = native.path.clone();
                let duration = native
                    .pipeline
                    .query_duration::<gst::ClockTime>()
                    .unwrap_or(gst::ClockTime::ZERO);
                let playing = native.state == audio::State::Playing;
                self.stop_native();
                if playing {
                    let fraction = if duration.is_zero() {
                        0.0
                    } else {
                        position.nseconds() as f32 / duration.nseconds() as f32
                    };
                    let _ = self.player.seek(&message, &path, fraction);
                }
            } else {
                native.rate_applied = apply_rate(&native.pipeline, speed);
            }
        }
    }

    pub fn speed(&self) -> f32 {
        self.player.speed()
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
        #[cfg(target_os = "linux")]
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
        self.player.status(message)
    }

    pub fn waveform(&self, message: &str) -> Option<&[u8]> {
        self.player.bars(message)
    }

    pub fn toggle_playback(&mut self, message: &str, path: &Path) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            let speed = self.speed() as f64;
            if let Some(native) = &mut self.native
                && native.message == message
                && native.path == path
            {
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
            self.player.stop();
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
            if let Ok(filter) = gst::ElementFactory::make("scaletempo").build() {
                pipeline.set_property("audio-filter", filter);
            } else if self.speed() > 1.0 {
                return self.player.toggle(message, path);
            }
            let uri = gtk4::gio::File::for_path(path).uri();
            pipeline.set_property("uri", uri.as_str());
            pipeline
                .set_state(gst::State::Playing)
                .map_err(|error| error.to_string())?;
            self.native = Some(NativePlayback {
                message: message.to_owned(),
                path: path.to_owned(),
                pipeline,
                state: audio::State::Loading,
                ended: false,
                rate_applied: self.speed() <= 1.0,
                pending_seek: None,
            });
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        self.player.toggle(message, path)
    }

    pub fn seek(&mut self, message: &str, path: &Path, fraction: f32) -> Result<(), String> {
        #[cfg(target_os = "linux")]
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
                return Ok(());
            }
            self.player.seek(message, path, fraction)
        }
        #[cfg(not(target_os = "linux"))]
        self.player.seek(message, path, fraction)
    }

    pub fn poll(&mut self) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        let speed = self.speed();
        #[cfg(target_os = "linux")]
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
        self.player.poll()
    }

    pub fn is_playing(&self) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(native) = &self.native {
            return !native.ended
                && matches!(native.state, audio::State::Loading | audio::State::Playing);
        }
        self.player.is_playing()
    }

    pub fn actually_playing(&self, message: &str) -> bool {
        self.playback_status(message).state == audio::State::Playing
    }

    pub fn stop_playback(&mut self) {
        #[cfg(target_os = "linux")]
        self.stop_native();
        self.player.stop();
    }

    #[cfg(target_os = "linux")]
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

    pub fn finish_recording(&mut self) -> Option<Result<Vec<f32>, String>> {
        self.recorder.take().map(audio::Recorder::finish)
    }
}

#[cfg(target_os = "linux")]
impl Drop for MediaService {
    fn drop(&mut self) {
        self.stop_native();
    }
}

#[cfg(all(test, target_os = "linux"))]
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
            state: audio::State::Loading,
            ended: false,
            rate_applied: true,
            pending_seek: None,
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
}
