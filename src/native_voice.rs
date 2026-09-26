//! Toolkit-neutral projection for native voice-message controls.
//!
//! Native renderers consume [`VoiceMessage`] and turn [`VoiceIntent`] values
//! back into the existing application actions.

use crate::audio::{State, Status, speed_label};
use crate::model::{Action, Media, MediaState};
use std::time::Duration;

/// Primary control shown for a voice message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VoiceControl {
    Download,
    Loading,
    Play,
    Pause,
}

/// Input emitted by a native voice-message control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceIntent {
    Activate,
    Seek(f32),
    CycleSpeed,
}

/// Input emitted by a native voice-recording control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingIntent {
    Start,
    Send,
    Cancel,
}

/// Existing microphone state required for a native recording projection.
pub struct VoiceRecordingInput<'a> {
    pub recording: bool,
    pub elapsed: Duration,
    /// RMS loudness values collected while recording.
    pub levels: &'a [f32],
}

/// Data needed to render native voice-recording controls without a UI toolkit.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceRecording {
    pub recording: bool,
    pub waveform: Vec<u8>,
    pub time: String,
}

impl VoiceRecording {
    /// Translates a toolkit event into an existing application action.
    pub fn action(&self, intent: RecordingIntent) -> Option<Action> {
        match (self.recording, intent) {
            (false, RecordingIntent::Start) => Some(Action::StartRecording),
            (true, RecordingIntent::Send) => Some(Action::SendRecording),
            (true, RecordingIntent::Cancel) => Some(Action::CancelRecording),
            _ => None,
        }
    }
}

/// Existing voice-message state required for a native projection.
pub struct VoiceMessageInput<'a> {
    pub chat: &'a str,
    pub message: &'a str,
    pub media: &'a Media,
    pub seconds: Option<u32>,
    pub waveform: &'a [u8],
    pub generated_waveform: Option<&'a [u8]>,
    pub playback: Status,
    pub speed: f32,
}

/// Data needed to render one native voice-message control without a UI toolkit.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceMessage {
    pub control: VoiceControl,
    pub waveform: Vec<u8>,
    /// Played fraction in the inclusive range `0.0..=1.0`.
    pub progress: f32,
    /// Elapsed time while active, otherwise advertised duration or file size.
    pub time: String,
    pub error: Option<String>,
    /// Playback-speed label, present once media is available locally.
    pub speed: Option<String>,
    chat: String,
    message: String,
    path: Option<std::path::PathBuf>,
}

impl VoiceMessage {
    /// Translates a toolkit event into an existing application action.
    pub fn action(&self, intent: VoiceIntent) -> Option<Action> {
        match intent {
            VoiceIntent::Activate => match (&self.path, self.control) {
                (Some(path), VoiceControl::Play | VoiceControl::Pause) => Some(Action::PlayVoice {
                    message: self.message.clone(),
                    path: path.clone(),
                }),
                (None, VoiceControl::Download) => Some(Action::Download {
                    chat: self.chat.clone(),
                    message: self.message.clone(),
                }),
                _ => None,
            },
            VoiceIntent::Seek(fraction) => self.path.as_ref().map(|path| Action::SeekVoice {
                message: self.message.clone(),
                path: path.clone(),
                fraction: finite_fraction(fraction),
            }),
            VoiceIntent::CycleSpeed => self.path.as_ref().map(|_| Action::CycleVoiceSpeed),
        }
    }
}

/// Projects voice-message state into native-renderer data.
pub fn project(input: VoiceMessageInput<'_>) -> VoiceMessage {
    let path = input.media.path.clone();
    let control = match (
        path.is_some(),
        input.media.state.clone(),
        input.playback.state,
    ) {
        (false, MediaState::Downloading, _) | (true, _, State::Loading) => VoiceControl::Loading,
        (false, _, _) => VoiceControl::Download,
        (true, _, State::Playing) => VoiceControl::Pause,
        (true, _, State::Idle | State::Paused) => VoiceControl::Play,
    };
    let waveform = if !input.waveform.is_empty() {
        input
            .waveform
            .iter()
            .take(crate::voice::BARS)
            .map(|bar| (*bar).min(100))
            .collect()
    } else if let Some(generated) = input.generated_waveform.filter(|bars| !bars.is_empty()) {
        generated
            .iter()
            .take(crate::voice::BARS)
            .map(|bar| (*bar).min(100))
            .collect()
    } else {
        vec![12; crate::voice::BARS]
    };
    let active = matches!(input.playback.state, State::Playing | State::Paused);
    let progress = if input.playback.total.is_zero() {
        0.0
    } else {
        finite_fraction(input.playback.position.as_secs_f32() / input.playback.total.as_secs_f32())
    };
    let time = if active {
        crate::util::duration(input.playback.position.as_secs() as u32)
    } else {
        input
            .seconds
            .or_else(|| {
                (!input.playback.total.is_zero()).then_some(input.playback.total.as_secs() as u32)
            })
            .map(crate::util::duration)
            .unwrap_or_else(|| crate::util::bytes(input.media.size))
    };

    VoiceMessage {
        control,
        waveform,
        progress,
        time,
        error: match &input.media.state {
            MediaState::Failed(error) => Some(error.clone()),
            MediaState::Idle | MediaState::Downloading => None,
        },
        speed: path.as_ref().map(|_| speed_label(input.speed)),
        chat: input.chat.to_owned(),
        message: input.message.to_owned(),
        path,
    }
}

/// Projects live microphone state into native-renderer data.
pub fn project_recording(input: VoiceRecordingInput<'_>) -> VoiceRecording {
    let loudest = input
        .levels
        .iter()
        .copied()
        .filter(|level| level.is_finite())
        .fold(0.0f32, f32::max);
    let waveform = if input.levels.is_empty() {
        vec![12; crate::voice::BARS]
    } else {
        input
            .levels
            .iter()
            .map(|level| {
                if loudest > 0.0 && level.is_finite() {
                    finite_fraction(*level / loudest) * 100.0
                } else {
                    0.0
                }
                .round() as u8
            })
            .collect()
    };

    VoiceRecording {
        recording: input.recording,
        waveform,
        time: crate::util::duration(input.elapsed.as_secs().min(u64::from(u32::MAX)) as u32),
    }
}

fn finite_fraction(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn media(path: Option<&str>, state: MediaState) -> Media {
        Media {
            mime: "audio/ogg".into(),
            size: 1_024,
            width: None,
            height: None,
            path: path.map(PathBuf::from),
            state,
        }
    }

    fn status(state: State, position: u64, total: u64) -> Status {
        Status {
            state,
            position: Duration::from_secs(position),
            total: Duration::from_secs(total),
        }
    }

    #[test]
    fn downloaded_playing_message_projects_pause_seek_and_speed_actions() {
        let media = media(Some("voice.ogg"), MediaState::Idle);
        let voice = project(VoiceMessageInput {
            chat: "chat",
            message: "message",
            media: &media,
            seconds: Some(30),
            waveform: &[1, 100],
            generated_waveform: None,
            playback: status(State::Playing, 9, 30),
            speed: 1.5,
        });

        assert_eq!(voice.control, VoiceControl::Pause);
        assert_eq!(voice.progress, 0.3);
        assert_eq!(voice.time, "0:09");
        assert_eq!(voice.waveform, vec![1, 100]);
        assert_eq!(voice.speed.as_deref(), Some("1.5x"));
        assert_eq!(
            voice.action(VoiceIntent::Activate),
            Some(Action::PlayVoice {
                message: "message".into(),
                path: PathBuf::from("voice.ogg"),
            })
        );
        assert_eq!(
            voice.action(VoiceIntent::Seek(2.0)),
            Some(Action::SeekVoice {
                message: "message".into(),
                path: PathBuf::from("voice.ogg"),
                fraction: 1.0,
            })
        );
        assert_eq!(
            voice.action(VoiceIntent::Seek(f32::NAN)),
            Some(Action::SeekVoice {
                message: "message".into(),
                path: PathBuf::from("voice.ogg"),
                fraction: 0.0,
            })
        );
        assert_eq!(
            voice.action(VoiceIntent::CycleSpeed),
            Some(Action::CycleVoiceSpeed)
        );
    }

    #[test]
    fn undownloaded_message_projects_download_and_fallback_waveform() {
        let media = media(None, MediaState::Failed("Unavailable".into()));
        let voice = project(VoiceMessageInput {
            chat: "chat",
            message: "message",
            media: &media,
            seconds: None,
            waveform: &[],
            generated_waveform: Some(&[20, 40]),
            playback: status(State::Idle, 0, 0),
            speed: 1.0,
        });

        assert_eq!(voice.control, VoiceControl::Download);
        assert_eq!(voice.waveform, vec![20, 40]);
        assert_eq!(voice.time, "1.0 KB");
        assert_eq!(voice.error.as_deref(), Some("Unavailable"));
        assert_eq!(voice.speed, None);
        assert_eq!(
            voice.action(VoiceIntent::Activate),
            Some(Action::Download {
                chat: "chat".into(),
                message: "message".into(),
            })
        );
        assert_eq!(voice.action(VoiceIntent::Seek(f32::NAN)), None);
    }

    #[test]
    fn empty_waveforms_use_visible_default() {
        let media = media(Some("voice.ogg"), MediaState::Idle);
        let voice = project(VoiceMessageInput {
            chat: "chat",
            message: "message",
            media: &media,
            seconds: None,
            waveform: &[],
            generated_waveform: Some(&[]),
            playback: Status {
                state: State::Playing,
                position: Duration::MAX,
                total: Duration::from_secs(1),
            },
            speed: 1.0,
        });

        assert_eq!(voice.waveform, vec![12; crate::voice::BARS]);
        assert_eq!(voice.progress, 1.0);
    }

    #[test]
    fn sender_waveform_is_bounded_for_native_rendering() {
        let media = media(Some("audio.ogg"), MediaState::Idle);
        let oversized = [200_u8; 128];
        let voice = project(VoiceMessageInput {
            chat: "chat",
            message: "message",
            media: &media,
            seconds: Some(1),
            waveform: &oversized,
            generated_waveform: None,
            playback: status(State::Idle, 0, 1),
            speed: 1.0,
        });
        assert_eq!(voice.waveform, vec![100; crate::voice::BARS]);
    }

    #[test]
    fn recording_projection_maps_controls_and_sanitizes_live_waveform() {
        let recording = project_recording(VoiceRecordingInput {
            recording: true,
            elapsed: Duration::from_secs(65),
            levels: &[0.1, f32::NAN, 0.2, -0.1],
        });

        assert!(recording.recording);
        assert_eq!(recording.time, "1:05");
        assert_eq!(recording.waveform, vec![50, 0, 100, 0]);
        assert_eq!(
            recording.action(RecordingIntent::Send),
            Some(Action::SendRecording)
        );
        assert_eq!(
            recording.action(RecordingIntent::Cancel),
            Some(Action::CancelRecording)
        );
        assert_eq!(recording.action(RecordingIntent::Start), None);

        let idle = project_recording(VoiceRecordingInput {
            recording: false,
            elapsed: Duration::ZERO,
            levels: &[],
        });
        assert_eq!(idle.waveform, vec![12; crate::voice::BARS]);
        assert_eq!(
            idle.action(RecordingIntent::Start),
            Some(Action::StartRecording)
        );
        assert_eq!(idle.action(RecordingIntent::Send), None);
        assert_eq!(idle.action(RecordingIntent::Cancel), None);
    }
}
