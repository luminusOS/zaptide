// GNOME desktop client: GTK4, libadwaita, D-Bus, GStreamer, and the
// Secret Service keyring are assumed throughout.
#[cfg(not(target_os = "linux"))]
compile_error!("ZapTide supports Linux only.");

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
pub(crate) mod media_retention;
pub(crate) mod message_window;
pub(crate) mod model;
pub(crate) mod native_actions;
pub(crate) mod native_attachments;
pub(crate) mod native_background;
pub mod native_chat_list;
pub(crate) mod native_composer;
pub(crate) mod native_emoji;
pub(crate) mod native_media;
pub(crate) mod native_media_widgets;
pub(crate) mod native_notifications;
pub mod native_portals;
pub mod native_preferences;
pub(crate) mod native_theme;
pub(crate) mod native_transcript;
pub(crate) mod native_tray;
pub(crate) mod native_voice;
pub(crate) mod notifier;
pub mod paths;
pub(crate) mod portal;
pub(crate) mod safety;
pub(crate) mod services;
pub mod settings;
pub(crate) mod sticker_meta;
pub(crate) mod theme;
pub(crate) mod timestretch;
pub(crate) mod util;
pub mod voice;
