//! ZapTide native GTK4 shell.

use zaptide::settings::Language;

fn main() -> anyhow::Result<()> {
    zaptide::logging::init();
    let dirs = zaptide::paths::AppDirs::discover();
    // A secondary launch may only activate the running app. Reading this
    // preference must not trigger Settings::load's legacy migration.
    #[derive(Default, serde::Deserialize)]
    struct StartupPreferences {
        #[serde(default)]
        language: Language,
    }
    let preference: StartupPreferences = std::fs::File::open(dirs.settings_file())
        .ok()
        .and_then(|file| serde_json::from_reader(file).ok())
        .unwrap_or_default();
    zaptide::i18n::init_for_language(preference.language);
    zaptide::application::run(dirs);
    Ok(())
}
