//! Where ZapTide keeps its files.
//!
//! Configuration, session state, and caches use separate standard platform
//! directories. Clearing a cache does not remove device keys.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

#[derive(Clone, Debug)]
pub struct AppDirs {
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
}

impl AppDirs {
    pub fn discover() -> Self {
        match Self::project() {
            Some(dirs) => dirs,
            None => {
                let fallback = std::env::current_dir().unwrap_or_default();
                Self {
                    config: fallback.join("zaptide-config"),
                    state: fallback.join("zaptide-state"),
                    cache: fallback.join("zaptide-cache"),
                }
            }
        }
    }

    /// Standard platform directories for the app.
    fn project() -> Option<Self> {
        let project = ProjectDirs::from("dev", "luminusos", "zaptide")?;
        Some(Self {
            config: project.config_dir().to_path_buf(),
            state: project
                .state_dir()
                .map(|path| path.to_path_buf())
                .unwrap_or_else(|| project.data_local_dir().to_path_buf()),
            cache: project.cache_dir().to_path_buf(),
        })
    }

    /// Places all data under one directory for tests and temporary runs.
    pub fn under(root: &std::path::Path) -> Self {
        Self {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    /// whatsapp-rust device identity, Signal sessions, and state keys.
    /// Deleting this database unlinks the computer.
    pub fn session_db(&self) -> PathBuf {
        self.state.join("session.db")
    }

    /// Local message archive.
    pub fn archive_db(&self) -> PathBuf {
        self.state.join("archive.db")
    }

    /// Current-run log, replaced at startup.
    pub fn log_file(&self) -> PathBuf {
        self.state.join("zaptide.log")
    }

    /// Panic log written before process exit.
    pub fn panic_log(&self) -> PathBuf {
        self.state.join("panic.log")
    }

    /// Icons ZapTide draws itself, where GTK's icon theme looks for them.
    pub fn icon_dir(&self) -> PathBuf {
        self.cache.join("icons")
    }

    /// Downloaded attachments keyed by message id.
    pub fn media_cache_dir(&self) -> PathBuf {
        self.cache.join("media")
    }

    /// Profile pictures keyed by chat.
    pub fn avatar_cache_dir(&self) -> PathBuf {
        self.cache.join("avatars")
    }

    /// Recent phone stickers keyed by file hash.
    pub fn sticker_cache_dir(&self) -> PathBuf {
        self.cache.join("stickers")
    }

    /// Saved stickers keyed by content hash. These are user data, not cache.
    pub fn saved_sticker_dir(&self) -> PathBuf {
        self.state.join("stickers")
    }

    /// Cached profile-picture path. `full` selects the info-dialog size.
    pub fn avatar_file(&self, id: &str, full: bool) -> PathBuf {
        let stem: String = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        self.avatar_cache_dir()
            .join(format!("{stem}{}.jpg", if full { "-full" } else { "" }))
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [&self.config, &self.state, &self.cache] {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            // Create new directories privately, even with a permissive umask.
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(dir)?;
            restrict_directory(dir)?;
        }
        Ok(())
    }
}

fn restrict_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("zaptide-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn ensure_restricts_base_directories() {
        use std::os::unix::fs::PermissionsExt;

        let root = root("permissions");
        let dirs = AppDirs::under(&root);
        dirs.ensure().unwrap();
        for path in [&dirs.config, &dirs.state, &dirs.cache] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_repairs_existing_directory_permissions_without_changing_data() {
        use std::os::unix::fs::PermissionsExt;

        let root = root("existing-permissions");
        let dirs = AppDirs::under(&root);
        for path in [&dirs.config, &dirs.state, &dirs.cache] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::write(path.join("fixture"), b"preserved").unwrap();
        }
        dirs.ensure().unwrap();
        for path in [&dirs.config, &dirs.state, &dirs.cache] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(std::fs::read(path.join("fixture")).unwrap(), b"preserved");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ensure_stops_when_an_application_directory_cannot_be_created() {
        let root = root("blocked-directory");
        let dirs = AppDirs::under(&root);
        std::fs::write(&dirs.state, b"existing file").unwrap();
        assert!(dirs.ensure().is_err());
        assert!(!dirs.cache.exists());
        assert_eq!(std::fs::read(&dirs.state).unwrap(), b"existing file");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zaptide_does_not_adopt_zapfast_data() {
        let root = root("isolated");
        let zapfast = AppDirs::under(&root.join("zapfast"));
        let zaptide = AppDirs::under(&root.join("zaptide"));
        zapfast.ensure().unwrap();
        std::fs::write(zapfast.settings_file(), b"zapfast settings").unwrap();
        std::fs::write(zapfast.session_db(), b"zapfast session").unwrap();
        std::fs::write(zapfast.archive_db(), b"zapfast archive").unwrap();

        zaptide.ensure().unwrap();

        assert!(!zaptide.settings_file().exists());
        assert!(!zaptide.session_db().exists());
        assert!(!zaptide.archive_db().exists());
        assert_eq!(
            std::fs::read(zapfast.settings_file()).unwrap(),
            b"zapfast settings"
        );
        assert_eq!(
            std::fs::read(zapfast.session_db()).unwrap(),
            b"zapfast session"
        );
        assert_eq!(
            std::fs::read(zapfast.archive_db()).unwrap(),
            b"zapfast archive"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
