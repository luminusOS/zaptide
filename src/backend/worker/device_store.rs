//! The whatsapp-rust device store, encrypted with SQLCipher.
//!
//! The file holds the device identity, Signal sessions and app-state keys, so
//! anyone who can read it can clone the linked device. It is keyed with the
//! account folder's keyring key (see `archive::device_store_key`), and a
//! plaintext store from an older build is encrypted in place before the
//! library opens it.
//!
//! The protocol library owns the device schema and all message-secret access.
//! Its new migration drops an unused column that 0.14 still writes. Retain that
//! column after migration, without reading or rewriting any stored secrets.

use std::{path::Path, sync::Arc};

use anyhow::{Context, Result};
use diesel::{RunQueryDsl, SqliteConnection, sql_types::Text};
use whatsapp_rust::store::{SqliteStore, SqliteStoreConfig};
use zeroize::Zeroizing;

use crate::archive::EncryptedDatabase;

type HookResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub(super) async fn open(path: &Path) -> Result<SqliteStore> {
    let session = path.to_owned();
    let key = tokio::task::spawn_blocking(move || crate::archive::device_store_key(&session))
        .await
        .context("Device store key task failed")??;
    open_with_key(path, key).await
}

async fn open_with_key(path: &Path, key: Zeroizing<[u8; 32]>) -> Result<SqliteStore> {
    let session = path.to_owned();
    let key = tokio::task::spawn_blocking(move || {
        crate::archive::open_encrypted(&session, &key, EncryptedDatabase::DeviceStore)?;
        Ok::<_, anyhow::Error>(key)
    })
    .await
    .context("Device store encryption task failed")??;
    let literal = Arc::new(crate::archive::sqlcipher_key_literal(&key));
    let config = SqliteStoreConfig::default()
        .with_connection_init(move |connection| unlock(connection, &literal));
    let store = SqliteStore::with_config(&path.to_string_lossy(), config).await;
    let path = path.to_owned();
    // Also repair after a later migration fails, before returning its error.
    tokio::task::spawn_blocking(move || preserve_legacy_column(&path, &key))
        .await
        .context("Device store compatibility task failed")??;
    store.context("Could not open the device store")
}

#[derive(diesel::QueryableByName)]
struct CipherVersion {
    #[diesel(sql_type = Text)]
    cipher_version: String,
}

/// Runs first on every pooled connection, readers included, before the
/// library's own pragmas, WAL setup and migrations.
fn unlock(connection: &mut SqliteConnection, literal: &Zeroizing<String>) -> HookResult {
    // diesel and SQLite keep their own unwiped copies of this statement, and
    // diesel instrumentation must never be enabled on this connection.
    let mut statement = Zeroizing::new(String::with_capacity(literal.len() + 16));
    statement.push_str("PRAGMA key = \"");
    statement.push_str(literal);
    statement.push('"');
    diesel::sql_query(statement.as_str()).execute(connection)?;
    // Without SQLCipher `PRAGMA key` is silently ignored and the store would
    // be written in plaintext.
    let version = diesel::sql_query("PRAGMA cipher_version")
        .get_result::<CipherVersion>(connection)
        .map_err(|_| "This build does not support encrypted device stores")?;
    if version.cipher_version.is_empty() {
        return Err("This build does not support encrypted device stores".into());
    }
    // PRAGMA key alone does not verify a key. Read a page before anything else.
    diesel::sql_query("SELECT count(*) FROM sqlite_master")
        .execute(connection)
        .map_err(|_| "The device store could not be unlocked with its OS keyring key")?;
    Ok(())
}

fn preserve_legacy_column(path: &Path, key: &[u8; 32]) -> Result<()> {
    if !path.try_exists()? {
        return Ok(());
    }
    let mut connection = crate::archive::open_keyed(path, key, EncryptedDatabase::DeviceStore)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let columns = transaction
        .prepare("PRAGMA table_info(msg_secrets)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.is_empty() && !columns.iter().any(|column| column == "created_at") {
        transaction.execute_batch(
            "ALTER TABLE msg_secrets ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use whatsapp_rust::store::traits::SignalStore;

    const KEY: [u8; 32] = [5; 32];
    const MARKER: &[u8] = b"synthetic device store marker";

    fn key() -> Zeroizing<[u8; 32]> {
        Zeroizing::new(KEY)
    }

    fn keyed(path: &Path) -> Connection {
        crate::archive::open_keyed(path, &KEY, EncryptedDatabase::DeviceStore).unwrap()
    }

    fn insert_secret(connection: &Connection, id: &str, secret: &[u8]) {
        // This is the column set used by the old library. All values are fixtures.
        connection.execute(
            "INSERT INTO msg_secrets (chat, sender, msg_id, secret, device_id, created_at, message_ts, expires_at)
             VALUES ('fixture', 'fixture', ?1, ?2, 1, 1, 1, 9223372036854775807)",
            rusqlite::params![id, secret],
        ).unwrap();
    }

    fn row_counts(connection: &Connection) -> Vec<(String, i64)> {
        let tables = connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        tables
            .into_iter()
            .map(|table| {
                let count = connection
                    .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                (table, count)
            })
            .collect()
    }

    fn unreadable_without_key(path: &Path) -> bool {
        Connection::open(path)
            .unwrap()
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            })
            .is_err()
    }

    fn contains_marker(path: &Path) -> bool {
        std::fs::read(path)
            .unwrap()
            .windows(MARKER.len())
            .any(|bytes| bytes == MARKER)
    }

    async fn plaintext_fixture(path: &Path) -> Vec<(String, i64)> {
        drop(SqliteStore::new(&path.to_string_lossy()).await.unwrap());
        let connection = Connection::open(path).unwrap();
        // As the previous build's compatibility step left it.
        connection
            .execute_batch(
                "ALTER TABLE msg_secrets ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0",
            )
            .unwrap();
        insert_secret(&connection, "one", MARKER);
        insert_secret(&connection, "two", b"\x01");
        // Leave the committed WAL on disk as after an interrupted old process.
        connection
            .set_db_config(
                rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                true,
            )
            .unwrap();
        let counts = row_counts(&connection);
        drop(connection);
        assert!(contains_marker(path) || contains_marker(&path.with_extension("db-wal")));
        counts
    }

    #[tokio::test]
    async fn library_migration_keeps_legacy_writes_working() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.db");
        let store = open_with_key(&path, key()).await.unwrap();
        store.put_identity("fixture", [3; 32]).await.unwrap();
        let connection = keyed(&path);
        insert_secret(&connection, "fixture", b"\x00");
        drop(connection);
        drop(store);
        let store = open_with_key(&path, key()).await.unwrap();
        assert_eq!(store.load_identity("fixture").await.unwrap(), Some([3; 32]));
        let connection = keyed(&path);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM msg_secrets", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(unreadable_without_key(&path));
    }

    #[tokio::test]
    async fn the_store_and_its_wal_are_encrypted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.db");
        let store = open_with_key(&path, key()).await.unwrap();
        let connection = keyed(&path);
        insert_secret(&connection, "marker", MARKER);
        drop(connection);
        let wal = path.with_extension("db-wal");
        assert!(wal.exists(), "the open store keeps its WAL");
        assert!(!contains_marker(&path));
        assert!(!contains_marker(&wal));
        assert!(unreadable_without_key(&path));
        assert!(open_with_key(&path, Zeroizing::new([6; 32])).await.is_err());
        drop(store);
        assert!(!contains_marker(&path));
        assert!(unreadable_without_key(&path));
    }

    #[tokio::test]
    async fn a_plaintext_store_migrates_with_the_same_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.db");
        let counts = plaintext_fixture(&path).await;
        let store = open_with_key(&path, key()).await.unwrap();
        assert_eq!(row_counts(&keyed(&path)), counts);
        assert!(unreadable_without_key(&path));
        assert!(!contains_marker(&path));
        assert!(!path.with_extension("db.encrypting").exists());
        drop(store);
        let _store = open_with_key(&path, key()).await.unwrap();
        assert_eq!(row_counts(&keyed(&path)), counts);
    }

    #[tokio::test]
    async fn an_interrupted_migration_recovers_on_the_next_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.db");
        let counts = plaintext_fixture(&path).await;
        // A crash between the export and the rename leaves the plaintext
        // original in place beside a complete encrypted staging copy.
        let copy = directory.path().join("copy/session.db");
        std::fs::create_dir(copy.parent().unwrap()).unwrap();
        std::fs::copy(&path, &copy).unwrap();
        let wal = path.with_extension("db-wal");
        if wal.exists() {
            std::fs::copy(&wal, copy.with_extension("db-wal")).unwrap();
        }
        drop(crate::archive::open_encrypted(&copy, &KEY, EncryptedDatabase::DeviceStore).unwrap());
        let staging = path.with_extension("db.encrypting");
        std::fs::rename(&copy, &staging).unwrap();
        let _store = open_with_key(&path, key()).await.unwrap();
        assert_eq!(row_counts(&keyed(&path)), counts);
        assert!(!staging.exists());
        assert!(unreadable_without_key(&path));
    }

    fn counting_unlock(
        key: &[u8; 32],
        calls: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> SqliteStoreConfig {
        let literal = crate::archive::sqlcipher_key_literal(key);
        let calls = Arc::clone(calls);
        SqliteStoreConfig::default().with_connection_init(move |connection| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            unlock(connection, &literal)
        })
    }

    #[tokio::test]
    async fn the_hook_rejects_a_wrong_key_and_unlocks_every_connection() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.db");
        drop(open_with_key(&path, key()).await.unwrap());
        let url = path.to_string_lossy();

        // Through with_config a wrong key only fails after r2d2's 30 s
        // connection timeout, so the hook is called on a bare connection.
        let mut connection = <SqliteConnection as diesel::Connection>::establish(&url).unwrap();
        let wrong = crate::archive::sqlcipher_key_literal(&[6; 32]);
        let error = unlock(&mut connection, &wrong).unwrap_err();
        assert_eq!(
            error.to_string(),
            "The device store could not be unlocked with its OS keyring key"
        );

        let calls = Arc::new(AtomicUsize::new(0));
        let store = SqliteStore::with_config(&url, counting_unlock(&KEY, &calls))
            .await
            .unwrap();
        assert!(
            calls.load(Ordering::Relaxed) >= 2,
            "the writer and the reader connection both ran the hook"
        );
        store.put_identity("fixture", [4; 32]).await.unwrap();
        assert_eq!(store.load_identity("fixture").await.unwrap(), Some([4; 32]));
    }

    #[test]
    fn an_existing_empty_store_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.db");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(crate::archive::open_encrypted(&path, &KEY, EncryptedDatabase::DeviceStore).unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn a_wrong_key_is_not_reported_as_a_failed_encryption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.db");
        drop(open_with_key(&path, key()).await.unwrap());
        let error = crate::archive::open_encrypted(&path, &[6; 32], EncryptedDatabase::DeviceStore)
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("device store could not be unlocked"),
            "{message}"
        );
        assert!(!message.contains("encrypt"), "{message}");
    }
}
