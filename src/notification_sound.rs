//! Short sound that accompanies a desktop notification.
//!
//! ZapTide does not play it. It hands the sound to the desktop together with
//! the notification, and the desktop decides whether to play it according to
//! its notification and sound policies.
//! What belongs to ZapTide is whether to ask for the sound at all: a burst of
//! messages asks once.

use std::time::{Duration, Instant};

/// Seconds offered in Preferences for the pause between two sounds.
pub const INTERVALS: [u32; 5] = [0, 10, 30, 60, 120];
pub const DEFAULT_INTERVAL: u32 = 30;

#[cfg(target_os = "linux")]
pub(crate) static SOUND: &[u8] = include_bytes!("../assets/sounds/message-notification.wav");

/// The bundled sound as a private file in the cache, for notification servers
/// that take a path.
///
/// The first call checks the whole file; later calls only check that it is
/// still a private file of the right size. Reading it again for every
/// notification would protect only against the user's own processes, which
/// could replace the sound in any case.
#[cfg(target_os = "linux")]
pub(crate) fn native_sound_file() -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt;

    static CHECKED: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
    let mut checked = CHECKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(path) = checked.as_ref()
        && std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            // SAFETY: geteuid has no arguments or side effects.
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0
                && metadata.len() == SOUND.len() as u64
        })
    {
        return Ok(path.clone());
    }
    let path = materialize_native_sound(&crate::paths::AppDirs::discover().cache)?;
    *checked = Some(path.clone());
    Ok(path)
}

#[cfg(target_os = "linux")]
fn materialize_native_sound(cache: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::io::{Error, ErrorKind, Read, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

    if !cache.is_absolute() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "cache path is not absolute",
        ));
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(cache)?;
    let directory = cache.join("notification-sounds");
    match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => (),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    // The cache ancestry is trusted to the current user; the dedicated directory is not followed if it is a symlink.
    let metadata = std::fs::symlink_metadata(&directory)?;
    // SAFETY: geteuid has no arguments or side effects.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "untrusted sound directory",
        ));
    }
    let path = directory.join("message-notification.wav");
    let validate = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
            || metadata.len() != SOUND.len() as u64
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "invalid cached sound file",
            ));
        }
        let mut bytes = Vec::with_capacity(SOUND.len());
        Read::by_ref(&mut file)
            .take(SOUND.len() as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes != SOUND {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "invalid cached sound contents",
            ));
        }
        Ok(())
    };
    match validate() {
        Ok(()) => return Ok(path),
        Err(error) if error.kind() == ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(SOUND)?;
    temporary.flush()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => (),
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.error),
    }
    validate()?;
    Ok(path)
}

/// Whether `seconds` is one of the offered intervals.
pub fn is_supported_interval(seconds: u32) -> bool {
    INTERVALS.contains(&seconds)
}

/// What a notification asks the desktop to do about sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundRequest {
    /// Play the bundled sound.
    Play,
    /// Play nothing, not even the desktop's default notification sound: a
    /// sound was asked for less than an interval ago.
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
    /// The request for the next notification, or `None` when the sound is
    /// turned off: the notification is then sent as it was before the sound
    /// existed, and the interval is left alone.
    pub fn request(&mut self, enabled: bool, interval_seconds: u32) -> Option<SoundRequest> {
        self.request_at(Instant::now(), enabled, interval_seconds)
    }

    fn request_at(
        &mut self,
        now: Instant,
        enabled: bool,
        interval_seconds: u32,
    ) -> Option<SoundRequest> {
        if !enabled {
            None
        } else if self.throttle.admit(now, interval_for(interval_seconds)) {
            Some(SoundRequest::Play)
        } else {
            Some(SoundRequest::Silent)
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

    #[cfg(target_os = "linux")]
    #[test]
    fn native_sound_is_stable_private_and_matches_the_embedded_bytes() {
        use std::os::unix::fs::MetadataExt;
        let cache = tempfile::tempdir().unwrap();
        let first = materialize_native_sound(cache.path()).unwrap();
        assert!(first.is_absolute());
        assert_eq!(std::fs::read(&first).unwrap(), SOUND);
        assert_eq!(std::fs::metadata(&first).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(first.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(materialize_native_sound(cache.path()).unwrap(), first);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_sound_rejects_symlinks_without_touching_the_target() {
        use std::os::unix::fs::symlink;
        let cache = tempfile::tempdir().unwrap();
        let path = materialize_native_sound(cache.path()).unwrap();
        let target = cache.path().join("other");
        std::fs::write(&target, b"untouched").unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(&target, &path).unwrap();
        assert!(materialize_native_sound(cache.path()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_sound_rejects_invalid_existing_files_and_directories() {
        use std::os::unix::fs::PermissionsExt;
        let cache = tempfile::tempdir().unwrap();
        let path = materialize_native_sound(cache.path()).unwrap();
        std::fs::write(&path, b"incorrect").unwrap();
        assert!(materialize_native_sound(cache.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"incorrect");
        std::fs::set_permissions(
            path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(
            materialize_native_sound(cache.path()).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_sound_rejects_a_non_directory_cache_path() {
        let cache = tempfile::tempdir().unwrap();
        let file = cache.path().join("file");
        std::fs::write(&file, b"untouched").unwrap();
        assert!(materialize_native_sound(&file).is_err());
        assert_eq!(std::fs::read(file).unwrap(), b"untouched");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_sound_rejects_a_symlinked_sound_directory() {
        use std::os::unix::fs::symlink;
        let cache = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        symlink(target.path(), cache.path().join("notification-sounds")).unwrap();
        assert!(materialize_native_sound(cache.path()).is_err());
        assert!(std::fs::read_dir(target.path()).unwrap().next().is_none());
    }

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
        assert_eq!(sound.request_at(start, true, 30), Some(SoundRequest::Play));
        assert_eq!(
            sound.request_at(at(start, 5), true, 30),
            Some(SoundRequest::Silent)
        );
        assert_eq!(
            sound.request_at(at(start, 30), true, 30),
            Some(SoundRequest::Play)
        );
    }

    #[test]
    fn a_disabled_sound_asks_nothing_and_never_consumes_the_interval() {
        let start = Instant::now();
        let mut sound = NotificationSound::default();
        assert_eq!(sound.request_at(start, false, 30), None);
        assert_eq!(sound.request_at(start, true, 30), Some(SoundRequest::Play));
    }

    #[test]
    fn a_changed_interval_applies_to_the_next_request() {
        let start = Instant::now();
        let mut sound = NotificationSound::default();
        assert_eq!(sound.request_at(start, true, 120), Some(SoundRequest::Play));
        assert_eq!(
            sound.request_at(at(start, 15), true, 120),
            Some(SoundRequest::Silent)
        );
        assert_eq!(
            sound.request_at(at(start, 15), true, 10),
            Some(SoundRequest::Play)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_embedded_sound_is_a_short_pcm_wav() {
        assert_eq!(&SOUND[..4], b"RIFF");
        assert_eq!(&SOUND[8..12], b"WAVE");
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(SOUND).unwrap();
        assert!(!crate::audio::waveform_file(file.path()).unwrap().is_empty());
        let bytes_per_second = u32::from_le_bytes(SOUND[28..32].try_into().unwrap()) as usize;
        assert!(bytes_per_second > 0 && SOUND.len() - 44 < bytes_per_second);
    }
}
