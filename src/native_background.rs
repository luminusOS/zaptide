//! Background Apps portal (`org.freedesktop.portal.Background`).
//!
//! Lets the Flatpak keep running with its window closed on desktops without a
//! tray: GNOME lists the app under Background Apps and the user can quit it
//! from there.

use gtk::{gio, glib, prelude::*};
use relm4::gtk;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";

/// True inside a Flatpak, the only place the portal tracks the app.
pub fn sandboxed() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// Asks the portal to let the app run in the background. `done` receives
/// whether it was allowed; any failure counts as a refusal.
pub fn request(done: impl FnOnce(bool) + 'static) {
    glib::spawn_future_local(async move {
        let allowed = ask().await.unwrap_or_else(|error| {
            log::warn!("background portal unavailable: {error}");
            false
        });
        done(allowed);
    });
}

async fn ask() -> Result<bool, glib::Error> {
    let connection = gio::bus_get_future(gio::BusType::Session).await?;
    let me = connection
        .unique_name()
        .unwrap_or_default()
        .trim_start_matches(':')
        .replace('.', "_");
    let token = format!("zaptide{}", glib::random_int());
    // The portal answers on a request path derived from our name and token;
    // subscribe before calling so the reply cannot be missed.
    let path = format!("{PATH}/request/{me}/{token}");
    let (sender, receiver) = tokio::sync::oneshot::channel::<bool>();
    let sender = std::cell::RefCell::new(Some(sender));
    let subscription = connection.subscribe_to_signal(
        Some(PORTAL),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&path),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            let params = signal.parameters;
            let results = glib::VariantDict::new(Some(&params.child_value(1)));
            let granted = params.child_value(0).get::<u32>() == Some(0)
                && results.lookup::<bool>("background").ok().flatten() == Some(true);
            if let Some(sender) = sender.borrow_mut().take() {
                let _ = sender.send(granted);
            }
        },
    );
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", token.as_str());
    options.insert("reason", "Keep ZapTide running to receive messages");
    options.insert("autostart", false);
    options.insert("dbus-activatable", false);
    connection
        .call_future(
            Some(PORTAL),
            PATH,
            "org.freedesktop.portal.Background",
            "RequestBackground",
            Some(&("", options.end()).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
        )
        .await?;
    // The subscription unsubscribes when dropped.
    let granted = receiver.await.unwrap_or(false);
    drop(subscription);
    Ok(granted)
}
