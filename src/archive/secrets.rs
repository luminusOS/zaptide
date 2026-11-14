//! The OS keyring items holding each account folder's database key.
//!
//! Inside a sandbox with a Secret portal, keys are stored in oo7's file
//! keyring, encrypted with a per-app secret the portal keeps in the host
//! keyring. Builds before oo7 wrote them to the host Secret Service, so a key
//! missing from the file keyring is looked up there once, copied, and read
//! back from disk before use; that old item is only ever read. Outside a
//! sandbox, or without a Secret portal, keys stay in the host Secret Service
//! as before: found in any collection, created in the default one only when
//! no item exists.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use zeroize::Zeroizing;

pub(super) const KEYRING_SERVICE: &str = "dev.luminusos.ZapTide";

pub(super) type Secret = Zeroizing<Vec<u8>>;

/// Where keys are kept and created.
pub(super) trait SecretStore {
    /// `Ok(None)` when no item exists; an error when the store cannot tell.
    fn get(&self, identity: &str) -> Result<Option<Secret>>;
    /// As `get`, but read anew from where the store persists it.
    fn reread(&self, identity: &str) -> Result<Option<Secret>> {
        self.get(identity)
    }
    fn set(&self, identity: &str, secret: &[u8]) -> Result<()>;
    fn delete(&self, identity: &str) -> Result<()>;
}

/// Where builds before oo7 kept keys. Read-only on purpose: an old build or
/// a failed migration may still need the item.
pub(super) trait LegacySecretStore {
    fn get(&self, identity: &str) -> Result<Option<Secret>>;
}

/// The result of looking a key up.
pub(super) enum Lookup {
    Found(Secret),
    /// Neither store has the key. `legacy` holds why the old store could not
    /// be searched, which matters when a database already needs that key.
    Missing {
        legacy: Option<anyhow::Error>,
    },
}

pub(super) struct Keyring<'a> {
    pub(super) current: &'a dyn SecretStore,
    pub(super) legacy: &'a dyn LegacySecretStore,
}

impl Keyring<'_> {
    pub(super) fn get(&self, identity: &str) -> Result<Lookup> {
        if let Some(secret) = self
            .current
            .get(identity)
            .context("Unlock your OS keyring and restart ZapTide")?
        {
            return Ok(Lookup::Found(secret));
        }
        let old = match self.legacy.get(identity) {
            Ok(Some(old)) => old,
            Ok(None) => return Ok(Lookup::Missing { legacy: None }),
            Err(error) => {
                return Ok(Lookup::Missing {
                    legacy: Some(error),
                });
            }
        };
        self.current
            .set(identity, &old)
            .context("Could not copy the archive key into the OS keyring")?;
        let saved = self
            .current
            .reread(identity)
            .context("Could not verify the copied archive key")?;
        if saved.as_deref().map(Vec::as_slice) == Some(old.as_slice()) {
            return Ok(Lookup::Found(old));
        }
        // Only this copy goes: the original stays where it was, and the next
        // start migrates from it again instead of trusting a damaged copy.
        if let Err(error) = self.current.delete(identity) {
            log::warn!("could not remove a damaged copy of an archive key: {error:#}");
        }
        Err(anyhow!(
            "The OS keyring did not retain the copied archive key. The original key and the archive have not been changed"
        ))
    }

    pub(super) fn set(&self, identity: &str, secret: &[u8]) -> Result<()> {
        self.current.set(identity, secret)
    }

    /// Reads back from the current store only, never migrating.
    pub(super) fn get_saved(&self, identity: &str) -> Result<Option<Secret>> {
        self.current.reread(identity)
    }

    pub(super) fn delete(&self, identity: &str) -> Result<()> {
        self.current.delete(identity)
    }
}

type Job = Box<dyn FnOnce() + Send>;

/// Every keyring operation runs, in order, on one thread of its own. The GTK
/// thread never waits on a keyring prompt, and a key deleted for a removed
/// account is gone before a new account in the same folder creates its key.
fn jobs() -> Result<&'static Mutex<mpsc::Sender<Job>>> {
    static JOBS: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    if let Some(jobs) = JOBS.get() {
        return Ok(jobs);
    }
    let (sender, receiver) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("keyring".into())
        .spawn(move || {
            // oo7's host service spawns session cleanup from Drop on this thread.
            let _runtime = crate::portal::enter();
            receiver.into_iter().for_each(|job| job());
        })
        .context("Could not start the keyring thread")?;
    Ok(JOBS.get_or_init(|| Mutex::new(sender)))
}

fn queue(job: Job) -> Result<()> {
    crate::shared_resource::lock(jobs()?)
        .send(job)
        .map_err(|_| anyhow!("The OS keyring thread stopped"))
}

fn with_stores<T>(operation: impl FnOnce(&Keyring<'_>) -> Result<T>) -> Result<T> {
    // Tests run on developer machines with a real keyring; a test that
    // reaches it would create or delete the user's own items.
    if cfg!(test) {
        anyhow::bail!("tests must not use the OS keyring");
    }
    let current = Oo7::open().context("Unlock your OS keyring and restart ZapTide")?;
    // Only oo7's file keyring is a new place. Outside it the key already is
    // where builds before oo7 kept it, and copying it would duplicate it.
    let legacy: &dyn LegacySecretStore = match *current.backend {
        Backend::File(_) => &SecretService,
        Backend::Host(_) => &NoLegacy,
    };
    operation(&Keyring {
        current: &current,
        legacy,
    })
}

/// Runs `operation` with the OS keyring stores and waits for its result.
pub(super) fn with_os_keyring<T: Send + 'static>(
    operation: impl FnOnce(&Keyring<'_>) -> Result<T> + Send + 'static,
) -> Result<T> {
    let (sender, receiver) = mpsc::sync_channel(1);
    queue(Box::new(move || {
        let _ = sender.send(with_stores(operation));
    }))?;
    receiver
        .recv()
        .map_err(|_| anyhow!("The OS keyring thread stopped"))?
}

/// As `with_os_keyring`, but gives up after `wait`. The operation still
/// finishes on the keyring thread; only the caller stops waiting.
pub(super) fn with_os_keyring_within<T: Send + 'static>(
    wait: Duration,
    operation: impl FnOnce(&Keyring<'_>) -> Result<T> + Send + 'static,
) -> Result<T> {
    let (sender, receiver) = mpsc::sync_channel(1);
    queue(Box::new(move || {
        let _ = sender.send(with_stores(operation));
    }))?;
    match receiver.recv_timeout(wait) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(anyhow!(
            "The OS keyring did not answer within {} seconds. Unlock it and restart ZapTide",
            wait.as_secs()
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!("The OS keyring thread stopped")),
    }
}

/// Runs `operation` on the keyring thread without waiting; its error is logged.
pub(super) fn with_os_keyring_later(
    what: &'static str,
    operation: impl FnOnce(&Keyring<'_>) -> Result<()> + Send + 'static,
) -> Result<()> {
    queue(Box::new(move || {
        if let Err(error) = with_stores(operation) {
            log::warn!("could not {what}: {error:#}");
        }
    }))
}

fn attributes(identity: &str) -> [(&'static str, String); 2] {
    // The attributes and label the earlier keyring crate used, so a build
    // before oo7 still finds a key created outside a sandbox.
    [
        ("service", KEYRING_SERVICE.to_owned()),
        ("username", identity.to_owned()),
    ]
}

fn label(identity: &str) -> String {
    format!("keyring:{identity}@{KEYRING_SERVICE}")
}

fn block_on<T: Send + 'static>(
    future: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    // ashpd's portal connection lives on the portal runtime, and oo7 reaches
    // the Secret portal through it.
    crate::portal::run(future).context("The OS keyring task stopped")?
}

fn unique<T>(mut items: Vec<T>) -> Result<Option<T>> {
    match items.len() {
        0 => Ok(None),
        1 => Ok(items.pop()),
        _ => Err(anyhow!(
            "The OS keyring holds more than one archive key for this folder"
        )),
    }
}

/// The one item for `attributes` in any collection, as the earlier keyring
/// crate searched: a key may live outside the default collection.
async fn host_item(
    service: &oo7::dbus::Service,
    attributes: &[(&'static str, String); 2],
) -> Result<Option<oo7::dbus::Item>> {
    let mut items = Vec::new();
    for collection in service.collections().await? {
        items.extend(collection.search_items(attributes).await?);
    }
    let Some(item) = unique(items)? else {
        return Ok(None);
    };
    if item.is_locked().await? {
        item.unlock(None).await?;
    }
    Ok(Some(item))
}

/// The oo7 file keyring's folder, as oo7 0.6 resolves it.
fn file_keyring_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|data| data.join("keyrings"))
}

enum Backend {
    /// A sandbox with a Secret portal: oo7's file keyring.
    File(oo7::Keyring),
    /// The host Secret Service, searching every collection.
    Host(oo7::dbus::Service),
}

struct Oo7 {
    backend: Arc<Backend>,
}

fn portal_unavailable(error: &oo7::Error) -> bool {
    matches!(error, oo7::Error::File(error)
        if matches!(**error, oo7::file::Error::Portal(ashpd::Error::PortalNotFound(_))))
}

impl Oo7 {
    fn open() -> Result<Self> {
        let backend = block_on(async {
            if !ashpd::is_sandboxed() {
                return Ok(Backend::Host(oo7::dbus::Service::new().await?));
            }
            match oo7::Keyring::sandboxed().await {
                Ok(keyring) => {
                    if keyring.is_locked().await? {
                        keyring.unlock().await?;
                    }
                    Ok(Backend::File(keyring))
                }
                Err(error) if portal_unavailable(&error) => {
                    log::warn!("no Secret portal; keeping keys in the host Secret Service");
                    Ok(Backend::Host(oo7::dbus::Service::new().await?))
                }
                Err(error) => Err(error.into()),
            }
        })?;
        Ok(Self {
            backend: Arc::new(backend),
        })
    }
}

impl SecretStore for Oo7 {
    fn get(&self, identity: &str) -> Result<Option<Secret>> {
        let backend = Arc::clone(&self.backend);
        let attributes = attributes(identity);
        block_on(async move {
            let secret = match &*backend {
                Backend::File(keyring) => {
                    let Some(item) = unique(keyring.search_items(&attributes).await?)? else {
                        return Ok(None);
                    };
                    if item.is_locked().await? {
                        item.unlock().await?;
                    }
                    item.secret().await?
                }
                Backend::Host(service) => {
                    let Some(item) = host_item(service, &attributes).await? else {
                        return Ok(None);
                    };
                    item.secret().await?
                }
            };
            Ok(Some(Zeroizing::new(secret.as_bytes().to_vec())))
        })
    }

    fn reread(&self, identity: &str) -> Result<Option<Secret>> {
        // A new instance loads the file keyring from disk again, so this
        // proves what was written there, not what the open one remembers.
        Self::open()?.get(identity)
    }

    fn set(&self, identity: &str, secret: &[u8]) -> Result<()> {
        let backend = Arc::clone(&self.backend);
        let attributes = attributes(identity);
        let label = label(identity);
        let secret = oo7::Secret::blob(secret);
        block_on(async move {
            match &*backend {
                Backend::File(keyring) => {
                    keyring
                        .create_item(&label, &attributes, secret, true)
                        .await?;
                    // oo7 syncs the file before renaming it into place, but
                    // not the folder, so the rename itself could be lost.
                    if let Some(folder) = file_keyring_dir() {
                        std::fs::File::open(folder)?.sync_all()?;
                    }
                }
                Backend::Host(service) => {
                    // Update the one existing item wherever it is; a second
                    // copy would make older builds fail as ambiguous.
                    if let Some(item) = host_item(service, &attributes).await? {
                        item.set_secret(secret).await?;
                    } else {
                        service
                            .default_collection()
                            .await?
                            .create_item(&label, &attributes, secret, true, None)
                            .await?;
                    }
                }
            }
            Ok(())
        })
    }

    fn delete(&self, identity: &str) -> Result<()> {
        let backend = Arc::clone(&self.backend);
        let attributes = attributes(identity);
        block_on(async move {
            match &*backend {
                Backend::File(keyring) => keyring.delete(&attributes).await?,
                Backend::Host(service) => {
                    for collection in service.collections().await? {
                        for item in collection.search_items(&attributes).await? {
                            item.delete(None).await?;
                        }
                    }
                }
            }
            Ok(())
        })
    }
}

/// The Secret Service as the earlier keyring crate searched it: every
/// collection, unlocking matches. Inside Flatpak it is reachable only through
/// the `org.freedesktop.secrets` talk-name.
struct SecretService;

impl LegacySecretStore for SecretService {
    fn get(&self, identity: &str) -> Result<Option<Secret>> {
        let attributes = attributes(identity);
        block_on(async move {
            let service = oo7::dbus::Service::new().await?;
            let Some(item) = host_item(&service, &attributes).await? else {
                return Ok(None);
            };
            Ok(Some(Zeroizing::new(
                item.secret().await?.as_bytes().to_vec(),
            )))
        })
    }
}

/// The current store already is the old one, so there is nothing to migrate.
struct NoLegacy;

impl LegacySecretStore for NoLegacy {
    fn get(&self, _identity: &str) -> Result<Option<Secret>> {
        Ok(None)
    }
}

#[cfg(test)]
pub(super) mod memory {
    use std::{collections::HashMap, sync::Mutex};

    use super::*;

    /// An in-memory keyring for tests.
    #[derive(Default)]
    pub(in crate::archive) struct Store {
        items: Mutex<HashMap<String, Vec<u8>>>,
        failure: Mutex<Option<String>>,
        damaged_writes: Mutex<bool>,
        unpersisted_writes: Mutex<bool>,
        unpersisted: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl Store {
        pub(in crate::archive) fn with(identity: &str, secret: &[u8]) -> Self {
            let store = Self::default();
            store.put(identity, secret);
            store
        }

        pub(in crate::archive) fn put(&self, identity: &str, secret: &[u8]) {
            self.items
                .lock()
                .unwrap()
                .insert(identity.to_owned(), secret.to_vec());
        }

        pub(in crate::archive) fn read(&self, identity: &str) -> Option<Vec<u8>> {
            self.items.lock().unwrap().get(identity).cloned()
        }

        /// Every later call fails, as a locked or absent keyring does.
        pub(in crate::archive) fn fail(&self, reason: &str) {
            *self.failure.lock().unwrap() = Some(reason.to_owned());
        }

        /// Later writes store different bytes than they were given.
        pub(in crate::archive) fn damage_writes(&self) {
            *self.damaged_writes.lock().unwrap() = true;
        }

        /// Later writes are seen by this instance but never reach disk, as
        /// when the file keyring could not be written.
        pub(in crate::archive) fn lose_writes(&self) {
            *self.unpersisted_writes.lock().unwrap() = true;
        }

        fn check(&self) -> Result<()> {
            match &*self.failure.lock().unwrap() {
                Some(reason) => Err(anyhow!("{reason}")),
                None => Ok(()),
            }
        }
    }

    impl SecretStore for Store {
        fn get(&self, identity: &str) -> Result<Option<Secret>> {
            self.check()?;
            if let Some(secret) = self.unpersisted.lock().unwrap().get(identity) {
                return Ok(Some(Zeroizing::new(secret.clone())));
            }
            Ok(self.read(identity).map(Zeroizing::new))
        }

        fn reread(&self, identity: &str) -> Result<Option<Secret>> {
            self.check()?;
            Ok(self.read(identity).map(Zeroizing::new))
        }

        fn set(&self, identity: &str, secret: &[u8]) -> Result<()> {
            self.check()?;
            let mut secret = secret.to_vec();
            if *self.damaged_writes.lock().unwrap()
                && let Some(byte) = secret.first_mut()
            {
                *byte ^= 0xff;
            }
            if *self.unpersisted_writes.lock().unwrap() {
                self.unpersisted
                    .lock()
                    .unwrap()
                    .insert(identity.to_owned(), secret);
                return Ok(());
            }
            self.put(identity, &secret);
            Ok(())
        }

        fn delete(&self, identity: &str) -> Result<()> {
            self.check()?;
            self.items.lock().unwrap().remove(identity);
            self.unpersisted.lock().unwrap().remove(identity);
            Ok(())
        }
    }

    impl LegacySecretStore for Store {
        fn get(&self, identity: &str) -> Result<Option<Secret>> {
            SecretStore::get(self, identity)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory::Store;
    use super::*;

    #[test]
    fn only_missing_portal_allows_host_fallback() {
        let missing = oo7::Error::from(oo7::file::Error::Portal(ashpd::Error::PortalNotFound(
            "org.freedesktop.portal.Secret".try_into().unwrap(),
        )));
        let denied = oo7::Error::from(oo7::file::Error::Portal(ashpd::Error::NoResponse));
        assert!(portal_unavailable(&missing));
        assert!(!portal_unavailable(&denied));
    }

    #[test]
    fn file_backend_reads_oo7_06_fixture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("default.keyring");
        std::fs::write(
            &path,
            include_bytes!("../../tests/fixtures/oo7-0.6/default.keyring"),
        )
        .unwrap();
        block_on(async move {
            let keyring =
                oo7::Keyring::sandboxed_with_path(&path, oo7::Secret::blob("test")).await?;
            let items = keyring.items().await?;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].secret().await?.as_bytes(), b"foo");
            keyring
                .create_item(
                    &label("synthetic-archive"),
                    &attributes("synthetic-archive"),
                    oo7::Secret::blob(b"synthetic archive key"),
                    true,
                )
                .await?;
            keyring
                .create_item(
                    &label("synthetic-archive"),
                    &attributes("synthetic-archive"),
                    oo7::Secret::blob(b"updated synthetic archive key"),
                    true,
                )
                .await?;
            drop(keyring);
            let reopened =
                oo7::Keyring::sandboxed_with_path(&path, oo7::Secret::blob("test")).await?;
            let matches = reopened
                .search_items(&attributes("synthetic-archive"))
                .await?;
            assert_eq!(matches.len(), 1);
            assert_eq!(
                matches[0].secret().await?.as_bytes(),
                b"updated synthetic archive key"
            );
            let all_items = reopened.items().await?;
            assert_eq!(all_items.len(), 2);
            let mut original_preserved = false;
            for item in all_items {
                if item.secret().await?.as_bytes() == b"foo" {
                    original_preserved = true;
                }
            }
            assert!(original_preserved);
            assert!(
                oo7::Keyring::sandboxed_with_path(&path, oo7::Secret::blob("wrong"))
                    .await
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn file_backend_reopens_existing_key_and_rejects_wrong_secret() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("default.keyring");
        let path_copy = path.clone();
        block_on(async move {
            let password = oo7::Secret::blob(b"synthetic portal secret");
            let keyring = oo7::Keyring::sandboxed_with_path(&path_copy, password.clone()).await?;
            keyring
                .create_item(
                    &label("synthetic"),
                    &attributes("synthetic"),
                    oo7::Secret::blob(b"synthetic archive key"),
                    true,
                )
                .await?;
            drop(keyring);
            let reopened = oo7::Keyring::sandboxed_with_path(&path_copy, password).await?;
            let backend = Oo7 {
                backend: Arc::new(Backend::File(reopened)),
            };
            let items = match &*backend.backend {
                Backend::File(keyring) => keyring.search_items(&attributes("synthetic")).await?,
                Backend::Host(_) => unreachable!(),
            };
            assert_eq!(
                items[0].secret().await?.as_bytes(),
                b"synthetic archive key"
            );
            assert!(
                oo7::Keyring::sandboxed_with_path(&path_copy, oo7::Secret::blob(b"wrong secret"))
                    .await
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn keyring_jobs_can_spawn_cleanup_on_drop_and_process_the_next_job() {
        struct Session(mpsc::Sender<()>);

        impl Drop for Session {
            fn drop(&mut self) {
                let sender = self.0.clone();
                tokio::spawn(async move {
                    sender.send(()).unwrap();
                });
            }
        }

        let (sender, receiver) = mpsc::channel();
        queue(Box::new(move || {
            let session = block_on(async { Ok(Session(sender)) }).unwrap();
            drop(session);
        }))
        .unwrap();
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();

        let (sender, receiver) = mpsc::channel();
        queue(Box::new(move || sender.send(()).unwrap())).unwrap();
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    fn found(lookup: Lookup) -> Vec<u8> {
        match lookup {
            Lookup::Found(secret) => secret.to_vec(),
            Lookup::Missing { .. } => panic!("the key should be found"),
        }
    }

    #[test]
    fn a_fresh_install_finds_no_key_anywhere() {
        let (current, legacy) = (Store::default(), Store::default());
        let keyring = Keyring {
            current: &current,
            legacy: &legacy,
        };
        assert!(matches!(
            keyring.get("archive-1").unwrap(),
            Lookup::Missing { legacy: None }
        ));
        assert_eq!(current.read("archive-1"), None);
    }

    #[test]
    fn a_current_key_is_used_without_asking_the_old_store() {
        let current = Store::with("archive-1", &[1; 32]);
        let legacy = Store::with("archive-1", &[2; 32]);
        legacy.fail("the old store must not be asked");
        let keyring = Keyring {
            current: &current,
            legacy: &legacy,
        };
        assert_eq!(found(keyring.get("archive-1").unwrap()), vec![1; 32]);
    }

    #[test]
    fn an_old_key_is_copied_read_back_and_kept() {
        let current = Store::default();
        let legacy = Store::with("archive-1", &[7; 32]);
        let keyring = Keyring {
            current: &current,
            legacy: &legacy,
        };
        assert_eq!(found(keyring.get("archive-1").unwrap()), vec![7; 32]);
        assert_eq!(current.read("archive-1"), Some(vec![7; 32]));
        assert_eq!(legacy.read("archive-1"), Some(vec![7; 32]));
        legacy.fail("migrated keys are not looked up again");
        assert_eq!(found(keyring.get("archive-1").unwrap()), vec![7; 32]);
    }

    #[test]
    fn a_copy_that_does_not_read_back_is_neither_used_nor_kept() {
        let current = Store::default();
        current.damage_writes();
        let legacy = Store::with("archive-1", &[7; 32]);
        let keyring = Keyring {
            current: &current,
            legacy: &legacy,
        };
        let error = keyring.get("archive-1").err().unwrap();
        assert!(error.to_string().contains("did not retain"));
        assert_eq!(current.read("archive-1"), None);
        assert_eq!(legacy.read("archive-1"), Some(vec![7; 32]));
    }

    #[test]
    fn an_unreachable_old_store_is_reported_as_missing_with_its_reason() {
        let current = Store::default();
        let legacy = Store::with("archive-1", &[7; 32]);
        legacy.fail("no Secret Service");
        let keyring = Keyring {
            current: &current,
            legacy: &legacy,
        };
        match keyring.get("archive-1").unwrap() {
            Lookup::Missing {
                legacy: Some(error),
            } => assert!(error.to_string().contains("no Secret Service")),
            _ => panic!("the key should be missing with the old store's error"),
        }
        assert_eq!(current.read("archive-1"), None);
        assert_eq!(legacy.read("archive-1"), Some(vec![7; 32]));
    }

    #[test]
    fn an_unavailable_current_store_stops_before_the_old_one() {
        let current = Store::default();
        current.fail("no keyring");
        let legacy = Store::with("archive-1", &[7; 32]);
        let keyring = Keyring {
            current: &current,
            legacy: &legacy,
        };
        assert!(keyring.get("archive-1").is_err());
        assert_eq!(legacy.read("archive-1"), Some(vec![7; 32]));
    }

    #[test]
    fn old_items_use_the_earlier_attributes() {
        assert_eq!(
            attributes("archive-1"),
            [
                ("service", "dev.luminusos.ZapTide".to_owned()),
                ("username", "archive-1".to_owned())
            ]
        );
        assert_eq!(
            label("archive-1"),
            "keyring:archive-1@dev.luminusos.ZapTide"
        );
    }

    #[test]
    fn a_copy_is_read_back_from_disk_not_from_memory() {
        let current = Store::default();
        current.lose_writes();
        let legacy = Store::with("archive-1", &[7; 32]);
        let keyring = Keyring {
            current: &current,
            legacy: &legacy,
        };
        let error = keyring.get("archive-1").err().unwrap();
        assert!(error.to_string().contains("did not retain"));
        assert!(SecretStore::get(&current, "archive-1").unwrap().is_none());
        assert_eq!(legacy.read("archive-1"), Some(vec![7; 32]));

        current.put("archive-2", &[1; 32]);
        current.set("archive-3", &[3; 32]).unwrap();
        let keyring = Keyring {
            current: &current,
            legacy: &NoLegacy,
        };
        assert_eq!(
            &**keyring.get_saved("archive-2").unwrap().unwrap(),
            &[1; 32]
        );
        assert!(keyring.get_saved("archive-3").unwrap().is_none());
    }

    #[test]
    fn without_a_new_place_nothing_is_migrated() {
        let current = Store::default();
        let keyring = Keyring {
            current: &current,
            legacy: &NoLegacy,
        };
        assert!(matches!(
            keyring.get("archive-1").unwrap(),
            Lookup::Missing { legacy: None }
        ));
        assert_eq!(current.read("archive-1"), None);
    }

    #[test]
    fn keyring_work_runs_in_order_and_a_caller_can_stop_waiting() {
        let (release, released) = mpsc::channel::<()>();
        let order = Arc::new(Mutex::new(Vec::new()));
        let first = Arc::clone(&order);
        queue(Box::new(move || {
            let _ = released.recv();
            first.lock().unwrap().push("first");
        }))
        .unwrap();
        let error = with_os_keyring_within(Duration::from_millis(20), |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("did not answer"), "{error}");
        let second = Arc::clone(&order);
        with_os_keyring_later("record", move |_| Ok(())).unwrap();
        queue(Box::new(move || second.lock().unwrap().push("second"))).unwrap();
        release.send(()).unwrap();
        let error = with_os_keyring(|_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("must not use"), "{error}");
        assert_eq!(*order.lock().unwrap(), ["first", "second"]);
    }
}
