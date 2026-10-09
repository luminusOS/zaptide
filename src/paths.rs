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

    pub fn archive_db(&self) -> PathBuf {
        self.state.join("archive.db")
    }

    /// The same layout, rooted in one account's folders. Settings, icons and
    /// logs stay shared through `config` and the root dirs.
    pub fn for_account(&self, id: crate::account::AccountId) -> AppDirs {
        AppDirs {
            config: self.config.clone(),
            state: self.accounts_state_dir().join(id.to_string()),
            cache: self.accounts_cache_dir().join(id.to_string()),
        }
    }

    pub fn accounts_state_dir(&self) -> PathBuf {
        self.state.join("accounts")
    }

    pub fn accounts_cache_dir(&self) -> PathBuf {
        self.cache.join("accounts")
    }

    /// One account's own settings; call it on that account's dirs.
    pub fn account_settings_file(&self) -> PathBuf {
        self.state.join("settings.json")
    }

    /// The linked accounts and the active one.
    pub fn accounts_file(&self) -> PathBuf {
        self.state.join("accounts.json")
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

    /// SQLite databases of a single-account setup, side files before their
    /// database so a move never leaves a log behind.
    const LEGACY_DATABASES: [&'static str; 8] = [
        "session.db-wal",
        "session.db-shm",
        "session.db-journal",
        "session.db",
        "archive.db-wal",
        "archive.db-shm",
        "archive.db-journal",
        "archive.db",
    ];

    /// Whether databases still sit at the root of the state directory, as
    /// before ZapTide kept each account in its own folder.
    pub fn has_legacy_session(&self) -> bool {
        Self::LEGACY_DATABASES
            .iter()
            .any(|name| self.state.join(name).exists())
    }

    /// Moves a single-account layout into `accounts/1/`.
    ///
    /// The archive is the only copy of the history, so nothing moves unless
    /// all of it can: a file already waiting at the destination stops the
    /// move before anything changes, and an encrypted archive moves only once
    /// its keyring key has been copied to the new folder's identity and read
    /// back. An interrupted move is finished by the next start.
    pub fn adopt_single_account(&self) -> std::io::Result<()> {
        self.adopt_single_account_with(|from, to| {
            crate::archive::copy_archive_key(from, to)
                .map_err(|error| std::io::Error::other(format!("{error:#}")))
        })
    }

    pub(crate) fn adopt_single_account_with(
        &self,
        copy_key: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let pending: Vec<&str> = Self::LEGACY_DATABASES
            .into_iter()
            .filter(|name| self.state.join(name).exists())
            .collect();
        let dest = self.for_account(crate::account::AccountId::FIRST);
        if !pending.is_empty() {
            self.adopt_databases(&dest, &pending, copy_key)?;
        }
        if let Err(error) = self.adopt_leftovers() {
            log::warn!("could not move saved stickers or caches yet: {error}");
        }
        Ok(())
    }

    /// Saved stickers and caches carry no key. They move into the first
    /// account whenever they are still at the root, so an interrupted move
    /// finishes on a later start.
    pub(crate) fn adopt_leftovers(&self) -> std::io::Result<()> {
        let dest = self.for_account(crate::account::AccountId::FIRST);
        adopt_directory(&self.saved_sticker_dir(), &dest.saved_sticker_dir())?;
        adopt_directory(&self.media_cache_dir(), &dest.media_cache_dir())?;
        adopt_directory(&self.avatar_cache_dir(), &dest.avatar_cache_dir())?;
        adopt_directory(&self.sticker_cache_dir(), &dest.sticker_cache_dir())
    }

    /// Moves back the databases an interrupted or failed move left in the
    /// first account's folder, restoring the single-account layout.
    fn roll_back_databases(&self, dest: &AppDirs) {
        for name in Self::LEGACY_DATABASES {
            let (moved, home) = (dest.state.join(name), self.state.join(name));
            if moved.exists()
                && !home.exists()
                && let Err(error) = std::fs::rename(&moved, &home)
            {
                log::error!("could not move {name} back: {error}");
            }
        }
    }

    fn adopt_databases(
        &self,
        dest: &AppDirs,
        pending: &[&str],
        copy_key: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        dest.ensure()?;
        // Refuse to mix two setups. Only an unrelated earlier file can be in
        // the way: a rename leaves nothing behind at its source.
        let mut blocked: Vec<PathBuf> = pending
            .iter()
            .map(|name| dest.state.join(name))
            .filter(|to| to.exists())
            .collect();
        if self.saved_sticker_dir().is_dir() && dest.saved_sticker_dir().exists() {
            blocked.push(dest.saved_sticker_dir());
        }
        if let Some(blocked) = blocked.first() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "{} holds a single-account setup, but {} already exists. Nothing was moved; move one of them away and start ZapTide again",
                    self.state.display(),
                    blocked.display()
                ),
            ));
        }
        self.move_databases(dest, pending, copy_key)
            .inspect_err(|_| {
                // Never leave a session or log split across two folders.
                self.roll_back_databases(dest);
            })
    }

    fn move_databases(
        &self,
        dest: &AppDirs,
        pending: &[&str],
        copy_key: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let legacy_archive = self.archive_db();
        if legacy_archive.exists() {
            copy_key(&legacy_archive, &dest.archive_db()).map_err(|error| {
                std::io::Error::other(format!(
                    "Could not move the archive's key to the new account folder, so the archive was left where it is: {error}"
                ))
            })?;
        }
        for name in pending {
            std::fs::rename(self.state.join(name), dest.state.join(name))?;
        }
        // Make the renames durable before anything opens the databases.
        for dir in [&self.state, &dest.state] {
            std::fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
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

fn adopt_directory(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() && !to.try_exists()? {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(from, to)?;
    }
    Ok(())
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
    fn each_account_has_its_own_state_and_cache_but_shares_config() {
        let root = root("for-account");
        let dirs = AppDirs::under(&root);
        let one = dirs.for_account(crate::account::AccountId(1));
        let two = dirs.for_account(crate::account::AccountId(2));
        assert_eq!(one.config, dirs.config);
        assert_eq!(one.session_db(), dirs.state.join("accounts/1/session.db"));
        assert_eq!(two.media_cache_dir(), dirs.cache.join("accounts/2/media"));
        assert_ne!(one.archive_db(), two.archive_db());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn legacy(name: &str) -> (PathBuf, AppDirs) {
        let root = root(name);
        let dirs = AppDirs::under(&root);
        dirs.ensure().unwrap();
        std::fs::write(dirs.state.join("session.db"), b"session").unwrap();
        std::fs::write(dirs.state.join("archive.db"), b"archive").unwrap();
        std::fs::write(dirs.state.join("archive.db-wal"), b"wal").unwrap();
        std::fs::create_dir_all(dirs.saved_sticker_dir()).unwrap();
        std::fs::write(dirs.saved_sticker_dir().join("sticker"), b"sticker").unwrap();
        std::fs::create_dir_all(dirs.media_cache_dir()).unwrap();
        std::fs::write(dirs.media_cache_dir().join("photo"), b"photo").unwrap();
        (root, dirs)
    }

    fn first(dirs: &AppDirs) -> AppDirs {
        dirs.for_account(crate::account::AccountId::FIRST)
    }

    #[test]
    fn migration_moves_everything_into_the_first_account() {
        let (root, dirs) = legacy("migration-moves");
        assert!(dirs.has_legacy_session());
        dirs.adopt_single_account_with(|_, _| Ok(())).unwrap();
        let one = first(&dirs);
        assert_eq!(std::fs::read(one.session_db()).unwrap(), b"session");
        assert_eq!(std::fs::read(one.archive_db()).unwrap(), b"archive");
        assert_eq!(
            std::fs::read(one.state.join("archive.db-wal")).unwrap(),
            b"wal"
        );
        assert_eq!(
            std::fs::read(one.saved_sticker_dir().join("sticker")).unwrap(),
            b"sticker"
        );
        assert_eq!(
            std::fs::read(one.media_cache_dir().join("photo")).unwrap(),
            b"photo"
        );
        assert!(!dirs.has_legacy_session());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_leaves_everything_when_the_key_cannot_move() {
        let (root, dirs) = legacy("migration-key");
        let error = dirs
            .adopt_single_account_with(|_, _| Err(std::io::Error::other("locked")))
            .unwrap_err();
        assert!(error.to_string().contains("left where it is"));
        assert_eq!(std::fs::read(dirs.archive_db()).unwrap(), b"archive");
        assert_eq!(std::fs::read(dirs.session_db()).unwrap(), b"session");
        assert!(!first(&dirs).session_db().exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_copies_the_key_before_moving_the_archive() {
        let (root, dirs) = legacy("migration-order");
        let archive = dirs.archive_db();
        let target = first(&dirs).archive_db();
        dirs.adopt_single_account_with(|from, to| {
            assert!(
                from.exists(),
                "the archive stays put while its key is copied"
            );
            assert_eq!((from, to), (archive.as_path(), target.as_path()));
            Ok(())
        })
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_finishes_an_interrupted_move() {
        let (root, dirs) = legacy("migration-resume");
        let one = first(&dirs);
        one.ensure().unwrap();
        // A crash after the log moved but before its database did.
        std::fs::rename(
            dirs.state.join("archive.db-wal"),
            one.state.join("archive.db-wal"),
        )
        .unwrap();
        dirs.adopt_single_account_with(|_, _| Ok(())).unwrap();
        assert_eq!(std::fs::read(one.archive_db()).unwrap(), b"archive");
        assert_eq!(
            std::fs::read(one.state.join("archive.db-wal")).unwrap(),
            b"wal"
        );
        assert!(!dirs.has_legacy_session());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_rolls_back_a_partial_move_when_the_key_cannot_move() {
        let (root, dirs) = legacy("migration-rollback");
        let one = first(&dirs);
        one.ensure().unwrap();
        // An earlier start moved these, then stopped.
        for name in ["session.db", "archive.db-wal"] {
            std::fs::rename(dirs.state.join(name), one.state.join(name)).unwrap();
        }
        assert!(
            dirs.adopt_single_account_with(|_, _| Err(std::io::Error::other("locked")))
                .is_err()
        );
        assert_eq!(std::fs::read(dirs.session_db()).unwrap(), b"session");
        assert_eq!(
            std::fs::read(dirs.state.join("archive.db-wal")).unwrap(),
            b"wal"
        );
        assert!(!one.session_db().exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_taken_folder_is_never_rolled_back_into_the_root() {
        let (root, dirs) = legacy("migration-foreign");
        std::fs::remove_file(dirs.session_db()).unwrap();
        let one = first(&dirs);
        one.ensure().unwrap();
        // Another setup's files, and one name the root also has.
        std::fs::write(one.session_db(), b"other session").unwrap();
        std::fs::write(one.archive_db(), b"other archive").unwrap();
        assert!(dirs.adopt_single_account_with(|_, _| Ok(())).is_err());
        assert!(!dirs.session_db().exists());
        assert_eq!(std::fs::read(one.session_db()).unwrap(), b"other session");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_stops_before_changing_anything_when_the_folder_is_taken() {
        let (root, dirs) = legacy("migration-blocked");
        let one = first(&dirs);
        one.ensure().unwrap();
        std::fs::write(one.session_db(), b"other").unwrap();
        assert!(dirs.adopt_single_account_with(|_, _| Ok(())).is_err());
        assert_eq!(std::fs::read(dirs.session_db()).unwrap(), b"session");
        assert_eq!(std::fs::read(dirs.archive_db()).unwrap(), b"archive");
        assert_eq!(std::fs::read(one.session_db()).unwrap(), b"other");
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
