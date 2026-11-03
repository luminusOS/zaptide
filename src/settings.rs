//! User preferences stored in JSON.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Verifying the locked-chat code costs about 20 ms per distinct typed string.
const CHAT_LOCK_ROUNDS: std::num::NonZeroU32 = std::num::NonZeroU32::new(200_000).unwrap();

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// Filename of the selected local JSON palette.
    pub custom_theme: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::theme::custom::read_cached_theme",
        skip_serializing_if = "Option::is_none"
    )]
    pub custom_theme_cache: Option<crate::theme::custom::CustomTheme>,
    /// Interface zoom factor.
    pub zoom: f32,
    /// Whether Enter sends and Shift+Enter adds a line. Off swaps them.
    pub enter_sends: bool,
    /// Send read receipts, subject to the account privacy setting.
    pub send_read_receipts: bool,
    /// Send typing state while composing.
    pub send_typing: bool,
    /// Fetch and show link previews; mirrors the account setting synced with the phone.
    pub link_previews: bool,
    /// Download attachments when they enter view instead of on click.
    #[serde(alias = "auto_download_images")]
    pub auto_download: bool,
    /// Show sender avatars outside groups too.
    pub show_sender_pictures: bool,
    /// Last open chat, restored at startup.
    pub last_chat: Option<String>,
    pub show_shortcut_hints: bool,
    /// Blur the desktop behind a translucent chat list when the compositor supports it.
    pub translucent_sidebar: bool,
    /// Recently used emoji, newest first.
    pub recent_emoji: Vec<String>,
    /// Emoji last used to react to a message, newest first. The quick
    /// reactions over a hovered message are filled from it.
    pub recent_reactions: Vec<String>,
    /// Keep ZapTide running after its window closes.
    pub keep_running_in_background: bool,
    /// Desktop notifications while away from the chat.
    pub notifications: bool,
    /// Show the sender and message text in notifications.
    pub notification_previews: bool,
    /// Seconds after you leave ZapTide (another app, a hidden window or a locked
    /// screen) before WhatsApp shows you as offline; 0 is never. The phone shows
    /// no notifications while you are online.
    pub offline_after_leaving_seconds: u32,
    /// Seconds without input in ZapTide before WhatsApp shows you as offline; 0 is never.
    pub offline_after_inactive_seconds: u32,
    /// Delete downloaded attachments from this computer once they are older
    /// than `media_retention_days`.
    pub media_retention: bool,
    pub media_retention_days: u32,
    /// Play a short sound with each desktop notification.
    pub notification_sound: bool,
    /// Seconds to wait between two notification sounds.
    pub notification_sound_interval: u32,
    /// Prefer address-book names over public profile names.
    pub names_from_contacts: bool,
    /// Voice and audio playback speed multiplier.
    pub voice_speed: f32,
    /// Also add saved contacts to the phone's address book.
    pub save_contacts_to_phone: bool,
    /// Legacy plaintext code, accepted once and rewritten as a verifier.
    #[serde(skip_serializing)]
    pub chat_lock_code: Option<String>,
    /// Salted PBKDF2 verifier for the local locked-chats code, `salt$hash`.
    pub chat_lock_code_hash: Option<String>,
    /// The one-time locked-chat code hint has been opened.
    pub chat_lock_hint_dismissed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            custom_theme: None,
            custom_theme_cache: None,
            zoom: 1.0,
            enter_sends: true,
            send_read_receipts: true,
            send_typing: true,
            link_previews: true,
            auto_download: true,
            show_sender_pictures: false,
            last_chat: None,
            show_shortcut_hints: true,
            translucent_sidebar: false,
            recent_emoji: Vec::new(),
            recent_reactions: Vec::new(),
            keep_running_in_background: true,
            notifications: true,
            notification_previews: true,
            offline_after_leaving_seconds: 10,
            offline_after_inactive_seconds: 300,
            media_retention: false,
            media_retention_days: crate::media_retention::DEFAULT_DAYS,
            notification_sound: true,
            notification_sound_interval: crate::notification_sound::DEFAULT_INTERVAL,
            names_from_contacts: true,
            save_contacts_to_phone: true,
            voice_speed: 1.0,
            chat_lock_code: None,
            chat_lock_code_hash: None,
            chat_lock_hint_dismissed: false,
        }
    }
}

impl Settings {
    pub(crate) fn cached_palette(&self) -> Option<crate::theme::Palette> {
        self.custom_theme
            .as_ref()
            .and(self.custom_theme_cache.as_ref())
            .map(|theme| theme.palette)
    }

    /// Days to keep downloads, or `None` while cleanup is off. A hand-edited
    /// value outside the supported range is brought back into it.
    pub fn active_media_retention(&self) -> Option<u32> {
        self.media_retention.then(|| {
            self.media_retention_days.clamp(
                crate::media_retention::MIN_DAYS,
                crate::media_retention::MAX_DAYS,
            )
        })
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str::<Self>(&contents) {
                Ok(mut settings) => {
                    if !crate::notification_sound::is_supported_interval(
                        settings.notification_sound_interval,
                    ) {
                        settings.notification_sound_interval =
                            crate::notification_sound::DEFAULT_INTERVAL;
                    }
                    if let Some(code) = settings.chat_lock_code.take() {
                        settings.set_chat_lock_code(Some(&code));
                        if let Err(error) = settings.save(path) {
                            log::warn!("could not replace the legacy locked-chat code: {error}");
                        }
                    }
                    settings
                }
                Err(_error) => {
                    log::warn!("settings file is unreadable, using defaults");
                    Self::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                log::warn!("could not read settings: {error}");
                Self::default()
            }
        }
    }

    /// Atomically replaces the settings file through a temporary file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, contents)?;
        std::fs::rename(&temp, path)
    }

    pub fn set_chat_lock_code(&mut self, code: Option<&str>) {
        self.chat_lock_code = None;
        self.chat_lock_code_hash = code
            .map(str::trim)
            .filter(|code| !code.is_empty())
            .map(Self::chat_lock_verifier);
    }

    /// `salt$hash`, both hex. Codes are short enough to be guessed offline,
    /// so the stored form is salted and slow rather than a bare digest.
    fn chat_lock_verifier(code: &str) -> String {
        let salt: [u8; 16] = ring::rand::generate(&ring::rand::SystemRandom::new())
            .expect("the system random generator is unavailable")
            .expose();
        let mut hash = [0u8; 32];
        ring::pbkdf2::derive(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            CHAT_LOCK_ROUNDS,
            &salt,
            code.as_bytes(),
            &mut hash,
        );
        format!("{}${}", hex(&salt), hex(&hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let parsed: Settings =
            serde_json::from_str(r#"{"theme":"light","future_field":1}"#).expect("parses");
        assert_eq!(parsed.theme, ThemeChoice::Light);
        assert!(parsed.enter_sends);
    }

    #[test]
    fn settings_saved_before_quick_reactions_load_with_none() {
        let old: Settings = serde_json::from_str(r#"{"theme":"light"}"#).unwrap();
        assert!(old.recent_reactions.is_empty());
    }

    #[test]
    fn media_cleanup_is_off_unless_asked_for() {
        let old: Settings = serde_json::from_str(r#"{"theme":"light"}"#).unwrap();
        assert!(!old.media_retention);
        assert_eq!(old.active_media_retention(), None);

        let on: Settings =
            serde_json::from_str(r#"{"media_retention":true,"media_retention_days":60}"#).unwrap();
        assert_eq!(on.active_media_retention(), Some(60));
    }

    #[test]
    fn a_hand_edited_retention_period_stays_in_range() {
        let mut settings = Settings {
            media_retention: true,
            media_retention_days: 0,
            ..Settings::default()
        };
        assert_eq!(settings.active_media_retention(), Some(1));
        settings.media_retention_days = u32::MAX;
        assert_eq!(settings.active_media_retention(), Some(3650));
    }

    #[test]
    fn damaged_theme_cache_does_not_discard_other_settings() {
        let settings: Settings = serde_json::from_str(r#"{"custom_theme":"mine.json","custom_theme_cache":{"damaged":true},"enter_sends":false}"#).unwrap();
        assert!(!settings.enter_sends);
        assert!(settings.custom_theme_cache.is_none());
        assert_eq!(settings.custom_theme.as_deref(), Some("mine.json"));
    }

    #[test]
    fn settings_from_before_the_notification_sound_keep_it_on_with_the_default_interval() {
        let settings: Settings = serde_json::from_str(r#"{"notifications":false}"#).unwrap();
        assert!(settings.notification_sound);
        assert_eq!(settings.notification_sound_interval, 30);
    }

    #[test]
    fn an_unsupported_sound_interval_on_disk_is_replaced_by_the_default() {
        let dir = std::env::temp_dir().join(format!("zaptide-sound-{}", std::process::id()));
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"notification_sound_interval":45,"enter_sends":false}"#,
        )
        .unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.notification_sound_interval, 30);
        assert!(!loaded.enter_sends);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("zaptide-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let settings = Settings {
            zoom: 1.25,
            enter_sends: false,
            voice_speed: 1.5,
            offline_after_leaving_seconds: 0,
            offline_after_inactive_seconds: 900,
            ..Settings::default()
        };
        settings.save(&path).expect("saves");
        assert_eq!(Settings::load(&path), settings);
        let _ = std::fs::remove_dir_all(dir);
    }
}
