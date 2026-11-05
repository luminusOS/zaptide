//! SQLCipher storage for the archive and the device store, unlocked with one
//! OS-keyring key per account folder.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::secrets::{self, Keyring, Lookup};

const HEADER: &[u8; 16] = b"SQLite format 3\0";
const ARCHIVE_FILE: &str = "archive.db";
const DEVICE_STORE_FILE: &str = "session.db";
const DEVICE_STORE_KEY_INFO: &[u8] = b"dev.luminusos.ZapTide device store key v1";

/// Which database a message is about, so an error names the right one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Database {
    Archive,
    DeviceStore,
}

impl Database {
    fn name(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::DeviceStore => "device store",
        }
    }
}

fn plaintext(path: &Path) -> Result<bool> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() == 0 {
        return Ok(true);
    }
    let mut header = [0; 16];
    file.read_exact(&mut header)?;
    Ok(&header == HEADER)
}

fn device_store_beside(archive: &Path) -> PathBuf {
    archive.with_file_name(DEVICE_STORE_FILE)
}

/// Both databases of a folder use its keyring key, so the key may only be
/// created while neither of them is encrypted.
fn ensure_nothing_encrypted(archive: &Path) -> Result<()> {
    for (path, database) in [
        (archive.to_owned(), Database::Archive),
        (device_store_beside(archive), Database::DeviceStore),
    ] {
        let what = database.name();
        ensure!(
            plaintext(&path)?,
            "The {what} is encrypted but its OS keyring key is missing. Restore the original keyring, or remove this account and link it again to start over with an empty {what}; the {what} has not been changed"
        );
    }
    Ok(())
}

fn folder_encrypted(archive: &Path) -> Result<bool> {
    Ok(!plaintext(archive)? || !plaintext(&device_store_beside(archive))?)
}

/// The SQLCipher key of the whatsapp-rust device store at `path`.
///
/// It is derived from the keyring key of the archive in the same folder
/// rather than stored as a second keyring item: moving, copying and
/// forgetting a folder's key then covers both databases, and no second item
/// can go missing on its own. HKDF keeps the two SQLCipher keys distinct.
pub(crate) fn device_store_key(path: &Path) -> Result<Zeroizing<[u8; 32]>> {
    derive_device_store_key(&*key_for(&path.with_file_name(ARCHIVE_FILE))?)
}

fn derive_device_store_key(folder_key: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>> {
    let algorithm = ring::hkdf::HKDF_SHA256;
    let mut key = Zeroizing::new([0; 32]);
    ring::hkdf::Salt::new(algorithm, &[])
        .extract(folder_key)
        .expand(&[DEVICE_STORE_KEY_INFO], algorithm)
        .and_then(|okm| okm.fill(key.as_mut()))
        .map_err(|_| anyhow::anyhow!("Could not derive the device store key"))?;
    Ok(key)
}

pub(super) fn key_for(path: &Path) -> Result<Zeroizing<[u8; 32]>> {
    let parent = path.parent().context("Archive has no parent directory")?;
    fs::create_dir_all(parent)?;
    let identity = identity_for(parent)?;
    let path = path.to_owned();
    secrets::with_os_keyring(move |keyring| key_in(&path, &identity, keyring))
}

/// Separate profiles must not overwrite each other's keys. The credential
/// label contains a digest, never a user path, phone number or message data.
fn identity_for(parent: &Path) -> Result<String> {
    let digest = Sha256::digest(parent.canonicalize()?.as_os_str().as_encoded_bytes());
    Ok(format!(
        "archive-{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

/// The keyring identity of the archive at `path`, while its folder exists.
pub fn archive_key_identity(path: &Path) -> Result<String> {
    identity_for(path.parent().context("Archive has no parent directory")?)
}

/// How long the window's thread waits for the keyring while moving an
/// account's files at startup; long enough to type a keyring password.
const COPY_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Copies an encrypted archive's key to the identity of `new_archive`'s
/// folder and reads it back, so the archive can move there. Gives up after
/// two minutes, leaving the files where they are.
pub fn copy_archive_key(old_archive: &Path, new_archive: &Path) -> Result<()> {
    if !folder_encrypted(old_archive)? {
        return Ok(());
    }
    let (old_archive, new_archive) = (old_archive.to_owned(), new_archive.to_owned());
    secrets::with_os_keyring_within(COPY_WAIT, move |keyring| {
        copy_archive_key_in(&old_archive, &new_archive, keyring)
    })
}

fn copy_archive_key_in(
    old_archive: &Path,
    new_archive: &Path,
    keyring: &Keyring<'_>,
) -> Result<()> {
    let old_parent = old_archive
        .parent()
        .context("Archive has no parent directory")?;
    let new_parent = new_archive
        .parent()
        .context("Archive has no parent directory")?;
    if !old_parent.exists() {
        return Ok(());
    }
    fs::create_dir_all(new_parent)?;
    if old_parent.canonicalize()? == new_parent.canonicalize()? {
        return Ok(());
    }
    let key = match keyring.get(&identity_for(old_parent)?)? {
        Lookup::Found(secret) => key_from(&secret)?,
        Lookup::Missing { legacy } => {
            ensure_key_may_be_missing(old_archive, legacy)?;
            return Ok(());
        }
    };
    let new_identity = identity_for(new_parent)?;
    keyring
        .set(&new_identity, key.as_ref())
        .context("Could not save the archive key in the OS keyring")?;
    // Read back before the only copy of the history moves under this key.
    let saved = keyring
        .get_saved(&new_identity)
        .context("Could not verify the copied archive key")?;
    ensure!(
        saved.as_deref().map(Vec::as_slice) == Some(key.as_slice()),
        "The OS keyring did not retain the copied archive key"
    );
    Ok(())
}

/// Deletes the keyring entry of an archive folder that is gone for good,
/// such as a removed account's. Never call it while the archive is in use.
///
/// The deletion is queued on the keyring thread and only its failure to
/// start is returned; later errors are logged. Keyring work runs in order, so
/// a new key for the same folder is created only after this one is gone.
/// Outside a sandbox, or without a Secret portal, the key is deleted from
/// the host Secret Service in every collection, which is also where builds
/// before oo7 kept it. Inside a sandbox it is deleted from oo7's file
/// keyring, and a copy an older build left in the host Secret Service stays.
pub fn forget_archive_key(identity: &str) -> Result<()> {
    let identity = identity.to_owned();
    secrets::with_os_keyring_later("delete an archive key", move |keyring| {
        forget_archive_key_in(&identity, keyring)
    })
}

fn forget_archive_key_in(identity: &str, keyring: &Keyring<'_>) -> Result<()> {
    keyring
        .delete(identity)
        .context("Could not delete the archive key from the OS keyring")
}

fn key_from(secret: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    ensure!(
        secret.len() == 32,
        "The archive key in the OS keyring is invalid"
    );
    let mut key = Zeroizing::new([0; 32]);
    key.copy_from_slice(secret);
    Ok(key)
}

/// A key that no store has is fine only while nothing in the folder needs it.
/// When the old store could not be searched, its error explains more than
/// "missing" does.
fn ensure_key_may_be_missing(archive: &Path, legacy: Option<anyhow::Error>) -> Result<()> {
    if let Some(error) = legacy
        && folder_encrypted(archive)?
    {
        return Err(error).context("Unlock your OS keyring and restart ZapTide");
    }
    ensure_nothing_encrypted(archive)
}

fn key_in(path: &Path, identity: &str, keyring: &Keyring<'_>) -> Result<Zeroizing<[u8; 32]>> {
    let legacy = match keyring.get(identity)? {
        Lookup::Found(secret) => return key_from(&secret),
        Lookup::Missing { legacy } => legacy,
    };
    ensure_key_may_be_missing(path, legacy)?;
    let mut key = Zeroizing::new([0; 32]);
    getrandom::fill(key.as_mut()).context("Could not generate an archive key")?;
    keyring
        .set(identity, key.as_ref())
        .context("Could not save the archive key in the OS keyring")?;
    // Read back before touching the only copy of the message history.
    let saved = keyring
        .get_saved(identity)
        .context("Could not verify the saved archive key")?;
    ensure!(
        saved.as_deref().map(Vec::as_slice) == Some(key.as_slice()),
        "The OS keyring did not retain the archive key"
    );
    Ok(key)
}

pub(crate) fn key_literal(key: &[u8; 32]) -> Zeroizing<String> {
    use std::fmt::Write;
    let mut literal = Zeroizing::new(String::with_capacity(67));
    literal.push_str("x'");
    for byte in key {
        write!(&mut *literal, "{byte:02x}").expect("writing to a String");
    }
    literal.push('\'');
    literal
}

/// Opens an existing SQLCipher database and proves `key` unlocks it.
pub(crate) fn keyed(path: &Path, key: &[u8; 32], database: Database) -> Result<Connection> {
    let what = database.name();
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "key", &*key_literal(key))?;
    let version: String = connection
        .query_row("PRAGMA cipher_version", [], |row| row.get(0))
        .with_context(|| format!("This build cannot open an encrypted {what}"))?;
    ensure!(
        !version.is_empty(),
        "This build cannot open an encrypted {what}"
    );
    // PRAGMA key alone does not verify a key. Read a page before any migration.
    connection
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        })
        .with_context(|| format!("The {what} could not be unlocked with its OS keyring key"))?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(connection)
}

fn private_file(path: &Path) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.sync_all()?;
    Ok(())
}

/// Opens the SQLCipher database at `path`, first encrypting a plaintext one
/// in place through a verified staging copy.
pub(crate) fn open(path: &Path, key: &[u8; 32], database: Database) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let staging = path.with_extension("db.encrypting");
    if !path.exists() {
        private_file(path)?;
    } else if fs::metadata(path)?.len() == 0 {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    } else if plaintext(path)? {
        encrypt_in_place(path, &staging, key, database)
            .with_context(|| format!("Could not encrypt the {}", database.name()))?;
    }
    let connection = keyed(path, key, database)?;
    if staging.try_exists()? {
        fs::remove_file(staging)?;
    }
    Ok(connection)
}

fn encrypt_in_place(path: &Path, staging: &Path, key: &[u8; 32], database: Database) -> Result<()> {
    let what = database.name();
    // Keep the original authoritative until the exported copy is complete.
    // Switching away from WAL folds in committed pages and removes sidecars
    // before replacing the main file. An interruption leaves the original
    // usable, with at most an encrypted staging file to discard next time.
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let mode: String = source.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
    ensure!(
        mode == "delete",
        "Close other programs using the {what} before encrypting it"
    );
    source.pragma_update(None, "temp_store", "MEMORY")?;
    if staging.try_exists()? {
        fs::remove_file(staging)?;
    }
    private_file(staging)?;
    source.execute(
        "ATTACH DATABASE ?1 AS encrypted KEY ?2",
        rusqlite::params![
            staging
                .to_str()
                .with_context(|| format!("The {what} path is not UTF-8"))?,
            &*key_literal(key)
        ],
    )?;
    source.query_row("SELECT sqlcipher_export('encrypted')", [], |_| Ok(()))?;
    // sqlcipher_export does not copy SQLite's application/user version.
    for pragma in ["user_version", "application_id"] {
        let value: i64 = source.pragma_query_value(None, pragma, |row| row.get(0))?;
        source.pragma_update(Some("encrypted"), pragma, value)?;
    }
    source.execute_batch("DETACH DATABASE encrypted")?;
    let verified = keyed(staging, key, database)?;
    let integrity: String = verified.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    ensure!(
        integrity == "ok",
        "The encrypted {what} failed its integrity check"
    );
    drop(verified);
    drop(source);
    fs::OpenOptions::new()
        .write(true)
        .open(staging)?
        .sync_all()?;
    fs::rename(staging, path)
        .with_context(|| format!("Could not replace the {what} with its encrypted copy"))?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::secrets::{KEYRING_SERVICE, memory::Store};
    use super::*;

    fn directory() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn keyring_service_is_isolated_from_zapfast() {
        assert_eq!(KEYRING_SERVICE, "dev.luminusos.ZapTide");
        assert_ne!(KEYRING_SERVICE, "rocks.zapfast.ZapFast");
    }

    #[test]
    fn identity_differs_per_archive_folder_and_reveals_no_path() {
        let directory = directory();
        let one = directory.path().join("1/archive.db");
        let two = directory.path().join("2/archive.db");
        fs::create_dir_all(one.parent().unwrap()).unwrap();
        fs::create_dir_all(two.parent().unwrap()).unwrap();
        let first = archive_key_identity(&one).unwrap();
        assert_ne!(first, archive_key_identity(&two).unwrap());
        assert!(first.starts_with("archive-") && !first.contains('/'));
    }

    #[test]
    fn copying_the_key_of_a_plaintext_archive_needs_no_keyring() {
        let directory = directory();
        let old = directory.path().join("archive.db");
        fs::write(&old, b"SQLite format 3\0rest").unwrap();
        copy_archive_key(&old, &directory.path().join("accounts/1/archive.db")).unwrap();
    }

    fn keyring<'a>(current: &'a Store, legacy: &'a Store) -> Keyring<'a> {
        Keyring { current, legacy }
    }

    #[test]
    fn an_encrypted_archive_key_is_copied_and_read_back() {
        let directory = directory();
        let old = directory.path().join("archive.db");
        let new = directory.path().join("accounts/1/archive.db");
        fs::write(&old, [7u8; 64]).unwrap();
        let old_identity = archive_key_identity(&old).unwrap();
        let (store, legacy) = (Store::with(&old_identity, &[9u8; 32]), Store::default());
        copy_archive_key_in(&old, &new, &keyring(&store, &legacy)).unwrap();
        let new_identity = archive_key_identity(&new).unwrap();
        assert_eq!(store.read(&new_identity), Some(vec![9u8; 32]));
        forget_archive_key_in(&new_identity, &keyring(&store, &legacy)).unwrap();
        assert_eq!(store.read(&new_identity), None);
        forget_archive_key_in(&new_identity, &keyring(&store, &legacy)).unwrap();
    }

    #[test]
    fn an_encrypted_archive_without_its_key_is_not_copied() {
        let directory = directory();
        let old = directory.path().join("archive.db");
        fs::write(&old, [7u8; 64]).unwrap();
        let (store, legacy) = (Store::default(), Store::default());
        let error = copy_archive_key_in(
            &old,
            &directory.path().join("accounts/1/archive.db"),
            &keyring(&store, &legacy),
        )
        .unwrap_err();
        assert!(error.to_string().contains("missing"));
    }

    #[test]
    fn the_device_store_key_is_derived_and_distinct() {
        let first = derive_device_store_key(&[1; 32]).unwrap();
        assert_eq!(*first, *derive_device_store_key(&[1; 32]).unwrap());
        assert_ne!(*first, [1; 32]);
        assert_ne!(*first, *derive_device_store_key(&[2; 32]).unwrap());
    }

    #[test]
    fn an_encrypted_device_store_blocks_a_new_key() {
        let directory = directory();
        let archive = directory.path().join("archive.db");
        fs::write(directory.path().join("session.db"), [7u8; 64]).unwrap();
        let (store, legacy) = (Store::default(), Store::default());
        assert!(
            key_in(&archive, "archive", &keyring(&store, &legacy))
                .unwrap_err()
                .to_string()
                .contains("device store is encrypted")
        );
        assert_eq!(store.read("archive"), None);
        assert!(!archive.exists());
    }

    #[test]
    fn the_key_of_a_folder_with_only_an_encrypted_device_store_is_copied() {
        let directory = directory();
        let old = directory.path().join("archive.db");
        let new = directory.path().join("accounts/1/archive.db");
        fs::write(directory.path().join("session.db"), [7u8; 64]).unwrap();
        assert!(folder_encrypted(&old).unwrap());
        let (store, legacy) = (Store::default(), Store::default());
        assert!(
            copy_archive_key_in(&old, &new, &keyring(&store, &legacy))
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        store.put(&archive_key_identity(&old).unwrap(), &[9u8; 32]);
        copy_archive_key_in(&old, &new, &keyring(&store, &legacy)).unwrap();
        assert_eq!(
            store.read(&archive_key_identity(&new).unwrap()),
            Some(vec![9u8; 32])
        );
    }

    #[test]
    fn an_existing_archive_and_device_store_keep_their_key_through_migration() {
        let directory = directory();
        let archive = directory.path().join("archive.db");
        let session = directory.path().join("session.db");
        let identity = archive_key_identity(&archive).unwrap();
        let folder_key = [4u8; 32];
        drop(open(&archive, &folder_key, Database::Archive).unwrap());
        let device_key = derive_device_store_key(&folder_key).unwrap();
        open(&session, &device_key, Database::DeviceStore)
            .unwrap()
            .execute_batch("CREATE TABLE fixture(value TEXT)")
            .unwrap();
        let (store, legacy) = (Store::default(), Store::with(&identity, &folder_key));
        let key = key_in(&archive, &identity, &keyring(&store, &legacy)).unwrap();
        assert_eq!(*key, folder_key);
        assert_eq!(store.read(&identity), Some(folder_key.to_vec()));
        assert_eq!(legacy.read(&identity), Some(folder_key.to_vec()));
        keyed(&archive, &key, Database::Archive).unwrap();
        keyed(
            &session,
            &derive_device_store_key(&key).unwrap(),
            Database::DeviceStore,
        )
        .unwrap();
    }

    #[test]
    fn a_failed_migration_neither_uses_nor_replaces_the_key() {
        let directory = directory();
        let archive = directory.path().join("archive.db");
        fs::write(&archive, [7u8; 64]).unwrap();
        let identity = archive_key_identity(&archive).unwrap();
        let (store, legacy) = (Store::default(), Store::with(&identity, &[4u8; 32]));
        store.damage_writes();
        assert!(
            key_in(&archive, &identity, &keyring(&store, &legacy))
                .unwrap_err()
                .to_string()
                .contains("did not retain")
        );
        assert_eq!(store.read(&identity), None);
        assert_eq!(legacy.read(&identity), Some(vec![4u8; 32]));
        assert_eq!(fs::read(&archive).unwrap(), [7u8; 64]);
    }

    #[test]
    fn an_unreachable_old_keyring_never_leads_to_a_new_key_for_encrypted_data() {
        let directory = directory();
        let archive = directory.path().join("archive.db");
        let identity = archive_key_identity(&archive).unwrap();
        let (store, legacy) = (Store::default(), Store::default());
        legacy.fail("no Secret Service");
        fs::write(directory.path().join("session.db"), [7u8; 64]).unwrap();
        assert!(
            key_in(&archive, &identity, &keyring(&store, &legacy))
                .unwrap_err()
                .to_string()
                .contains("Unlock")
        );
        assert_eq!(store.read(&identity), None);
        fs::remove_file(directory.path().join("session.db")).unwrap();
        let key = key_in(&archive, &identity, &keyring(&store, &legacy)).unwrap();
        assert_eq!(store.read(&identity), Some(key.to_vec()));
    }

    #[test]
    fn an_old_key_that_is_missing_too_is_never_replaced() {
        let directory = directory();
        let archive = directory.path().join("archive.db");
        fs::write(&archive, [7u8; 64]).unwrap();
        let identity = archive_key_identity(&archive).unwrap();
        let (store, legacy) = (Store::default(), Store::default());
        assert!(
            key_in(&archive, &identity, &keyring(&store, &legacy))
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        assert_eq!(store.read(&identity), None);
        assert_eq!(legacy.read(&identity), None);
    }

    fn read_secret(connection: &Connection) -> String {
        connection
            .query_row("SELECT value FROM secrets", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn keyring_unlock_reuses_keys_and_never_replaces_a_missing_key() {
        let directory = directory();
        let path = directory.path().join("archive.db");
        let (store, legacy) = (Store::default(), Store::default());
        let entry = keyring(&store, &legacy);
        let key = key_in(&path, "archive", &entry).unwrap();
        assert_eq!(*key, *key_in(&path, "archive", &entry).unwrap());
        let connection = open(&path, &key, Database::Archive).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE example(value TEXT); INSERT INTO example VALUES ('fixture');",
            )
            .unwrap();
        drop(connection);
        let original = fs::read(&path).unwrap();
        entry.delete("archive").unwrap();
        assert!(
            key_in(&path, "archive", &entry)
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        assert_eq!(store.read("archive"), None);
        assert_eq!(fs::read(&path).unwrap(), original);
        store.put("archive", &[1; 12]);
        assert!(
            key_in(&path, "archive", &entry)
                .unwrap_err()
                .to_string()
                .contains("invalid")
        );
        store.fail("locked");
        assert!(
            key_in(&path, "archive", &entry)
                .unwrap_err()
                .to_string()
                .contains("Unlock")
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn encrypted_database_and_wal_reject_missing_or_wrong_keys() {
        let directory = directory();
        let path = directory.path().join("archive.db");
        let connection = open(&path, &[7; 32], Database::Archive).unwrap();
        connection.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE secrets (value TEXT); INSERT INTO secrets VALUES ('private archive marker');").unwrap();
        for file in [&path, &path.with_extension("db-wal")] {
            let bytes = fs::read(file).unwrap();
            assert!(
                !bytes
                    .windows(22)
                    .any(|bytes| bytes == b"private archive marker")
            );
        }
        assert!(!plaintext(&path).unwrap());
        assert!(open(&path, &[8; 32], Database::Archive).is_err());
        assert!(
            Connection::open(&path)
                .unwrap()
                .query_row("SELECT count(*) FROM sqlite_master", [], |row| row
                    .get::<_, i64>(0))
                .is_err()
        );
        drop(connection);
        assert_eq!(
            read_secret(&open(&path, &[7; 32], Database::Archive).unwrap()),
            "private archive marker"
        );
    }

    #[test]
    fn migration_preserves_wal_data_schema_and_version() {
        let directory = directory();
        let path = directory.path().join("archive.db");
        let source = Connection::open(&path).unwrap();
        source.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA user_version=9; PRAGMA application_id=42; CREATE TABLE secrets (value TEXT); CREATE INDEX secret_values ON secrets(value); INSERT INTO secrets VALUES ('from the phone');").unwrap();
        // Leave the committed WAL on disk as after an interrupted old process.
        source
            .set_db_config(
                rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                true,
            )
            .unwrap();
        drop(source);
        assert!(path.with_extension("db-wal").exists());
        fs::write(path.with_extension("db.encrypting"), b"interrupted export").unwrap();
        let encrypted = open(&path, &[9; 32], Database::Archive).unwrap();
        assert_eq!(read_secret(&encrypted), "from the phone");
        assert_eq!(
            encrypted
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            9
        );
        assert_eq!(
            encrypted
                .pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))
                .unwrap(),
            42
        );
        assert_eq!(
            encrypted
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='secret_values'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert!(!plaintext(&path).unwrap());
        assert!(!path.with_extension("db.encrypting").exists());
        assert!(!path.with_extension("db-wal").exists());
    }

    #[test]
    fn a_failed_migration_leaves_the_original_readable() {
        let directory = directory();
        let path = directory.path().join("archive.db");
        let source = Connection::open(&path).unwrap();
        source
            .execute_batch(
                "CREATE TABLE secrets(value TEXT); INSERT INTO secrets VALUES ('keep me');",
            )
            .unwrap();
        drop(source);
        // A blocked staging path simulates a filesystem failure before replace.
        fs::create_dir(path.with_extension("db.encrypting")).unwrap();
        assert!(open(&path, &[9; 32], Database::Archive).is_err());
        assert!(plaintext(&path).unwrap());
        assert_eq!(read_secret(&Connection::open(&path).unwrap()), "keep me");
        fs::remove_dir(path.with_extension("db.encrypting")).unwrap();
        assert_eq!(
            read_secret(&open(&path, &[9; 32], Database::Archive).unwrap()),
            "keep me"
        );
    }
}
