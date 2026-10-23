//! Translations through gettext. Strings are looked up in the `zaptide` domain.

pub use gettextrs::gettext;

const DOMAIN: &str = "zaptide";

/// Binds the message catalogue. `ZAPTIDE_LOCALEDIR` overrides the install
/// location, which is useful for a build that has not been installed.
///
/// Call it once, before the program starts any other thread.
pub fn init() {
    // SAFETY: setlocale is not thread-safe, and the contract above keeps this
    // the only thread running.
    let _ = unsafe { gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "") };
    let dir = std::env::var_os("ZAPTIDE_LOCALEDIR")
        .map(std::path::PathBuf::from)
        .or_else(|| option_env!("ZAPTIDE_LOCALEDIR").map(std::path::PathBuf::from))
        .unwrap_or_else(|| {
            // A Flatpak installs under /app instead of /usr.
            if std::env::var_os("FLATPAK_ID").is_some() {
                "/app/share/locale".into()
            } else {
                "/usr/share/locale".into()
            }
        });
    let _ = gettextrs::bindtextdomain(DOMAIN, dir);
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
}
