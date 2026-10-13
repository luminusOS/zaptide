//! Icons drawn or installed by the application itself.

use super::*;

/// A funnel, which neither Adwaita nor GTK ships; a `-symbolic` name lets
/// GTK recolour it with the theme.
pub(super) const FILTER_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path d="M2.5 2h11a1 1 0 0 1 .78 1.63L10 8.98V13a1 1 0 0 1-.55.9l-2 1A1 1 0 0 1 6 14V8.98L1.72 3.63A1 1 0 0 1 2.5 2z" fill="#2e3436"/></svg>"##;

/// The app's symbolic icon, drawn in the tray. The names match the copies
/// the Flatpak exports to the host's icon theme.
pub(super) const TRAY_ICON: &str = include_str!("../../data/icons/zaptide-symbolic.svg");

/// The tray icon with a dot for unread chats. The `error` class lets hosts
/// recolour the dot like GTK does.
pub(super) const TRAY_UNREAD_ICON: &str =
    include_str!("../../data/icons/zaptide-unread-symbolic.svg");

/// The full-colour app icon, for the window and About dialog when the host
/// has no installed copy (AppImage, `cargo run`).
pub(super) const APP_ICON: &str = include_str!("../../data/icons/zaptide.svg");

pub(super) fn install_icons(dir: &std::path::Path) {
    let tray = [
        ("dev.luminusos.ZapTide-symbolic.svg", TRAY_ICON),
        (
            "dev.luminusos.ZapTide-unread-symbolic.svg",
            TRAY_UNREAD_ICON,
        ),
    ];
    let icons = [
        ("dev.luminusos.ZapTide.svg", APP_ICON),
        ("zaptide-filter-symbolic.svg", FILTER_ICON),
    ];
    write_icons(dir, tray.iter().chain(&icons));
    // Trays draw a symbolic icon at the panel's size and colour only when
    // the host theme knows its name; the Flatpak exports the tray icons, an
    // AppImage has to put them in the user's theme.
    if std::env::var_os("APPIMAGE").is_some() {
        let hicolor = gtk::glib::user_data_dir().join("icons/hicolor");
        // The colour icon goes in the theme too: with only the symbolic one
        // there, GTK resolves the app's name to it and About shows a bubble.
        let app = [("dev.luminusos.ZapTide.svg", APP_ICON)];
        let tray_changed = write_icons(&hicolor.join("symbolic/apps"), tray.iter());
        if write_icons(&hicolor.join("scalable/apps"), app.iter()) | tray_changed {
            // Theme caches rescan only when the theme directory changes.
            let _ = std::fs::File::open(&hicolor)
                .and_then(|theme| theme.set_modified(std::time::SystemTime::now()));
        }
    }
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(dir);
    }
}

/// Writes the icons that differ from their copy in `dir`; true if any did.
pub(super) fn write_icons<'a>(
    dir: &std::path::Path,
    icons: impl Iterator<Item = &'a (&'a str, &'a str)>,
) -> bool {
    let mut changed = false;
    if std::fs::create_dir_all(dir).is_ok() {
        for (name, svg) in icons {
            let icon = dir.join(name);
            if std::fs::read_to_string(&icon).ok().as_deref() != Some(*svg) {
                changed |= std::fs::write(&icon, svg).is_ok();
            }
        }
    }
    changed
}

/// A paper-plane send icon filled with the button's text color, so it follows
/// the theme like a symbolic icon.
pub(super) fn paper_plane_icon() -> gtk::DrawingArea {
    let icon = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    icon.set_draw_func(|area, cairo, _, _| {
        let color = area.color();
        cairo.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            color.alpha().into(),
        );
        for (index, (x, y)) in [
            (1.2, 1.6),
            (15.2, 8.0),
            (1.2, 14.4),
            (3.1, 8.8),
            (9.5, 8.0),
            (3.1, 7.2),
        ]
        .into_iter()
        .enumerate()
        {
            if index == 0 {
                cairo.move_to(x, y);
            } else {
                cairo.line_to(x, y);
            }
        }
        cairo.close_path();
        let _ = cairo.fill();
    });
    icon
}
