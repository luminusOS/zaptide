//! User preferences stored in JSON.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Verifying the locked-chat code costs about 20 ms, paid once per distinct
/// typed string. ponytail: fixed cost, revisit if it lags the search field.
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
    #[serde(
        default,
        deserialize_with = "crate::theme::custom::read_cached_theme",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_theme_cache: Option<crate::theme::custom::CustomTheme>,
    /// Interface zoom factor.
    pub zoom: f32,
    pub sidebar_width: f32,
    /// Whether Enter sends and Shift+Enter adds a line. Off swaps them.
    pub enter_sends: bool,
    /// Send read receipts, subject to the account privacy setting.
    pub send_read_receipts: bool,
    /// Send typing state while composing.
    pub send_typing: bool,
    /// Download attachments when they enter view instead of on click.
    #[serde(alias = "auto_download_images")]
    pub auto_download: bool,
    /// Show sender avatars outside groups too.
    pub show_sender_pictures: bool,
    /// Last open chat, restored at startup.
    pub last_chat: Option<String>,
    pub show_shortcut_hints: bool,
    /// Recently used emoji, newest first.
    pub recent_emoji: Vec<String>,
    /// Keep ZapTide running after its window closes.
    pub keep_running_in_background: bool,
    /// Desktop notifications while away from the chat.
    pub notifications: bool,
    /// Show the sender and message text in notifications.
    pub notification_previews: bool,
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
            system_theme_cache: None,
            zoom: 1.0,
            sidebar_width: 320.0,
            enter_sends: true,
            send_read_receipts: true,
            send_typing: true,
            auto_download: true,
            show_sender_pictures: false,
            last_chat: None,
            show_shortcut_hints: true,
            recent_emoji: Vec::new(),
            keep_running_in_background: true,
            notifications: true,
            notification_previews: true,
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
        let theme = if self.custom_theme.is_some() {
            self.custom_theme_cache.as_ref()
        } else if self.theme == ThemeChoice::System {
            self.system_theme_cache.as_ref()
        } else {
            None
        };
        theme.map(|theme| theme.palette)
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str::<Self>(&contents) {
                Ok(mut settings) => {
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
    fn damaged_theme_cache_does_not_discard_other_settings() {
        let settings: Settings = serde_json::from_str(r#"{"custom_theme":"mine.json","custom_theme_cache":{"damaged":true},"enter_sends":false}"#).unwrap();
        assert!(!settings.enter_sends);
        assert!(settings.custom_theme_cache.is_none());
        assert_eq!(settings.custom_theme.as_deref(), Some("mine.json"));
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("zaptide-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let settings = Settings {
            zoom: 1.25,
            enter_sends: false,
            voice_speed: 1.5,
            ..Settings::default()
        };
        settings.save(&path).expect("saves");
        assert_eq!(Settings::load(&path), settings);
        let _ = std::fs::remove_dir_all(dir);
    }
}
