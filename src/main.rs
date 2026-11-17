//! ZapTide native GTK4 shell.

fn main() -> anyhow::Result<()> {
    zaptide::logging::init();
    zaptide::i18n::init();
    let dirs = zaptide::paths::AppDirs::discover();
    zaptide::application::run(dirs);
    Ok(())
}
