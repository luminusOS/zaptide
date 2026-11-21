//! GTK-independent preferences model for native settings surfaces.
//!
//! This module deliberately models only controls persisted by [`Settings`].
//! Account identity and archive/cache paths are runtime or path data, not
//! preferences, so they are not invented here. Secret values are write-only:
//! snapshots expose whether they are configured, never their contents.

use crate::i18n::gettext;
use crate::settings::{Language, Settings, ThemeChoice};

/// Existing JSON field that persists a control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsField {
    Language,
    Theme,
    CustomTheme,
    Zoom,
    EnterSends,
    SendReadReceipts,
    SendTyping,
    LinkPreviews,
    AutoDownload,
    ShowSenderPictures,
    NamesFromContacts,
    SaveContactsToPhone,
    ShowShortcutHints,
    TranslucentSidebar,
    ChatLockCodeHash,
    Notifications,
    NotificationPreviews,
    OfflineAfterLeavingSeconds,
    OfflineAfterInactiveSeconds,
    NotificationSound,
    NotificationSoundInterval,
    KeepRunningInBackground,
    VoiceSpeed,
    MediaRetention,
    MediaRetentionDays,
    DownloadDirectory,
    AutoDownloadImages,
    AutoDownloadVideos,
    AutoDownloadAudio,
    AutoDownloadDocuments,
    AutoDownloadOther,
}

impl SettingsField {}

/// Safe snapshot for a native preferences view.
#[derive(Clone, Debug, PartialEq)]
pub struct NativePreferences {
    pub appearance: AppearancePreferences,
    pub account: AccountPreferences,
    pub privacy: PrivacyPreferences,
    pub notifications: NotificationPreferences,
    pub storage: StoragePreferences,
    pub background: BackgroundPreferences,
    pub protocol: ProtocolPreferences,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppearancePreferences {
    pub language: Language,
    pub theme: ThemeChoice,
    pub custom_theme: Option<String>,
    pub zoom: f32,
    pub enter_sends: bool,
    pub show_sender_pictures: bool,
    pub show_shortcut_hints: bool,
    pub translucent_sidebar: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountPreferences {
    pub names_from_contacts: bool,
    pub save_contacts_to_phone: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrivacyPreferences {
    pub send_read_receipts: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationPreferences {
    pub enabled: bool,
    pub previews: bool,
    pub offline_after_leaving_seconds: u32,
    pub offline_after_inactive_seconds: u32,
    pub sound: bool,
    pub sound_interval: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoragePreferences {
    pub auto_download: bool,
    pub auto_download_images: bool,
    pub auto_download_videos: bool,
    pub auto_download_audio: bool,
    pub auto_download_documents: bool,
    pub auto_download_other: bool,
    /// Custom download folder as an absolute path, if one was chosen. The
    /// dialog shows this; `None` means the application's own cache directory.
    pub download_directory: Option<String>,
    pub media_retention: bool,
    pub media_retention_days: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundPreferences {
    pub keep_running: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProtocolPreferences {
    pub send_typing: bool,
    pub voice_speed: f32,
}

impl From<&Settings> for NativePreferences {
    fn from(settings: &Settings) -> Self {
        Self {
            appearance: AppearancePreferences {
                language: settings.language,
                theme: settings.theme,
                custom_theme: settings.custom_theme.clone(),
                zoom: settings.zoom,
                enter_sends: settings.enter_sends,
                show_sender_pictures: settings.show_sender_pictures,
                show_shortcut_hints: settings.show_shortcut_hints,
                translucent_sidebar: settings.translucent_sidebar,
            },
            account: AccountPreferences {
                names_from_contacts: settings.names_from_contacts,
                save_contacts_to_phone: settings.save_contacts_to_phone,
            },
            privacy: PrivacyPreferences {
                send_read_receipts: settings.send_read_receipts,
            },
            notifications: NotificationPreferences {
                enabled: settings.notifications,
                previews: settings.notification_previews,
                offline_after_leaving_seconds: settings.offline_after_leaving_seconds,
                offline_after_inactive_seconds: settings.offline_after_inactive_seconds,
                sound: settings.notification_sound,
                sound_interval: settings.notification_sound_interval,
            },
            storage: StoragePreferences {
                auto_download: settings.auto_download,
                auto_download_images: settings.attachment_auto_download.images,
                auto_download_videos: settings.attachment_auto_download.videos,
                auto_download_audio: settings.attachment_auto_download.audio,
                auto_download_documents: settings.attachment_auto_download.documents,
                auto_download_other: settings.attachment_auto_download.other,
                download_directory: settings
                    .download_directory
                    .as_ref()
                    .map(|path| path.display().to_string()),
                media_retention: settings.media_retention,
                media_retention_days: settings.media_retention_days,
            },
            background: BackgroundPreferences {
                keep_running: settings.keep_running_in_background,
            },
            protocol: ProtocolPreferences {
                send_typing: settings.send_typing,
                voice_speed: settings.voice_speed,
            },
        }
    }
}

/// Typed mutation request. Secret-bearing variants have no corresponding
/// getter in [`NativePreferences`].
#[derive(Clone, PartialEq)]
pub enum PreferenceChange {
    SetLanguage(Language),
    SetTheme(ThemeChoice),
    SetCustomTheme(Option<String>),
    SetZoom(f32),
    SetEnterSends(bool),
    SetSendReadReceipts(bool),
    SetSendTyping(bool),
    SetLinkPreviews(bool),
    SetAutoDownload(bool),
    SetShowSenderPictures(bool),
    SetNamesFromContacts(bool),
    SetSaveContactsToPhone(bool),
    SetShowShortcutHints(bool),
    SetTranslucentSidebar(bool),
    SetChatLockCode(Option<String>),
    SetNotifications(bool),
    SetNotificationPreviews(bool),
    SetOfflineAfterLeavingSeconds(u32),
    SetOfflineAfterInactiveSeconds(u32),
    SetNotificationSound(bool),
    SetNotificationSoundInterval(u32),
    SetKeepRunningInBackground(bool),
    SetVoiceSpeed(f32),
    SetMediaRetention(bool),
    SetMediaRetentionDays(u32),
    /// An absolute local folder, or `None` to go back to the application's
    /// own cache directory.
    SetDownloadDirectory(Option<String>),
    SetAutoDownloadImages(bool),
    SetAutoDownloadVideos(bool),
    SetAutoDownloadAudio(bool),
    SetAutoDownloadDocuments(bool),
    SetAutoDownloadOther(bool),
}

impl std::fmt::Debug for PreferenceChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use PreferenceChange as Change;

        match self {
            Change::SetLanguage(value) => {
                formatter.debug_tuple("SetLanguage").field(value).finish()
            }
            Change::SetTheme(value) => formatter.debug_tuple("SetTheme").field(value).finish(),
            Change::SetCustomTheme(value) => formatter
                .debug_tuple("SetCustomTheme")
                .field(value)
                .finish(),
            Change::SetZoom(value) => formatter.debug_tuple("SetZoom").field(value).finish(),
            Change::SetEnterSends(value) => {
                formatter.debug_tuple("SetEnterSends").field(value).finish()
            }
            Change::SetSendReadReceipts(value) => formatter
                .debug_tuple("SetSendReadReceipts")
                .field(value)
                .finish(),
            Change::SetSendTyping(value) => {
                formatter.debug_tuple("SetSendTyping").field(value).finish()
            }
            Change::SetLinkPreviews(value) => formatter
                .debug_tuple("SetLinkPreviews")
                .field(value)
                .finish(),
            Change::SetAutoDownload(value) => formatter
                .debug_tuple("SetAutoDownload")
                .field(value)
                .finish(),
            Change::SetShowSenderPictures(value) => formatter
                .debug_tuple("SetShowSenderPictures")
                .field(value)
                .finish(),
            Change::SetNamesFromContacts(value) => formatter
                .debug_tuple("SetNamesFromContacts")
                .field(value)
                .finish(),
            Change::SetSaveContactsToPhone(value) => formatter
                .debug_tuple("SetSaveContactsToPhone")
                .field(value)
                .finish(),
            Change::SetShowShortcutHints(value) => formatter
                .debug_tuple("SetShowShortcutHints")
                .field(value)
                .finish(),
            Change::SetTranslucentSidebar(value) => formatter
                .debug_tuple("SetTranslucentSidebar")
                .field(value)
                .finish(),
            Change::SetChatLockCode(_) => formatter
                .debug_tuple("SetChatLockCode")
                .field(&"[REDACTED]")
                .finish(),
            Change::SetNotifications(value) => formatter
                .debug_tuple("SetNotifications")
                .field(value)
                .finish(),
            Change::SetNotificationPreviews(value) => formatter
                .debug_tuple("SetNotificationPreviews")
                .field(value)
                .finish(),
            Change::SetOfflineAfterLeavingSeconds(value) => formatter
                .debug_tuple("SetOfflineAfterLeavingSeconds")
                .field(value)
                .finish(),
            Change::SetOfflineAfterInactiveSeconds(value) => formatter
                .debug_tuple("SetOfflineAfterInactiveSeconds")
                .field(value)
                .finish(),
            Change::SetNotificationSound(value) => formatter
                .debug_tuple("SetNotificationSound")
                .field(value)
                .finish(),
            Change::SetNotificationSoundInterval(value) => formatter
                .debug_tuple("SetNotificationSoundInterval")
                .field(value)
                .finish(),
            Change::SetKeepRunningInBackground(value) => formatter
                .debug_tuple("SetKeepRunningInBackground")
                .field(value)
                .finish(),
            Change::SetVoiceSpeed(value) => {
                formatter.debug_tuple("SetVoiceSpeed").field(value).finish()
            }
            Change::SetMediaRetention(value) => formatter
                .debug_tuple("SetMediaRetention")
                .field(value)
                .finish(),
            Change::SetMediaRetentionDays(value) => formatter
                .debug_tuple("SetMediaRetentionDays")
                .field(value)
                .finish(),
            Change::SetDownloadDirectory(value) => formatter
                .debug_tuple("SetDownloadDirectory")
                .field(value)
                .finish(),
            Change::SetAutoDownloadImages(value) => formatter
                .debug_tuple("SetAutoDownloadImages")
                .field(value)
                .finish(),
            Change::SetAutoDownloadVideos(value) => formatter
                .debug_tuple("SetAutoDownloadVideos")
                .field(value)
                .finish(),
            Change::SetAutoDownloadAudio(value) => formatter
                .debug_tuple("SetAutoDownloadAudio")
                .field(value)
                .finish(),
            Change::SetAutoDownloadDocuments(value) => formatter
                .debug_tuple("SetAutoDownloadDocuments")
                .field(value)
                .finish(),
            Change::SetAutoDownloadOther(value) => formatter
                .debug_tuple("SetAutoDownloadOther")
                .field(value)
                .finish(),
        }
    }
}

impl PreferenceChange {
    /// Existing JSON field changed by this request. Secret values are never returned.
    pub const fn settings_field(&self) -> SettingsField {
        match self {
            Self::SetLanguage(_) => SettingsField::Language,
            Self::SetTheme(_) => SettingsField::Theme,
            Self::SetCustomTheme(_) => SettingsField::CustomTheme,
            Self::SetZoom(_) => SettingsField::Zoom,
            Self::SetEnterSends(_) => SettingsField::EnterSends,
            Self::SetSendReadReceipts(_) => SettingsField::SendReadReceipts,
            Self::SetSendTyping(_) => SettingsField::SendTyping,
            Self::SetLinkPreviews(_) => SettingsField::LinkPreviews,
            Self::SetAutoDownload(_) => SettingsField::AutoDownload,
            Self::SetShowSenderPictures(_) => SettingsField::ShowSenderPictures,
            Self::SetNamesFromContacts(_) => SettingsField::NamesFromContacts,
            Self::SetSaveContactsToPhone(_) => SettingsField::SaveContactsToPhone,
            Self::SetShowShortcutHints(_) => SettingsField::ShowShortcutHints,
            Self::SetTranslucentSidebar(_) => SettingsField::TranslucentSidebar,
            Self::SetChatLockCode(_) => SettingsField::ChatLockCodeHash,
            Self::SetNotifications(_) => SettingsField::Notifications,
            Self::SetNotificationPreviews(_) => SettingsField::NotificationPreviews,
            Self::SetOfflineAfterLeavingSeconds(_) => SettingsField::OfflineAfterLeavingSeconds,
            Self::SetOfflineAfterInactiveSeconds(_) => SettingsField::OfflineAfterInactiveSeconds,
            Self::SetNotificationSound(_) => SettingsField::NotificationSound,
            Self::SetNotificationSoundInterval(_) => SettingsField::NotificationSoundInterval,
            Self::SetKeepRunningInBackground(_) => SettingsField::KeepRunningInBackground,
            Self::SetVoiceSpeed(_) => SettingsField::VoiceSpeed,
            Self::SetMediaRetention(_) => SettingsField::MediaRetention,
            Self::SetMediaRetentionDays(_) => SettingsField::MediaRetentionDays,
            Self::SetDownloadDirectory(_) => SettingsField::DownloadDirectory,
            Self::SetAutoDownloadImages(_) => SettingsField::AutoDownloadImages,
            Self::SetAutoDownloadVideos(_) => SettingsField::AutoDownloadVideos,
            Self::SetAutoDownloadAudio(_) => SettingsField::AutoDownloadAudio,
            Self::SetAutoDownloadDocuments(_) => SettingsField::AutoDownloadDocuments,
            Self::SetAutoDownloadOther(_) => SettingsField::AutoDownloadOther,
        }
    }
}

/// GTK/libadwaita preferences dialog for the native shell.
///
/// Build with [`NativePreferencesDialog::new`], present its dialog, then drain
/// typed edits with [`NativePreferencesDialog::take_changes`]. Invalid edits
/// are reported separately through [`NativePreferencesDialog::take_errors`].
pub struct NativePreferencesDialog {
    dialog: libadwaita::PreferencesDialog,
    open_themes_folder: gtk4::Button,
    custom_theme_row: libadwaita::ComboRow,
    custom_theme_choices: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    custom_theme_handler: gtk4::glib::SignalHandlerId,
    download_dir_row: libadwaita::ActionRow,
    choose_download_dir: gtk4::Button,
    restore_download_dir: gtk4::Button,
    changes: std::rc::Rc<std::cell::RefCell<Vec<PreferenceChange>>>,
    errors: std::rc::Rc<std::cell::RefCell<Vec<ValidationError>>>,
    on_change: std::rc::Rc<dyn Fn()>,
}

#[derive(Clone)]
struct PreferenceChangeSink {
    changes: std::rc::Rc<std::cell::RefCell<Vec<PreferenceChange>>>,
    errors: std::rc::Rc<std::cell::RefCell<Vec<ValidationError>>>,
    on_change: std::rc::Rc<dyn Fn()>,
}

impl PreferenceChangeSink {
    fn push(&self, change: PreferenceChange) {
        push_change(&self.changes, &self.errors, &self.on_change, change);
    }
}

impl NativePreferencesDialog {
    /// Create dialog initialized from secret-free native preference snapshot.
    pub fn new(
        preferences: &NativePreferences,
        theme_choices: &[String],
        on_change: impl Fn() + 'static,
    ) -> Self {
        use libadwaita::prelude::*;

        let dialog = libadwaita::PreferencesDialog::new();
        dialog.set_title(&gettext("Preferences"));
        dialog.set_can_close(true);
        dialog.set_search_enabled(true);
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let on_change: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(on_change);
        let change_sink = PreferenceChangeSink {
            changes: changes.clone(),
            errors: errors.clone(),
            on_change: on_change.clone(),
        };
        let appearance = libadwaita::PreferencesPage::new();
        appearance.set_title(&gettext("Appearance"));
        appearance.set_icon_name(Some("preferences-desktop-appearance-symbolic"));
        let appearance_group = libadwaita::PreferencesGroup::new();
        appearance_group.set_title(&gettext("Display and input"));
        appearance.add(&appearance_group);
        dialog.add(&appearance);

        let language_row = libadwaita::ComboRow::new();
        language_row.set_title(&gettext("Language"));
        language_row.set_subtitle(&gettext("Quit and reopen ZapTide to apply the language"));
        let languages = [
            (Language::System, gettext("System default")),
            (Language::English, "English".into()),
            (Language::Spanish, "Español".into()),
            (Language::PortugueseBrazil, "Português (Brasil)".into()),
        ];
        language_row.set_model(Some(&gtk4::StringList::new(
            &languages.each_ref().map(|(_, label)| label.as_str()),
        )));
        if let Some(index) = languages
            .iter()
            .position(|(language, _)| *language == preferences.appearance.language)
        {
            language_row.set_selected(index as u32);
        }
        {
            let change_sink = change_sink.clone();
            language_row.connect_selected_notify(move |row| {
                if let Some((language, _)) = languages.get(row.selected() as usize) {
                    change_sink.push(PreferenceChange::SetLanguage(*language));
                }
            });
        }
        appearance_group.add(&language_row);

        let theme_row = libadwaita::ComboRow::new();
        theme_row.set_title(&gettext("Theme"));
        theme_row.set_subtitle(&gettext("Choose how ZapTide looks"));
        let themes = gtk4::StringList::new(&[
            &gettext("Follow system"),
            &gettext("Light"),
            &gettext("Dark"),
        ]);
        theme_row.set_model(Some(&themes));
        theme_row.set_selected(match preferences.appearance.theme {
            ThemeChoice::System => 0,
            ThemeChoice::Light => 1,
            ThemeChoice::Dark => 2,
        });
        {
            let change_sink = change_sink.clone();
            theme_row.connect_selected_notify(move |row| {
                let value = match row.selected() {
                    0 => ThemeChoice::System,
                    1 => ThemeChoice::Light,
                    _ => ThemeChoice::Dark,
                };
                change_sink.push(PreferenceChange::SetTheme(value));
            });
        }
        appearance_group.add(&theme_row);

        let custom_theme_row = libadwaita::ComboRow::new();
        custom_theme_row.set_title(&gettext("Custom palette"));
        custom_theme_row.set_subtitle(&gettext("Choose a palette from the themes folder"));
        let custom_theme_choices =
            std::rc::Rc::new(std::cell::RefCell::new(theme_choices.to_vec()));
        let mut custom_theme_labels = vec![gettext("System palette")];
        custom_theme_labels.extend(theme_choices.iter().cloned());
        let custom_theme_label_refs: Vec<_> =
            custom_theme_labels.iter().map(String::as_str).collect();
        let custom_theme_model = gtk4::StringList::new(&custom_theme_label_refs);
        custom_theme_row.set_model(Some(&custom_theme_model));
        let selected_custom_theme = preferences
            .appearance
            .custom_theme
            .as_ref()
            .and_then(|selected| {
                theme_choices
                    .iter()
                    .position(|filename| filename == selected)
            })
            .map_or(0, |index| index as u32 + 1);
        custom_theme_row.set_selected(selected_custom_theme);
        let choice_sink = change_sink.clone();
        let choice_names = custom_theme_choices.clone();
        let custom_theme_handler = custom_theme_row.connect_selected_notify(move |row| {
            let value = row
                .selected()
                .checked_sub(1)
                .and_then(|index| choice_names.borrow().get(index as usize).cloned());
            choice_sink.push(PreferenceChange::SetCustomTheme(value));
        });
        appearance_group.add(&custom_theme_row);

        let open_themes_folder = gtk4::Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text(gettext("Open Themes Folder"))
            .valign(gtk4::Align::Center)
            .css_classes(["flat"])
            .build();
        set_accessible_label(&open_themes_folder, &gettext("Open Themes Folder"));
        custom_theme_row.add_suffix(&open_themes_folder);

        add_numeric_row(
            &appearance_group,
            &gettext("Zoom"),
            &gettext("UI scale, from 0.6× to 2.0×"),
            (f64::from(preferences.appearance.zoom), 0.6, 2.0, 0.05, 2),
            change_sink.clone(),
            PreferenceChange::SetZoom,
        );
        add_switch(
            &appearance_group,
            &gettext("Send messages with Enter"),
            &gettext("Shift+Enter inserts a new line"),
            preferences.appearance.enter_sends,
            change_sink.clone(),
            PreferenceChange::SetEnterSends,
        );
        add_switch(
            &appearance_group,
            &gettext("Show sender pictures"),
            &gettext("Show avatars next to messages"),
            preferences.appearance.show_sender_pictures,
            change_sink.clone(),
            PreferenceChange::SetShowSenderPictures,
        );
        add_switch(
            &appearance_group,
            &gettext("Show shortcut hints"),
            &gettext("Show keyboard shortcut hints"),
            preferences.appearance.show_shortcut_hints,
            change_sink.clone(),
            PreferenceChange::SetShowShortcutHints,
        );
        add_switch(
            &appearance_group,
            &gettext("Translucent sidebar"),
            &gettext(
                "Blur the desktop behind the chat list. Needs a compositor that supports background blur",
            ),
            preferences.appearance.translucent_sidebar,
            change_sink.clone(),
            PreferenceChange::SetTranslucentSidebar,
        );

        let account_page = libadwaita::PreferencesPage::new();
        account_page.set_title(&gettext("Account"));
        account_page.set_icon_name(Some("avatar-default-symbolic"));
        let account_group = libadwaita::PreferencesGroup::new();
        account_group.set_title(&gettext("Contacts"));
        account_page.add(&account_group);
        dialog.add(&account_page);
        add_switch(
            &account_group,
            &gettext("Prefer contact names"),
            &gettext("Use address-book names instead of profile names"),
            preferences.account.names_from_contacts,
            change_sink.clone(),
            PreferenceChange::SetNamesFromContacts,
        );
        add_switch(
            &account_group,
            &gettext("Save contacts to phone"),
            &gettext("Also add saved contacts to the device address book"),
            preferences.account.save_contacts_to_phone,
            change_sink.clone(),
            PreferenceChange::SetSaveContactsToPhone,
        );

        let notifications_page = libadwaita::PreferencesPage::new();
        notifications_page.set_title(&gettext("Notifications"));
        notifications_page.set_icon_name(Some("preferences-system-notifications-symbolic"));
        let notifications_group = libadwaita::PreferencesGroup::new();
        notifications_group.set_title(&gettext("Notifications"));
        notifications_page.add(&notifications_group);
        dialog.add(&notifications_page);
        add_switch(
            &notifications_group,
            &gettext("Desktop notifications"),
            &gettext("Notify when messages arrive while away"),
            preferences.notifications.enabled,
            change_sink.clone(),
            PreferenceChange::SetNotifications,
        );
        add_switch(
            &notifications_group,
            &gettext("Show previews"),
            &gettext("Include the sender and message text"),
            preferences.notifications.previews,
            change_sink.clone(),
            PreferenceChange::SetNotificationPreviews,
        );
        let sound_row = libadwaita::SwitchRow::new();
        sound_row.set_title(&gettext("Notification sound"));
        sound_row.set_subtitle(&gettext(
            "Play a short sound with notifications, where the desktop supports it",
        ));
        sound_row.set_active(preferences.notifications.sound);
        {
            let change_sink = change_sink.clone();
            sound_row.connect_active_notify(move |row| {
                change_sink.push(PreferenceChange::SetNotificationSound(row.is_active()));
            });
        }
        notifications_group.add(&sound_row);

        let interval_row = libadwaita::ComboRow::new();
        interval_row.set_title(&gettext("Pause between sounds"));
        interval_row.set_subtitle(&gettext("Messages that arrive sooner stay silent"));
        interval_row.set_model(Some(&gtk4::StringList::new(&[
            &gettext("No pause"),
            &gettext("10 seconds"),
            &gettext("30 seconds"),
            &gettext("1 minute"),
            &gettext("2 minutes"),
        ])));
        interval_row.set_selected(
            crate::notification_sound::INTERVALS
                .iter()
                .position(|seconds| *seconds == preferences.notifications.sound_interval)
                .map_or(2, |index| index as u32),
        );
        {
            let change_sink = change_sink.clone();
            interval_row.connect_selected_notify(move |row| {
                if let Some(seconds) = crate::notification_sound::INTERVALS
                    .get(row.selected() as usize)
                    .copied()
                {
                    change_sink.push(PreferenceChange::SetNotificationSoundInterval(seconds));
                }
            });
        }
        sound_row
            .bind_property("active", &interval_row, "sensitive")
            .sync_create()
            .build();
        notifications_group.add(&interval_row);
        add_online_status(
            &notifications_page,
            &preferences.notifications,
            &change_sink,
        );

        let storage_page = libadwaita::PreferencesPage::new();
        storage_page.set_title(&gettext("Storage"));
        storage_page.set_icon_name(Some("drive-harddisk-symbolic"));
        let storage_group = libadwaita::PreferencesGroup::new();
        storage_group.set_title(&gettext("Downloads"));
        storage_page.add(&storage_group);
        dialog.add(&storage_page);
        let auto_download_row = add_switch(
            &storage_group,
            &gettext("Automatically download attachments"),
            &gettext("Download attachments when they enter view"),
            preferences.storage.auto_download,
            change_sink.clone(),
            PreferenceChange::SetAutoDownload,
        );
        add_attachment_type_switches(
            &storage_page,
            &preferences.storage,
            &auto_download_row,
            &change_sink,
        );
        let (download_dir_row, choose_download_dir, restore_download_dir) = add_download_location(
            &storage_page,
            preferences.storage.download_directory.as_deref(),
        );
        add_media_retention(&storage_page, &preferences.storage, &change_sink);

        let background_page = libadwaita::PreferencesPage::new();
        background_page.set_title(&gettext("Background"));
        background_page.set_icon_name(Some("preferences-system-time-symbolic"));
        let background_group = libadwaita::PreferencesGroup::new();
        background_group.set_title(&gettext("Background and updates"));
        background_page.add(&background_group);
        dialog.add(&background_page);
        add_switch(
            &background_group,
            &gettext("Keep running in background"),
            &gettext("Keep ZapTide available after closing its window"),
            preferences.background.keep_running,
            change_sink.clone(),
            PreferenceChange::SetKeepRunningInBackground,
        );

        let protocol_page = libadwaita::PreferencesPage::new();
        protocol_page.set_title(&gettext("Messaging"));
        protocol_page.set_icon_name(Some("mail-send-symbolic"));
        let protocol_group = libadwaita::PreferencesGroup::new();
        protocol_group.set_title(&gettext("Messaging and media"));
        protocol_page.add(&protocol_group);
        dialog.add(&protocol_page);
        let voice_row = libadwaita::ComboRow::new();
        voice_row.set_title(&gettext("Voice playback speed"));
        voice_row.set_model(Some(&gtk4::StringList::new(&["1×", "1.5×", "2×"])));
        voice_row.set_selected(match preferences.protocol.voice_speed {
            1.5 => 1,
            2.0 => 2,
            _ => 0,
        });
        {
            let change_sink = change_sink.clone();
            voice_row.connect_selected_notify(move |row| {
                let value = [1.0, 1.5, 2.0]
                    .get(row.selected() as usize)
                    .copied()
                    .unwrap_or(1.0);
                change_sink.push(PreferenceChange::SetVoiceSpeed(value));
            });
        }
        protocol_group.add(&voice_row);

        Self {
            dialog,
            open_themes_folder,
            custom_theme_row,
            custom_theme_choices,
            custom_theme_handler,
            download_dir_row,
            choose_download_dir,
            restore_download_dir,
            changes,
            errors,
            on_change,
        }
    }

    /// Borrow dialog for presentation and parent-window integration.
    pub fn dialog(&self) -> &libadwaita::PreferencesDialog {
        &self.dialog
    }

    /// Button that opens the local themes directory through the desktop handler.
    pub fn open_themes_folder_button(&self) -> &gtk4::Button {
        &self.open_themes_folder
    }

    /// Refresh available local palettes without treating model replacement as a user edit.
    pub fn set_custom_theme_choices(&mut self, filenames: &[String], selected: Option<&str>) {
        use gtk4::prelude::*;
        use libadwaita::prelude::*;

        *self.custom_theme_choices.borrow_mut() = filenames.to_vec();
        let mut labels = vec![gettext("System palette")];
        labels.extend(filenames.iter().cloned());
        let label_refs: Vec<_> = labels.iter().map(String::as_str).collect();
        let model = gtk4::StringList::new(&label_refs);
        self.custom_theme_row
            .block_signal(&self.custom_theme_handler);
        self.custom_theme_row.set_model(Some(&model));
        let selected_index = selected
            .and_then(|filename| filenames.iter().position(|candidate| candidate == filename))
            .map_or(0, |index| index as u32 + 1);
        self.custom_theme_row.set_selected(selected_index);
        self.custom_theme_row
            .unblock_signal(&self.custom_theme_handler);
    }

    /// Button that opens a folder picker for newly downloaded attachments.
    pub fn choose_download_directory_button(&self) -> &gtk4::Button {
        &self.choose_download_dir
    }

    /// Button that returns downloads to the application's own cache directory.
    pub fn restore_download_directory_button(&self) -> &gtk4::Button {
        &self.restore_download_dir
    }

    /// Records a folder chosen through the external picker (or `None` from
    /// the restore-default button) and updates what the row shows. The pick
    /// itself happens outside this module since it needs the portal and the
    /// window, not just preference state.
    pub fn apply_download_directory_choice(&mut self, path: Option<String>) {
        use libadwaita::prelude::*;
        push_change(
            &self.changes,
            &self.errors,
            &self.on_change,
            PreferenceChange::SetDownloadDirectory(path.clone()),
        );
        self.download_dir_row
            .set_subtitle(&download_directory_subtitle(path.as_deref()));
        self.restore_download_dir.set_visible(path.is_some());
    }

    pub fn present(&self, parent: &impl gtk4::prelude::IsA<gtk4::Widget>) {
        use libadwaita::prelude::*;
        self.dialog.present(Some(parent));
    }

    /// Return pending valid edits as typed settings mutations.
    pub fn take_changes(&self) -> Vec<PreferenceChange> {
        std::mem::take(&mut *self.changes.borrow_mut())
    }

    /// Return and clear validation failures from attempted edits.
    pub fn take_errors(&self) -> Vec<ValidationError> {
        std::mem::take(&mut *self.errors.borrow_mut())
    }
}

fn push_change(
    changes: &std::rc::Rc<std::cell::RefCell<Vec<PreferenceChange>>>,
    errors: &std::rc::Rc<std::cell::RefCell<Vec<ValidationError>>>,
    on_change: &std::rc::Rc<dyn Fn()>,
    change: PreferenceChange,
) {
    // Secret mutation paths deliberately avoid applying against temporary settings:
    // chat-code application performs a costly verifier derivation.
    let validation = match &change {
        PreferenceChange::SetZoom(value) if !value.is_finite() || !(0.6..=2.0).contains(value) => {
            Some(ValidationError::ZoomOutOfRange)
        }
        PreferenceChange::SetCustomTheme(Some(filename))
            if filename.trim().is_empty()
                || filename == "."
                || filename == ".."
                || filename.contains('/')
                || filename.contains('\\') =>
        {
            Some(ValidationError::InvalidCustomThemeFilename)
        }
        PreferenceChange::SetVoiceSpeed(value) if ![1.0, 1.5, 2.0].contains(value) => {
            Some(ValidationError::UnsupportedVoiceSpeed)
        }
        PreferenceChange::SetOfflineAfterLeavingSeconds(value)
        | PreferenceChange::SetOfflineAfterInactiveSeconds(value)
            if *value > MAX_OFFLINE_SECONDS =>
        {
            Some(ValidationError::OfflineDelayOutOfRange)
        }
        PreferenceChange::SetMediaRetentionDays(value) if !retention_days_in_range(*value) => {
            Some(ValidationError::MediaRetentionOutOfRange)
        }
        PreferenceChange::SetDownloadDirectory(Some(path)) if !is_absolute_path(path) => {
            Some(ValidationError::InvalidDownloadDirectory)
        }
        PreferenceChange::SetNotificationSoundInterval(value)
            if !crate::notification_sound::is_supported_interval(*value) =>
        {
            Some(ValidationError::UnsupportedSoundInterval)
        }
        _ => None,
    };
    if let Some(error) = validation {
        errors.borrow_mut().push(error);
    } else {
        changes.borrow_mut().push(change);
    }
    on_change();
}

/// One switch per attachment category, each sensitive only while
/// `master` (the overall "automatically download" switch) is on. Turning a
/// category off never blocks downloading it manually from its message.
fn add_attachment_type_switches(
    page: &libadwaita::PreferencesPage,
    preferences: &StoragePreferences,
    master: &libadwaita::SwitchRow,
    change_sink: &PreferenceChangeSink,
) {
    use libadwaita::prelude::*;
    let group = libadwaita::PreferencesGroup::new();
    group.set_title(&gettext("Download automatically"));
    group.set_description(Some(&gettext(
        "Turn off a category to keep its attachments from downloading until you open them",
    )));
    page.add(&group);

    type PreferenceRow = (String, bool, fn(bool) -> PreferenceChange);
    let rows: [PreferenceRow; 5] = [
        (
            gettext("Images"),
            preferences.auto_download_images,
            PreferenceChange::SetAutoDownloadImages,
        ),
        (
            gettext("Videos"),
            preferences.auto_download_videos,
            PreferenceChange::SetAutoDownloadVideos,
        ),
        (
            gettext("Audio and voice messages"),
            preferences.auto_download_audio,
            PreferenceChange::SetAutoDownloadAudio,
        ),
        (
            gettext("Documents"),
            preferences.auto_download_documents,
            PreferenceChange::SetAutoDownloadDocuments,
        ),
        (
            gettext("Other files"),
            preferences.auto_download_other,
            PreferenceChange::SetAutoDownloadOther,
        ),
    ];
    for (title, active, make_change) in rows {
        let row = libadwaita::SwitchRow::new();
        row.set_title(&title);
        row.set_active(active);
        row.set_sensitive(preferences.auto_download);
        master
            .bind_property("active", &row, "sensitive")
            .sync_create()
            .build();
        let sink = change_sink.clone();
        row.connect_active_notify(move |row| sink.push(make_change(row.is_active())));
        group.add(&row);
    }
}

/// Text for the current folder: the chosen path, or a note that attachments
/// go to the application's own cache directory.
fn download_directory_subtitle(current: Option<&str>) -> String {
    current.map_or_else(
        || gettext("Default location (application cache folder)"),
        str::to_owned,
    )
}

/// Folder row with buttons to choose a custom download location or restore
/// the default one. The actual folder picker runs outside this module
/// (it needs the portal and the parent window); callers wire the returned
/// buttons and report the result back through
/// [`NativePreferencesDialog::apply_download_directory_choice`].
fn add_download_location(
    page: &libadwaita::PreferencesPage,
    current: Option<&str>,
) -> (libadwaita::ActionRow, gtk4::Button, gtk4::Button) {
    use libadwaita::prelude::*;
    let group = libadwaita::PreferencesGroup::new();
    group.set_title(&gettext("Download location"));
    group.set_description(Some(&gettext(
        "Where ZapTide saves newly downloaded photos, videos, audio and files",
    )));
    page.add(&group);

    let row = libadwaita::ActionRow::new();
    row.set_title(&gettext("Folder"));
    row.set_subtitle(&download_directory_subtitle(current));

    let choose = gtk4::Button::builder()
        .icon_name("folder-open-symbolic")
        .valign(gtk4::Align::Center)
        .tooltip_text(gettext("Choose a Folder"))
        .css_classes(["flat"])
        .build();
    set_accessible_label(&choose, "Choose a download folder");
    row.add_suffix(&choose);

    let restore = gtk4::Button::builder()
        .icon_name("edit-undo-symbolic")
        .valign(gtk4::Align::Center)
        .tooltip_text(gettext("Restore Default Location"))
        .css_classes(["flat"])
        .visible(current.is_some())
        .build();
    set_accessible_label(&restore, "Restore the default download location");
    row.add_suffix(&restore);

    group.add(&row);
    (row, choose, restore)
}

const RETENTION_CHOICES: [u32; 5] = [30, 60, 90, 180, 365];

fn add_media_retention(
    page: &libadwaita::PreferencesPage,
    preferences: &StoragePreferences,
    change_sink: &PreferenceChangeSink,
) -> (libadwaita::SwitchRow, libadwaita::ComboRow) {
    use libadwaita::prelude::*;
    let group = libadwaita::PreferencesGroup::new();
    group.set_title(&gettext("Free up space"));
    group.set_description(Some(&gettext(
        "Messages stay. A removed file downloads again when you open it, if WhatsApp still has it. Files you can't download again from WhatsApp are lost.",
    )));
    page.add(&group);

    let switch = libadwaita::SwitchRow::new();
    switch.set_title(&gettext("Delete old downloads"));
    switch.set_subtitle(&gettext(
        "Remove photos, videos, audio and files from this computer after a while",
    ));
    switch.set_active(preferences.media_retention);
    let sink = change_sink.clone();
    switch.connect_active_notify(move |row| {
        sink.push(PreferenceChange::SetMediaRetention(row.is_active()))
    });
    group.add(&switch);

    let row = libadwaita::ComboRow::new();
    row.set_title(&gettext("Keep downloads for"));
    let (labels, values, selected) = retention_choices(preferences.media_retention_days);
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    row.set_model(Some(&gtk4::StringList::new(&labels)));
    row.set_selected(selected as u32);
    switch
        .bind_property("active", &row, "sensitive")
        .sync_create()
        .build();
    group.add(&row);
    let sink = change_sink.clone();
    row.connect_selected_notify(move |row| {
        if let Some(days) = values.get(row.selected() as usize) {
            sink.push(PreferenceChange::SetMediaRetentionDays(*days));
        }
    });
    (switch, row)
}

/// Labels, values and the selected index. A period that is not a preset, such
/// as one from a hand-edited file, stays in force, so it is listed first.
fn retention_choices(current: u32) -> (Vec<String>, Vec<u32>, usize) {
    let preset = |days: u32| match days {
        30 => gettext("30 days"),
        60 => gettext("60 days"),
        90 => gettext("90 days"),
        180 => gettext("180 days"),
        _ => gettext("1 year"),
    };
    let mut labels: Vec<String> = RETENTION_CHOICES.iter().map(|days| preset(*days)).collect();
    let mut values = RETENTION_CHOICES.to_vec();
    let selected = values.iter().position(|days| *days == current);
    if selected.is_none() {
        labels.insert(
            0,
            gettext("Custom ({} days)").replace("{}", &current.to_string()),
        );
        values.insert(0, current);
    }
    (labels, values, selected.unwrap_or(0))
}

fn add_online_status(
    page: &libadwaita::PreferencesPage,
    preferences: &NotificationPreferences,
    change_sink: &PreferenceChangeSink,
) {
    use libadwaita::prelude::*;
    let group = libadwaita::PreferencesGroup::new();
    group.set_title(&gettext("Online status"));
    group.set_description(Some(&gettext(
        "While ZapTide shows you as online, your phone does not receive notifications.",
    )));
    page.add(&group);
    add_offline_row(
        &group,
        (
            &gettext("Go offline when I leave ZapTide"),
            &gettext("Switch to another app or lock the screen"),
        ),
        &[
            (&gettext("After 10 seconds"), 10),
            (&gettext("After 1 minute"), 60),
            (&gettext("After 5 minutes"), 300),
            (&gettext("Never"), 0),
        ],
        preferences.offline_after_leaving_seconds,
        change_sink,
        PreferenceChange::SetOfflineAfterLeavingSeconds,
    );
    add_offline_row(
        &group,
        (
            &gettext("Go offline when inactive"),
            &gettext("No typing, clicks or pointer movement in ZapTide"),
        ),
        &[
            (&gettext("After 1 minute"), 60),
            (&gettext("After 5 minutes"), 300),
            (&gettext("After 15 minutes"), 900),
            (&gettext("Never"), 0),
        ],
        preferences.offline_after_inactive_seconds,
        change_sink,
        PreferenceChange::SetOfflineAfterInactiveSeconds,
    );
}

/// A row that chooses one of `choices`; a value of 0 means never.
fn add_offline_row(
    group: &libadwaita::PreferencesGroup,
    (title, subtitle): (&str, &str),
    choices: &[(&str, u32)],
    current: u32,
    change_sink: &PreferenceChangeSink,
    change: fn(u32) -> PreferenceChange,
) {
    use libadwaita::prelude::*;
    let row = libadwaita::ComboRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);
    let (labels, values, selected) = offline_choices(choices, current);
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    row.set_model(Some(&gtk4::StringList::new(&labels)));
    row.set_selected(selected as u32);
    group.add(&row);
    let sink = change_sink.clone();
    row.connect_selected_notify(move |row| {
        if let Some(value) = values.get(row.selected() as usize) {
            sink.push(change(*value));
        }
    });
}

/// The labels, values and selected index of a row. A value that is not one of
/// `choices`, such as one from a hand-edited file, stays in force, so the row
/// lists it first instead of showing another choice.
fn offline_choices(choices: &[(&str, u32)], current: u32) -> (Vec<String>, Vec<u32>, usize) {
    let mut labels: Vec<String> = choices.iter().map(|(label, _)| (*label).into()).collect();
    let mut values: Vec<u32> = choices.iter().map(|(_, value)| *value).collect();
    let selected = values.iter().position(|value| *value == current);
    if selected.is_none() {
        labels.insert(
            0,
            gettext("Custom ({seconds} s)").replace("{seconds}", &current.to_string()),
        );
        values.insert(0, current);
    }
    (labels, values, selected.unwrap_or(0))
}

fn add_switch(
    group: &libadwaita::PreferencesGroup,
    title: &str,
    subtitle: &str,
    active: bool,
    change_sink: PreferenceChangeSink,
    make_change: fn(bool) -> PreferenceChange,
) -> libadwaita::SwitchRow {
    use libadwaita::prelude::*;
    let row = libadwaita::SwitchRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_active(active);
    row.connect_active_notify(move |row| change_sink.push(make_change(row.is_active())));
    group.add(&row);
    row
}

pub(crate) fn set_accessible_label(
    widget: &impl gtk4::prelude::IsA<gtk4::Accessible>,
    label: &str,
) {
    use gtk4::prelude::*;
    widget.update_property(&[gtk4::accessible::Property::Label(label)]);
}

fn add_numeric_row(
    group: &libadwaita::PreferencesGroup,
    title: &str,
    subtitle: &str,
    // Value, minimum, maximum, step, and decimal places shown.
    range: (f64, f64, f64, f64, u32),
    change_sink: PreferenceChangeSink,
    make_change: fn(f32) -> PreferenceChange,
) {
    use libadwaita::prelude::*;
    let (value, min, max, step, digits) = range;
    let adjustment = gtk4::Adjustment::new(value, min, max, step, step * 10.0, 0.0);
    let row = libadwaita::SpinRow::new(Some(&adjustment), step, digits);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_numeric(true);
    row.connect_value_notify(move |row| change_sink.push(make_change(row.value() as f32)));
    group.add(&row);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationError {
    ZoomOutOfRange,
    InvalidCustomThemeFilename,
    UnsupportedVoiceSpeed,
    OfflineDelayOutOfRange,
    MediaRetentionOutOfRange,
    UnsupportedSoundInterval,
    InvalidDownloadDirectory,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::ZoomOutOfRange => "zoom must be finite and between 0.6 and 2.0",
            Self::InvalidCustomThemeFilename => "custom theme must be a local filename",
            Self::UnsupportedVoiceSpeed => "voice speed must be 1.0, 1.5, or 2.0",
            Self::OfflineDelayOutOfRange => "offline delay must be at most 24 hours",
            Self::MediaRetentionOutOfRange => "download retention must be 1 to 3650 days",
            Self::UnsupportedSoundInterval => {
                "notification sound interval must be 0, 10, 30, 60, or 120 seconds"
            }
            Self::InvalidDownloadDirectory => "download folder must be an absolute local path",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ValidationError {}

const MAX_OFFLINE_SECONDS: u32 = 24 * 60 * 60;

fn retention_days_in_range(days: u32) -> bool {
    (crate::media_retention::MIN_DAYS..=crate::media_retention::MAX_DAYS).contains(&days)
}

fn is_absolute_path(path: &str) -> bool {
    !path.trim().is_empty() && std::path::Path::new(path).is_absolute()
}

fn checked_offline_delay(seconds: u32) -> Result<u32, ValidationError> {
    if seconds > MAX_OFFLINE_SECONDS {
        return Err(ValidationError::OfflineDelayOutOfRange);
    }
    Ok(seconds)
}

impl PreferenceChange {
    /// Validate and apply one change. Invalid values leave `settings` untouched.
    pub fn apply(self, settings: &mut Settings) -> Result<(), ValidationError> {
        match self {
            Self::SetLanguage(value) => settings.language = value,
            Self::SetTheme(value) => settings.theme = value,
            Self::SetCustomTheme(value) => {
                if value.as_deref().is_some_and(|filename| {
                    filename.trim().is_empty()
                        || filename == "."
                        || filename == ".."
                        || filename.contains('/')
                        || filename.contains('\\')
                }) {
                    return Err(ValidationError::InvalidCustomThemeFilename);
                }
                settings.custom_theme = value;
                settings.custom_theme_cache = None;
            }
            Self::SetZoom(value) => {
                if !value.is_finite() || !(0.6..=2.0).contains(&value) {
                    return Err(ValidationError::ZoomOutOfRange);
                }
                settings.zoom = value;
            }
            Self::SetEnterSends(value) => settings.enter_sends = value,
            Self::SetSendReadReceipts(value) => settings.send_read_receipts = value,
            Self::SetSendTyping(value) => settings.send_typing = value,
            Self::SetLinkPreviews(value) => settings.link_previews = value,
            Self::SetAutoDownload(value) => settings.auto_download = value,
            Self::SetShowSenderPictures(value) => settings.show_sender_pictures = value,
            Self::SetNamesFromContacts(value) => settings.names_from_contacts = value,
            Self::SetSaveContactsToPhone(value) => settings.save_contacts_to_phone = value,
            Self::SetShowShortcutHints(value) => settings.show_shortcut_hints = value,
            Self::SetTranslucentSidebar(value) => settings.translucent_sidebar = value,
            Self::SetChatLockCode(value) => settings.set_chat_lock_code(value.as_deref()),
            Self::SetNotifications(value) => settings.notifications = value,
            Self::SetNotificationPreviews(value) => settings.notification_previews = value,
            Self::SetOfflineAfterLeavingSeconds(value) => {
                settings.offline_after_leaving_seconds = checked_offline_delay(value)?
            }
            Self::SetOfflineAfterInactiveSeconds(value) => {
                settings.offline_after_inactive_seconds = checked_offline_delay(value)?
            }
            Self::SetNotificationSound(value) => settings.notification_sound = value,
            Self::SetNotificationSoundInterval(value) => {
                if !crate::notification_sound::is_supported_interval(value) {
                    return Err(ValidationError::UnsupportedSoundInterval);
                }
                settings.notification_sound_interval = value;
            }
            Self::SetKeepRunningInBackground(value) => settings.keep_running_in_background = value,
            Self::SetVoiceSpeed(value) => {
                if ![1.0, 1.5, 2.0].contains(&value) {
                    return Err(ValidationError::UnsupportedVoiceSpeed);
                }
                settings.voice_speed = value;
            }
            Self::SetMediaRetention(value) => settings.media_retention = value,
            Self::SetMediaRetentionDays(value) => {
                if !retention_days_in_range(value) {
                    return Err(ValidationError::MediaRetentionOutOfRange);
                }
                settings.media_retention_days = value;
            }
            Self::SetDownloadDirectory(value) => {
                if let Some(path) = &value
                    && !is_absolute_path(path)
                {
                    return Err(ValidationError::InvalidDownloadDirectory);
                }
                settings.download_directory = value.map(std::path::PathBuf::from);
            }
            Self::SetAutoDownloadImages(value) => {
                settings.attachment_auto_download.images = value;
            }
            Self::SetAutoDownloadVideos(value) => {
                settings.attachment_auto_download.videos = value;
            }
            Self::SetAutoDownloadAudio(value) => {
                settings.attachment_auto_download.audio = value;
            }
            Self::SetAutoDownloadDocuments(value) => {
                settings.attachment_auto_download.documents = value;
            }
            Self::SetAutoDownloadOther(value) => {
                settings.attachment_auto_download.other = value;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_debug_redacts_secret_inputs() {
        let chat_lock = format!(
            "{:?}",
            PreferenceChange::SetChatLockCode(Some("2468".into()))
        );

        assert!(chat_lock.contains("[REDACTED]"));
        assert!(!chat_lock.contains("2468"));
    }

    #[test]
    fn a_custom_offline_delay_stays_selectable_and_in_force() {
        let choices = [("After 10 seconds", 10), ("Never", 0)];
        let (labels, values, selected) = offline_choices(&choices, 10);
        assert_eq!((labels.len(), values[selected]), (2, 10));
        let (labels, values, selected) = offline_choices(&choices, 30);
        assert_eq!(labels[selected], "Custom (30 s)");
        assert_eq!(values, [30, 10, 0]);
        assert_eq!(selected, 0);
    }

    #[test]
    fn a_custom_retention_period_stays_selectable_and_in_force() {
        let (labels, values, selected) = retention_choices(90);
        assert_eq!((labels.len(), values[selected]), (5, 90));
        let (labels, values, selected) = retention_choices(45);
        assert_eq!(labels[selected], "Custom (45 days)");
        assert_eq!((values[selected], values.len()), (45, 6));
    }

    #[test]
    fn retention_periods_outside_the_supported_range_are_rejected() {
        let mut settings = Settings::default();
        let before = settings.clone();
        for days in [0, 3651, u32::MAX] {
            assert_eq!(
                PreferenceChange::SetMediaRetentionDays(days).apply(&mut settings),
                Err(ValidationError::MediaRetentionOutOfRange)
            );
        }
        assert_eq!(settings, before);
        PreferenceChange::SetMediaRetentionDays(60)
            .apply(&mut settings)
            .unwrap();
        PreferenceChange::SetMediaRetention(true)
            .apply(&mut settings)
            .unwrap();
        assert_eq!(settings.active_media_retention(), Some(60));
        assert_eq!(
            NativePreferences::from(&settings).storage,
            StoragePreferences {
                auto_download: true,
                auto_download_images: true,
                auto_download_videos: true,
                auto_download_audio: true,
                auto_download_documents: true,
                auto_download_other: true,
                download_directory: None,
                media_retention: true,
                media_retention_days: 60,
            }
        );
    }

    #[test]
    fn offline_delays_beyond_a_day_are_rejected() {
        let mut settings = Settings::default();
        let before = settings.clone();
        for change in [
            PreferenceChange::SetOfflineAfterLeavingSeconds(MAX_OFFLINE_SECONDS + 1),
            PreferenceChange::SetOfflineAfterInactiveSeconds(u32::MAX),
        ] {
            assert_eq!(
                change.apply(&mut settings),
                Err(ValidationError::OfflineDelayOutOfRange)
            );
        }
        assert_eq!(settings, before);
        PreferenceChange::SetOfflineAfterInactiveSeconds(MAX_OFFLINE_SECONDS)
            .apply(&mut settings)
            .expect("a day is allowed");
    }

    #[test]
    fn invalid_changes_are_rejected_without_mutating_settings() {
        let mut settings = Settings::default();
        let before = settings.clone();

        assert_eq!(
            PreferenceChange::SetZoom(f32::NAN).apply(&mut settings),
            Err(ValidationError::ZoomOutOfRange)
        );
        assert_eq!(
            PreferenceChange::SetCustomTheme(Some("../secret.json".into())).apply(&mut settings),
            Err(ValidationError::InvalidCustomThemeFilename)
        );
        assert_eq!(
            PreferenceChange::SetVoiceSpeed(1.25).apply(&mut settings),
            Err(ValidationError::UnsupportedVoiceSpeed)
        );
        assert_eq!(settings, before);
    }

    #[test]
    fn every_offered_sound_interval_can_be_chosen() {
        let mut settings = Settings::default();
        for seconds in [0, 10, 30, 60, 120] {
            PreferenceChange::SetNotificationSoundInterval(seconds)
                .apply(&mut settings)
                .unwrap();
            assert_eq!(settings.notification_sound_interval, seconds);
        }
    }

    #[test]
    fn an_unoffered_sound_interval_is_rejected_without_mutating_settings() {
        let mut settings = Settings::default();
        let before = settings.clone();

        assert_eq!(
            PreferenceChange::SetNotificationSoundInterval(45).apply(&mut settings),
            Err(ValidationError::UnsupportedSoundInterval)
        );
        assert_eq!(settings, before);

        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let on_change: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(|| {});
        push_change(
            &changes,
            &errors,
            &on_change,
            PreferenceChange::SetNotificationSoundInterval(45),
        );
        assert!(changes.borrow().is_empty());
        assert_eq!(
            *errors.borrow(),
            [ValidationError::UnsupportedSoundInterval]
        );
    }

    #[test]
    fn the_sound_switch_changes_only_the_sound_and_the_snapshot_reflects_it() {
        let mut settings = Settings::default();
        let notifications = settings.notifications;
        PreferenceChange::SetNotificationSound(false)
            .apply(&mut settings)
            .unwrap();

        let snapshot = NativePreferences::from(&settings);
        assert!(!snapshot.notifications.sound);
        assert_eq!(snapshot.notifications.sound_interval, 30);
        assert_eq!(settings.notifications, notifications);
        assert_eq!(
            PreferenceChange::SetNotificationSound(true).settings_field(),
            SettingsField::NotificationSound
        );
        assert_eq!(
            PreferenceChange::SetNotificationSoundInterval(10).settings_field(),
            SettingsField::NotificationSoundInterval
        );
    }

    #[test]
    fn preference_change_notifies_parent_for_valid_and_invalid_edits() {
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        let notified = notifications.clone();
        let on_change: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(move || {
            notified.set(notified.get() + 1);
        });

        push_change(
            &changes,
            &errors,
            &on_change,
            PreferenceChange::SetNotifications(false),
        );
        push_change(
            &changes,
            &errors,
            &on_change,
            PreferenceChange::SetZoom(3.0),
        );

        assert_eq!(changes.borrow().len(), 1);
        assert_eq!(errors.borrow().len(), 1);
        assert_eq!(notifications.get(), 2);
    }

    #[test]
    fn validated_ranges_and_write_only_secret_clear_are_supported() {
        let mut settings = Settings::default();
        PreferenceChange::SetZoom(2.0).apply(&mut settings).unwrap();
        PreferenceChange::SetVoiceSpeed(1.5)
            .apply(&mut settings)
            .unwrap();
        PreferenceChange::SetChatLockCode(Some("1234".into()))
            .apply(&mut settings)
            .unwrap();
        PreferenceChange::SetChatLockCode(None)
            .apply(&mut settings)
            .unwrap();

        assert_eq!(settings.zoom, 2.0);
        assert_eq!(settings.voice_speed, 1.5);
        assert!(settings.chat_lock_code_hash.is_none());
    }

    #[test]
    #[ignore = "requires a display"]
    fn language_row_restores_selection_and_emits_changes() {
        use libadwaita::prelude::*;

        fn find_language_row(widget: &gtk4::Widget) -> Option<libadwaita::ComboRow> {
            if let Some(row) = widget.downcast_ref::<libadwaita::ComboRow>()
                && row.title() == gettext("Language")
            {
                return Some(row.clone());
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(row) = find_language_row(&widget) {
                    return Some(row);
                }
                child = widget.next_sibling();
            }
            None
        }

        libadwaita::init().unwrap();
        let settings = Settings {
            language: Language::Spanish,
            ..Settings::default()
        };
        let dialog = NativePreferencesDialog::new(&NativePreferences::from(&settings), &[], || {});
        let window = libadwaita::Window::new();
        window.present();
        dialog.present(&window);
        let page = dialog.dialog().visible_page().expect("appearance page");
        let row = find_language_row(page.upcast_ref()).expect("language selector");
        assert_eq!(row.selected(), 2);
        assert!(dialog.take_changes().is_empty());
        assert!(row.is_focusable());
        for (index, language) in [
            (3, Language::PortugueseBrazil),
            (1, Language::English),
            (0, Language::System),
        ] {
            row.set_selected(index);
            assert_eq!(
                dialog.take_changes(),
                [PreferenceChange::SetLanguage(language)]
            );
        }
        window.destroy();
    }

    #[test]
    #[ignore = "requires a display; run with GSETTINGS_BACKEND=memory cargo test --lib native_preferences::tests::retention_rows_follow_the_switch_and_report_edits -- --ignored"]
    fn retention_rows_follow_the_switch_and_report_edits() {
        use gtk4::prelude::*;
        use libadwaita::prelude::*;

        gtk4::init().expect("GTK display");
        libadwaita::init().expect("libadwaita");
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = PreferenceChangeSink {
            changes: changes.clone(),
            errors,
            on_change: std::rc::Rc::new(|| {}),
        };
        let page = libadwaita::PreferencesPage::new();
        let storage = StoragePreferences {
            auto_download: true,
            auto_download_images: true,
            auto_download_videos: true,
            auto_download_audio: true,
            auto_download_documents: true,
            auto_download_other: true,
            download_directory: None,
            media_retention: false,
            media_retention_days: 90,
        };

        let (switch, days) = add_media_retention(&page, &storage, &sink);

        assert!(!switch.is_active());
        assert!(!days.is_sensitive());
        assert_eq!(days.selected(), 2);
        switch.set_active(true);
        assert!(days.is_sensitive());
        days.set_selected(0);
        assert_eq!(
            *changes.borrow(),
            [
                PreferenceChange::SetMediaRetention(true),
                PreferenceChange::SetMediaRetentionDays(30)
            ]
        );
    }

    #[test]
    #[ignore = "requires a display; run with xvfb-run -a cargo test native_preferences::tests::dropped_preferences_dialog_releases_widget_tree -- --ignored"]
    fn dropped_preferences_dialog_releases_widget_tree() {
        use gtk4::prelude::*;

        gtk4::init().expect("GTK display");
        libadwaita::init().expect("libadwaita");
        let settings = Settings::default();
        let snapshot = NativePreferences::from(&settings);
        let dialog = NativePreferencesDialog::new(&snapshot, &[], || {});
        let weak = dialog.dialog().downgrade();

        drop(dialog);
        while gtk4::glib::MainContext::default().iteration(false) {}

        assert!(
            weak.upgrade().is_none(),
            "unpresented dialog must be released"
        );
    }
}
