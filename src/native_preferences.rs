//! GTK-independent preferences model for native settings surfaces.
//!
//! This module deliberately models only controls persisted by [`Settings`].
//! Account identity and archive/cache paths are runtime or path data, not
//! preferences, so they are not invented here. Secret values are write-only:
//! snapshots expose whether they are configured, never their contents.

use crate::i18n::gettext;
use crate::settings::{Settings, ThemeChoice};

/// Existing JSON field that persists a control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsField {
    Theme,
    CustomTheme,
    Zoom,
    EnterSends,
    SendReadReceipts,
    SendTyping,
    AutoDownload,
    ShowSenderPictures,
    NamesFromContacts,
    SaveContactsToPhone,
    ShowShortcutHints,
    TranslucentSidebar,
    ChatLockCodeHash,
    Notifications,
    NotificationPreviews,
    KeepRunningInBackground,
    VoiceSpeed,
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
    pub chat_lock_configured: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationPreferences {
    pub enabled: bool,
    pub previews: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoragePreferences {
    pub auto_download: bool,
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
                chat_lock_configured: settings.chat_lock_code_hash.is_some(),
            },
            notifications: NotificationPreferences {
                enabled: settings.notifications,
                previews: settings.notification_previews,
            },
            storage: StoragePreferences {
                auto_download: settings.auto_download,
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
    SetTheme(ThemeChoice),
    SetCustomTheme(Option<String>),
    SetZoom(f32),
    SetEnterSends(bool),
    SetSendReadReceipts(bool),
    SetSendTyping(bool),
    SetAutoDownload(bool),
    SetShowSenderPictures(bool),
    SetNamesFromContacts(bool),
    SetSaveContactsToPhone(bool),
    SetShowShortcutHints(bool),
    SetTranslucentSidebar(bool),
    SetChatLockCode(Option<String>),
    SetNotifications(bool),
    SetNotificationPreviews(bool),
    SetKeepRunningInBackground(bool),
    SetVoiceSpeed(f32),
}

impl std::fmt::Debug for PreferenceChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use PreferenceChange as Change;

        match self {
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
            Change::SetKeepRunningInBackground(value) => formatter
                .debug_tuple("SetKeepRunningInBackground")
                .field(value)
                .finish(),
            Change::SetVoiceSpeed(value) => {
                formatter.debug_tuple("SetVoiceSpeed").field(value).finish()
            }
        }
    }
}

impl PreferenceChange {
    /// Existing JSON field changed by this request. Secret values are never returned.
    pub const fn settings_field(&self) -> SettingsField {
        match self {
            Self::SetTheme(_) => SettingsField::Theme,
            Self::SetCustomTheme(_) => SettingsField::CustomTheme,
            Self::SetZoom(_) => SettingsField::Zoom,
            Self::SetEnterSends(_) => SettingsField::EnterSends,
            Self::SetSendReadReceipts(_) => SettingsField::SendReadReceipts,
            Self::SetSendTyping(_) => SettingsField::SendTyping,
            Self::SetAutoDownload(_) => SettingsField::AutoDownload,
            Self::SetShowSenderPictures(_) => SettingsField::ShowSenderPictures,
            Self::SetNamesFromContacts(_) => SettingsField::NamesFromContacts,
            Self::SetSaveContactsToPhone(_) => SettingsField::SaveContactsToPhone,
            Self::SetShowShortcutHints(_) => SettingsField::ShowShortcutHints,
            Self::SetTranslucentSidebar(_) => SettingsField::TranslucentSidebar,
            Self::SetChatLockCode(_) => SettingsField::ChatLockCodeHash,
            Self::SetNotifications(_) => SettingsField::Notifications,
            Self::SetNotificationPreviews(_) => SettingsField::NotificationPreviews,
            Self::SetKeepRunningInBackground(_) => SettingsField::KeepRunningInBackground,
            Self::SetVoiceSpeed(_) => SettingsField::VoiceSpeed,
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
    changes: std::rc::Rc<std::cell::RefCell<Vec<PreferenceChange>>>,
    errors: std::rc::Rc<std::cell::RefCell<Vec<ValidationError>>>,
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
        dialog.set_title("Preferences");
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
        appearance.set_title("Appearance");
        appearance.set_icon_name(Some("preferences-desktop-appearance-symbolic"));
        let appearance_group = libadwaita::PreferencesGroup::new();
        appearance_group.set_title("Display and input");
        appearance.add(&appearance_group);
        dialog.add(&appearance);

        let theme_row = libadwaita::ComboRow::new();
        theme_row.set_title("Theme");
        theme_row.set_subtitle("Choose how ZapTide looks");
        let themes = gtk4::StringList::new(&["Follow system", "Light", "Dark"]);
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
        custom_theme_row.set_title("Custom palette");
        custom_theme_row.set_subtitle("Choose a palette from the themes folder");
        let custom_theme_choices =
            std::rc::Rc::new(std::cell::RefCell::new(theme_choices.to_vec()));
        let mut custom_theme_labels = vec!["System palette".to_owned()];
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
        set_accessible_label(&open_themes_folder, "Open themes folder");
        custom_theme_row.add_suffix(&open_themes_folder);

        add_numeric_row(
            &appearance_group,
            "Zoom",
            "UI scale, from 0.6× to 2.0×",
            (f64::from(preferences.appearance.zoom), 0.6, 2.0, 0.05, 2),
            change_sink.clone(),
            PreferenceChange::SetZoom,
        );
        add_switch(
            &appearance_group,
            "Send messages with Enter",
            "Shift+Enter inserts a new line",
            preferences.appearance.enter_sends,
            change_sink.clone(),
            PreferenceChange::SetEnterSends,
        );
        add_switch(
            &appearance_group,
            "Show sender pictures",
            "Show avatars next to messages",
            preferences.appearance.show_sender_pictures,
            change_sink.clone(),
            PreferenceChange::SetShowSenderPictures,
        );
        add_switch(
            &appearance_group,
            "Show shortcut hints",
            "Show keyboard shortcut hints",
            preferences.appearance.show_shortcut_hints,
            change_sink.clone(),
            PreferenceChange::SetShowShortcutHints,
        );
        add_switch(
            &appearance_group,
            "Translucent sidebar",
            "Blur the desktop behind the chat list. Needs a compositor that supports background blur",
            preferences.appearance.translucent_sidebar,
            change_sink.clone(),
            PreferenceChange::SetTranslucentSidebar,
        );

        let account_page = libadwaita::PreferencesPage::new();
        account_page.set_title("Account");
        account_page.set_icon_name(Some("avatar-default-symbolic"));
        let account_group = libadwaita::PreferencesGroup::new();
        account_group.set_title("Contacts");
        account_page.add(&account_group);
        dialog.add(&account_page);
        add_switch(
            &account_group,
            "Prefer contact names",
            "Use address-book names instead of profile names",
            preferences.account.names_from_contacts,
            change_sink.clone(),
            PreferenceChange::SetNamesFromContacts,
        );
        add_switch(
            &account_group,
            "Save contacts to phone",
            "Also add saved contacts to the device address book",
            preferences.account.save_contacts_to_phone,
            change_sink.clone(),
            PreferenceChange::SetSaveContactsToPhone,
        );

        let notifications_page = libadwaita::PreferencesPage::new();
        notifications_page.set_title("Notifications");
        notifications_page.set_icon_name(Some("preferences-system-notifications-symbolic"));
        let notifications_group = libadwaita::PreferencesGroup::new();
        notifications_group.set_title("Notifications");
        notifications_page.add(&notifications_group);
        dialog.add(&notifications_page);
        add_switch(
            &notifications_group,
            "Desktop notifications",
            "Notify when messages arrive while away",
            preferences.notifications.enabled,
            change_sink.clone(),
            PreferenceChange::SetNotifications,
        );
        add_switch(
            &notifications_group,
            "Show previews",
            "Include the sender and message text",
            preferences.notifications.previews,
            change_sink.clone(),
            PreferenceChange::SetNotificationPreviews,
        );

        let storage_page = libadwaita::PreferencesPage::new();
        storage_page.set_title("Storage");
        storage_page.set_icon_name(Some("drive-harddisk-symbolic"));
        let storage_group = libadwaita::PreferencesGroup::new();
        storage_group.set_title("Downloads");
        storage_page.add(&storage_group);
        dialog.add(&storage_page);
        add_switch(
            &storage_group,
            "Automatically download attachments",
            "Download attachments when they enter view",
            preferences.storage.auto_download,
            change_sink.clone(),
            PreferenceChange::SetAutoDownload,
        );

        let background_page = libadwaita::PreferencesPage::new();
        background_page.set_title("Background");
        background_page.set_icon_name(Some("preferences-system-time-symbolic"));
        let background_group = libadwaita::PreferencesGroup::new();
        background_group.set_title("Background and updates");
        background_page.add(&background_group);
        dialog.add(&background_page);
        add_switch(
            &background_group,
            "Keep running in background",
            "Keep ZapTide available after closing its window",
            preferences.background.keep_running,
            change_sink.clone(),
            PreferenceChange::SetKeepRunningInBackground,
        );

        let protocol_page = libadwaita::PreferencesPage::new();
        protocol_page.set_title("Messaging");
        protocol_page.set_icon_name(Some("mail-send-symbolic"));
        let protocol_group = libadwaita::PreferencesGroup::new();
        protocol_group.set_title("Messaging and media");
        protocol_page.add(&protocol_group);
        dialog.add(&protocol_page);
        let voice_row = libadwaita::ComboRow::new();
        voice_row.set_title("Voice playback speed");
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
            changes,
            errors,
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
        let mut labels = vec!["System palette".to_owned()];
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
        _ => None,
    };
    if let Some(error) = validation {
        errors.borrow_mut().push(error);
    } else {
        changes.borrow_mut().push(change);
    }
    on_change();
}

fn add_switch(
    group: &libadwaita::PreferencesGroup,
    title: &str,
    subtitle: &str,
    active: bool,
    change_sink: PreferenceChangeSink,
    make_change: fn(bool) -> PreferenceChange,
) {
    use libadwaita::prelude::*;
    let row = libadwaita::SwitchRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_active(active);
    row.connect_active_notify(move |row| change_sink.push(make_change(row.is_active())));
    group.add(&row);
}

pub(crate) fn set_accessible_label(
    widget: &impl gtk4::prelude::IsA<gtk4::Accessible>,
    label: &str,
) {
    use gtk4::prelude::*;
    widget.update_property(&[gtk4::accessible::Property::Label(label)]);
}

pub(crate) fn present_secret_entry(
    parent: &impl gtk4::prelude::IsA<gtk4::Widget>,
    heading: &str,
    body: &str,
    accessible_name: &str,
    on_save: impl Fn(String) + 'static,
) {
    use gtk4::prelude::*;
    use libadwaita::prelude::*;

    let entry = gtk4::PasswordEntry::new();
    entry.set_show_peek_icon(false);
    set_accessible_label(&entry, accessible_name);
    let alert = libadwaita::AlertDialog::new(Some(heading), Some(body));
    alert.set_extra_child(Some(&entry));
    alert.set_focus(Some(&entry));
    alert.add_response("cancel", &gettext("Cancel"));
    alert.add_response("save", &gettext("Save"));
    alert.set_close_response("cancel");
    alert.set_default_response(Some("save"));
    alert.set_response_enabled("save", false);
    {
        let alert = alert.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(alert) = alert.upgrade() {
                alert.set_response_enabled("save", !entry.text().is_empty());
            }
        });
    }
    let entry_ref = entry.downgrade();
    alert.connect_response(None, move |_, response| {
        let value = entry_ref
            .upgrade()
            .and_then(|entry| secret_value_for_response(response, entry.text().as_str()));
        if let Some(entry) = entry_ref.upgrade() {
            entry.set_text("");
        }
        if let Some(value) = value {
            on_save(value);
        }
    });
    alert.present(Some(parent));
}

fn secret_value_for_response(response: &str, value: &str) -> Option<String> {
    (response == "save" && !value.is_empty()).then(|| value.to_owned())
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
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::ZoomOutOfRange => "zoom must be finite and between 0.6 and 2.0",
            Self::InvalidCustomThemeFilename => "custom theme must be a local filename",
            Self::UnsupportedVoiceSpeed => "voice speed must be 1.0, 1.5, or 2.0",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ValidationError {}

impl PreferenceChange {
    /// Validate and apply one change. Invalid values leave `settings` untouched.
    pub fn apply(self, settings: &mut Settings) -> Result<(), ValidationError> {
        match self {
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
            Self::SetAutoDownload(value) => settings.auto_download = value,
            Self::SetShowSenderPictures(value) => settings.show_sender_pictures = value,
            Self::SetNamesFromContacts(value) => settings.names_from_contacts = value,
            Self::SetSaveContactsToPhone(value) => settings.save_contacts_to_phone = value,
            Self::SetShowShortcutHints(value) => settings.show_shortcut_hints = value,
            Self::SetTranslucentSidebar(value) => settings.translucent_sidebar = value,
            Self::SetChatLockCode(value) => settings.set_chat_lock_code(value.as_deref()),
            Self::SetNotifications(value) => settings.notifications = value,
            Self::SetNotificationPreviews(value) => settings.notification_previews = value,
            Self::SetKeepRunningInBackground(value) => settings.keep_running_in_background = value,
            Self::SetVoiceSpeed(value) => {
                if ![1.0, 1.5, 2.0].contains(&value) {
                    return Err(ValidationError::UnsupportedVoiceSpeed);
                }
                settings.voice_speed = value;
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
    fn secret_dialog_requires_save_and_rejects_empty_lock_codes() {
        assert_eq!(secret_value_for_response("cancel", "private"), None);
        assert_eq!(secret_value_for_response("close", "private"), None);
        assert_eq!(secret_value_for_response("save", ""), None);
        assert_eq!(
            secret_value_for_response("save", "private").as_deref(),
            Some("private")
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
