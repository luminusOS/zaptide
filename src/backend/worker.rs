//! Tokio worker for WhatsApp, the archive, attachments, and profile pictures.
//!
//! Messages are archived before reaching the UI. Privacy ids (`@lid`) are
//! canonicalized to phone-number ids as soon as their mapping is known.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use whatsapp_rust::download::MediaType;
use whatsapp_rust::features::message_edit::{
    SecretEncKind, decrypt_secret_encrypted_with_fallback, extract_secret_encrypted,
};
use whatsapp_rust::features::{PresencePolicy, ProfilePictureLookup};
use whatsapp_rust::media::{
    AudioOptions, DocumentOptions, ImageOptions, VideoOptions, audio_message, document_message,
    image_message, video_message,
};
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::{
    Bot, BotHandle, Client, Jid, MessageBuilderExt, MessageExt, MessageField, SendOptions, wa,
};
use whatsapp_rust::send::RevokeType;
use whatsapp_rust::types::events as wa_events;
use whatsapp_rust::types::message::{MessageInfo, MessageSource};
use whatsapp_rust::types::presence::{ChatPresence, ReceiptType};
use whatsapp_rust::upload::UploadOptions;
use whatsapp_rust::wacore::download::Downloadable;
use whatsapp_rust::wacore::history_sync::{HistorySyncStream, MAX_DECOMPRESSED};
use whatsapp_rust::wacore::store::DevicePropsOverride;
use whatsapp_rust::wacore_binary::jid::JidExt;
use whatsapp_rust::waproto::buffa::Message as _;
use whatsapp_rust::{MediaRetryResult, MediaReuploadRequest};

mod answers;
mod avatars;
mod calls;
mod commands;
mod contacts;
mod device_store;
mod downloads;
mod events;
mod failures;
mod groups;
mod history;
mod identity;
mod ingest;
mod maintenance;
mod newsletters;
mod older;
mod outbound;
mod poll_history;
mod polls;
mod presence;
mod privacy;
mod profile;
mod protocol;
mod reactions;
mod reads;
mod retention;
mod revoke;
mod session;
mod stars;
mod stickers;
mod transport;
use stars::STARRED_LIMIT;

#[cfg(test)]
use history::ParsedHistory;
#[cfg(test)]
use history::parse_conversation;
use history::{HistoryReaction, HistoryReactionBody};
use outbound::{
    PastedImageRequest, StagedDownload, download_staging_path, media_path,
    stream_session_cache_file,
};
#[cfg(test)]
use outbound::{THUMBNAIL_SIDE, consume_attachment_reply, encode_jpeg, thumbnail_jpeg};
use protocol::{base_message, classify};

use super::{
    Command, Completion, Done, DownloadFailure, Event, LinkStatus, PageKey, Wake,
    read_sync::ReadSync,
};
use crate::archive::Archive;
use crate::link_preview;

const PAGE: usize = 60;
use crate::model::{
    Chat, ChatId, ChatKind, Contact, Content, Delivery, LinkPreview, Media, MentionRef, Message,
    Quoted, Reaction,
};
use crate::paths::AppDirs;

/// Delay after the last history chunk before sync is complete.
const SYNC_QUIET: Duration = Duration::from_secs(20);

fn emit_after_history_chunk(on_demand: bool, failed: bool, progress: Option<u32>) -> bool {
    on_demand || failed || progress.is_some_and(|progress| progress >= 100)
}
/// Profile-picture cache lifetime.
const AVATAR_FRESH: Duration = Duration::from_secs(24 * 60 * 60);
/// Part of that lifetime given up per contact, so pictures fetched together
/// after linking go stale over half a day instead of all at once.
const AVATAR_SPREAD: Duration = Duration::from_secs(12 * 60 * 60);
/// Profile-picture lookups and downloads in flight at once.
const AVATAR_FETCHES: usize = 4;
const AVATAR_TIMEOUT: Duration = Duration::from_secs(20);
/// Phone history-request timeout.
const PHONE_PATIENCE: Duration = Duration::from_secs(30);
/// Phone history-request batch size.
const PHONE_BATCH: i32 = 50;
/// Leave room for app-state conflict recovery while bounding a stalled collection write.
const READ_SYNC_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// Retry completed downloads when their archive identity cannot yet be read.
const DOWNLOAD_VALIDATION_RETRY: Duration = Duration::from_secs(5);
/// `HistorySync.sync_type` for on-demand history responses.
const ON_DEMAND: i32 = 6;
/// Prevent unrelated response IDs from growing while a send is not yet registered.
const MAX_EARLY_HISTORY_IDS: usize = 16;
/// Sticker download batch size for the picker.
const STICKER_FETCH_LIMIT: usize = 40;
/// Bump to re-derive stored rows after `classify` or `thumbnail_of` change.
const DERIVED_VERSION: &str = "7";
/// Rows visited per maintenance step; bounds how long a queued event waits.
const MAINTENANCE_BATCH: usize = 256;
/// Turns handled while maintenance waits before one step runs regardless,
/// so a steady stream of events, as history sync sends, cannot starve it.
const MAINTENANCE_DEFERRALS: u32 = 32;
/// Archive meta key holding `version:rowid` of an unfinished re-derive pass.
const DERIVED_CURSOR: &str = "derived_cursor";
/// How long private content waits for phone lock state before it is shown
/// unconfirmed. A healthy sync answers well within this.
const PRIVACY_GRACE: Duration = Duration::from_secs(10);

/// Waits longer after each failed lock-state recovery, so a collection the
/// server keeps refusing is not rebuilt every few seconds.
fn privacy_backoff(attempts: u32) -> Duration {
    Duration::from_secs(30)
        .saturating_mul(1 << attempts.saturating_sub(1).min(5))
        .min(Duration::from_secs(15 * 60))
}

/// Recover locks independently: a bad RegularHigh snapshot must not abort
/// RegularLow before its lock mutations are applied.
async fn recover_chat_preferences<F, Fut, E>(snapshot: bool, mut resync: F) -> (bool, bool)
where
    F: FnMut(Vec<whatsapp_rust::WAPatchName>, whatsapp_rust::AppStateResyncMode) -> Fut,
    Fut: Future<Output = Result<whatsapp_rust::AppStateResyncReport, E>>,
    E: std::fmt::Display,
{
    use whatsapp_rust::{AppStateResyncMode, WAPatchName};
    let mode = if snapshot {
        AppStateResyncMode::Snapshot
    } else {
        AppStateResyncMode::Incremental
    };
    let mut collections = vec![WAPatchName::RegularLow];
    if snapshot {
        collections.push(WAPatchName::RegularHigh);
    }
    let (mut locks, mut complete) = (false, true);
    for name in collections {
        let synced = match resync(vec![name], mode).await {
            Ok(report) => {
                if !report.all_synced() {
                    log::warn!(
                        "chat settings recovery incomplete ({name:?}, {mode:?}): fatal {:?}, retryable {:?}, skipped {:?}",
                        report.fatal,
                        report.retryable,
                        report.skipped
                    );
                }
                report.all_synced() && report.synced.contains(&name)
            }
            Err(error) => {
                log::warn!("chat settings recovery failed ({name:?}, {mode:?}): {error}");
                false
            }
        };
        if name == WAPatchName::RegularLow {
            locks = synced;
        }
        complete &= synced;
    }
    (locks, complete)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadSyncOutcome {
    Success,
    Failed,
    TimedOut,
}

async fn bounded_read_sync<F, E>(timeout: Duration, future: F) -> ReadSyncOutcome
where
    F: Future<Output = Result<(), E>>,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(Ok(())) => ReadSyncOutcome::Success,
        Ok(Err(_)) => ReadSyncOutcome::Failed,
        Err(_) => ReadSyncOutcome::TimedOut,
    }
}

fn account_allows_receipts(
    settings: &whatsapp_rust::wacore::iq::privacy::PrivacySettingsResponse,
) -> bool {
    use whatsapp_rust::wacore::iq::privacy::{PrivacyCategory, PrivacyValue};
    matches!(
        settings.get_value(&PrivacyCategory::ReadReceipts),
        Some(PrivacyValue::All)
    )
}

/// The library's persisted privacy value is refreshed during connection setup,
/// which can finish after messages arrive and does not track later phone edits.
/// Check the account before disclosing a read/play; an unavailable setting is
/// not permission to send a receipt. Chat-state sync does not use this gate.
async fn receipts_allowed(
    client: &Client,
    jid: &Jid,
    commands: &mpsc::UnboundedSender<Command>,
    session_generation: u64,
) -> bool {
    if jid.is_group() {
        return true;
    }
    match client.fetch_privacy_settings().await {
        Ok(settings) => {
            let allowed = account_allows_receipts(&settings);
            let _ = Completion {
                generation: session_generation,
                done: Done::ReceiptsPrivacy { disabled: !allowed },
            }
            .send(commands);
            allowed
        }
        Err(error) => {
            log::debug!("receipt withheld: account privacy unavailable: {error}");
            false
        }
    }
}

/// Downloadable recent sticker from the phone.
struct PhoneSticker(wa::StickerMetadata);

impl Downloadable for PhoneSticker {
    fn direct_path(&self) -> Option<&str> {
        self.0.direct_path.as_deref()
    }

    fn media_key(&self) -> Option<&[u8]> {
        self.0.media_key.as_deref()
    }

    fn file_enc_sha256(&self) -> Option<&[u8]> {
        self.0.file_enc_sha256.as_deref()
    }

    fn file_sha256(&self) -> Option<&[u8]> {
        self.0.file_sha256.as_deref()
    }

    fn file_length(&self) -> Option<u64> {
        self.0.file_length
    }

    fn app_info(&self) -> MediaType {
        MediaType::Sticker
    }
}

/// App version in WhatsApp device-property format.
fn app_version() -> wa::device_props::AppVersion {
    let mut parts = env!("CARGO_PKG_VERSION")
        .split('.')
        .map(|part| part.parse::<u32>().ok());
    wa::device_props::AppVersion {
        primary: parts.next().flatten(),
        secondary: parts.next().flatten(),
        tertiary: parts.next().flatten(),
        ..Default::default()
    }
}

fn message_raw_fingerprint(raw: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(raw).into()
}

/// Stable sticker hash across messages and the phone's recent list.
fn sticker_hash(sha256: Option<&[u8]>, enc_sha256: Option<&[u8]>) -> Option<String> {
    let bytes = sha256
        .filter(|bytes| !bytes.is_empty())
        .or(enc_sha256.filter(|bytes| !bytes.is_empty()))?;
    Some(hex::encode(bytes))
}

fn archive_cleanup_markers(dirs: &AppDirs) -> [PathBuf; 3] {
    [
        dirs.state.join("archive-cleanup-required"),
        dirs.config.join("archive-cleanup-required"),
        dirs.cache.join("archive-cleanup-required"),
    ]
}

fn archive_cleanup_marker_exists(markers: &[PathBuf; 3]) -> std::io::Result<bool> {
    for marker in markers {
        if marker.try_exists()? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn persist_archive_cleanup_marker(markers: &[PathBuf; 3]) -> bool {
    let mut persisted = false;
    for marker in markers {
        let write = (|| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(marker)?;
            file.write_all(b"required\n")?;
            file.sync_all()?;
            if let Some(parent) = marker.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok::<_, std::io::Error>(())
        })();
        match write {
            Ok(()) => persisted = true,
            Err(_error) => log::error!("could not durably persist archive cleanup marker"),
        }
    }
    persisted
}

fn remove_archive_cleanup_markers(markers: &[PathBuf; 3]) -> std::io::Result<()> {
    for marker in markers {
        match std::fs::remove_file(marker) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    for parent in markers
        .iter()
        .filter_map(|marker| marker.parent())
        .collect::<std::collections::HashSet<_>>()
    {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

fn remove_if_present(path: PathBuf) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_dir_if_present(path: PathBuf) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn clear_logged_out_data(dirs: &AppDirs, archive: &Archive) -> anyhow::Result<()> {
    archive.clear()?;
    let session = dirs.session_db();
    for suffix in ["", "-wal", "-shm", "-journal", ".encrypting"] {
        let mut path = session.clone().into_os_string();
        path.push(suffix);
        remove_if_present(PathBuf::from(path))?;
    }
    if let Some(parent) = session.parent() {
        sync_directory(parent)?;
    }
    let cache_directories = [
        dirs.avatar_cache_dir(),
        dirs.media_cache_dir(),
        dirs.sticker_cache_dir(),
    ];
    for directory in &cache_directories {
        remove_dir_if_present(directory.clone())?;
    }
    let cache_parents = cache_directories
        .iter()
        .filter_map(|directory| directory.parent())
        .collect::<std::collections::HashSet<_>>();
    for parent in cache_parents {
        sync_directory(parent)?;
    }
    Ok(())
}

async fn wait_for_shutdown<F: Future>(timeout: Duration, shutdown: F) -> bool {
    tokio::time::timeout(timeout, shutdown).await.is_ok()
}

async fn wait_for_reconnect(inbox: &mut mpsc::UnboundedReceiver<Command>) -> bool {
    loop {
        match inbox.recv().await {
            Some(Command::Reconnect) => return true,
            Some(Command::Shutdown) | None => return false,
            _ => {}
        }
    }
}

async fn write_session_cache_file(
    directory: &Path,
    path: &Path,
    bytes: &[u8],
    generation: u64,
    current_generation: &AtomicU64,
    request_generation: Option<(u64, &AtomicU64)>,
    cache_lock: &tokio::sync::Mutex<()>,
) -> Result<PathBuf, String> {
    let _guard = cache_lock.lock().await;
    let active = || {
        current_generation.load(Ordering::Acquire) == generation
            && request_generation
                .is_none_or(|(expected, current)| current.load(Ordering::Acquire) == expected)
    };
    if !active() {
        return Err("The linked account changed during the download".to_owned());
    }
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(|error| error.to_string())?;
    if !active() {
        return Err("The linked account changed during the download".to_owned());
    }
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| error.to_string())?;
    if !active() {
        let _ = tokio::fs::remove_file(path).await;
        return Err("The linked account changed during the download".to_owned());
    }
    Ok(path.to_owned())
}

/// One agent for every profile-picture download, so connections to the
/// picture servers are reused and a stalled server cannot hold a thread.
static AVATAR_AGENT: std::sync::LazyLock<ureq::Agent> = std::sync::LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(AVATAR_TIMEOUT))
        .build()
        .new_agent()
});

fn avatar_spread(id: &str) -> Duration {
    // FNV-1a: stable across runs and builds, unlike the std hasher.
    let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    Duration::from_secs(hash % AVATAR_SPREAD.as_secs())
}

fn avatar_is_fresh(id: &str, age: Duration) -> bool {
    age < AVATAR_FRESH - avatar_spread(id)
}

/// The picture a cached avatar file came from, and the query that found it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct KnownPicture {
    jid: String,
    preview: bool,
    id: String,
}

impl KnownPicture {
    fn sidecar(path: &Path) -> PathBuf {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(".id");
        PathBuf::from(sidecar)
    }

    fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(Self::sidecar(path)).ok()?;
        let mut lines = text.lines();
        let jid = lines.next()?.to_owned();
        let preview = match lines.next()? {
            "preview" => true,
            "image" => false,
            _ => return None,
        };
        let id = lines.next().filter(|id| !id.is_empty())?.to_owned();
        Some(Self { jid, preview, id })
    }

    fn encode(&self) -> String {
        let kind = if self.preview { "preview" } else { "image" };
        format!("{}\n{kind}\n{}\n", self.jid, self.id)
    }
}

enum FetchedPicture {
    Missing,
    /// The server confirmed the cached file is still the current picture.
    Unchanged,
    Downloaded {
        bytes: Vec<u8>,
        picture: Option<KnownPicture>,
    },
}

/// The network side of a profile-picture fetch, separate so tests can stand
/// in for WhatsApp and the picture servers.
trait PictureSource: Send + Sync + 'static {
    fn lookup(
        &self,
        jid: &Jid,
        preview: bool,
        existing_id: Option<&str>,
    ) -> impl Future<Output = Result<ProfilePictureLookup, String>> + Send;

    fn download(&self, url: String) -> impl Future<Output = Result<Vec<u8>, String>> + Send;
}

struct ClientPictures(Arc<Client>);

impl PictureSource for ClientPictures {
    async fn lookup(
        &self,
        jid: &Jid,
        preview: bool,
        existing_id: Option<&str>,
    ) -> Result<ProfilePictureLookup, String> {
        let options = whatsapp_rust::features::ProfilePictureLookupOptions::new(jid)
            .preview(preview)
            .existing_id(existing_id)
            .timeout(Some(AVATAR_TIMEOUT));
        self.0
            .contacts()
            .lookup_profile_picture_with_options(options)
            .await
            .map_err(|error| error.to_string())
    }

    async fn download(&self, url: String) -> Result<Vec<u8>, String> {
        tokio::task::spawn_blocking(move || {
            AVATAR_AGENT
                .get(&url)
                .call()
                .and_then(|mut response| response.body_mut().read_to_vec())
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

/// Looks the picture up for each candidate id, the requested size first,
/// and downloads it. At most `AVATAR_FETCHES` run at once per account, so a
/// chat list full of stale pictures cannot start hundreds of requests and
/// blocking threads together.
async fn fetch_picture(
    source: &impl PictureSource,
    permits: &tokio::sync::Semaphore,
    still_wanted: impl Fn() -> bool,
    candidates: &[Jid],
    full: bool,
    known: Option<&KnownPicture>,
) -> Result<FetchedPicture, String> {
    let _permit = permits.acquire().await.map_err(|error| error.to_string())?;
    // A logout or picture update while queued makes the answer useless.
    if !still_wanted() {
        return Err("The profile-picture request is no longer current".to_owned());
    }
    let mut failed = false;
    for jid in candidates {
        let jid_text = jid.to_string();
        for preview in [!full, false] {
            let existing = known
                .filter(|known| known.jid == jid_text && known.preview == preview)
                .map(|known| known.id.as_str());
            match source.lookup(jid, preview, existing).await {
                Ok(ProfilePictureLookup::Found(picture)) => {
                    let bytes = source.download(picture.url).await?;
                    let picture = (!picture.id.is_empty()).then_some(KnownPicture {
                        jid: jid_text,
                        preview,
                        id: picture.id,
                    });
                    return Ok(FetchedPicture::Downloaded { bytes, picture });
                }
                // wacore reads an empty reply to a lookup with an id as
                // unchanged; a removed picture still arrives as PictureUpdate.
                Ok(ProfilePictureLookup::Unchanged) if existing.is_some() => {
                    return Ok(FetchedPicture::Unchanged);
                }
                Ok(ProfilePictureLookup::RateOverlimit) => {
                    log::debug!("picture lookup rate-limited");
                    failed = true;
                }
                Ok(_) => {}
                Err(error) => {
                    log::debug!("picture lookup failed: {error}");
                    failed = true;
                }
            }
        }
    }
    if failed {
        Err("lookup failed".to_owned())
    } else {
        Ok(FetchedPicture::Missing)
    }
}

/// Writes a downloaded picture and the id it came with, in one locked step
/// checked against logout and picture updates. The old id goes first, so a
/// failed write can only cost a download later, never keep an old picture.
async fn write_avatar_cache(
    path: &Path,
    bytes: &[u8],
    picture: Option<&KnownPicture>,
    generation: u64,
    current_generation: &AtomicU64,
    request_generation: (u64, &AtomicU64),
    cache_lock: &tokio::sync::Mutex<()>,
) -> Result<PathBuf, String> {
    let _guard = cache_lock.lock().await;
    let (expected, current) = request_generation;
    let active = || {
        current_generation.load(Ordering::Acquire) == generation
            && current.load(Ordering::Acquire) == expected
    };
    let stale = || "The linked account or picture changed during the download".to_owned();
    if !active() {
        return Err(stale());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "Profile-picture cache path is invalid".to_owned())?;
    let sidecar = KnownPicture::sidecar(path);
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| error.to_string())?;
    match tokio::fs::remove_file(&sidecar).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error.to_string());
        }
        _ => {}
    }
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(picture) = picture {
        let _ = tokio::fs::write(&sidecar, picture.encode()).await;
    }
    if !active() {
        let _ = tokio::fs::remove_file(path).await;
        let _ = tokio::fs::remove_file(&sidecar).await;
        return Err(stale());
    }
    Ok(path.to_owned())
}

/// Marks a cached picture the server confirmed as fresh again.
async fn keep_session_cache_file(
    path: &Path,
    generation: u64,
    current_generation: &AtomicU64,
    request_generation: (u64, &AtomicU64),
    cache_lock: &tokio::sync::Mutex<()>,
) -> Result<PathBuf, String> {
    let _guard = cache_lock.lock().await;
    let (expected, current) = request_generation;
    if current_generation.load(Ordering::Acquire) != generation
        || current.load(Ordering::Acquire) != expected
    {
        return Err("The linked account changed during the lookup".to_owned());
    }
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(std::time::SystemTime::now()))
        .map_err(|error| error.to_string())?;
    Ok(path.to_owned())
}

pub async fn run(
    dirs: AppDirs,
    events: std::sync::mpsc::Sender<Event>,
    commands: mpsc::UnboundedSender<Command>,
    mut inbox: mpsc::UnboundedReceiver<Command>,
    waker: Arc<dyn Wake>,
) {
    let archive = loop {
        let path = dirs.archive_db();
        let cleanup_dirs = dirs.clone();
        let cleanup_markers = archive_cleanup_markers(&dirs);
        let cleanup_required = match archive_cleanup_marker_exists(&cleanup_markers) {
            Ok(required) => required,
            Err(_error) => {
                log::error!("could not check local archive cleanup state");
                let _ = events.send(Event::Link(LinkStatus::Failed(
                    "Local conversation cleanup state could not be checked".to_owned(),
                )));
                waker.wake();
                if !wait_for_reconnect(&mut inbox).await {
                    return;
                }
                continue;
            }
        };
        let opened = tokio::task::spawn_blocking(move || {
            let archive = Archive::open(&path)?;
            let cleanup_required = cleanup_required || archive.logout_cleanup_required()?;
            if cleanup_required {
                clear_logged_out_data(&cleanup_dirs, &archive)?;
                archive.finish_logout_cleanup()?;
            }
            Ok::<_, anyhow::Error>((archive, cleanup_required))
        })
        .await;
        match opened {
            Ok(Ok((archive, cleanup_required))) => {
                if cleanup_required
                    && let Err(error) = remove_archive_cleanup_markers(&cleanup_markers)
                {
                    log::error!("could not remove archive cleanup marker: {error}");
                    let _ = events.send(Event::Link(LinkStatus::Failed(
                        "Local conversation cleanup could not be finalized".to_owned(),
                    )));
                    waker.wake();
                    if !wait_for_reconnect(&mut inbox).await {
                        return;
                    }
                } else {
                    break archive;
                }
            }
            result => {
                let error = match result {
                    Ok(Err(error)) => format!("{error:#}"),
                    Err(_) => "Archive unlock worker failed".to_owned(),
                    Ok(Ok(_)) => unreachable!(),
                };
                log::error!("could not unlock the message archive: {error}");
                let _ = events.send(Event::Link(LinkStatus::Failed(error)));
                waker.wake();
                // Do not connect with a disposable archive: history is replayed
                // only once and would be lost if the keyring were locked.
                if !wait_for_reconnect(&mut inbox).await {
                    return;
                }
            }
        }
    };
    let (wa_sender, wa_events) = mpsc::unbounded_channel();
    let mut worker = Worker::new(WorkerParts {
        dirs,
        events,
        commands,
        waker,
        archive,
        wa_sender,
        wa_events,
    });

    worker.load_state();
    worker.recover_interrupted_answers();

    worker.start_bot().await;
    worker.remove_stale_staging();
    worker.start_maintenance();
    let mut tick = tokio::time::interval(Duration::from_secs(5));

    while worker.turn(&mut inbox, &mut tick).await {}
    worker.drain_shutdown_events().await;
    if !worker.deferred_incoming.is_empty() {
        log::warn!("incoming messages awaiting archive recovery need history replay after restart");
    }
    worker.end_call_for_shutdown().await;
    let _ = worker.stop_bot().await;
}

/// Startup archive upkeep, run a step at a time between events: whatsapp-rust
/// acknowledges a message once it is queued for this worker, so a long pass
/// before the event loop would leave acknowledged messages only in memory.
enum Maintenance {
    Backfill(Backfill),
    ConfirmAnswers,
    RelocateMedia(Relocation),
}

struct Backfill {
    after: i64,
    updated: usize,
    failed: usize,
    retry_required: bool,
    started: Instant,
}

#[derive(Default)]
struct Relocation {
    after: i64,
    moved: usize,
    forgotten: usize,
}

enum RuntimeEvent {
    WhatsApp(Arc<wa_events::Event>),
    PreferencesRecovered {
        generation: u64,
        locks: bool,
        complete: bool,
    },
}

struct PendingOlder {
    request: u64,
    queued: Option<(u64, super::PageKey)>,
    request_id: u64,
    asked: Instant,
    before: super::PageKey,
    protocol_id: Option<String>,
    received_count: usize,
    more_on_phone: Option<bool>,
    /// History can race request-send completion; retain response identity briefly.
    early_responses: HashMap<String, EarlyOlderResponse>,
}

#[derive(Default)]
struct EarlyOlderResponse {
    received_count: usize,
    more_on_phone: Option<bool>,
}

struct UiEvents(mpsc::UnboundedSender<RuntimeEvent>);

impl wa_events::EventHandler for UiEvents {
    fn handle_event(&self, event: Arc<wa_events::Event>) {
        let _ = self.0.send(RuntimeEvent::WhatsApp(event));
    }
}

struct DeferredDownload {
    retry_at: Instant,
    completion: Option<Command>,
}

impl Drop for DeferredDownload {
    fn drop(&mut self) {
        if let Some(Command::Completed(completion)) = self.completion.take() {
            completion.done.discard();
        }
    }
}

struct PendingRevoke {
    attempt_id: u64,
    content: Content,
    edited: bool,
}

/// A transcript read whose answer was withheld with the rest of the private
/// content, to be read again once content is shown.
#[derive(Clone, Debug, PartialEq)]
enum WithheldPage {
    /// `Command::LoadChat`.
    Page(ChatId, Option<super::PageKey>, u64),
    /// `Command::LoadUntil`.
    Until(ChatId, String, super::PageKey),
}

struct PrivacyGate {
    /// Private content may reach the UI.
    privacy_ready: bool,
    /// Phone lock state is known to be mirrored in the archive.
    privacy_confirmed: bool,
    /// Lock state must be rebuilt from a snapshot rather than caught up.
    privacy_snapshot: bool,
    /// When content is shown even though lock state is still unconfirmed.
    privacy_reveal_at: Option<Instant>,
    privacy_attempts: u32,
    privacy_warned: bool,
    privacy_recovering: bool,
    privacy_generation: u64,
    privacy_retry: Instant,
    /// Transcript pages asked for while private content was withheld. Their
    /// answers never reached the interface, which still waits for them, so
    /// they are read again once content is shown.
    withheld_pages: Vec<WithheldPage>,
}

struct Identity {
    me_pn: Option<String>,
    me_lid: Option<String>,
    me_name: Option<String>,
    me_about: Option<String>,
    /// Privacy-id user part to phone-number user part.
    lid_to_pn: HashMap<String, String>,
    contacts: HashMap<String, Contact>,
}

struct GroupInfoQueue {
    /// Groups queued or already requested. Queries are rate-limited.
    group_info_requested: HashSet<String>,
    /// Pending group metadata queue.
    group_info_queue: std::collections::VecDeque<String>,
    /// Group metadata attempt counts.
    group_info_tries: HashMap<String, u32>,
    /// Next retry time for failed group metadata requests.
    group_info_retry: Vec<(Instant, String)>,
}

struct OlderRequests {
    /// Pending phone-history request and response identity by chat.
    pending_older: HashMap<ChatId, PendingOlder>,
    next_older_request_id: u64,
    /// Chats already notified about a phone-history timeout.
    older_warned: HashSet<ChatId>,
}

struct WorkerParts {
    dirs: AppDirs,
    events: std::sync::mpsc::Sender<Event>,
    commands: mpsc::UnboundedSender<Command>,
    waker: Arc<dyn Wake>,
    archive: Archive,
    wa_sender: mpsc::UnboundedSender<RuntimeEvent>,
    wa_events: mpsc::UnboundedReceiver<RuntimeEvent>,
}

struct Worker {
    privacy: PrivacyGate,
    read_sync: ReadSync,
    poll_decrypting: usize,
    poll_history: poll_history::Requests,
    /// Answer message id to (chat, buttons message id), until the send ends.
    answer_sends: HashMap<String, (ChatId, String)>,
    pending_revokes: HashMap<(ChatId, String), PendingRevoke>,
    next_revoke_attempt: u64,
    poll_sending: HashSet<(ChatId, String)>,
    /// What the window wants announced as this account's presence.
    presence_wish: tokio::sync::watch::Sender<presence::PresenceWish>,
    calls: calls::Calls,
    retention: retention::Retention,
    dirs: AppDirs,
    events: std::sync::mpsc::Sender<Event>,
    commands: mpsc::UnboundedSender<Command>,
    waker: Arc<dyn Wake>,
    archive: Archive,
    client: Option<Arc<Client>>,
    handle: Option<BotHandle>,
    wa_sender: mpsc::UnboundedSender<RuntimeEvent>,
    wa_events: mpsc::UnboundedReceiver<RuntimeEvent>,
    identity: Identity,
    status: LinkStatus,
    session_generation: u64,
    session_generation_shared: Arc<AtomicU64>,
    /// Last forward still sending per destination; the next one to that chat
    /// waits for it so a batch arrives in the order it was picked.
    forward_tails: HashMap<ChatId, tokio::task::JoinHandle<bool>>,
    avatar_generations: HashMap<(String, bool), Arc<AtomicU64>>,
    session_cache_lock: Arc<tokio::sync::Mutex<()>>,
    pairing_phone: Option<String>,
    pair_code: Option<String>,
    pair_request_id: u64,
    archive_cleanup_failed: bool,
    qr: Option<String>,
    syncing: bool,
    sync_deadline: Option<Instant>,
    group_info: GroupInfoQueue,
    newsletter_names: HashMap<String, String>,
    newsletter_requested: HashSet<String>,
    newsletter_queue: std::collections::VecDeque<String>,
    newsletter_running: bool,
    newsletter_list_running: bool,
    newsletter_list_pending: bool,
    newsletter_retry: Vec<(Instant, String)>,
    newsletter_tries: HashMap<String, u32>,
    presence_subscribed: HashSet<String>,
    older: OlderRequests,
    /// Deferred profile-picture requests and retry counts.
    pending_avatars: HashMap<(String, bool), u32>,
    avatar_permits: Arc<tokio::sync::Semaphore>,
    /// Active recent-sticker downloads by hash.
    sticker_fetches: HashSet<String>,
    /// Active chat-sticker downloads by chat, message id, and raw fingerprint.
    sticker_downloads: HashSet<(ChatId, String, [u8; 32])>,
    stickers_dirty: bool,
    /// Completed downloads waiting for an archive read, not another network fetch.
    deferred_downloads: Vec<DeferredDownload>,
    /// Correlates selected-file completion events without exposing error details.
    next_attachment_batch: u64,
    maintenance: Option<Maintenance>,
    maintenance_deferred: u32,
    /// Events emitted inside an archive batch, held until it commits so the
    /// window never shows a row that a rollback took away.
    uncommitted_events: std::sync::Mutex<Option<Vec<Event>>>,
    deferred_incoming: std::collections::VecDeque<(Arc<wa::Message>, MessageInfo)>,
}

/// An archive batch begun by `Worker::begin_batch`.
#[must_use = "finish the batch so it commits"]
struct Batch<'a> {
    worker: &'a mut Worker,
    /// False when an outer batch owns the commit, or when the batch could not
    /// begin and the writes commit one by one as without a batch.
    owned: bool,
    state: Option<BatchState>,
}

struct BatchState {
    lid_to_pn: HashMap<String, String>,
    contacts: Option<HashMap<String, Contact>>,
}

impl std::ops::Deref for Batch<'_> {
    type Target = Worker;

    fn deref(&self) -> &Worker {
        self.worker
    }
}

impl std::ops::DerefMut for Batch<'_> {
    fn deref_mut(&mut self) -> &mut Worker {
        self.worker
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        if self.owned {
            self.worker.archive.rollback_batch();
            self.worker.uncommitted().take();
            self.restore_state();
        }
    }
}

impl Batch<'_> {
    fn protect_state(&mut self, contacts: bool) {
        assert!(self.owned && self.state.is_none());
        self.state = Some(BatchState {
            lid_to_pn: self.worker.identity.lid_to_pn.clone(),
            contacts: contacts.then(|| self.worker.identity.contacts.clone()),
        });
    }

    fn restore_state(&mut self) {
        if let Some(state) = self.state.take() {
            self.worker.identity.lid_to_pn = state.lid_to_pn;
            if let Some(contacts) = state.contacts {
                self.worker.identity.contacts = contacts;
            }
        }
    }

    fn finish(mut self) -> bool {
        if !self.owned {
            return true;
        }
        let committed = self.worker.archive.commit_batch();
        let events = self.worker.uncommitted().take().unwrap_or_default();
        self.owned = false;
        if let Err(error) = committed {
            self.restore_state();
            log::warn!("could not save a batch of archive changes: {error}");
            self.worker.emit_chats();
            self.worker.emit(Event::RefreshTranscript);
            return false;
        }
        self.state = None;
        if !events.is_empty() {
            for event in events {
                let _ = self.worker.events.send(event);
            }
            self.worker.waker.wake();
        }
        true
    }
}

impl Worker {
    fn new(parts: WorkerParts) -> Self {
        let WorkerParts {
            dirs,
            events,
            commands,
            waker,
            archive,
            wa_sender,
            wa_events,
        } = parts;
        let privacy_confirmed = archive
            .meta("chat_privacy_ready_v1")
            .ok()
            .flatten()
            .as_deref()
            == Some("complete");
        // Only an archive filled before lock state was mirrored needs its lock
        // state rebuilt from a snapshot; a new link receives it with the first sync.
        let privacy_snapshot =
            !privacy_confirmed && archive.chats().is_ok_and(|chats| !chats.is_empty());
        Self {
            older: OlderRequests {
                pending_older: HashMap::new(),
                next_older_request_id: 0,
                older_warned: HashSet::new(),
            },
            group_info: GroupInfoQueue {
                group_info_requested: HashSet::new(),
                group_info_queue: std::collections::VecDeque::new(),
                group_info_tries: HashMap::new(),
                group_info_retry: Vec::new(),
            },
            newsletter_names: HashMap::new(),
            newsletter_requested: HashSet::new(),
            newsletter_queue: std::collections::VecDeque::new(),
            newsletter_running: false,
            newsletter_list_running: false,
            newsletter_list_pending: false,
            newsletter_retry: Vec::new(),
            newsletter_tries: HashMap::new(),
            identity: Identity {
                me_pn: None,
                me_lid: None,
                me_name: None,
                me_about: None,
                lid_to_pn: HashMap::new(),
                contacts: HashMap::new(),
            },
            privacy: PrivacyGate {
                privacy_ready: privacy_confirmed,
                privacy_confirmed,
                privacy_snapshot,
                // Existing archives retain the offline fallback. A fresh link starts
                // its grace only when recovery begins, not while waiting for pairing.
                privacy_reveal_at: privacy_snapshot.then(|| Instant::now() + PRIVACY_GRACE),
                privacy_attempts: 0,
                privacy_warned: false,
                privacy_recovering: false,
                privacy_generation: 0,
                privacy_retry: Instant::now(),
                withheld_pages: Vec::new(),
            },
            dirs,
            events,
            commands,
            waker,
            archive,
            client: None,
            handle: None,
            wa_sender,
            wa_events,
            status: LinkStatus::Starting,
            session_generation: 0,
            session_generation_shared: Arc::new(AtomicU64::new(0)),
            forward_tails: HashMap::new(),
            avatar_generations: HashMap::new(),
            session_cache_lock: Arc::new(tokio::sync::Mutex::new(())),
            pairing_phone: None,
            pair_code: None,
            pair_request_id: 0,
            archive_cleanup_failed: false,
            qr: None,
            syncing: false,
            sync_deadline: None,
            presence_subscribed: HashSet::new(),
            pending_avatars: HashMap::new(),
            avatar_permits: Arc::new(tokio::sync::Semaphore::new(AVATAR_FETCHES)),
            sticker_fetches: HashSet::new(),
            sticker_downloads: HashSet::new(),
            stickers_dirty: false,
            deferred_downloads: Vec::new(),
            next_attachment_batch: 0,
            maintenance: None,
            maintenance_deferred: 0,
            uncommitted_events: std::sync::Mutex::new(None),
            deferred_incoming: std::collections::VecDeque::new(),
            read_sync: ReadSync::default(),
            poll_decrypting: 0,
            poll_history: Default::default(),
            answer_sends: HashMap::new(),
            pending_revokes: HashMap::new(),
            next_revoke_attempt: 0,
            poll_sending: HashSet::new(),
            presence_wish: tokio::sync::watch::Sender::new(Default::default()),
            calls: Default::default(),
            retention: Default::default(),
        }
    }

    /// Handles one command, event or timer, or else one maintenance step.
    /// Returns false on shutdown.
    async fn turn(
        &mut self,
        inbox: &mut mpsc::UnboundedReceiver<Command>,
        tick: &mut tokio::time::Interval,
    ) -> bool {
        if self.maintenance.is_some() {
            if self.maintenance_deferred >= MAINTENANCE_DEFERRALS {
                self.maintain();
                return true;
            }
            self.maintenance_deferred += 1;
        }
        let deadline = self.sync_deadline;
        tokio::select! {
            command = inbox.recv() => {
                match command {
                    Some(Command::Shutdown) | None => return false,
                    Some(command) => self.handle_command(command).await,
                }
            }
            Some(event) = self.wa_events.recv() => self.handle_runtime_event(event).await,
            _ = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                self.sync_deadline = None;
                self.set_syncing(false);
                self.emit_chats();
            }
            _ = tick.tick() => {
                self.retry_deferred_incoming();
                self.retry_deferred_downloads(Instant::now()).await;
                self.reveal_unconfirmed_after_grace();
                self.refresh_legacy_preferences();
                self.expire_older_requests();
                self.retry_avatars();
                self.pump_group_info();
                self.pump_newsletter_info();
                self.pump_read_sync();
                self.pump_poll_votes();
                self.pump_poll_history();
                self.pump_media_retention();
                self.flush_stickers();
            }
            // yield_now is pending on its first poll, so anything already
            // queued when this turn starts is handled before the next step.
            () = tokio::task::yield_now(), if self.maintenance.is_some() => self.maintain(),
        }
        true
    }

    async fn handle_runtime_event(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::WhatsApp(event) => self.handle_wa_event(event).await,
            RuntimeEvent::PreferencesRecovered {
                generation,
                locks,
                complete,
            } => {
                self.preferences_recovered(generation, locks, complete);
            }
        }
    }

    async fn drain_shutdown_events(&mut self) {
        // whatsapp-rust may acknowledge messages as soon as it queues them here.
        for _ in 0..super::EVENT_BATCH_LIMIT {
            let Ok(event) = self.wa_events.try_recv() else {
                break;
            };
            self.handle_runtime_event(event).await;
        }
    }

    fn ephemeral_expiration(&self, chat: &str) -> Option<u32> {
        self.archive
            .ephemeral_expiration(chat)
            .ok()
            .flatten()
            .filter(|expiration| *expiration > 0)
    }

    fn apply_ephemeral(&self, chat: &str, message: &mut wa::Message) -> Option<u32> {
        apply_ephemeral_expiration(message, self.ephemeral_expiration(chat))
    }

    fn emit_profile(&self) {
        self.emit(Event::AccountKnown(
            self.identity.me_pn.is_some() || self.identity.me_lid.is_some(),
        ));
        self.emit(Event::Profile {
            phone: self
                .identity
                .me_pn
                .as_deref()
                .and_then(crate::model::phone_of)
                .map(crate::util::phone),
            name: self.identity.me_name.clone(),
        });
    }

    fn emit(&self, mut event: Event) {
        if let Event::Syncing(syncing) = &mut event {
            *syncing |= !self.privacy.privacy_ready;
        }
        // An upgraded archive has no reliable lock state until the library's
        // authenticated replay has completed. Keep private content off the UI
        // and out of notifications during recovery, including failed retries.
        if !self.privacy.privacy_ready
            && matches!(
                event,
                Event::Chats(_)
                    | Event::ChatUpdated(_)
                    | Event::Messages { .. }
                    | Event::MessageUpdated(_)
                    | Event::MessagesUpdated(_)
                    | Event::Incoming { .. }
                    | Event::Contacts(_)
                    | Event::Typing { .. }
                    | Event::Calls { .. }
            )
        {
            return;
        }
        if let Some(uncommitted) = self.uncommitted().as_mut() {
            uncommitted.push(event);
            return;
        }
        let _ = self.events.send(event);
        self.waker.wake();
    }

    fn uncommitted(&self) -> std::sync::MutexGuard<'_, Option<Vec<Event>>> {
        self.uncommitted_events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Groups archive writes until the batch is finished.
    /// A row that fails is still skipped alone, as it is without a batch.
    fn begin_batch(&mut self) -> Batch<'_> {
        let mut uncommitted = self.uncommitted();
        if uncommitted.is_some() {
            drop(uncommitted);
            return Batch {
                worker: self,
                owned: false,
                state: None,
            };
        }
        if let Err(error) = self.archive.begin_batch() {
            log::warn!("could not begin an archive batch: {error}");
            drop(uncommitted);
            return Batch {
                worker: self,
                owned: false,
                state: None,
            };
        }
        *uncommitted = Some(Vec::new());
        drop(uncommitted);
        Batch {
            worker: self,
            owned: true,
            state: None,
        }
    }

    /// Syncs a chat setting to the phone without blocking the worker.
    fn tell_phone<F, Fut>(&self, chat: &str, call: F)
    where
        F: FnOnce(Arc<Client>, Jid) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(chat)) else {
            return;
        };
        tokio::spawn(async move {
            if call(client, jid).await.is_err() {
                log::warn!("could not synchronize a chat preference");
            }
        });
    }

    fn emit_chats(&self) {
        match self.archive.chats() {
            Ok(mut chats) => {
                // Early preference sync can create an empty privacy-id row.
                // Once mapped, its preferences live on the canonical chat, so
                // the mapped duplicate hides here. History sync also brings
                // chats with no messages at all; those stay visible, as on
                // the phone, and opening one asks the phone for its history.
                chats.retain(|chat| self.canonical_str(&chat.id) == chat.id);
                for chat in &mut chats {
                    self.polish_chat(chat);
                }
                log::info!("chat list holds {} chats", chats.len());
                self.emit(Event::Chats(chats));
            }
            Err(error) => log::warn!("could not list chats: {error}"),
        }
    }

    fn emit_chat(&self, id: &str) {
        if let Ok(Some(mut chat)) = self.archive.chat(id) {
            self.polish_chat(&mut chat);
            self.emit(Event::ChatUpdated(Box::new(chat)));
        }
    }

    fn polish_chat(&self, chat: &mut Chat) {
        if !chat.is_group()
            && chat.kind != ChatKind::Broadcast
            && (self.contact_name(&chat.id).is_some() || chat.id == self.me())
        {
            chat.name = self.chat_name(&chat.id, None);
        }
        if let Some(last) = chat.last.as_mut() {
            if !last.from_me
                && let Some(name) = self.contact_name(&last.sender)
            {
                last.sender_name = Some(name);
            }
            last.summary = self.preview_mentions(&last.summary);
        }
    }

    fn emit_message(&self, chat: &str, id: &str) {
        if let Some(message) = self.updated_message(chat, id) {
            self.emit(Event::MessageUpdated(Box::new(message)));
        }
    }

    fn updated_message(&self, chat: &str, id: &str) -> Option<Message> {
        let mut message = self.archive.message(chat, id).ok().flatten()?;
        self.polish(&mut message);
        Some(message)
    }

    fn emit_messages(&self, messages: Vec<Message>) {
        if !messages.is_empty() {
            self.emit(Event::MessagesUpdated(messages));
        }
    }

    fn set_status(&mut self, status: LinkStatus) {
        if self.status != status {
            log::info!("link: {}", status.log_label());
            self.status = status.clone();
            self.emit(Event::Link(status));
        }
    }

    fn set_syncing(&mut self, syncing: bool) {
        if self.syncing != syncing {
            self.syncing = syncing;
            self.emit(Event::Syncing(syncing));
        }
    }

    fn clear_history_sync_state(&mut self) {
        self.sync_deadline = None;
        self.set_syncing(false);
        self.older.pending_older.clear();
        self.older.older_warned.clear();
    }

    fn unlinked(&self) -> LinkStatus {
        LinkStatus::Unlinked {
            qr: self.qr.clone(),
            pair_code: self.pair_code.clone(),
            pairing_phone: self.pairing_phone.clone(),
        }
    }

    /// Canonical id used for our account.
    fn me(&self) -> String {
        self.identity
            .me_pn
            .clone()
            .or_else(|| self.identity.me_lid.clone())
            .unwrap_or_else(|| "me".to_owned())
    }

    fn is_me(&self, id: &str) -> bool {
        self.identity.me_pn.as_deref() == Some(id) || self.identity.me_lid.as_deref() == Some(id)
    }

    fn archive_matches_account(&self, pn: Option<&str>, lid: Option<&str>) -> bool {
        match (self.identity.me_pn.as_deref(), pn) {
            (Some(previous), Some(current)) => previous == current,
            _ => match (self.identity.me_lid.as_deref(), lid) {
                (Some(previous), Some(current)) => previous == current,
                _ => true,
            },
        }
    }

    fn load_state(&mut self) {
        self.identity.me_pn = self.archive.meta("me_pn").ok().flatten();
        self.identity.me_lid = self.archive.meta("me_lid").ok().flatten();
        self.identity.me_name = self.archive.meta("me_name").ok().flatten();
        self.identity.me_about = self.archive.meta("me_about").ok().flatten();
        if let Ok(lids) = self.archive.lids() {
            self.identity.lid_to_pn = lids.into_iter().collect();
        }
        if let Ok(contacts) = self.archive.contacts() {
            self.identity.contacts = contacts
                .into_iter()
                .map(|contact| (contact.id.clone(), contact))
                .collect();
        }
        self.emit(Event::Contacts(
            self.identity.contacts.values().cloned().collect(),
        ));
        self.emit_chats();
        self.emit_profile();
    }

    /// Refreshes stored quote ids and names with current mappings.
    fn polish(&self, message: &mut Message) {
        self.polish_poll(message);
        // Archived messages may predate the contact's current name.
        if !message.from_me
            && let Some(name) = self.contact_name(&message.sender)
        {
            message.sender_name = Some(name);
        }
        if let Some(quoted) = message.quoted.as_mut() {
            let sender = self.canonical_str(&quoted.sender);
            if sender != quoted.sender || quoted.sender_name.is_none() {
                quoted.sender_name = self.name_for(&sender);
                quoted.sender = sender;
            } else if quoted.sender_name.as_deref() != Some("You")
                && let Some(name) = self.contact_name(&quoted.sender)
            {
                quoted.sender_name = Some(name);
            }
            quoted.summary = self.pn_tokens(&quoted.summary);
            for mention in &mut quoted.mentions {
                let original_name = self.whatsapp_name(mention);
                if let Some(phone) = self.identity.lid_to_pn.get(&mention.user) {
                    mention.user = phone.clone();
                }
                mention.id = self.canonical_str(&mention.id);
                mention.name = self
                    .whatsapp_name(mention)
                    .or(original_name)
                    .or_else(|| mention.name.take());
            }
            if quoted.mentions.is_empty() {
                quoted.mentions = self.mention_tokens(&quoted.summary);
                for mention in &mut quoted.mentions {
                    mention.name = self.whatsapp_name(mention);
                }
            }
        }
        for mention in &mut message.mentions {
            mention.id = self.canonical_str(&mention.id);
        }
        // Senders may omit the mention list; recover it from `@user` tokens,
        // as is done for quotes, so the names still show.
        if message.mentions.is_empty() {
            let text = match &message.content {
                Content::Text { text, .. } => Some(text.as_str()),
                Content::Image { caption, .. }
                | Content::Video { caption, .. }
                | Content::Document { caption, .. } => caption.as_deref(),
                _ => None,
            };
            if let Some(text) = text {
                // Only tokens that map to someone we know: a number typed
                // after `@` that is not a person stays plain text.
                message.mentions = self
                    .mention_tokens(text)
                    .into_iter()
                    .filter(|mention| {
                        self.identity.lid_to_pn.contains_key(&mention.user)
                            || self.contact_name(&mention.id).is_some()
                    })
                    .collect();
            }
        }
        for mention in &mut message.mentions {
            mention.name = self.whatsapp_name(mention);
        }
    }

    /// The name a mentioned person goes by on WhatsApp (their push name),
    /// looked up under every id they may have been seen with.
    fn whatsapp_name(&self, mention: &MentionRef) -> Option<String> {
        let known = self.identity.lid_to_pn.get(&mention.user);
        [
            Some(mention.id.clone()),
            Some(format!("{}@lid", mention.user)),
            known.map(|pn| format!("{pn}@s.whatsapp.net")),
        ]
        .into_iter()
        .flatten()
        .find_map(|id| {
            self.identity
                .contacts
                .get(&id)?
                .push_name
                .clone()
                .filter(|name| !name.is_empty())
        })
    }
}

// --- free helpers ----------------------------------------------------------

fn outgoing_forward(original: &wa::Message, expiration: Option<u32>) -> (wa::Message, Option<u32>) {
    let mut message = base_message(original).prepare_for_forward();
    if let Some(mut context) = context_of(&message).cloned() {
        // A forward belongs to the destination chat. The library retains the
        // source timer, including when the destination has no timer at all.
        context.expiration = None;
        context.ephemeral_setting_timestamp = None;
        context.ephemeral_shared_secret = None;
        message.set_context_info(context);
    }
    let expiration = apply_ephemeral_expiration(&mut message, expiration);
    (message, expiration)
}

fn apply_ephemeral_expiration(message: &mut wa::Message, expiration: Option<u32>) -> Option<u32> {
    let expiration = expiration.filter(|expiration| *expiration > 0)?;
    message
        .set_ephemeral_expiration(expiration)
        .then_some(expiration)
}

struct OutgoingSession {
    client: Arc<Client>,
    commands: mpsc::UnboundedSender<Command>,
    generation: u64,
    generation_shared: Arc<AtomicU64>,
}

async fn send_outgoing(
    session: OutgoingSession,
    chat: ChatId,
    jid: Jid,
    id: String,
    message: wa::Message,
    ephemeral_expiration: Option<u32>,
) -> bool {
    send_outgoing_with_kind(session, chat, jid, id, message, ephemeral_expiration, false).await
}

async fn send_outgoing_with_kind(
    session: OutgoingSession,
    chat: ChatId,
    jid: Jid,
    id: String,
    message: wa::Message,
    ephemeral_expiration: Option<u32>,
    contact: bool,
) -> bool {
    let OutgoingSession {
        client,
        commands,
        generation: session_generation,
        generation_shared: session_generation_shared,
    } = session;
    let session_is_current =
        || session_generation_shared.load(Ordering::Acquire) == session_generation;
    if !session_is_current() {
        return false;
    }
    let result = async {
        if !session_is_current() {
            return Err("The linked account changed before the message was sent".to_owned());
        }
        if jid.is_group() {
            // Uses whatsapp-rust's send cache; only a miss queries the server,
            // exactly as encryption would. No separate burst of metadata queries.
            let group = client
                .groups()
                .query_info(&jid)
                .await
                .map_err(|error| error.to_string())?;
            let lids = group
                .participants
                .iter()
                .filter(|jid| jid.is_lid())
                .filter_map(|lid| {
                    group
                        .phone_jid_for_lid_user(lid.user_base())
                        .map(|pn| (lid.user_base().to_owned(), pn.user_base().to_owned()))
                })
                .collect();
            let recipients = group
                .participants
                .iter()
                .map(Jid::to_non_ad_string)
                .collect();
            let (stored, mut saved) = mpsc::unbounded_channel();
            Completion { generation: session_generation, done: Done::GroupRecipients {
                    chat: chat.clone(),
                    id: id.clone(),
                    recipients,
                    lids,
                    stored,
                } }.send(&commands)
                .map_err(|_| "The application is shutting down".to_owned())?;
            if saved.recv().await != Some(true) {
                return Err("Could not save the group message recipients".to_owned());
            }
        }
        if !session_is_current() {
            return Err("The linked account changed before the message was sent".to_owned());
        }
        let mut options = SendOptions::default().with_message_id(id.clone());
        if let Some(expiration) = ephemeral_expiration {
            options = options.with_ephemeral_expiration(expiration);
        }
        let send = client.send_message_with_options(jid, message, options);
        tokio::pin!(send);
        loop {
            tokio::select! {
                result = &mut send => {
                    result.map_err(|error| error.to_string())?;
                    break;
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if !session_is_current() {
                        return Err("The linked account changed before the message was sent".to_owned());
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    if !session_is_current() {
        return false;
    }
    let success = result.is_ok();
    let error = result.err().map(|_| sanitized_send_error().to_owned());
    let completion = if contact {
        Completion {
            generation: session_generation,
            done: Done::ContactSent { chat, id, error },
        }
    } else {
        Completion {
            generation: session_generation,
            done: Done::Sent { chat, id, error },
        }
    };
    let _ = completion.send(&commands);
    success
}

fn sanitized_send_error() -> &'static str {
    "Could not send message"
}

fn forwarded_row(
    mut source: Message,
    chat: ChatId,
    sender: String,
    id: String,
    timestamp: i64,
    mentions: Vec<MentionRef>,
    thumbnail: Option<Vec<u8>>,
) -> Message {
    source.id = id;
    source.chat = chat;
    source.sender = sender;
    source.sender_name = None;
    source.from_me = true;
    source.timestamp = timestamp;
    source.status = Delivery::Pending;
    source.delivered_at = None;
    source.read_at = None;
    source.quoted = None;
    source.reactions.clear();
    source.edited = false;
    source.mentions = mentions;
    source.forwarded = true;
    source.thumbnail = thumbnail;
    source
}

/// Fallback chat name from a phone number or bare id.
fn fallback_name(id: &str) -> String {
    match crate::model::phone_of(id) {
        Some(digits) => crate::util::phone(digits),
        None if ChatKind::from_id(id) == ChatKind::Group => "Group".to_owned(),
        None => id.split('@').next().unwrap_or(id).to_owned(),
    }
}

/// Normalizes WhatsApp timestamps to seconds.
fn seconds(timestamp: i64) -> i64 {
    if timestamp > 100_000_000_000 {
        timestamp / 1000
    } else {
        timestamp.max(0)
    }
}

fn extension_for(mime: &str, file_name: Option<&str>) -> String {
    if let Some(extension) = file_name
        .and_then(|name| Path::new(name).extension())
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| {
            (1..=8).contains(&extension.len())
                && extension
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        return extension;
    }
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/mp4" => "mp4",
        "video/3gpp" => "3gp",
        "audio/ogg" => "ogg",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        "audio/wav" => "wav",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        _ => mime.rsplit('/').next().unwrap_or("bin"),
    }
    .chars()
    .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
    .take(16)
    .collect::<String>()
}

fn media(
    mime: Option<&String>,
    size: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
) -> Media {
    Media {
        mime: mime.cloned().unwrap_or_default(),
        size: size.unwrap_or(0),
        width,
        height,
        path: None,
        state: Default::default(),
    }
}

fn non_empty(text: &Option<String>) -> Option<String> {
    text.clone().filter(|text| !text.trim().is_empty())
}

/// Builds a text body with optional quote and mention context.
fn outgoing_text(
    text: String,
    mut context: Option<wa::ContextInfo>,
    mentions: &[String],
) -> wa::Message {
    if !mentions.is_empty() {
        context.get_or_insert_default().mentioned_jid = mentions.to_vec();
    }
    match context {
        Some(context) => wa::Message::text_with_context(text, context),
        None => wa::Message::text(text),
    }
}

/// Wire message for text that links a page, with the title and description the
/// recipient sees under it.
fn with_link_preview(
    message: wa::Message,
    text: &str,
    preview: crate::model::LinkPreview,
    thumbnail: Option<Vec<u8>>,
    expiration: Option<u32>,
) -> wa::Message {
    use whatsapp_rust::prelude::MessageField;
    let context = message
        .extended_text_message
        .as_option()
        .and_then(|extended| extended.context_info.as_option().cloned());
    let mut wire = wa::Message {
        extended_text_message: MessageField::some(wa::message::ExtendedTextMessage {
            text: Some(text.to_owned()),
            matched_text: Some(preview.url),
            title: preview.title,
            description: preview.description,
            jpeg_thumbnail: thumbnail,
            context_info: context.map_or_else(MessageField::none, MessageField::some),
            ..Default::default()
        }),
        ..Default::default()
    };
    apply_ephemeral_expiration(&mut wire, expiration);
    wire
}

/// Wire message answering a quick-reply button.
fn buttons_response(button_id: &str, label: &str, context: Option<wa::ContextInfo>) -> wa::Message {
    use wa::__buffa::oneof::message::buttons_response_message::Response;
    use whatsapp_rust::prelude::MessageField;
    wa::Message {
        buttons_response_message: MessageField::some(wa::message::ButtonsResponseMessage {
            selected_button_id: Some(button_id.to_owned()),
            context_info: context.map_or_else(MessageField::none, MessageField::some),
            r#type: Some(wa::message::buttons_response_message::Type::DISPLAY_TEXT),
            response: Some(Response::SelectedDisplayText(label.to_owned())),
        }),
        ..Default::default()
    }
}

/// Wire message answering a list row.
fn list_response(
    row_id: &str,
    title: &str,
    description: Option<String>,
    context: Option<wa::ContextInfo>,
) -> wa::Message {
    use whatsapp_rust::prelude::MessageField;
    wa::Message {
        list_response_message: MessageField::some(wa::message::ListResponseMessage {
            title: Some(title.to_owned()),
            list_type: Some(wa::message::list_response_message::ListType::SINGLE_SELECT),
            single_select_reply: MessageField::some(
                wa::message::list_response_message::SingleSelectReply {
                    selected_row_id: Some(row_id.to_owned()),
                },
            ),
            context_info: context.map_or_else(MessageField::none, MessageField::some),
            description,
        }),
        ..Default::default()
    }
}

fn response_choice_id(base: &wa::Message) -> Option<String> {
    base.buttons_response_message
        .as_option()
        .and_then(|response| response.selected_button_id.clone())
        .or_else(|| {
            base.list_response_message
                .as_option()
                .and_then(|response| response.single_select_reply.as_option())
                .and_then(|reply| reply.selected_row_id.clone())
        })
}

fn answer_choice_id(archive: &Archive, chat: &str, id: &str) -> Option<String> {
    let raw = archive.raw(chat, id).ok().flatten()?;
    let message = wa::Message::decode_from_slice(&raw).ok()?;
    response_choice_id(message.get_base_message())
}

fn full_choice(base: &wa::Message, id: &str) -> Option<(String, Option<String>)> {
    if let Some(buttons) = base.buttons_message.as_option() {
        return buttons
            .buttons
            .iter()
            .find(|button| button.button_id.as_deref() == Some(id))
            .and_then(|button| button.button_text.as_option()?.display_text.clone())
            .map(|label| (label, None));
    }
    base.list_message
        .as_option()?
        .sections
        .iter()
        .flat_map(|section| &section.rows)
        .find(|row| row.row_id.as_deref() == Some(id))
        .and_then(|row| Some((row.title.clone()?, row.description.clone())))
}

/// Extracts quote and mention context from a message.
fn context_of(base: &wa::Message) -> Option<&wa::ContextInfo> {
    if let Some(text) = base.extended_text_message.as_option() {
        return text.context_info.as_option();
    }
    if let Some(image) = base.image_message.as_option() {
        return image.context_info.as_option();
    }
    if let Some(video) = base
        .video_message
        .as_option()
        .or(base.ptv_message.as_option())
    {
        return video.context_info.as_option();
    }
    if let Some(audio) = base.audio_message.as_option() {
        return audio.context_info.as_option();
    }
    if let Some(document) = base.document_message.as_option() {
        return document.context_info.as_option();
    }
    if let Some(sticker) = base.sticker_message.as_option() {
        return sticker.context_info.as_option();
    }
    if let Some(location) = base.location_message.as_option() {
        return location.context_info.as_option();
    }
    if let Some(location) = base.live_location_message.as_option() {
        return location.context_info.as_option();
    }
    if let Some(contact) = base.contact_message.as_option() {
        return contact.context_info.as_option();
    }
    if let Some(contacts) = base.contacts_array_message.as_option() {
        return contacts.context_info.as_option();
    }
    if let Some(poll) = base
        .poll_creation_message
        .as_option()
        .or(base.poll_creation_message_v2.as_option())
        .or(base.poll_creation_message_v3.as_option())
    {
        return poll.context_info.as_option();
    }
    if let Some(buttons) = base.buttons_message.as_option() {
        return buttons.context_info.as_option();
    }
    if let Some(list) = base.list_message.as_option() {
        return list.context_info.as_option();
    }
    if let Some(response) = base.buttons_response_message.as_option() {
        return response.context_info.as_option();
    }
    if let Some(response) = base.list_response_message.as_option() {
        return response.context_info.as_option();
    }
    None
}

/// Returns raw JIDs mentioned by a message.
fn mentioned_of(base: &wa::Message) -> Vec<String> {
    context_of(base)
        .map(|context| context.mentioned_jid.clone())
        .unwrap_or_default()
}

fn forwarded_of(base: &wa::Message) -> bool {
    context_of(base).is_some_and(|context| {
        context.is_forwarded.unwrap_or(false) || context.forwarding_score.unwrap_or(0) > 0
    })
}

fn first_link(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| token.starts_with("http://") || token.starts_with("https://"))
        .map(|url| url.trim_end_matches(['.', ',', ')', ']']).to_owned())
}

/// Extracts the attachment or link-preview thumbnail, dropping one that is
/// too large or not JPEG, PNG, or WebP.
fn thumbnail_of(base: &wa::Message) -> Option<Vec<u8>> {
    let bytes = if let Some(image) = base.image_message.as_option() {
        image.jpeg_thumbnail.clone()
    } else if let Some(video) = base
        .video_message
        .as_option()
        .or(base.ptv_message.as_option())
    {
        video.jpeg_thumbnail.clone()
    } else if let Some(document) = base.document_message.as_option() {
        document.jpeg_thumbnail.clone()
    } else if let Some(text) = base.extended_text_message.as_option() {
        text.jpeg_thumbnail.clone()
    } else if let Some(location) = base.location_message.as_option() {
        location.jpeg_thumbnail.clone()
    } else if let Some(live) = base.live_location_message.as_option() {
        live.jpeg_thumbnail.clone()
    } else {
        None
    };
    bytes.filter(|bytes| crate::native_media::is_inline_thumbnail(bytes))
}

/// Displayed emoji for a reaction: `text`, else `groupingKey` when text is empty.
fn reaction_emoji(text: Option<&str>, grouping_key: Option<&str>) -> Option<String> {
    [text, grouping_key]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|emoji| !emoji.is_empty())
        .map(str::to_owned)
}

fn message_secret_from_raw(raw: &[u8]) -> Option<Vec<u8>> {
    let message = wa::Message::decode_from_slice(raw).ok()?;
    let base = message.get_base_message();
    message
        .message_context_info
        .as_option()
        .or(base.message_context_info.as_option())
        .and_then(|context| context.message_secret.clone())
        .filter(|secret| secret.len() == 32)
}

fn ensure_message_secret(raw: Vec<u8>, secret: Option<&[u8]>) -> Vec<u8> {
    let Some(secret) = secret.filter(|secret| secret.len() == 32) else {
        return raw;
    };
    if message_secret_from_raw(&raw).is_some() {
        return raw;
    }
    let Ok(mut message) = wa::Message::decode_from_slice(&raw) else {
        return raw;
    };
    let mut context = message
        .message_context_info
        .into_option()
        .unwrap_or_default();
    context.message_secret = Some(secret.to_vec());
    message.message_context_info = MessageField::some(context);
    message.encode_to_vec()
}

#[cfg(test)]
mod tests;
