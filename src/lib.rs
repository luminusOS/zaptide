// GNOME desktop client: GTK4, libadwaita, D-Bus, GStreamer, and the
// Secret Service keyring are assumed throughout.
#[cfg(not(target_os = "linux"))]
compile_error!("ZapTide supports Linux only.");

/// Builds a value of a `#[non_exhaustive]` type, such as a protobuf message or a
/// whatsapp-rust options struct, from its `Default` and the given fields.
/// Struct expressions cannot name such types outside their crate. Unlike a
/// struct expression, an omitted field silently keeps its default.
macro_rules! from_default {
    (@value $field:ident $value:expr) => {
        $value
    };
    (@value $field:ident) => {
        $field
    };
    ($type:path { $($field:ident $(: $value:expr)?),* $(,)? }) => {{
        let mut value = <$type>::default();
        $(value.$field = from_default!(@value $field $($value)?);)*
        value
    }};
}

pub(crate) mod account;
pub mod application;
pub(crate) mod archive;
pub mod audio;
pub mod backend;
pub mod calls;
pub(crate) mod color;
pub(crate) mod contact_cards;
pub(crate) mod countries;
pub(crate) mod event_drain;
pub(crate) mod glib_notifier;
pub mod i18n;
pub(crate) mod link_preview;
pub mod logging;
pub(crate) mod media_retention;
pub(crate) mod message_window;
pub(crate) mod model;
pub(crate) mod native_actions;
pub(crate) mod native_attachments;
pub(crate) mod native_background;
pub mod native_chat_list;
pub(crate) mod native_composer;
pub(crate) mod native_emoji;
pub mod native_media;
pub(crate) mod native_media_widgets;
pub(crate) mod native_notifications;
pub mod native_portals;
pub mod native_preferences;
pub(crate) mod native_theme;
pub(crate) mod native_transcript;
pub(crate) mod native_tray;
pub(crate) mod native_voice;
pub(crate) mod notification_sound;
pub(crate) mod notifier;
pub mod paths;
pub(crate) mod portal;
pub(crate) mod safety;
pub mod services;
pub mod settings;
mod shared_resource;
pub(crate) mod theme;
pub(crate) mod util;
pub mod voice;
