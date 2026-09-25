//! ZapTide native GTK4 shell.

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,zaptide=info"),
    )
    .init();
    let dirs = zaptide::paths::AppDirs::discover();
    zaptide::application::run(dirs);
    Ok(())
}
