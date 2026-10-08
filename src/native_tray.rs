//! Tray icon: a StatusNotifierItem whose menu is exported over
//! `com.canonical.dbusmenu`, both served by ksni on its own thread.

/// What the tray asks the application to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// Clicking the icon shows or hides the window.
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

mod imp {
    use super::{TrayAction, TrayState, unread_label};
    use gtk4 as gtk;
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

        fn glyph(&self) -> &'static str {
            if self.state.unread_chats > 0 {
                "dev.luminusos.ZapTide-unread-symbolic"
            } else {
                "dev.luminusos.ZapTide-symbolic"
            }
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

        fn icon_name(&self) -> String {
            self.glyph().into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            // The name resolves through the host's icon theme, where the
            // Flatpak exports it and an AppImage installs it; when it is
            // missing there (`cargo run`), hosts draw these pixels instead.
            pixmap(&self.icon_dir, self.glyph())
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
            menu.extend([
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

    /// The installed symbolic icon as light ARGB32, since panels are dark and
    /// a pixmap cannot be recoloured by the host.
    fn pixmap(dir: &str, name: &str) -> Vec<ksni::Icon> {
        use gtk::{gdk, gdk::prelude::TextureExt, glib};
        // GTK's own loader (glycin) decodes the SVG; the bundled gdk-pixbuf
        // has no SVG loader in an AppImage.
        let render = || -> Option<ksni::Icon> {
            let svg = std::fs::read_to_string(format!("{dir}/{name}.svg"))
                .ok()?
                .replace("#2e3436", "#eeeeec")
                // Pad the glyph inside the 22px canvas so it matches the
                // visual size of other panel icons.
                .replacen(
                    r#"width="16" height="16" viewBox="0 0 16 16""#,
                    r#"width="22" height="22" viewBox="-2.25 -2.25 20.5 20.5""#,
                    1,
                );
            let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(svg.into_bytes()))
                .map_err(|error| log::warn!("tray pixmap: {error}"))
                .ok()?;
            let mut downloader = gdk::TextureDownloader::new(&texture);
            downloader.set_format(gdk::MemoryFormat::A8r8g8b8);
            let (bytes, stride) = downloader.download_bytes();
            let (width, height) = (texture.width() as usize, texture.height() as usize);
            let mut data = Vec::with_capacity(width * height * 4);
            for row in bytes.chunks(stride).take(height) {
                data.extend_from_slice(&row[..width * 4]);
            }
            Some(ksni::Icon {
                width: width as i32,
                height: height as i32,
                data,
            })
        };
        render().into_iter().collect()
    }

    pub struct TrayHandle(ksni::blocking::Handle<Tray>);

    impl TrayHandle {
        /// Starts the tray, or `None` when there is no session bus.
        pub fn spawn(
            icon_dir: &std::path::Path,
            state: TrayState,
            actions: relm4::Sender<TrayAction>,
        ) -> Option<Self> {
            // Flatpak cannot own the org.kde.StatusNotifierItem-* name.
            let sandboxed = std::env::var_os("FLATPAK_ID").is_some();
            let tray = Tray {
                state,
                icon_dir: icon_dir.to_string_lossy().into_owned(),
                actions,
            };
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
