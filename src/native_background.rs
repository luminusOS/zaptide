//! Background Apps portal (`org.freedesktop.portal.Background`).
//!
//! A Flatpak is tracked as a background app implicitly while it runs, so it
//! can keep running with its window closed on desktops without a tray; GNOME
//! lists it under Background Apps, where the user can quit it. The portal also
//! shows a one-line status, set here.

use gtk::{gio, glib, prelude::*};
use relm4::gtk;

/// True inside a Flatpak, the only place the portal tracks the app.
pub fn sandboxed() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// Sets the status line shown next to the app in Background Apps.
pub fn set_status(message: &str) {
    let options = glib::VariantDict::new(None);
    options.insert("message", message);
    let args = (options.end(),).to_variant();
    glib::spawn_future_local(async move {
        let call = async {
            gio::bus_get_future(gio::BusType::Session)
                .await?
                .call_future(
                    Some("org.freedesktop.portal.Desktop"),
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.Background",
                    "SetStatus",
                    Some(&args),
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                )
                .await
        };
        if let Err(error) = call.await {
            log::debug!("background status not set: {error}");
        }
    });
}
