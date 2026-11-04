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
    use ashpd::desktop::settings::{ColorScheme, Settings};
    use futures_util::StreamExt;
    use gtk::glib;
    use gtk4 as gtk;
    use ksni::blocking::TrayMethods;
    use ksni::menu::{CheckmarkItem, StandardItem};
    use ksni::{MenuItem, OfflineReason, ToolTip};
    use std::rc::Rc;

    struct Tray {
        state: TrayState,
        light_theme: bool,
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
            pixmap(&self.icon_dir, self.glyph(), self.light_theme)
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

    /// The installed symbolic icon as ARGB32, light on a dark theme and dark
    /// on a light one, since a pixmap cannot be recoloured by the host.
    fn pixmap(dir: &str, name: &str, light_theme: bool) -> Vec<ksni::Icon> {
        use gtk::{gdk, gdk::prelude::TextureExt, glib};
        // GTK's own loader (glycin) decodes the SVG; the bundled gdk-pixbuf
        // has no SVG loader in an AppImage.
        let render = || -> Option<ksni::Icon> {
            let svg = std::fs::read_to_string(format!("{dir}/{name}.svg"))
                .ok()?
                .replace("#2e3436", if light_theme { "#2e3436" } else { "#eeeeec" })
                // The SVG already keeps its own margin; hosts scale 22px down.
                .replacen(r#"width="16" height="16""#, r#"width="22" height="22""#, 1);
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

    pub(super) fn light_theme(scheme: ColorScheme) -> bool {
        scheme != ColorScheme::PreferDark
    }

    pub(super) async fn forward_color_scheme(
        light_themes: tokio::sync::mpsc::UnboundedSender<bool>,
    ) {
        let settings = match Settings::new().await {
            Ok(settings) => settings,
            Err(error) => {
                log::debug!("desktop color scheme unavailable: {error}");
                return;
            }
        };
        // Subscribed before the first read, so a change that lands while the
        // read is in flight is delivered after it and wins.
        let mut changes = match settings.receive_color_scheme_changed().await {
            Ok(changes) => Box::pin(changes),
            Err(error) => {
                log::debug!("desktop color scheme unavailable: {error}");
                return;
            }
        };
        match settings.color_scheme().await {
            Ok(scheme) => {
                let _ = light_themes.send(light_theme(scheme));
            }
            Err(error) => log::debug!("desktop color scheme unavailable: {error}"),
        }
        loop {
            tokio::select! {
                () = light_themes.closed() => return,
                change = changes.next() => match change {
                    Some(scheme) => {
                        let _ = light_themes.send(light_theme(scheme));
                    }
                    None => return,
                },
            }
        }
    }

    fn watch_desktop_theme(handle: Rc<ksni::blocking::Handle<Tray>>) -> DesktopTheme {
        let (light_themes, mut changes) = tokio::sync::mpsc::unbounded_channel();
        crate::portal::spawn(forward_color_scheme(light_themes));
        DesktopTheme(glib::spawn_future_local(async move {
            while let Some(light_theme) = changes.recv().await {
                handle.update(move |tray| tray.light_theme = light_theme);
            }
        }))
    }

    /// Follows the desktop's color scheme until dropped.
    pub struct DesktopTheme(glib::JoinHandle<()>);

    impl Drop for DesktopTheme {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    pub struct TrayHandle {
        handle: Rc<ksni::blocking::Handle<Tray>>,
        _desktop_theme: DesktopTheme,
    }

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
                light_theme: true,
                icon_dir: icon_dir.to_string_lossy().into_owned(),
                actions,
            };
            match tray
                .disable_dbus_name(sandboxed)
                .assume_sni_available(true)
                .spawn()
            {
                Ok(handle) => {
                    let handle = Rc::new(handle);
                    let desktop_theme = watch_desktop_theme(handle.clone());
                    Some(Self {
                        handle,
                        _desktop_theme: desktop_theme,
                    })
                }
                Err(error) => {
                    log::warn!("tray icon unavailable: {error}");
                    None
                }
            }
        }

        pub fn update(&self, state: TrayState) {
            self.handle.update(move |tray| tray.state = state);
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn normal_and_unread_pixmaps_have_contrast_in_both_desktop_schemes() {
            let directory = tempfile::tempdir().unwrap();
            for (name, svg) in [
                ("normal", include_str!("../data/icons/zaptide-symbolic.svg")),
                (
                    "unread",
                    include_str!("../data/icons/zaptide-unread-symbolic.svg"),
                ),
            ] {
                std::fs::write(directory.path().join(format!("{name}.svg")), svg).unwrap();
                for (light_theme, foreground) in
                    [(true, [0x2e, 0x34, 0x36]), (false, [0xee, 0xee, 0xec])]
                {
                    let icons =
                        super::pixmap(directory.path().to_str().unwrap(), name, light_theme);
                    assert_eq!(icons.len(), 1, "{name}, light={light_theme}");
                    let icon = &icons[0];
                    assert_eq!((icon.width, icon.height), (22, 22));
                    assert_eq!(icon.data.len(), 22 * 22 * 4);
                    assert!(
                        icon.data
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .any(|pixel| pixel[0] == 255 && pixel[1..] == foreground),
                        "{name}, light={light_theme}"
                    );
                }
            }
        }
    }
}

pub use imp::TrayHandle;

#[cfg(test)]
mod tests {
    use ashpd::desktop::settings::ColorScheme;
    use ashpd::zvariant::{OwnedValue, Value};

    #[test]
    fn desktop_color_scheme_controls_tray_contrast() {
        for (scheme, light) in [(0_u32, true), (1, false), (2, true), (99, true)] {
            let value = OwnedValue::try_from(Value::from(scheme)).expect("a plain value");
            let scheme = ColorScheme::try_from(value).expect("a known or unknown scheme");
            assert_eq!(super::imp::light_theme(scheme), light);
        }
    }

    #[test]
    fn a_malformed_color_scheme_does_not_change_tray_contrast() {
        let value = OwnedValue::try_from(Value::from("dark")).expect("a plain value");
        assert!(ColorScheme::try_from(value).is_err());
    }

    #[test]
    #[ignore = "talks to the session's settings portal"]
    fn the_session_portal_delivers_the_current_color_scheme() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        crate::portal::spawn(super::imp::forward_color_scheme(sender));
        let first = crate::portal::run(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv()).await
        })
        .expect("the portal runtime finishes");
        match first {
            Ok(Some(light)) => eprintln!("the portal delivered a scheme: light={light}"),
            Ok(None) | Err(_) => eprintln!("skipped: the session has no settings portal"),
        }
    }

    #[test]
    fn unread_label_counts_chats() {
        assert_eq!(super::unread_label(0), None);
        assert_eq!(super::unread_label(1).as_deref(), Some("1 unread chat"));
        assert_eq!(super::unread_label(4).as_deref(), Some("4 unread chats"));
    }
}
