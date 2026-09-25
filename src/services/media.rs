//! GTK-main-context facade for the existing Rust audio and Opus engines.
//!
//! Device I/O, file reads, decode, and time-stretch remain in `audio`; this
//! service gives the native component one lifecycle boundary for playback and
//! recording without exposing engine internals to widgets.

use std::path::Path;

use crate::audio;

#[derive(Default)]
pub struct MediaService {
    player: audio::Player,
    recorder: Option<audio::Recorder>,
}

impl MediaService {
    pub fn set_speed(&mut self, speed: f32) {
        self.player.set_speed(speed);
    }

    pub fn speed(&self) -> f32 {
        self.player.speed()
    }

    pub fn cycle_speed(&mut self) -> f32 {
        self.player.cycle_speed()
    }

    pub fn playback_status(&self, message: &str) -> audio::Status {
        self.player.status(message)
    }

    pub fn waveform(&self, message: &str) -> Option<&[u8]> {
        self.player.bars(message)
    }

    pub fn toggle_playback(&mut self, message: &str, path: &Path) -> Result<(), String> {
        self.player.toggle(message, path)
    }

    pub fn seek(&mut self, message: &str, path: &Path, fraction: f32) -> Result<(), String> {
        self.player.seek(message, path, fraction)
    }

    pub fn poll(&mut self) -> Result<(), String> {
        self.player.poll()
    }

    pub fn is_playing(&self) -> bool {
        self.player.is_playing()
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
