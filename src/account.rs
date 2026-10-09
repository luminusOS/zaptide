//! The WhatsApp numbers linked to this ZapTide, and their per-account files.
//!
//! Each account keeps its own session, archive, caches and keyring key under
//! `state/accounts/<id>/` and `cache/accounts/<id>/`. Ids only grow, so a new
//! account never inherits the folders of a removed one.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths::AppDirs;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub u32);

impl AccountId {
    pub const FIRST: Self = Self(1);
}

impl Default for AccountId {
    fn default() -> Self {
        AccountId::FIRST
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// What the switcher shows before an account connects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountEntry {
    pub id: AccountId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    /// False until the phone has linked; pending accounts are discarded at
    /// start once another account is linked.
    #[serde(default)]
    pub linked: bool,
}

impl AccountEntry {
    /// Profile name, else number, else "Account <id>".
    pub fn label(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| self.phone.clone())
            .unwrap_or_else(|| format!("Account {}", self.id))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Registry {
    pub accounts: Vec<AccountEntry>,
    pub active: Option<AccountId>,
    pub next_id: u32,
}

impl Registry {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|error| {
                log::warn!("accounts file is unreadable: {error}");
                Self::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                log::warn!("could not read the accounts file: {error}");
                Self::default()
            }
        }
    }

    /// Atomically replaces the file through a temporary file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_json(self, path)
    }

    pub fn add(&mut self) -> AccountId {
        let highest = self
            .accounts
            .iter()
            .map(|entry| entry.id.0)
            .max()
            .unwrap_or(0);
        let id = AccountId(self.next_id.max(highest + 1).max(1));
        self.next_id = id.0 + 1;
        self.accounts.push(AccountEntry {
            id,
            name: None,
            phone: None,
            linked: false,
        });
        id
    }

    pub fn remove(&mut self, id: AccountId) {
        self.accounts.retain(|entry| entry.id != id);
        if self.active == Some(id) {
            self.active = self.linked_ids().first().copied();
        }
    }

    pub fn entry(&self, id: AccountId) -> Option<&AccountEntry> {
        self.accounts.iter().find(|entry| entry.id == id)
    }

    pub fn entry_mut(&mut self, id: AccountId) -> Option<&mut AccountEntry> {
        self.accounts.iter_mut().find(|entry| entry.id == id)
    }

    pub fn linked_ids(&self) -> Vec<AccountId> {
        self.accounts
            .iter()
            .filter(|entry| entry.linked)
            .map(|entry| entry.id)
            .collect()
    }

    /// Drops accounts that never linked, unless none is linked (a fresh
    /// install links its first account through a pending one).
    pub fn discard_pending(&mut self) -> Vec<AccountId> {
        if self.linked_ids().is_empty() {
            return Vec::new();
        }
        let pending: Vec<AccountId> = self
            .accounts
            .iter()
            .filter(|entry| !entry.linked)
            .map(|entry| entry.id)
            .collect();
        for id in &pending {
            self.remove(*id);
        }
        pending
    }
}

/// The settings that belong to one account. While it is active they are
/// applied onto the window's [`crate::settings::Settings`], so the
/// preferences keep editing a single struct.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountSettings {
    pub last_chat: Option<String>,
    pub send_read_receipts: bool,
    pub send_typing: bool,
    pub chat_lock_code_hash: Option<String>,
    pub chat_lock_hint_dismissed: bool,
}

impl Default for AccountSettings {
    fn default() -> Self {
        Self::from_settings(&crate::settings::Settings::default())
    }
}

impl AccountSettings {
    pub fn from_settings(settings: &crate::settings::Settings) -> Self {
        Self {
            last_chat: settings.last_chat.clone(),
            send_read_receipts: settings.send_read_receipts,
            send_typing: settings.send_typing,
            chat_lock_code_hash: settings.chat_lock_code_hash.clone(),
            chat_lock_hint_dismissed: settings.chat_lock_hint_dismissed,
        }
    }

    pub fn apply_to(&self, settings: &mut crate::settings::Settings) {
        settings.last_chat = self.last_chat.clone();
        settings.send_read_receipts = self.send_read_receipts;
        settings.send_typing = self.send_typing;
        settings.chat_lock_code_hash = self.chat_lock_code_hash.clone();
        settings.chat_lock_hint_dismissed = self.chat_lock_hint_dismissed;
    }

    /// The account's saved choices, or `current`'s when the file is missing
    /// or damaged: falling back to defaults would unlock locked chats and
    /// turn read receipts back on.
    pub fn load_or(path: &Path, current: &crate::settings::Settings) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|error| {
                log::warn!("account settings are unreadable, keeping the current ones: {error}");
                Self::from_settings(current)
            }),
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("could not read account settings: {error}");
                }
                Self::from_settings(current)
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_json(self, path)
    }
}

/// The accounts to run, after any one-time move and cleanup.
pub struct Prepared {
    pub registry: Registry,
    /// The single-account files could not move; run as before, one account.
    pub legacy: bool,
}

pub fn prepare(dirs: &AppDirs, global: &crate::settings::Settings) -> Prepared {
    prepare_with(dirs, global, |from, to| {
        crate::archive::copy_archive_key(from, to)
            .map_err(|error| std::io::Error::other(format!("{error:#}")))
    })
}

fn prepare_with(
    dirs: &AppDirs,
    global: &crate::settings::Settings,
    copy_key: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Prepared {
    let path = dirs.accounts_file();
    let mut registry = Registry::load(&path);
    if registry.accounts.is_empty() && dirs.has_legacy_session() {
        // A first start, or a move that stopped half-way: finish it before
        // trusting the folders, so no database is left behind at the root.
        if let Err(error) = dirs.adopt_single_account_with(copy_key) {
            log::error!("could not move the existing account into its folder: {error}");
            return Prepared {
                registry,
                legacy: true,
            };
        }
        let settings = dirs.for_account(AccountId::FIRST).account_settings_file();
        if !settings.exists()
            && let Err(error) = AccountSettings::from_settings(global).save(&settings)
        {
            log::warn!("could not save the first account's settings: {error}");
        }
    }
    if registry.accounts.is_empty() {
        // A missing or damaged file while account folders exist (a failed
        // save, a crash after the move): rebuild it from the folders rather
        // than treat linked accounts as orphans.
        let next_id = registry.next_id;
        registry = recover(dirs);
        registry.next_id = registry.next_id.max(next_id);
    }
    if registry.accounts.is_empty() {
        registry.active = Some(registry.add());
    }
    if registry
        .entry(AccountId::FIRST)
        .is_some_and(|entry| entry.linked)
        && let Err(error) = dirs.adopt_leftovers()
    {
        log::warn!("could not move saved stickers or caches into the first account: {error}");
    }
    for id in registry.discard_pending() {
        delete_account_data(dirs, id);
    }
    if registry
        .active
        .is_none_or(|active| registry.entry(active).is_none())
    {
        registry.active = registry.accounts.first().map(|entry| entry.id);
    }
    delete_orphans(dirs, &registry);
    if let Err(error) = registry.save(&path) {
        log::warn!("could not save the accounts file: {error}");
    }
    Prepared {
        registry,
        legacy: false,
    }
}

/// Forgets an account's archive key and deletes its folders. Errors are
/// logged; whatever is left is removed as an orphan on the next start.
pub fn delete_account_data(dirs: &AppDirs, id: AccountId) {
    let account = dirs.for_account(id);
    let archive = account.archive_db();
    if archive.exists() {
        match crate::archive::archive_key_identity(&archive) {
            Ok(identity) => {
                if let Err(error) = crate::archive::forget_archive_key(&identity) {
                    log::warn!("could not delete account {id}'s archive key: {error:#}");
                }
            }
            Err(error) => log::warn!("could not name account {id}'s archive key: {error:#}"),
        }
    }
    for folder in [&account.state, &account.cache] {
        if let Err(error) = std::fs::remove_dir_all(folder)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("could not delete {}: {error}", folder.display());
        }
    }
}

/// Every numeric account folder, linked when it holds a session.
fn recover(dirs: &AppDirs) -> Registry {
    let mut ids: Vec<u32> = std::fs::read_dir(dirs.accounts_state_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect();
    ids.sort_unstable();
    let mut registry = Registry::default();
    for id in ids {
        let id = AccountId(id);
        registry.accounts.push(AccountEntry {
            id,
            name: None,
            phone: None,
            linked: dirs.for_account(id).session_db().exists(),
        });
    }
    registry.next_id = registry.accounts.last().map_or(1, |entry| entry.id.0 + 1);
    registry.active = registry.linked_ids().first().copied();
    log::warn!(
        "rebuilt the accounts file from {} account folders",
        registry.accounts.len()
    );
    registry
}

/// Folders left by a removal that stopped half-way.
fn delete_orphans(dirs: &AppDirs, registry: &Registry) {
    for base in [dirs.accounts_state_dir(), dirs.accounts_cache_dir()] {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if registry.entry(AccountId(id)).is_none() {
                delete_account_data(dirs, AccountId(id));
            }
        }
    }
}

fn write_json(value: &impl Serialize, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(contents.as_bytes())?;
        // A crash after the rename must not leave an empty file behind.
        file.sync_all()?;
    }
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_grow_and_are_never_reused() {
        let mut registry = Registry::default();
        let first = registry.add();
        let second = registry.add();
        assert_eq!((first, second), (AccountId(1), AccountId(2)));
        registry.remove(second);
        assert_eq!(registry.add(), AccountId(3));
    }

    #[test]
    fn pending_accounts_are_discarded_only_when_a_linked_one_exists() {
        let mut registry = Registry::default();
        let only = registry.add();
        registry.active = Some(only);
        assert!(
            registry.discard_pending().is_empty(),
            "a fresh install keeps its pending account"
        );
        registry.entry_mut(only).unwrap().linked = true;
        let pending = registry.add();
        registry.active = Some(pending);
        assert_eq!(registry.discard_pending(), vec![pending]);
        assert_eq!(registry.active, Some(only));
    }

    #[test]
    fn registry_round_trips_and_tolerates_damage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        let mut registry = Registry::default();
        let id = registry.add();
        registry.active = Some(id);
        registry.entry_mut(id).unwrap().name = Some("Work".into());
        registry.save(&path).unwrap();
        assert_eq!(Registry::load(&path), registry);
        std::fs::write(&path, b"{damaged").unwrap();
        assert_eq!(Registry::load(&path), Registry::default());
    }

    fn dirs(name: &str) -> (tempfile::TempDir, crate::paths::AppDirs) {
        let root = tempfile::tempdir().unwrap();
        let dirs = crate::paths::AppDirs::under(&root.path().join(name));
        dirs.ensure().unwrap();
        (root, dirs)
    }

    #[test]
    fn a_fresh_install_starts_with_one_pending_account() {
        let (_root, dirs) = dirs("fresh");
        let prepared = prepare_with(&dirs, &crate::settings::Settings::default(), |_, _| Ok(()));
        assert!(!prepared.legacy);
        assert_eq!(prepared.registry.active, Some(AccountId::FIRST));
        assert!(!prepared.registry.entry(AccountId::FIRST).unwrap().linked);
        assert_eq!(Registry::load(&dirs.accounts_file()), prepared.registry);
    }

    #[test]
    fn a_legacy_setup_becomes_linked_account_one_with_its_settings() {
        let (_root, dirs) = dirs("legacy");
        std::fs::write(dirs.session_db(), b"session").unwrap();
        let global = crate::settings::Settings {
            send_typing: false,
            ..Default::default()
        };
        let prepared = prepare_with(&dirs, &global, |_, _| Ok(()));
        assert!(!prepared.legacy);
        assert!(prepared.registry.entry(AccountId::FIRST).unwrap().linked);
        let one = dirs.for_account(AccountId::FIRST);
        assert_eq!(std::fs::read(one.session_db()).unwrap(), b"session");
        assert!(
            !AccountSettings::load_or(&one.account_settings_file(), &Default::default())
                .send_typing
        );
    }

    #[test]
    fn a_failed_migration_keeps_the_legacy_layout() {
        let (_root, dirs) = dirs("legacy-failed");
        std::fs::write(dirs.archive_db(), b"archive").unwrap();
        let prepared = prepare_with(&dirs, &Default::default(), |_, _| {
            Err(std::io::Error::other("locked"))
        });
        assert!(prepared.legacy);
        assert_eq!(std::fs::read(dirs.archive_db()).unwrap(), b"archive");
        assert!(!dirs.accounts_file().exists());
    }

    #[test]
    fn orphans_are_deleted() {
        let (_root, dirs) = dirs("orphans");
        let mut registry = Registry::default();
        let kept = registry.add();
        registry.entry_mut(kept).unwrap().linked = true;
        registry.active = Some(kept);
        registry.save(&dirs.accounts_file()).unwrap();
        let kept_dirs = dirs.for_account(kept);
        kept_dirs.ensure().unwrap();
        let orphan = dirs.for_account(AccountId(7));
        orphan.ensure().unwrap();
        std::fs::write(orphan.session_db(), b"left behind").unwrap();
        let unrelated = dirs.accounts_state_dir().join("not-a-number");
        std::fs::create_dir_all(&unrelated).unwrap();
        prepare_with(&dirs, &Default::default(), |_, _| Ok(()));
        assert!(!orphan.state.exists() && !orphan.cache.exists());
        assert!(kept_dirs.state.exists());
        assert!(unrelated.exists(), "only numeric folders are touched");
    }

    #[test]
    fn a_pending_account_is_discarded_when_another_is_linked() {
        let (_root, dirs) = dirs("pending");
        let mut registry = Registry::default();
        let linked = registry.add();
        registry.entry_mut(linked).unwrap().linked = true;
        let pending = registry.add();
        registry.active = Some(pending);
        registry.save(&dirs.accounts_file()).unwrap();
        dirs.for_account(pending).ensure().unwrap();
        let prepared = prepare_with(&dirs, &Default::default(), |_, _| Ok(()));
        assert_eq!(prepared.registry.active, Some(linked));
        assert!(prepared.registry.entry(pending).is_none());
        assert!(!dirs.for_account(pending).state.exists());
    }

    #[test]
    fn a_damaged_accounts_file_never_deletes_linked_accounts() {
        let (_root, dirs) = dirs("damaged");
        for id in [AccountId(1), AccountId(3)] {
            let account = dirs.for_account(id);
            account.ensure().unwrap();
            std::fs::write(account.session_db(), b"session").unwrap();
        }
        std::fs::write(dirs.accounts_file(), b"{damaged").unwrap();
        let mut prepared = prepare_with(&dirs, &Default::default(), |_, _| Ok(()));
        assert_eq!(
            prepared.registry.linked_ids(),
            vec![AccountId(1), AccountId(3)]
        );
        assert!(dirs.for_account(AccountId(3)).session_db().exists());
        assert_eq!(prepared.registry.add(), AccountId(4));
    }

    #[test]
    fn a_missing_accounts_file_never_deletes_linked_accounts() {
        let (_root, dirs) = dirs("missing");
        for id in [AccountId(1), AccountId(2)] {
            let account = dirs.for_account(id);
            account.ensure().unwrap();
            std::fs::write(account.session_db(), b"session").unwrap();
        }
        let prepared = prepare_with(&dirs, &Default::default(), |_, _| Ok(()));
        assert_eq!(
            prepared.registry.linked_ids(),
            vec![AccountId(1), AccountId(2)]
        );
        assert!(dirs.for_account(AccountId(2)).session_db().exists());
    }

    #[test]
    fn a_crash_before_the_accounts_file_was_written_finishes_the_move() {
        let (_root, dirs) = dirs("crash-after-move");
        let one = dirs.for_account(AccountId::FIRST);
        one.ensure().unwrap();
        // The databases moved, then the app stopped before the rest.
        std::fs::write(one.session_db(), b"session").unwrap();
        std::fs::create_dir_all(dirs.saved_sticker_dir()).unwrap();
        std::fs::write(dirs.saved_sticker_dir().join("sticker"), b"sticker").unwrap();
        let global = crate::settings::Settings {
            send_read_receipts: false,
            ..Default::default()
        };
        let prepared = prepare_with(&dirs, &global, |_, _| Ok(()));
        assert!(!prepared.legacy);
        assert_eq!(prepared.registry.linked_ids(), vec![AccountId::FIRST]);
        assert_eq!(
            std::fs::read(one.saved_sticker_dir().join("sticker")).unwrap(),
            b"sticker"
        );
        assert!(
            !AccountSettings::load_or(&one.account_settings_file(), &global).send_read_receipts,
            "settings never saved for the account fall back to the window's"
        );
    }

    #[test]
    fn a_crash_in_the_middle_of_the_move_is_finished_before_recovery() {
        let (_root, dirs) = dirs("crash-mid-move");
        let one = dirs.for_account(AccountId::FIRST);
        one.ensure().unwrap();
        std::fs::write(one.session_db(), b"session").unwrap();
        std::fs::write(one.state.join("archive.db-wal"), b"wal").unwrap();
        std::fs::write(dirs.archive_db(), b"archive").unwrap();
        let prepared = prepare_with(&dirs, &Default::default(), |_, _| Ok(()));
        assert!(!prepared.legacy);
        assert_eq!(prepared.registry.linked_ids(), vec![AccountId::FIRST]);
        assert_eq!(std::fs::read(one.archive_db()).unwrap(), b"archive");
        assert!(!dirs.archive_db().exists());
    }

    #[test]
    fn root_stickers_stay_put_for_a_pending_first_account() {
        let (_root, dirs) = dirs("fresh-with-stickers");
        std::fs::create_dir_all(dirs.saved_sticker_dir()).unwrap();
        prepare_with(&dirs, &Default::default(), |_, _| Ok(()));
        assert!(dirs.saved_sticker_dir().exists());
    }

    #[test]
    fn ids_are_not_reused_after_the_last_account_is_gone() {
        let (_root, dirs) = dirs("empty-registry");
        let registry = Registry {
            next_id: 4,
            ..Default::default()
        };
        registry.save(&dirs.accounts_file()).unwrap();
        let prepared = prepare_with(&dirs, &Default::default(), |_, _| Ok(()));
        assert_eq!(prepared.registry.active, Some(AccountId(4)));
    }

    #[test]
    fn damaged_account_settings_keep_the_previous_choices() {
        let (_root, dirs) = dirs("damaged-settings");
        let path = dirs.state.join("settings.json");
        std::fs::write(&path, b"{damaged").unwrap();
        let global = crate::settings::Settings {
            send_typing: false,
            chat_lock_code_hash: Some("salt$hash".into()),
            ..Default::default()
        };
        let loaded = AccountSettings::load_or(&path, &global);
        assert!(!loaded.send_typing);
        assert_eq!(loaded.chat_lock_code_hash.as_deref(), Some("salt$hash"));
    }

    #[test]
    fn account_settings_overlay_global_settings() {
        let mut global = crate::settings::Settings::default();
        let account = AccountSettings {
            send_read_receipts: false,
            last_chat: Some("chat".into()),
            ..Default::default()
        };
        account.apply_to(&mut global);
        assert!(!global.send_read_receipts);
        assert_eq!(global.last_chat.as_deref(), Some("chat"));
        assert_eq!(AccountSettings::from_settings(&global), account);
    }

    #[test]
    fn label_prefers_name_then_phone() {
        let mut entry = AccountEntry {
            id: AccountId(4),
            name: Some("  ".into()),
            phone: None,
            linked: true,
        };
        assert_eq!(entry.label(), "Account 4");
        entry.phone = Some("+1 555 0100".into());
        assert_eq!(entry.label(), "+1 555 0100");
        entry.name = Some("Work".into());
        assert_eq!(entry.label(), "Work");
    }
}
