// GNOME desktop client: GTK4, libadwaita, D-Bus, GStreamer, and the
// Secret Service keyring are assumed throughout.
#[cfg(not(target_os = "linux"))]
compile_error!("ZapTide supports Linux only.");

pub mod application;
pub mod archive;
pub mod audio;
pub mod backend;
pub mod color;
pub mod contact_cards;
pub mod countries;
pub mod event_drain;
pub mod glib_notifier;
pub mod message_window;
pub mod model;
pub mod native_actions;
pub mod native_attachments;
pub mod native_chat_list;
pub mod native_composer;
pub mod native_emoji;
pub mod native_media;
pub mod native_media_widgets;
pub mod native_notifications;
pub mod native_portals;
pub mod native_preferences;
pub mod native_theme;
pub mod native_transcript;
pub mod native_tray;
pub mod native_voice;
pub mod notifier;
pub mod paths;
pub mod safety;
pub mod services;
pub mod settings;
pub mod sticker_meta;
pub mod theme;
pub mod timestretch;
pub mod util;
pub mod voice;
