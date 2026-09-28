//! Tray icon: a StatusNotifierItem whose menu is exported over
//! `com.canonical.dbusmenu`, both served by ksni on its own thread.

/// What the tray asks the application to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ToggleWindow,
    NewChat,
    ToggleNotifications,
    Preferences,
    Quit,
    /// Whether a tray host (KDE, or GNOME's AppIndicator extension) shows it.
    Shown(bool),
}

/// Application state the tray reflects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayState {
    pub window_visible: bool,
    pub unread_chats: usize,
    pub notifications: bool,
}

/// "1 unread chat", "3 unread chats"; nothing when all are read.
pub fn unread_label(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some("1 unread chat".into()),
        count => Some(format!("{count} unread chats")),
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{TrayAction, TrayState, unread_label};
    use ksni::blocking::TrayMethods;
    use ksni::menu::{CheckmarkItem, StandardItem};
    use ksni::{MenuItem, OfflineReason, ToolTip};

    struct Tray {
        state: TrayState,
        icon_dir: String,
        actions: relm4::Sender<TrayAction>,
    }

    impl Tray {
        fn item(label: &str, icon: &str, action: TrayAction) -> MenuItem<Self> {
            StandardItem {
                label: label.into(),
                icon_name: icon.into(),
                activate: Box::new(move |tray: &mut Self| tray.actions.emit(action)),
                ..Default::default()
            }
            .into()
        }
    }

    impl ksni::Tray for Tray {
        fn id(&self) -> String {
            "zaptide".into()
        }

        fn title(&self) -> String {
            "ZapTide".into()
        }

        fn category(&self) -> ksni::Category {
            ksni::Category::Communications
        }

        fn icon_theme_path(&self) -> String {
            self.icon_dir.clone()
        }

        fn icon_name(&self) -> String {
            if self.state.unread_chats > 0 {
                "zaptide-tray-unread-symbolic".into()
            } else {
                "zaptide-tray-symbolic".into()
            }
        }

        fn tool_tip(&self) -> ToolTip {
            ToolTip {
                title: "ZapTide".into(),
                description: unread_label(self.state.unread_chats).unwrap_or_default(),
                ..Default::default()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.actions.emit(TrayAction::ToggleWindow);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let mut menu = Vec::new();
            if let Some(unread) = unread_label(self.state.unread_chats) {
                menu.push(
                    StandardItem {
                        label: unread,
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
                menu.push(MenuItem::Separator);
            }
            let window = if self.state.window_visible {
                "Hide ZapTide"
            } else {
                "Show ZapTide"
            };
            menu.extend([
                Self::item(window, "", TrayAction::ToggleWindow),
                Self::item(
                    "New Chat…",
                    "chat-message-new-symbolic",
                    TrayAction::NewChat,
                ),
                MenuItem::Separator,
                CheckmarkItem {
                    label: "Notifications".into(),
                    checked: self.state.notifications,
                    activate: Box::new(|tray: &mut Self| {
                        tray.actions.emit(TrayAction::ToggleNotifications)
                    }),
                    ..Default::default()
                }
                .into(),
                Self::item(
                    "Preferences",
                    "preferences-system-symbolic",
                    TrayAction::Preferences,
                ),
                MenuItem::Separator,
                Self::item(
                    "Quit ZapTide",
                    "application-exit-symbolic",
                    TrayAction::Quit,
                ),
            ]);
            menu
        }

        fn watcher_online(&self) {
            self.actions.emit(TrayAction::Shown(true));
        }

        fn watcher_offline(&self, _reason: OfflineReason) -> bool {
            // Keep serving: the host may come back, as when GNOME's
            // extension is switched off and on again.
            self.actions.emit(TrayAction::Shown(false));
            true
        }
    }

    pub struct TrayHandle(ksni::blocking::Handle<Tray>);

    impl TrayHandle {
        /// Starts the tray, or `None` when there is no session bus.
        pub fn spawn(
            icon_dir: &std::path::Path,
            state: TrayState,
            actions: relm4::Sender<TrayAction>,
        ) -> Option<Self> {
            let tray = Tray {
                state,
                icon_dir: icon_dir.to_string_lossy().into_owned(),
                actions,
            };
            // Flatpak cannot own the org.kde.StatusNotifierItem-* name.
            let sandboxed = std::env::var_os("FLATPAK_ID").is_some();
            match tray
                .disable_dbus_name(sandboxed)
                .assume_sni_available(true)
                .spawn()
            {
                Ok(handle) => Some(Self(handle)),
                Err(error) => {
                    log::warn!("tray icon unavailable: {error}");
                    None
                }
            }
        }

        pub fn update(&self, state: TrayState) {
            self.0.update(move |tray| tray.state = state);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{TrayAction, TrayState};

    /// No tray outside Linux yet.
    pub struct TrayHandle;

    impl TrayHandle {
        pub fn spawn(
            _icon_dir: &std::path::Path,
            _state: TrayState,
            _actions: relm4::Sender<TrayAction>,
        ) -> Option<Self> {
            None
        }

        pub fn update(&self, _state: TrayState) {}
    }
}

pub use imp::TrayHandle;

#[cfg(test)]
mod tests {
    #[test]
    fn unread_label_counts_chats() {
        assert_eq!(super::unread_label(0), None);
        assert_eq!(super::unread_label(1).as_deref(), Some("1 unread chat"));
        assert_eq!(super::unread_label(4).as_deref(), Some("4 unread chats"));
    }
}
