//! Background Apps portal (`org.freedesktop.portal.Background`).
//!
//! A Flatpak is tracked as a background app implicitly while it runs, so it
//! can keep running with its window closed on desktops without a tray; GNOME
//! lists it under Background Apps, where the user can quit it. The portal also
//! shows a one-line status, set here.

use ashpd::desktop::background::{BackgroundProxy, SetStatusOptions};

/// True inside a Flatpak, the only place the portal tracks the app.
pub fn sandboxed() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

fn status_options(message: &str) -> SetStatusOptions {
    SetStatusOptions::default().set_message(message)
}

/// Sets the status line shown next to the app in Background Apps.
pub fn set_status(message: &str) {
    let options = status_options(message);
    crate::portal::spawn(async move {
        let result = async { BackgroundProxy::new().await?.set_status(options).await }.await;
        if let Err(error) = result {
            log::debug!("background status not set: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ashpd::zvariant::serialized::Context;
    use ashpd::zvariant::{LE, Type, Value, to_bytes};
    use std::collections::HashMap;

    #[test]
    fn the_status_is_sent_as_a_vardict_not_a_boxed_variant() {
        assert_eq!(SetStatusOptions::SIGNATURE.to_string(), "a{sv}");
        let bytes = to_bytes(Context::new_dbus(LE, 0), &status_options("3 unread chats"))
            .expect("the options serialize");
        let (options, _): (HashMap<String, Value>, _) =
            bytes.deserialize().expect("the body is an a{sv}");
        assert_eq!(options.get("message"), Some(&Value::from("3 unread chats")));
    }

    #[test]
    #[ignore = "talks to the session's background portal"]
    fn the_session_portal_accepts_a_status() {
        let outcome = crate::portal::run(async {
            BackgroundProxy::new()
                .await?
                .set_status(status_options("No unread chats"))
                .await
        })
        .expect("the portal runtime finishes");
        eprintln!("background status outcome: {outcome:?}");
    }
}
