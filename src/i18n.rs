//! Translations through gettext. Strings are looked up in the `zaptide` domain.

use std::{path::Path, sync::OnceLock};

use ::gettext::Catalog;

use crate::settings::Language;

// System default keeps GNU gettext's locale negotiation. An explicit choice
// reads only ZapTide's catalogue, even on hosts with only the C locale.
static SELECTED: OnceLock<Catalog> = OnceLock::new();

/// Translates a ZapTide string using the interface language chosen at startup.
pub fn gettext(message: impl AsRef<str>) -> String {
    let message = message.as_ref();
    match SELECTED.get() {
        Some(catalog) => catalog.gettext(message).to_owned(),
        None => gettextrs::gettext(message),
    }
}

const DOMAIN: &str = "zaptide";

/// Binds the message catalogue. `ZAPTIDE_LOCALEDIR` overrides the install
/// location, which is useful for a build that has not been installed.
///
/// Call it once, before the program starts any other thread.
pub fn init() {
    init_for_language(Language::System);
}

/// Initializes the shared interface language before starting GTK or worker threads.
pub fn init_for_language(language: Language) {
    // SAFETY: setlocale is not thread-safe, and the contract above keeps this
    // the only thread running.
    let _ = unsafe { gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "") };
    let dir = std::env::var_os("ZAPTIDE_LOCALEDIR")
        .map(std::path::PathBuf::from)
        .or_else(|| option_env!("ZAPTIDE_LOCALEDIR").map(std::path::PathBuf::from))
        .or_else(|| {
            std::env::var_os("APPDIR")
                .map(|dir| std::path::PathBuf::from(dir).join("usr/share/locale"))
        })
        .unwrap_or_else(|| {
            // A Flatpak installs under /app instead of /usr.
            if crate::portal::sandboxed() {
                "/app/share/locale".into()
            } else {
                "/usr/share/locale".into()
            }
        });
    match selected_catalog(&dir, language) {
        Ok(Some(catalog)) => {
            let _ = SELECTED.set(catalog);
        }
        Ok(None) => {}
        Err(error) => log::warn!("could not load the selected interface catalogue: {error}"),
    }
    let _ = gettextrs::bindtextdomain(DOMAIN, dir);
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
}

fn selected_catalog(dir: &Path, language: Language) -> std::io::Result<Option<Catalog>> {
    let code = match language {
        Language::System => return Ok(None),
        Language::English => return Ok(Some(Catalog::empty())),
        Language::Spanish => "es",
        Language::PortugueseBrazil => "pt_BR",
    };
    let file = std::fs::File::open(dir.join(code).join("LC_MESSAGES/zaptide.mo"))?;
    Catalog::parse(file)
        .map(Some)
        .map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "compile the catalogues with just locale first"]
    fn explicit_languages_use_their_own_catalogues() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/locale");
        assert!(selected_catalog(&dir, Language::System).unwrap().is_none());
        let source = "Forward…";
        for (language, label) in [
            (Language::Spanish, "Reenviar…"),
            (Language::PortugueseBrazil, "Encaminhar…"),
            (Language::English, "Forward…"),
        ] {
            let catalog = selected_catalog(&dir, language).unwrap().unwrap();
            assert_eq!(catalog.gettext(source), label);
        }
    }
}
