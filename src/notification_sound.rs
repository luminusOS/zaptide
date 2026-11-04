//! Short sound that accompanies a desktop notification.
//!
//! ZapTide does not play it. It hands the sound to the desktop together with
//! the notification (notification portal, version 2), and the desktop decides
//! whether to play it, so Do Not Disturb and the per-app sound setting apply.
//! What belongs to ZapTide is whether to ask for the sound at all: a burst of
//! messages asks once.

use std::time::{Duration, Instant};

/// Seconds offered in Preferences for the pause between two sounds.
pub const INTERVALS: [u32; 5] = [0, 10, 30, 60, 120];
pub const DEFAULT_INTERVAL: u32 = 30;

/// Only the Linux portal takes a sound along with a notification.
#[cfg(target_os = "linux")]
pub(crate) static SOUND: &[u8] = include_bytes!("../assets/sounds/message-notification.wav");

/// Whether `seconds` is one of the offered intervals.
pub fn is_supported_interval(seconds: u32) -> bool {
    INTERVALS.contains(&seconds)
}

/// What a notification asks the desktop to do about sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundRequest {
    /// Play the bundled sound.
    Play,
    /// Play nothing, not even the desktop's default notification sound.
    Silent,
}

/// Lets one sound through per interval, however many notifications arrive.
#[derive(Debug, Default)]
struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    fn admit(&mut self, now: Instant, interval: Duration) -> bool {
        let quiet = self
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= interval);
        if quiet {
            self.last = Some(now);
        }
        quiet
    }
}

/// Asks for the sound at most once per configured interval, across every
/// conversation and account.
#[derive(Debug, Default)]
pub struct NotificationSound {
    throttle: Throttle,
}

impl NotificationSound {
    /// The request for the next notification. A disabled sound is silent and
    /// leaves the interval alone.
    pub fn request(&mut self, enabled: bool, interval_seconds: u32) -> SoundRequest {
        self.request_at(Instant::now(), enabled, interval_seconds)
    }

    fn request_at(&mut self, now: Instant, enabled: bool, interval_seconds: u32) -> SoundRequest {
        if enabled && self.throttle.admit(now, interval_for(interval_seconds)) {
            SoundRequest::Play
        } else {
            SoundRequest::Silent
        }
    }
}

/// Unsupported values from a hand-edited file fall back to the default.
fn interval_for(seconds: u32) -> Duration {
    let seconds = if is_supported_interval(seconds) {
        seconds
    } else {
        DEFAULT_INTERVAL
    };
    Duration::from_secs(u64::from(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, seconds: u64) -> Instant {
        start + Duration::from_secs(seconds)
    }

    #[test]
    fn the_first_sound_always_plays() {
        let mut throttle = Throttle::default();
        assert!(throttle.admit(Instant::now(), Duration::from_secs(30)));
    }

    #[test]
    fn a_burst_inside_the_interval_plays_once() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        let interval = Duration::from_secs(30);
        assert!(throttle.admit(start, interval));
        assert!(!throttle.admit(at(start, 1), interval));
        assert!(!throttle.admit(at(start, 29), interval));
    }

    #[test]
    fn the_interval_boundary_allows_the_next_sound() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        let interval = Duration::from_secs(30);
        assert!(throttle.admit(start, interval));
        assert!(throttle.admit(at(start, 30), interval));
    }

    #[test]
    fn a_suppressed_sound_does_not_extend_the_interval() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        let interval = Duration::from_secs(30);
        assert!(throttle.admit(start, interval));
        assert!(!throttle.admit(at(start, 20), interval));
        assert!(throttle.admit(at(start, 30), interval));
    }

    #[test]
    fn no_interval_lets_every_sound_through() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        assert!(throttle.admit(start, Duration::ZERO));
        assert!(throttle.admit(start, Duration::ZERO));
    }

    #[test]
    fn a_clock_that_goes_backwards_is_treated_as_inside_the_interval() {
        let start = Instant::now();
        let mut throttle = Throttle::default();
        let interval = Duration::from_secs(30);
        assert!(throttle.admit(at(start, 10), interval));
        assert!(!throttle.admit(start, interval));
    }

    #[test]
    fn only_the_offered_intervals_are_supported() {
        for seconds in INTERVALS {
            assert!(is_supported_interval(seconds));
        }
        assert!(!is_supported_interval(45));
        assert!(is_supported_interval(DEFAULT_INTERVAL));
    }

    #[test]
    fn an_unsupported_interval_falls_back_to_the_default() {
        assert_eq!(interval_for(45), Duration::from_secs(30));
        assert_eq!(interval_for(0), Duration::ZERO);
        assert_eq!(interval_for(120), Duration::from_secs(120));
    }

    #[test]
    fn a_burst_asks_for_one_sound_and_the_rest_are_silent() {
        let start = Instant::now();
        let mut sound = NotificationSound::default();
        assert_eq!(sound.request_at(start, true, 30), SoundRequest::Play);
        assert_eq!(
            sound.request_at(at(start, 5), true, 30),
            SoundRequest::Silent
        );
        assert_eq!(
            sound.request_at(at(start, 30), true, 30),
            SoundRequest::Play
        );
    }

    #[test]
    fn a_disabled_sound_is_silent_and_never_consumes_the_interval() {
        let start = Instant::now();
        let mut sound = NotificationSound::default();
        assert_eq!(sound.request_at(start, false, 30), SoundRequest::Silent);
        assert_eq!(sound.request_at(start, true, 30), SoundRequest::Play);
    }

    #[test]
    fn a_changed_interval_applies_to_the_next_request() {
        let start = Instant::now();
        let mut sound = NotificationSound::default();
        assert_eq!(sound.request_at(start, true, 120), SoundRequest::Play);
        assert_eq!(
            sound.request_at(at(start, 15), true, 120),
            SoundRequest::Silent
        );
        assert_eq!(
            sound.request_at(at(start, 15), true, 10),
            SoundRequest::Play
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_embedded_sound_is_a_short_pcm_wav() {
        assert_eq!(&SOUND[..4], b"RIFF");
        assert_eq!(&SOUND[8..12], b"WAVE");
        let decoder =
            rodio::Decoder::new(std::io::Cursor::new(SOUND)).expect("embedded WAV decodes");
        assert!(rodio::Source::total_duration(&decoder).is_some_and(|d| d.as_millis() < 1000));
    }
}
