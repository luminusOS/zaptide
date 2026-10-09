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

mod commands;
mod contacts;
mod device_store;
mod events;
mod history;
mod outbound;
mod poll_history;
mod polls;
mod protocol;
mod reactions;

#[cfg(test)]
use history::ParsedHistory;
#[cfg(test)]
use history::parse_conversation;
use history::{HistoryReaction, HistoryReactionBody, parse_history};
use outbound::{PastedImageRequest, download_staging_path, media_path};
#[cfg(test)]
use outbound::{THUMBNAIL_SIDE, consume_attachment_reply, encode_jpeg, thumbnail_jpeg};
use protocol::classify;

use super::{Command, Event, LinkStatus, Wake, read_sync::ReadSync};
use crate::archive::Archive;

const PAGE: usize = 60;
use crate::model::{
    Chat, ChatId, ChatKind, Contact, Content, Delivery, LinkPreview, Media, MentionRef, Message,
    Quoted, Reaction,
};
use crate::paths::AppDirs;

/// Delay after the last history chunk before sync is complete.
const SYNC_QUIET: Duration = Duration::from_secs(20);
/// Profile-picture cache lifetime.
const AVATAR_FRESH: Duration = Duration::from_secs(24 * 60 * 60);
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
            let _ = commands.send(Command::ReceiptsPrivacy {
                session_generation,
                disabled: !allowed,
            });
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
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
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
    for suffix in ["", "-wal", "-shm", "-journal"] {
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
    let mut worker = Worker {
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
        dirs,
        events,
        commands,
        waker,
        archive,
        client: None,
        handle: None,
        wa_sender,
        wa_events,
        me_pn: None,
        me_lid: None,
        me_name: None,
        me_about: None,
        lid_to_pn: HashMap::new(),
        contacts: HashMap::new(),
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
        group_info_requested: HashSet::new(),
        group_info_queue: std::collections::VecDeque::new(),
        group_info_tries: HashMap::new(),
        group_info_retry: Vec::new(),
        presence_subscribed: HashSet::new(),
        pending_older: HashMap::new(),
        next_older_request_id: 0,
        older_warned: HashSet::new(),
        pending_avatars: HashMap::new(),
        sticker_fetches: HashSet::new(),
        sticker_downloads: HashSet::new(),
        deferred_downloads: Vec::new(),
        next_attachment_batch: 0,
        read_sync: ReadSync::default(),
        poll_decrypting: 0,
        poll_history: Default::default(),
        answer_sends: HashMap::new(),
        pending_revokes: HashMap::new(),
        next_revoke_attempt: 0,
        poll_sending: HashSet::new(),
    };
    worker.load_state();
    worker.recover_interrupted_answers();
    worker.backfill();
    worker.reconcile_confirmed_answers();
    worker.relocate_media();
    worker.start_bot().await;
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        let deadline = worker.sync_deadline;
        tokio::select! {
            command = inbox.recv() => {
                match command {
                    Some(Command::Shutdown) | None => break,
                    Some(command) => worker.handle_command(command).await,
                }
            }
            Some(event) = worker.wa_events.recv() => match event {
                RuntimeEvent::WhatsApp(event) => worker.handle_wa_event(event).await,
                RuntimeEvent::PreferencesRecovered {
                    generation,
                    locks,
                    complete,
                } => {
                    worker.preferences_recovered(generation, locks, complete);
                }
            },
            _ = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                worker.sync_deadline = None;
                worker.set_syncing(false);
                worker.emit_chats();
            }
            _ = tick.tick() => {
                worker.retry_deferred_downloads(Instant::now()).await;
                worker.reveal_unconfirmed_after_grace();
                worker.refresh_legacy_preferences();
                worker.expire_older_requests();
                worker.retry_avatars();
                worker.pump_group_info();
                worker.pump_read_sync();
                worker.pump_poll_votes();
                worker.pump_poll_history();
            }
        }
    }
    let _ = worker.stop_bot().await;
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
        if let Some(Command::Downloaded {
            result: Ok(path), ..
        }) = &self.completion
        {
            let _ = std::fs::remove_file(path);
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
    Page(ChatId, Option<super::PageKey>),
    /// `Command::LoadUntil`.
    Until(ChatId, String, super::PageKey),
}

struct Worker {
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
    read_sync: ReadSync,
    poll_decrypting: usize,
    poll_history: poll_history::Requests,
    /// Answer message id to (chat, buttons message id), until the send ends.
    answer_sends: HashMap<String, (ChatId, String)>,
    pending_revokes: HashMap<(ChatId, String), PendingRevoke>,
    next_revoke_attempt: u64,
    poll_sending: HashSet<(ChatId, String)>,
    dirs: AppDirs,
    events: std::sync::mpsc::Sender<Event>,
    commands: mpsc::UnboundedSender<Command>,
    waker: Arc<dyn Wake>,
    archive: Archive,
    client: Option<Arc<Client>>,
    handle: Option<BotHandle>,
    wa_sender: mpsc::UnboundedSender<RuntimeEvent>,
    wa_events: mpsc::UnboundedReceiver<RuntimeEvent>,
    me_pn: Option<String>,
    me_lid: Option<String>,
    me_name: Option<String>,
    me_about: Option<String>,
    /// Privacy-id user part to phone-number user part.
    lid_to_pn: HashMap<String, String>,
    contacts: HashMap<String, Contact>,
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
    /// Groups queued or already requested. Queries are rate-limited.
    group_info_requested: HashSet<String>,
    /// Pending group metadata queue.
    group_info_queue: std::collections::VecDeque<String>,
    /// Group metadata attempt counts.
    group_info_tries: HashMap<String, u32>,
    /// Next retry time for failed group metadata requests.
    group_info_retry: Vec<(Instant, String)>,
    presence_subscribed: HashSet<String>,
    /// Pending phone-history request and response identity by chat.
    pending_older: HashMap<ChatId, PendingOlder>,
    next_older_request_id: u64,
    /// Chats already notified about a phone-history timeout.
    older_warned: HashSet<ChatId>,
    /// Deferred profile-picture requests and retry counts.
    pending_avatars: HashMap<(String, bool), u32>,
    /// Active recent-sticker downloads by hash.
    sticker_fetches: HashSet<String>,
    /// Active chat-sticker downloads by chat, message id, and raw fingerprint.
    sticker_downloads: HashSet<(ChatId, String, [u8; 32])>,
    /// Completed downloads waiting for an archive read, not another network fetch.
    deferred_downloads: Vec<DeferredDownload>,
    /// Correlates selected-file completion events without exposing error details.
    next_attachment_batch: u64,
}

impl Worker {
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
        self.emit(Event::Profile {
            phone: self
                .me_pn
                .as_deref()
                .and_then(crate::model::phone_of)
                .map(crate::util::phone),
            name: self.me_name.clone(),
        });
    }

    fn emit(&self, mut event: Event) {
        if let Event::Syncing(syncing) = &mut event {
            *syncing |= !self.privacy_ready;
        }
        // An upgraded archive has no reliable lock state until the library's
        // authenticated replay has completed. Keep private content off the UI
        // and out of notifications during recovery, including failed retries.
        if !self.privacy_ready
            && matches!(
                event,
                Event::Chats(_)
                    | Event::ChatUpdated(_)
                    | Event::Messages { .. }
                    | Event::MessageUpdated(_)
                    | Event::Incoming { .. }
                    | Event::Contacts(_)
                    | Event::Typing { .. }
            )
        {
            return;
        }
        let _ = self.events.send(event);
        self.waker.wake();
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

    /// Resolves phone numbers in chat-row previews.
    fn polish_chat(&self, chat: &mut Chat) {
        if let Some(last) = chat.last.as_mut() {
            last.summary = self.pn_tokens(&last.summary);
        }
    }

    fn emit_message(&self, chat: &str, id: &str) {
        if let Ok(Some(mut message)) = self.archive.message(chat, id) {
            self.polish(&mut message);
            self.emit(Event::MessageUpdated(Box::new(message)));
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
        self.pending_older.clear();
        self.older_warned.clear();
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
        self.me_pn
            .clone()
            .or_else(|| self.me_lid.clone())
            .unwrap_or_else(|| "me".to_owned())
    }

    fn is_me(&self, id: &str) -> bool {
        self.me_pn.as_deref() == Some(id) || self.me_lid.as_deref() == Some(id)
    }

    fn archive_matches_account(&self, pn: Option<&str>, lid: Option<&str>) -> bool {
        match (self.me_pn.as_deref(), pn) {
            (Some(previous), Some(current)) => previous == current,
            _ => match (self.me_lid.as_deref(), lid) {
                (Some(previous), Some(current)) => previous == current,
                _ => true,
            },
        }
    }

    fn load_state(&mut self) {
        self.me_pn = self.archive.meta("me_pn").ok().flatten();
        self.me_lid = self.archive.meta("me_lid").ok().flatten();
        self.me_name = self.archive.meta("me_name").ok().flatten();
        self.me_about = self.archive.meta("me_about").ok().flatten();
        if let Ok(lids) = self.archive.lids() {
            self.lid_to_pn = lids.into_iter().collect();
        }
        if let Ok(contacts) = self.archive.contacts() {
            self.contacts = contacts
                .into_iter()
                .map(|contact| (contact.id.clone(), contact))
                .collect();
        }
        self.emit(Event::Contacts(self.contacts.values().cloned().collect()));
        self.emit_chats();
        self.emit_profile();
    }

    /// Re-derives archived rows from raw protobufs after parser changes. Also
    /// repairs moved attachment paths or clears missing files for redownload.
    fn relocate_media(&mut self) {
        let dir = self.dirs.media_cache_dir();
        let rows = match self.archive.media_paths() {
            Ok(rows) => rows,
            Err(error) => {
                log::warn!("could not list attachments: {error}");
                return;
            }
        };
        let (mut moved, mut forgotten) = (0, 0);
        for (chat, id, path) in rows {
            if path.exists() {
                continue;
            }
            let candidate = path.file_name().map(|name| dir.join(name));
            match candidate.filter(|candidate| candidate.exists()) {
                Some(candidate) => {
                    if self.archive.set_media_path(&chat, &id, &candidate).is_ok() {
                        moved += 1;
                    }
                }
                None => {
                    if self.archive.clear_media_path(&chat, &id).is_ok() {
                        forgotten += 1;
                    }
                }
            }
        }
        if moved + forgotten > 0 {
            log::info!(
                "attachments: {moved} re-pointed to {}, {forgotten} to fetch again",
                dir.display()
            );
        }
    }

    fn backfill(&mut self) {
        // Bump to re-derive stored rows after `classify` or `thumbnail_of` change.
        const VERSION: &str = "5";
        if self.archive.meta("derived").ok().flatten().as_deref() == Some(VERSION) {
            return;
        }
        let rows = match self.archive.rows_with_raw() {
            Ok(rows) => rows,
            Err(error) => {
                log::warn!("could not read the archive for re-deriving: {error}");
                return;
            }
        };
        let started = Instant::now();
        let mut updated = 0;
        let mut retry_required = false;
        for (chat, id, raw) in rows {
            // Malformed or unsupported raw messages have no derived projection to retry.
            let Ok(message) = wa::Message::decode_from_slice(&raw) else {
                continue;
            };
            let base = message.get_base_message();
            let Some(mut content) = classify(base) else {
                continue;
            };
            let existing = match self.archive.message(&chat, &id) {
                Ok(Some(existing)) => existing,
                Ok(None) => continue,
                Err(_error) => {
                    retry_required = true;
                    continue;
                }
            };
            if matches!(existing.content, Content::Revoked) {
                continue;
            }
            if let (Some(new), Some(old)) = (content.media_mut(), existing.content.media()) {
                new.path = old.path.clone();
            }
            let mentions = self.mentions_of(&mentioned_of(base));
            let thumbnail = thumbnail_of(base);
            if self
                .archive
                .set_derived(
                    &chat,
                    &id,
                    &content,
                    &mentions,
                    thumbnail.as_deref(),
                    forwarded_of(base),
                )
                .is_ok()
            {
                updated += 1;
            } else {
                retry_required = true;
            }
        }
        if retry_required {
            log::warn!("some archived messages could not be re-derived; will retry later");
            return;
        }
        if self.archive.set_meta("derived", VERSION).is_err() {
            log::warn!("could not save the archive re-derivation version");
            return;
        }
        if updated > 0 {
            log::info!(
                "re-derived {updated} archived messages in {:.1?}",
                started.elapsed()
            );
            self.emit_chats();
        }
    }

    async fn start_bot(&mut self) {
        let path = self.dirs.session_db();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let store = match device_store::open(&path).await {
            Ok(store) => store,
            Err(error) => {
                self.set_status(LinkStatus::Failed(format!(
                    "Could not open the device store: {error}"
                )));
                return;
            }
        };
        let sender = self.wa_sender.clone();
        let bot = Bot::builder()
            .with_backend(store)
            // WhatsApp reads the linked-device name, version, and icon at pairing.
            .with_device_props(
                DevicePropsOverride::new()
                    .with_os("ZapTide")
                    .with_version(app_version())
                    .with_platform_type(wa::device_props::PlatformType::DESKTOP),
            )
            .with_event_handler(UiEvents(sender))
            .build()
            .await;
        match bot {
            Ok(bot) => {
                let handle = bot.spawn();
                self.client = Some(handle.client());
                self.handle = Some(handle);
                self.set_status(LinkStatus::Connecting);
            }
            Err(error) => self.set_status(LinkStatus::Failed(format!(
                "Could not start WhatsApp: {error}"
            ))),
        }
    }

    fn begin_privacy_recovery(&mut self) -> bool {
        if self.privacy_confirmed
            || self.privacy_recovering
            || Instant::now() < self.privacy_retry
            || !matches!(self.status, LinkStatus::Connected)
        {
            return false;
        }
        if !self.privacy_ready && self.privacy_reveal_at.is_none() {
            self.privacy_reveal_at = Some(Instant::now() + PRIVACY_GRACE);
        }
        self.privacy_recovering = true;
        true
    }

    fn refresh_legacy_preferences(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.begin_privacy_recovery() {
            return;
        }
        let sender = self.wa_sender.clone();
        let generation = self.privacy_generation;
        if !self.privacy_ready {
            self.emit(Event::Syncing(true));
        }
        let snapshot = self.privacy_snapshot;
        tokio::spawn(async move {
            let (locks, complete) = recover_chat_preferences(snapshot, |collections, mode| {
                client.resync_app_state(collections, mode)
            })
            .await;
            // Use the same queue as the replayed mutations, so lock updates
            // are applied before the completion marker can expose chat rows.
            let _ = sender.send(RuntimeEvent::PreferencesRecovered {
                generation,
                locks,
                complete,
            });
        });
    }

    /// `locks` says the lock collection synced; `complete` that everything
    /// requested did, so the one-time recovery need not run again.
    fn preferences_recovered(&mut self, generation: u64, locks: bool, complete: bool) {
        // A completed task from an unlinked device cannot authorize showing
        // chats belonging to the next linked account.
        if generation != self.privacy_generation {
            return;
        }
        self.privacy_recovering = false;
        if locks
            && (!complete
                || self
                    .archive
                    .set_meta("chat_privacy_ready_v1", "complete")
                    .is_ok())
        {
            // Without `complete` the marker stays unset, so the next start
            // retries the settings this run could not recover.
            self.privacy_confirmed = true;
            self.privacy_snapshot = false;
            self.privacy_reveal_at = None;
            self.reveal_private_content();
            if !complete {
                self.warn_unconfirmed_preferences();
            }
        } else {
            self.privacy_attempts = self.privacy_attempts.saturating_add(1);
            self.privacy_retry = Instant::now() + privacy_backoff(self.privacy_attempts);
            log::warn!(
                "chat lock state recovery failed (attempt {}); retrying later",
                self.privacy_attempts
            );
            // Waiting longer does not help a failed sync: show what is known.
            self.reveal_unconfirmed();
        }
    }

    fn reveal_unconfirmed_after_grace(&mut self) {
        if !self.privacy_snapshot && !matches!(self.status, LinkStatus::Connected) {
            // A pending resync survives reconnects. Keep its deadline so a
            // silent attempt can still reveal once the link comes back.
            if !self.privacy_recovering {
                self.privacy_reveal_at = None;
            }
            return;
        }
        if self
            .privacy_reveal_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.reveal_unconfirmed();
        }
    }

    /// Shows chats whose lock state could not be confirmed yet. Chats already
    /// known to be locked stay hidden, and later lock updates still apply.
    fn reveal_unconfirmed(&mut self) {
        self.privacy_reveal_at = None;
        if self.privacy_ready {
            return;
        }
        self.reveal_private_content();
        self.warn_unconfirmed_preferences();
    }

    fn warn_unconfirmed_preferences(&mut self) {
        if !self.privacy_warned {
            self.privacy_warned = true;
            self.emit(Event::Info(
                "Chat settings could not be read from your phone yet. Some chats may show the settings stored on this computer."
                    .to_owned(),
            ));
        }
    }

    fn reveal_private_content(&mut self) {
        if self.privacy_ready {
            return;
        }
        self.privacy_ready = true;
        self.load_state();
        self.emit(Event::Syncing(self.syncing));
        // The picker may have been sent an empty Received shelf meanwhile.
        self.emit_stickers();
        // Answer the reads made while content was withheld, now that the
        // chat list they belong to has been sent.
        for page in std::mem::take(&mut self.withheld_pages) {
            match page {
                WithheldPage::Page(chat, before) => self.send_page(&chat, before),
                WithheldPage::Until(chat, id, before) => self.load_until(chat, id, before),
            }
        }
    }

    async fn stop_bot(&mut self) -> bool {
        self.client = None;
        let Some(handle) = self.handle.take() else {
            return true;
        };
        if wait_for_shutdown(Duration::from_secs(5), handle.shutdown()).await {
            true
        } else {
            log::warn!("the WhatsApp connection did not stop in time");
            false
        }
    }

    // --- ids -------------------------------------------------------------

    fn learn_lid(&mut self, lid: &str, pn: &str) {
        if lid.is_empty() || pn.is_empty() {
            return;
        }
        if self.lid_to_pn.get(lid).is_some_and(|known| known == pn) {
            return;
        }
        self.lid_to_pn.insert(lid.to_owned(), pn.to_owned());
        match self.archive.put_lid(lid, pn) {
            Ok(true) => self.emit_chats(),
            Ok(false) => {}
            Err(error) => log::warn!("could not remember an id mapping: {error}"),
        }
    }

    fn learn_pair(&mut self, a: &Jid, b: &Jid) {
        if a.is_lid() && b.is_pn() {
            self.learn_lid(a.user_base(), b.user_base());
        } else if a.is_pn() && b.is_lid() {
            self.learn_lid(b.user_base(), a.user_base());
        }
    }

    fn learn_source(&mut self, source: &MessageSource) {
        if let Some(alt) = &source.sender_alt {
            let sender = source.sender.clone();
            self.learn_pair(&sender, alt);
        }
        if let Some(alt) = &source.recipient_alt {
            let chat = source
                .recipient
                .clone()
                .unwrap_or_else(|| source.chat.clone());
            self.learn_pair(&chat, alt);
        }
    }

    /// Returns the archive id for a JID, resolving known privacy ids.
    fn canonical(&self, jid: &Jid) -> String {
        if jid.is_lid()
            && let Some(pn) = self.lid_to_pn.get(jid.user_base())
        {
            let pn = format!("{pn}@s.whatsapp.net");
            return if self.is_me(&pn) { self.me() } else { pn };
        }
        let id = jid.to_non_ad_string();
        if self.is_me(&id) {
            return self.me();
        }
        id
    }

    fn canonical_str(&self, id: &str) -> String {
        match id.parse::<Jid>() {
            Ok(jid) => self.canonical(&jid),
            Err(_) => id.to_owned(),
        }
    }

    fn jid_of(id: &str) -> Option<Jid> {
        id.parse().ok()
    }

    // --- names -----------------------------------------------------------

    fn saved_name(&self, id: &str) -> Option<String> {
        self.contacts
            .get(id)
            .and_then(|contact| contact.full_name.clone())
            .filter(|name| !name.is_empty())
    }

    fn contact_name(&self, id: &str) -> Option<String> {
        self.contacts.get(id).and_then(Contact::label)
    }

    /// Resolves a name for a quote or mention.
    fn name_for(&self, id: &str) -> Option<String> {
        if self.is_me(id) || id == self.me() {
            return Some("You".to_owned());
        }
        if let Some(name) = self.contact_name(id) {
            return Some(name);
        }
        crate::model::phone_of(id).map(crate::util::phone)
    }

    /// Returns the best current chat name.
    fn chat_name(&self, id: &str, push_name: Option<&str>) -> String {
        if id == self.me() {
            return "You".to_owned();
        }
        if let Some(name) = self
            .contacts
            .get(id)
            .and_then(|contact| contact.full_name.clone())
            .filter(|name| !name.is_empty())
        {
            return name;
        }
        if let Some(digits) = crate::model::phone_of(id) {
            return crate::util::phone(digits);
        }
        if let Some(name) = push_name
            .filter(|name| !name.is_empty())
            .or_else(|| self.contacts.get(id)?.push_name.as_deref())
        {
            return format!("~{name}");
        }
        fallback_name(id)
    }

    fn remember_push_name(&mut self, id: &str, push_name: &str) {
        if push_name.is_empty() || id == self.me() {
            return;
        }
        let contact = self
            .contacts
            .entry(id.to_owned())
            .or_insert_with(|| Contact {
                id: id.to_owned(),
                full_name: None,
                push_name: None,
            });
        if contact.push_name.as_deref() == Some(push_name) {
            return;
        }
        contact.push_name = Some(push_name.to_owned());
        let contact = contact.clone();
        if let Err(error) = self.archive.upsert_contact(&contact) {
            log::warn!("could not save a contact: {error}");
        }
        self.emit(Event::Contacts(vec![contact]));
        self.refresh_chat_name(id);
    }

    /// Replaces a fallback chat name when a better one is known.
    fn refresh_chat_name(&mut self, id: &str) {
        let Ok(Some(chat)) = self.archive.chat(id) else {
            return;
        };
        if chat.kind == ChatKind::Group {
            return;
        }
        let name = self.chat_name(id, None);
        if name != chat.name {
            let _ = self.archive.rename_chat(id, &name);
            self.emit_chat(id);
        }
    }

    fn ensure_chat(&mut self, id: &str, push_name: Option<&str>) {
        match self.archive.chat(id) {
            Ok(Some(chat)) => {
                if chat.kind != ChatKind::Group {
                    let name = self.chat_name(id, push_name);
                    if name != chat.name {
                        let _ = self.archive.rename_chat(id, &name);
                    }
                }
            }
            Ok(None) => {
                let name = self.chat_name(id, push_name);
                if self.archive.ensure_chat(id, &name).is_err() {
                    log::warn!("could not create a chat");
                }
            }
            Err(_error) => log::warn!("could not read a chat"),
        }
        if ChatKind::from_id(id) == ChatKind::Group {
            self.request_group_info(id, false);
        }
    }

    /// Queues a group metadata request, at the front when `force` is true.
    fn request_group_info(&mut self, id: &str, force: bool) {
        if force {
            self.group_info_requested.remove(id);
        } else {
            let known = self.archive.chat(id).ok().flatten().is_some_and(|chat| {
                chat.name != fallback_name(id) && !chat.participants.is_empty()
            });
            if known {
                return;
            }
        }
        if !self.group_info_requested.insert(id.to_owned()) {
            return;
        }
        if force {
            self.group_info_queue.push_front(id.to_owned());
        } else {
            self.group_info_queue.push_back(id.to_owned());
        }
    }

    fn group_retry_delay(tries: u32) -> Duration {
        Duration::from_secs(30 * 2u64.pow(tries.saturating_sub(1).min(5)))
            .min(Duration::from_secs(600))
    }

    /// Sends a limited number of group metadata requests per tick.
    fn pump_group_info(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = {
            let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.group_info_retry)
                .into_iter()
                .partition(|(at, _)| *at <= now);
            self.group_info_retry = later;
            due.into_iter().map(|(_, id)| id).collect()
        };
        for id in due {
            if self.group_info_requested.insert(id.clone()) {
                self.group_info_queue.push_back(id);
            }
        }
        for _ in 0..2 {
            let Some(id) = self.group_info_queue.pop_front() else {
                return;
            };
            self.query_group_info(&id);
        }
    }

    /// Schedules metadata retry with backoff, or stops on permanent failure.
    fn handle_failed_group(&mut self, chat: String, permanent: bool) {
        self.group_info_requested.remove(&chat);
        if permanent {
            self.group_info_tries.remove(&chat);
        } else {
            let tries = self.group_info_tries.entry(chat.clone()).or_insert(0);
            *tries += 1;
            if *tries <= 7 {
                self.group_info_retry
                    .push((Instant::now() + Self::group_retry_delay(*tries), chat));
            }
        }
    }

    fn query_group_info(&mut self, id: &str) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(id)) else {
            // Requeue until the link is available.
            self.group_info_requested.remove(id);
            let tries = self.group_info_tries.entry(id.to_owned()).or_insert(0);
            *tries += 1;
            self.group_info_retry.push((
                Instant::now() + Self::group_retry_delay(*tries),
                id.to_owned(),
            ));
            return;
        };
        let commands = self.commands.clone();
        let chat = id.to_owned();
        let me: Vec<String> = [self.me_pn.clone(), self.me_lid.clone()]
            .into_iter()
            .flatten()
            .collect();
        let lids = self.lid_to_pn.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            match client.groups().get_metadata(&jid).await {
                Ok(metadata) => {
                    let canonical = |jid: &Jid| -> String {
                        if jid.is_lid()
                            && let Some(pn) = lids.get(jid.user_base())
                        {
                            return format!("{pn}@s.whatsapp.net");
                        }
                        jid.to_non_ad_string()
                    };
                    let mut participants = Vec::new();
                    let mut admin = false;
                    for participant in &metadata.participants {
                        let id = participant
                            .phone_number
                            .as_ref()
                            .map(canonical)
                            .unwrap_or_else(|| canonical(&participant.jid));
                        let mine = me.contains(&id)
                            || participant
                                .lid
                                .as_ref()
                                .is_some_and(|lid| me.contains(&lid.to_non_ad_string()))
                            || me.contains(&participant.jid.to_non_ad_string());
                        if mine && participant.is_admin() {
                            admin = true;
                        }
                        participants.push(id);
                    }
                    let _ = commands.send(Command::GroupInfo {
                        session_generation,
                        chat,
                        name: (!metadata.subject.is_empty()).then(|| metadata.subject.clone()),
                        participants,
                        read_only: metadata.is_announcement && !admin,
                        // GroupEphemeralSettings carries a trigger mode, not a
                        // timestamp; a zero setting timestamp keeps later
                        // authoritative updates (protocol messages) able to
                        // override the value fetched here.
                        ephemeral_expiration: metadata
                            .ephemeral
                            .as_ref()
                            .and_then(|value| value.expiration),
                        ephemeral_setting_timestamp: None,
                    });
                }
                Err(error) => {
                    let text = error.to_string();
                    // Missing, forbidden, and unauthorized groups do not retry.
                    let permanent = ["item-not-found", "forbidden", "not-authorized"]
                        .iter()
                        .any(|word| text.contains(word));
                    log::warn!("could not fetch group metadata");
                    let _ = commands.send(Command::GroupInfoFailed {
                        session_generation,
                        chat,
                        permanent,
                    });
                }
            }
        });
    }

    fn remember_identity(&mut self, pn: Option<Jid>, lid: Option<Jid>, name: Option<String>) {
        if let Some(pn) = pn {
            let pn = pn.to_non_ad_string();
            let _ = self.archive.set_meta("me_pn", &pn);
            self.me_pn = Some(pn);
        }
        if let Some(lid) = lid {
            let lid = lid.to_non_ad_string();
            let _ = self.archive.set_meta("me_lid", &lid);
            self.me_lid = Some(lid);
        }
        if let (Some(pn), Some(lid)) = (self.me_pn.clone(), self.me_lid.clone())
            && let (Some(pn), Some(lid)) = (Self::jid_of(&pn), Self::jid_of(&lid))
        {
            self.learn_pair(&lid, &pn);
        }
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            let _ = self.archive.set_meta("me_name", &name);
            self.me_name = Some(name);
        }
        self.emit_profile();
    }

    /// Content is hidden again and lock state must be read from the next
    /// account: used whenever a link ends, however it ends.
    fn reset_privacy(&mut self) {
        self.privacy_ready = false;
        self.privacy_confirmed = false;
        self.privacy_snapshot = false;
        self.privacy_reveal_at = None;
        self.privacy_attempts = 0;
        self.privacy_warned = false;
        self.privacy_recovering = false;
        self.privacy_retry = Instant::now();
        // Reads for the unlinked account must not be answered for the next.
        self.withheld_pages.clear();
    }

    async fn on_logged_out(&mut self) {
        self.privacy_generation = self.privacy_generation.wrapping_add(1);
        self.pair_request_id = self.pair_request_id.wrapping_add(1);
        self.session_generation = self.session_generation.wrapping_add(1);
        self.session_generation_shared
            .store(self.session_generation, Ordering::Release);
        self.clear_history_sync_state();
        // The archive is cleared on logout, so pending answers have nothing to reopen.
        self.answer_sends.clear();
        self.pending_revokes.clear();
        self.deferred_downloads.clear();
        let markers = archive_cleanup_markers(&self.dirs);
        let archive_marker = self.archive.mark_logout_cleanup_required().is_ok();
        let file_marker = persist_archive_cleanup_marker(&markers);
        if !archive_marker && !file_marker {
            log::error!("could not persist any logout cleanup marker");
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Error(
                "Local conversations could not be cleared. Restart is blocked to protect data."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Local conversation cleanup marker unavailable".to_owned(),
            ));
            return;
        }
        let (wa_sender, wa_events) = mpsc::unbounded_channel();
        self.wa_sender = wa_sender;
        self.wa_events = wa_events;
        if !self.stop_bot().await {
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Chats(Vec::new()));
            self.emit(Event::Contacts(Vec::new()));
            self.emit(Event::Error(
                "Local conversations could not be cleared because the device session did not stop."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Device session shutdown timed out before local cleanup".to_owned(),
            ));
            return;
        }
        let cleanup_result = {
            let _cache_guard = self.session_cache_lock.lock().await;
            clear_logged_out_data(&self.dirs, &self.archive)
        };
        if let Err(error) = cleanup_result {
            log::warn!("could not clear account data: {error}");
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Chats(Vec::new()));
            self.emit(Event::Contacts(Vec::new()));
            self.emit(Event::Error(
                "Local conversations could not be cleared. Restart is blocked to protect data."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Local conversation cleanup failed".to_owned(),
            ));
            return;
        }
        if let Err(error) = remove_archive_cleanup_markers(&markers) {
            log::warn!("could not remove archive cleanup marker: {error}");
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Chats(Vec::new()));
            self.emit(Event::Error(
                "Local conversations were cleared, but cleanup could not be finalized. Restart is blocked."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Local conversation cleanup could not be finalized".to_owned(),
            ));
            return;
        }
        if let Err(error) = self.archive.finish_logout_cleanup() {
            log::error!("could not finish the encrypted archive cleanup marker: {error}");
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Error(
                "Local conversations were cleared, but cleanup could not be finalized. Restart is blocked."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Local conversation cleanup could not be finalized".to_owned(),
            ));
            return;
        }
        self.lid_to_pn.clear();
        self.contacts.clear();
        self.group_info_requested.clear();
        self.group_info_queue.clear();
        self.group_info_tries.clear();
        self.group_info_retry.clear();
        self.presence_subscribed.clear();
        self.read_sync = ReadSync::default();
        self.poll_decrypting = 0;
        self.poll_sending.clear();
        self.poll_history = Default::default();
        self.pending_avatars.clear();
        self.avatar_generations.clear();
        self.sticker_fetches.clear();
        self.sticker_downloads.clear();
        self.me_pn = None;
        self.me_lid = None;
        self.me_name = None;
        self.emit_profile();
        self.me_about = None;
        self.qr = None;
        self.pair_code = None;
        self.pairing_phone = None;
        self.emit(Event::Chats(Vec::new()));
        self.emit(Event::Contacts(Vec::new()));
        self.reset_privacy();
        self.set_status(LinkStatus::LoggedOut);
        // Recreate the store so the next connection starts linking.
        self.start_bot().await;
    }

    fn on_contact_update(&mut self, update: &wa_events::ContactUpdate) {
        if let (Some(lid), Some(pn)) = (&update.action.lid_jid, &update.action.pn_jid)
            && let (Some(lid), Some(pn)) = (Self::jid_of(lid), Self::jid_of(pn))
        {
            self.learn_pair(&lid, &pn);
        }
        let id = self.canonical(&update.jid);
        let name = update
            .action
            .full_name
            .clone()
            .or_else(|| update.action.first_name.clone())
            .filter(|name| !name.is_empty());
        let contact = self.contacts.entry(id.clone()).or_insert_with(|| Contact {
            id: id.clone(),
            full_name: None,
            push_name: None,
        });
        if contact.full_name == name {
            return;
        }
        contact.full_name = name;
        let contact = contact.clone();
        if let Err(error) = self.archive.upsert_contact(&contact) {
            log::warn!("could not save a contact: {error}");
        }
        self.emit(Event::Contacts(vec![contact]));
        self.refresh_chat_name(&id);
    }

    fn on_receipt(&mut self, receipt: &wa_events::Receipt) {
        self.learn_source(&receipt.source);
        let chat = self.canonical(&receipt.source.chat);
        log::debug!(
            "receipt {:?} from {} (chat {chat}, from me: {}, offline: {}) for {:?}",
            receipt.r#type,
            receipt.source.sender,
            receipt.source.is_from_me,
            receipt.offline,
            receipt.message_ids
        );
        let status = match receipt.r#type {
            ReceiptType::Delivered => Delivery::Delivered,
            // An inactive-device receipt still means delivered.
            ReceiptType::Inactive => Delivery::Delivered,
            ReceiptType::Read => Delivery::Read,
            ReceiptType::Played => Delivery::Played,
            ReceiptType::ReadSelf | ReceiptType::PlayedSelf => {
                // The receipt time is when the phone read, not the position
                // it read through. A delayed receipt must leave newer messages.
                // A peer named by an unmapped privacy id is found by message id.
                let mut read = Vec::new();
                for id in &receipt.message_ids {
                    let target = match self.archive.message(&chat, id).ok().flatten() {
                        Some(message) => (!message.from_me).then(|| chat.clone()),
                        None => self.archive.incoming_chat_of(id).ok().flatten(),
                    };
                    if let Some(target) = target {
                        let _ = self.archive.mark_read_to(&target, id);
                        if !read.contains(&target) {
                            read.push(target);
                        }
                    }
                }
                for target in read {
                    self.emit_chat(&target);
                }
                return;
            }
            // Own-device delivery counts as read only in the self chat.
            ReceiptType::Sender if chat == self.me() => Delivery::Read,
            _ => return,
        };
        let at = receipt.timestamp.timestamp();
        if ChatKind::from_id(&chat) == ChatKind::Group {
            let recipient = self.canonical(&receipt.source.sender);
            if self.is_me(&recipient) {
                return;
            }
            for id in &receipt.message_ids {
                if !self
                    .archive
                    .message(&chat, id)
                    .ok()
                    .flatten()
                    .is_some_and(|row| row.from_me)
                {
                    continue;
                }
                match self
                    .archive
                    .group_receipt(&chat, id, &recipient, status, at)
                {
                    Ok(true) => self.emit_message(&chat, id),
                    Ok(false) => {}
                    Err(error) => log::warn!("could not file a group receipt: {error}"),
                }
            }
            self.emit_chat(&chat);
            return;
        }
        let mut newest = 0;
        let mut changed = 0;
        for id in &receipt.message_ids {
            match self.archive.set_status(&chat, id, status, at) {
                Ok(true) => {
                    changed += 1;
                    self.emit_message(&chat, id);
                }
                Ok(false) => {}
                Err(_error) => log::warn!("could not file a receipt"),
            }
            if let Ok(Some(message)) = self.archive.message(&chat, id) {
                newest = newest.max(message.timestamp);
            }
        }
        log::debug!(
            "receipt moved {changed} of {} messages to {status:?}",
            receipt.message_ids.len(),
        );
        // Read receipts advance all earlier messages.
        if status >= Delivery::Read
            && newest > 0
            && let Ok(ids) = self.archive.advance_statuses(&chat, newest, status, at)
        {
            for id in ids {
                self.emit_message(&chat, &id);
            }
        }
        self.emit_chat(&chat);
    }

    /// Returns raw mention tokens and canonical ids.
    fn mentions_of(&self, raw: &[String]) -> Vec<MentionRef> {
        raw.iter()
            .filter_map(|jid| {
                let user = jid.split('@').next()?.to_owned();
                if user.is_empty() {
                    return None;
                }
                Some(MentionRef {
                    user,
                    id: self.canonical_str(jid),
                    name: None,
                })
            })
            .collect()
    }

    fn ingest(&mut self, message: &Arc<wa::Message>, info: &MessageInfo) {
        self.learn_source(&info.source);
        if info.source.chat.is_status_broadcast() {
            return;
        }
        let chat = self.canonical(&info.source.chat);
        let from_me = info.source.is_from_me;
        let sender = if from_me {
            self.me()
        } else {
            self.canonical(&info.source.sender)
        };
        let push_name = (!info.push_name.is_empty()).then(|| info.push_name.clone());
        let base = message.get_base_message();
        if let Some(expiration) = base.get_ephemeral_expiration()
            && self
                .archive
                .ephemeral_expiration(&chat)
                .ok()
                .flatten()
                .is_none()
        {
            self.ensure_chat(&chat, push_name.as_deref());
            let _ = self.archive.set_ephemeral(&chat, expiration, 0);
        }

        if let Some(protocol) = base.protocol_message.as_option() {
            use wa::message::protocol_message::Type;
            if protocol.r#type == Some(Type::EPHEMERAL_SETTING) {
                if let Some(expiration) = protocol.ephemeral_expiration {
                    let timestamp = protocol
                        .ephemeral_setting_timestamp
                        .unwrap_or_else(|| info.timestamp.timestamp());
                    let used_fallback = protocol.ephemeral_setting_timestamp.is_none();
                    self.ensure_chat(&chat, push_name.as_deref());
                    let accepted = self
                        .archive
                        .set_ephemeral(&chat, expiration, timestamp)
                        .unwrap_or(false);
                    log::debug!(
                        target: "zaptide::disappearing",
                        "protocol timer update: duration={expiration}s timestamp={timestamp} fallback_timestamp={used_fallback} accepted={accepted}"
                    );
                    if accepted {
                        self.emit_chat(&chat);
                    }
                } else {
                    log::debug!(
                        target: "zaptide::disappearing",
                        "protocol timer update missing expiration"
                    );
                }
                return;
            }
            let Some(target) = protocol.key.as_option().and_then(|key| key.id.clone()) else {
                return;
            };
            match protocol.r#type {
                Some(Type::REVOKE) => {
                    self.confirm_revoke(&chat, &target);
                }
                Some(Type::MESSAGE_EDIT) => {
                    if let Some(edited) = protocol.edited_message.as_option()
                        && let Some(mut content) = classify(edited.get_base_message())
                    {
                        // Preserve downloaded media when updating a caption.
                        if let Ok(Some(existing)) = self.archive.message(&chat, &target)
                            && let (Some(new), Some(old)) =
                                (content.media_mut(), existing.content.media())
                        {
                            new.path = old.path.clone();
                        }
                        if let Ok(true) = self.archive.set_content(&chat, &target, &content, true) {
                            self.emit_message(&chat, &target);
                            self.emit_chat(&chat);
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if let Some(reaction) = base.reaction_message.as_option() {
            self.store_plain_reaction(&chat, &sender, from_me, reaction);
            return;
        }
        if base.enc_reaction_message.is_set() {
            self.store_enc_reaction(&chat, &sender, from_me, base);
            return;
        }
        if let Some(update) = base.poll_update_message.as_option() {
            self.ingest_poll_vote(
                &chat,
                &info.id,
                &info.source.sender.to_non_ad_string(),
                from_me,
                info.timestamp.timestamp(),
                update,
            );
            return;
        }
        let Some(content) = classify(base) else {
            return;
        };
        let quoted = self.quoted_of(base);
        let mentions = self.mentions_of(&mentioned_of(base));
        let row = Message {
            id: info.id.to_string(),
            chat: chat.clone(),
            sender,
            sender_name: if from_me {
                None
            } else {
                push_name.as_ref().map(ToString::to_string)
            },
            from_me,
            timestamp: info.timestamp.timestamp(),
            content,
            status: if from_me {
                Delivery::Sent
            } else {
                Delivery::None
            },
            delivered_at: None,
            read_at: None,
            quoted,
            reactions: Vec::new(),
            edited: false,
            mentions,
            forwarded: forwarded_of(base),
            thumbnail: thumbnail_of(base),
        };
        let is_poll = matches!(row.content, Content::Poll { .. });
        self.remember_poll(&row, message, &info.source.sender.to_non_ad_string(), None);
        self.store_message(row, Some(message.encode_to_vec()), push_name.as_deref());
        if is_poll {
            self.pump_poll_votes();
        }
    }

    fn ingest_undecryptable(&mut self, info: &MessageInfo) {
        self.learn_source(&info.source);
        if info.source.chat.is_status_broadcast() || info.source.is_from_me {
            return;
        }
        let chat = self.canonical(&info.source.chat);
        if self
            .archive
            .message(&chat, &info.id)
            .ok()
            .flatten()
            .is_some()
        {
            return;
        }
        let push_name = (!info.push_name.is_empty()).then(|| info.push_name.clone());
        let row = Message {
            id: info.id.to_string(),
            chat,
            sender: self.canonical(&info.source.sender),
            sender_name: push_name.as_ref().map(ToString::to_string),
            from_me: false,
            timestamp: info.timestamp.timestamp(),
            content: Content::Unsupported {
                what: "Waiting for this message. Open WhatsApp on your phone".to_owned(),
            },
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        };
        self.store_message(row, None, push_name.as_deref());
    }

    /// Archives a message and emits chat and row updates.
    fn store_message(&mut self, message: Message, raw: Option<Vec<u8>>, push_name: Option<&str>) {
        let chat = message.chat.clone();
        self.ensure_chat(&chat, if message.from_me { None } else { push_name });
        if let Some(push_name) = push_name
            && !message.from_me
        {
            let sender = message.sender.clone();
            self.remember_push_name(&sender, push_name);
        }
        let is_new = self
            .archive
            .message(&chat, &message.id)
            .ok()
            .flatten()
            .is_none();
        if let Err(error) = self.archive.insert_message(&message, raw.as_deref()) {
            log::warn!("could not store a message: {error}");
            return;
        }
        if message.from_me
            && matches!(
                message.status,
                Delivery::Sent | Delivery::Delivered | Delivery::Read | Delivery::Played
            )
            && let (Some(quoted), Some(raw)) = (message.quoted.as_ref(), raw.as_deref())
        {
            self.confirm_answer(&chat, &quoted.id, raw);
        }
        if matches!(
            message.content,
            Content::Buttons { .. } | Content::List { .. }
        ) {
            self.confirm_answers_for(&chat, &message.id);
        }
        let unread = is_new
            && !message.from_me
            && self
                .archive
                .read_through(&chat)
                .ok()
                .flatten()
                // A new live message may share the read message's second. Its
                // distinct id already passed the duplicate check above.
                .is_none_or(|through| message.timestamp >= through);
        if unread {
            let _ = self.archive.bump_unread(&chat);
        } else if message.from_me
            && matches!(
                message.status,
                Delivery::Sent | Delivery::Delivered | Delivery::Read | Delivery::Played
            )
        {
            // A reply sent from the phone/another companion reads the preceding
            // conversation there. Replayed replies cannot clear newer arrivals.
            let _ = self.archive.mark_read_to(&chat, &message.id);
        }
        let mut stored = self
            .archive
            .message(&chat, &message.id)
            .ok()
            .flatten()
            .unwrap_or(message);
        self.polish(&mut stored);
        // Notify only for live incoming messages, not history replay.
        let incoming = (unread && !self.syncing).then(|| stored.clone());
        self.emit(Event::Messages {
            chat: chat.clone(),
            messages: vec![stored],
            older: false,
            complete: false,
        });
        self.emit_chat(&chat);
        if let Some(message) = incoming {
            self.emit(Event::Incoming {
                chat,
                message: Box::new(message),
            });
        }
    }

    fn quoted_of(&self, base: &wa::Message) -> Option<Quoted> {
        let context = context_of(base)?;
        let id = context.stanza_id.clone().filter(|id| !id.is_empty())?;
        let sender = context
            .participant
            .as_deref()
            .map(|participant| self.canonical_str(participant))
            .unwrap_or_default();
        let (summary, listed) = context
            .quoted_message
            .as_option()
            .map(|quoted| {
                let base = quoted.get_base_message();
                (
                    classify(base)
                        .map(|content| content.summary())
                        .unwrap_or_default(),
                    self.mentions_of(&mentioned_of(base)),
                )
            })
            .unwrap_or_default();
        // Recover quote mentions from `@user` tokens when metadata is missing.
        let summary = self.pn_tokens(&summary);
        let mentions = if listed.is_empty() {
            self.mention_tokens(&summary)
        } else {
            listed
        };
        Some(Quoted {
            sender_name: self.name_for(&sender),
            id,
            sender,
            summary,
            mentions,
        })
    }

    /// Replaces known privacy ids in `@user` tokens with phone-number ids.
    fn pn_tokens(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find('@') {
            out.push_str(&rest[..at]);
            out.push('@');
            let after = &rest[at + 1..];
            let digits = after
                .char_indices()
                .find(|(_, c)| !c.is_ascii_digit())
                .map_or(after.len(), |(index, _)| index);
            match self.lid_to_pn.get(&after[..digits]) {
                Some(pn) if digits > 0 => {
                    out.push_str(pn);
                    rest = &after[digits..];
                }
                _ => rest = after,
            }
        }
        out.push_str(rest);
        out
    }

    /// Infers canonical mention ids from `@user` tokens.
    fn mention_tokens(&self, text: &str) -> Vec<MentionRef> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        let mut offset = 0;
        while found.len() < 128 {
            let Some(at) = text[offset..].find('@').map(|at| at + offset) else {
                break;
            };
            let after = &text[at + 1..];
            let digits = after
                .char_indices()
                .find(|(_, c)| !c.is_ascii_digit())
                .map_or(after.len(), |(index, _)| index);
            let user = &after[..digits];
            let boundary_before = at == 0
                || text[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\''));
            let boundary_after = after[digits..]
                .chars()
                .next()
                .is_none_or(|c| !crate::safety::mention_token_char(c));
            if digits >= 5 && boundary_before && boundary_after && seen.insert(user.to_owned()) {
                let id = match self.lid_to_pn.get(user) {
                    Some(pn) => format!("{pn}@s.whatsapp.net"),
                    None => format!("{user}@s.whatsapp.net"),
                };
                let id = self.canonical_str(&id);
                found.push(MentionRef {
                    user: user.to_owned(),
                    id,
                    name: None,
                });
            }
            offset = at + 1;
        }
        found
    }

    // --- history ---------------------------------------------------------

    async fn on_history_sync(&mut self, lazy: &wa_events::LazyHistorySync) {
        let on_demand =
            lazy.sync_type() == ON_DEMAND || lazy.peer_data_request_session_id().is_some();
        if !on_demand {
            self.sync_deadline = Some(Instant::now() + SYNC_QUIET);
            self.set_syncing(true);
        }
        let compressed = lazy.compressed_bytes().clone();
        let parsed = tokio::task::spawn_blocking(move || parse_history(&compressed)).await;
        match parsed {
            Ok(Ok(parsed)) => {
                if on_demand {
                    log::info!(
                        "poll recovery: on-demand history received; chats={}, messages={}, standalone_votes={}",
                        parsed.chats.len(),
                        parsed
                            .chats
                            .iter()
                            .map(|chat| chat.messages.len())
                            .sum::<usize>(),
                        parsed
                            .chats
                            .iter()
                            .map(|chat| chat.poll_updates.len())
                            .sum::<usize>()
                    );
                }
                let filed = self.apply_history(parsed, !on_demand);
                if on_demand {
                    self.answer_older(
                        filed,
                        lazy.peer_data_request_session_id().map(str::to_owned),
                    );
                }
            }
            Ok(Err(error)) => {
                log::warn!("a history chunk could not be read");
                self.emit(Event::Error(format!(
                    "Could not read part of the chat history: {error}"
                )));
            }
            Err(_error) => log::warn!("history parsing worker failed"),
        }
        if !on_demand && lazy.progress().is_some_and(|progress| progress >= 100) {
            self.sync_deadline = Some(Instant::now() + Duration::from_secs(3));
        }
        self.emit_chats();
    }

    /// Complete only requests identified by the phone's response session id.
    fn answer_older(
        &mut self,
        filed: Vec<(ChatId, usize, Option<bool>)>,
        protocol_id: Option<String>,
    ) {
        let Some(protocol_id) = protocol_id else {
            // ON_DEMAND type alone cannot identify which local retry produced this chunk.
            return;
        };
        for (chat, count, more_on_phone) in filed {
            let Some(request) = self.pending_older.get_mut(&chat) else {
                continue;
            };
            if request.protocol_id.as_deref() == Some(protocol_id.as_str()) {
                self.record_older_response(chat, count, more_on_phone);
            } else if request.protocol_id.is_none() {
                if let Some(early) = request.early_responses.get_mut(&protocol_id) {
                    early.received_count = early.received_count.saturating_add(count);
                    if let Some(more_on_phone) = more_on_phone {
                        early.more_on_phone = Some(more_on_phone);
                    }
                } else if request.early_responses.len() < MAX_EARLY_HISTORY_IDS {
                    let mut early = EarlyOlderResponse {
                        received_count: count,
                        ..Default::default()
                    };
                    early.more_on_phone = more_on_phone;
                    request.early_responses.insert(protocol_id.clone(), early);
                } else {
                    log::warn!(
                        "too many unrelated history IDs arrived before request registration"
                    );
                }
            }
        }
    }

    fn record_older_response(&mut self, chat: ChatId, count: usize, more_on_phone: Option<bool>) {
        let Some(request) = self.pending_older.get_mut(&chat) else {
            return;
        };
        request.received_count = request.received_count.saturating_add(count);
        // `more_on_phone` comes from EndOfHistoryTransferType and marks the target chat's final chunk.
        if let Some(more_on_phone) = more_on_phone {
            request.more_on_phone = Some(more_on_phone);
        }
        if request.more_on_phone.is_some() {
            let request = self
                .pending_older
                .remove(&chat)
                .expect("request still present");
            self.finish_older_request(
                chat,
                request.received_count,
                request.more_on_phone,
                request.before,
            );
        }
    }

    fn finish_older_request(
        &mut self,
        chat: ChatId,
        count: usize,
        more_on_phone: Option<bool>,
        (before_time, before_id): super::PageKey,
    ) {
        let more = count > 0 && more_on_phone != Some(false);
        match self
            .archive
            .messages(&chat, Some((before_time, &before_id)), 500)
        {
            Ok(mut messages) => {
                for message in &mut messages {
                    self.polish(message);
                }
                self.emit(Event::Messages {
                    chat: chat.clone(),
                    messages,
                    older: true,
                    complete: false,
                })
            }
            Err(error) => log::warn!("could not read older messages: {error}"),
        }
        self.emit(Event::OlderFetched { chat, more });
    }

    fn older_request_started(&mut self, chat: ChatId, request_id: u64, protocol_id: String) {
        let Some(request) = self.pending_older.get_mut(&chat) else {
            return;
        };
        if request.request_id != request_id {
            return;
        }
        request.protocol_id = Some(protocol_id.clone());
        let early = request.early_responses.remove(&protocol_id);
        if let Some(early) = early {
            self.record_older_response(chat, early.received_count, early.more_on_phone);
        }
    }

    /// Times out unanswered phone-history requests.
    fn expire_older_requests(&mut self) {
        let expired: Vec<ChatId> = self
            .pending_older
            .iter()
            .filter(|(_, request)| request.asked.elapsed() > PHONE_PATIENCE)
            .map(|(chat, _)| chat.clone())
            .collect();
        for chat in expired {
            self.pending_older.remove(&chat);
            self.emit(Event::OlderFetched {
                chat: chat.clone(),
                more: true,
            });
            // Report the timeout once per chat; later retries back off silently.
            if self.older_warned.insert(chat) {
                self.emit(Event::Error(
                    "Your phone did not send older messages. Check that it is online".to_owned(),
                ));
            }
        }
    }

    fn fetch_older(&mut self, chat: ChatId) {
        if self.archive_cleanup_failed {
            return;
        }
        if self.pending_older.contains_key(&chat) {
            return;
        }
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            // Offline requests retry after reconnection; the banner shows state.
            self.emit(Event::OlderFetched { chat, more: true });
            return;
        };
        // Chats without messages request history from the current time.
        let (id, from_me, timestamp) = match self.archive.oldest(&chat) {
            Ok(Some(oldest)) => (oldest.id, oldest.from_me, oldest.timestamp),
            _ => (String::new(), false, crate::util::now()),
        };
        let asked = Instant::now();
        self.next_older_request_id = self.next_older_request_id.wrapping_add(1);
        let request_id = self.next_older_request_id;
        self.pending_older.insert(
            chat.clone(),
            PendingOlder {
                request_id,
                asked,
                before: (timestamp, id.clone()),
                protocol_id: None,
                received_count: 0,
                more_on_phone: None,
                early_responses: HashMap::new(),
            },
        );
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            // The pinned library documents the returned stanza ID as the
            // correlation value carried by peerDataRequestSessionId.
            match client
                // Despite its `Ms` name, the protocol field takes Unix seconds.
                // https://github.com/tulir/whatsmeow/commit/54650307d891f89ab346a57953d316106caee371
                .fetch_message_history(&jid, &id, from_me, timestamp, PHONE_BATCH)
                .await
            {
                Ok(protocol_id) => {
                    let _ = commands.send(Command::OlderStarted {
                        session_generation,
                        request_id,
                        chat,
                        protocol_id,
                    });
                }
                Err(error) => {
                    log::warn!("could not request older messages");
                    let _ = commands.send(Command::OlderFailed {
                        session_generation,
                        request_id,
                        chat: chat.clone(),
                        error: format!("Could not request older messages from your phone: {error}"),
                    });
                }
            }
        });
    }

    // --- commands --------------------------------------------------------

    /// Save the same audience the protocol library uses to encrypt the send.
    fn save_group_recipients(&self, chat: &str, id: &str, recipients: &[String]) -> bool {
        let recipients: Vec<_> = recipients
            .iter()
            .map(|id| self.canonical_str(id))
            .filter(|id| !self.is_me(id))
            .collect();
        match self
            .archive
            .snapshot_group_recipients(chat, id, &recipients)
        {
            Ok(()) => true,
            Err(error) => {
                log::warn!("could not save the group message audience: {error}");
                false
            }
        }
    }

    fn send_text(
        &mut self,
        chat: ChatId,
        text: String,
        quoting: Option<String>,
        mentions: Vec<String>,
    ) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            self.emit(Event::Sent {
                chat,
                success: false,
            });
            return;
        };
        let (context, quoted_row) = self.quote_context(&chat, quoting.as_deref());
        let mut message = outgoing_text(text.clone(), context, &mentions);
        let expiration = self.apply_ephemeral(&chat, &mut message);
        let mentions = self.mentions_of(&mentions);
        let id = client.generate_message_id();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let row = Message {
            id: id.clone(),
            chat: chat.clone(),
            sender: self.me(),
            sender_name: None,
            from_me: true,
            timestamp: crate::util::now(),
            content: Content::text(text),
            status: Delivery::Pending,
            delivered_at: None,
            read_at: None,
            quoted: quoted_row,
            reactions: Vec::new(),
            edited: false,
            mentions,
            forwarded: false,
            thumbnail: None,
        };
        self.store_message(row, Some(message.encode_to_vec()), None);
        tokio::spawn(send_outgoing(
            OutgoingSession {
                client,
                commands: self.commands.clone(),
                generation: session_generation,
                generation_shared: session_generation_shared,
            },
            chat,
            jid,
            id,
            message,
            expiration,
        ));
    }

    /// Answers a buttons or list message with one of its choices. The wire
    /// message is a `ButtonsResponseMessage` or `ListResponseMessage`; the
    /// local row and its archived body are a plain reply quoting the
    /// original, which is what the user sees.
    fn answer_choice(&mut self, chat: ChatId, message_id: String, choice: String) {
        // Every early exit rebuilds the row, so a control the interface
        // left enabled matches the stored state again.
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            self.emit_message(&chat, &message_id);
            return;
        };
        let original = self.archive.message(&chat, &message_id).ok().flatten();
        let picked = original.as_ref().and_then(|original| {
            let content = &original.content;
            if original.from_me
                || content.answer().is_some()
                || self
                    .answer_sends
                    .values()
                    .any(|(pending_chat, pending_id)| {
                        pending_chat == &chat && pending_id == &message_id
                    })
            {
                return None;
            }
            let (label, description) = content.choice(&choice)?;
            Some((label.to_owned(), description.map(str::to_owned)))
        });
        let (Some(original), Some((label, description))) = (original, picked) else {
            // A second click on an answered message is ignored; anything else
            // (revoked, edited away, from me) is worth telling the user.
            let already_answered = self
                .archive
                .message(&chat, &message_id)
                .ok()
                .flatten()
                .is_some_and(|row| row.content.answer().is_some());
            let pending = self
                .answer_sends
                .values()
                .any(|(pending_chat, pending_id)| {
                    pending_chat == &chat && pending_id == &message_id
                });
            if !already_answered && !pending {
                self.emit(Event::Error("This reply is no longer available".to_owned()));
            }
            self.emit_message(&chat, &message_id);
            return;
        };
        // Interrupted sends remain visible as failed until retry. Remove only
        // attempts that quote this question and carry this exact choice.
        if let Ok(failed) = self.archive.failed_quoted_messages(&chat, &message_id) {
            for (id, raw) in failed {
                let sent_choice = wa::Message::decode_from_slice(&raw)
                    .ok()
                    .and_then(|raw| response_choice_id(raw.get_base_message()));
                if sent_choice.as_deref() == Some(&choice)
                    && let Ok(true) = self.archive.delete_message(&chat, &id)
                {
                    self.emit(Event::MessageDeleted {
                        chat: chat.clone(),
                        id,
                    });
                }
            }
        }
        let (context, quoted_row) = self.quote_context(&chat, Some(&message_id));
        let (wire_label, wire_description) = self
            .archive
            .raw(&chat, &message_id)
            .ok()
            .flatten()
            .and_then(|raw| wa::Message::decode_from_slice(&raw).ok())
            .and_then(|raw| full_choice(raw.get_base_message(), &choice))
            .unwrap_or((label.clone(), description));
        let mut message = match &original.content {
            Content::List { .. } => list_response(&choice, &wire_label, wire_description, context),
            _ => buttons_response(&choice, &wire_label, context),
        };
        let expiration = self.apply_ephemeral(&chat, &mut message);
        let id = client.generate_message_id();
        let row = Message {
            id: id.clone(),
            chat: chat.clone(),
            sender: self.me(),
            sender_name: None,
            from_me: true,
            timestamp: crate::util::now(),
            content: Content::text(label.clone()),
            status: Delivery::Pending,
            delivered_at: None,
            read_at: None,
            quoted: quoted_row,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        };
        self.store_message(row, Some(message.encode_to_vec()), None);
        self.answer_sends
            .insert(id.clone(), (chat.clone(), message_id.clone()));
        tokio::spawn(send_outgoing(
            OutgoingSession {
                client,
                commands: self.commands.clone(),
                generation: self.session_generation,
                generation_shared: self.session_generation_shared.clone(),
            },
            chat,
            jid,
            id,
            message,
            expiration,
        ));
    }

    /// Clears an answer recorded by an older version after an interrupted send.
    fn reopen_answer(&mut self, chat: &str, message_id: &str) {
        let Ok(Some(original)) = self.archive.message(chat, message_id) else {
            return;
        };
        let Some(reopened) = original
            .content
            .answer()
            .and_then(|_| original.content.with_answer(None))
        else {
            return;
        };
        if let Ok(true) = self
            .archive
            .set_content(chat, message_id, &reopened, original.edited)
        {
            self.emit_message(chat, message_id);
        }
    }

    fn recover_interrupted_answers(&mut self) {
        let Ok(rows) = self.archive.pending_quoted_messages() else {
            return;
        };
        for (chat, id, quoted, raw) in rows {
            let Some(parent) = self.archive.message(&chat, &quoted).ok().flatten() else {
                continue;
            };
            let response = wa::Message::decode_from_slice(&raw).ok();
            let choice = response
                .as_ref()
                .and_then(|message| response_choice_id(message.get_base_message()));
            if choice.is_none()
                || !matches!(
                    parent.content,
                    Content::Buttons { .. } | Content::List { .. }
                )
            {
                continue;
            }
            self.reopen_answer(&chat, &quoted);
            let _ = self
                .archive
                .set_status(&chat, &id, Delivery::Failed, crate::util::now());
        }
    }

    fn confirm_answer(&self, chat: &str, original: &str, raw: &[u8]) {
        let choice = wa::Message::decode_from_slice(raw)
            .ok()
            .and_then(|message| response_choice_id(message.get_base_message()));
        let parent = self.archive.message(chat, original).ok().flatten();
        if let (Some(choice), Some(parent)) = (choice, parent)
            && parent.content.answer().is_none()
            && parent.content.choice(&choice).is_some()
            && let Some(marked) = parent.content.with_answer(Some(choice))
            && let Ok(true) = self
                .archive
                .set_content(chat, original, &marked, parent.edited)
        {
            self.emit_message(chat, original);
        }
    }

    fn reconcile_confirmed_answers(&self) {
        if let Ok(rows) = self.archive.confirmed_quoted_messages() {
            for (chat, original, raw) in rows {
                self.confirm_answer(&chat, &original, &raw);
            }
        }
    }

    fn confirm_answers_for(&self, chat: &str, original: &str) {
        if let Ok(rows) = self.archive.confirmed_answers_for(chat, original) {
            for raw in rows {
                self.confirm_answer(chat, original, &raw);
            }
        }
    }

    /// Builds protocol context and archive metadata for an available quoted message.
    fn quote_context(
        &self,
        chat: &ChatId,
        quoting: Option<&str>,
    ) -> (Option<wa::ContextInfo>, Option<Quoted>) {
        let Some((context, row)) = quoting.and_then(|id| {
            let raw = self.archive.raw(chat, id).ok().flatten()?;
            let quoted = wa::Message::decode_from_slice(&raw).ok()?;
            let row = self.archive.message(chat, id).ok().flatten()?;
            let jid = Self::jid_of(chat)?;
            let sender = Self::jid_of(&row.sender).unwrap_or_else(|| jid.clone());
            let context = whatsapp_rust::wacore::proto_helpers::build_quote_context_with_info(
                row.id.clone(),
                &sender,
                &jid,
                &jid,
                &quoted,
            );
            Some((context, row))
        }) else {
            return (None, None);
        };
        let quoted = Quoted {
            mentions: row.mentions.clone(),
            id: row.id,
            sender_name: if row.from_me {
                Some("You".to_owned())
            } else {
                row.sender_name
                    .clone()
                    .or_else(|| self.name_for(&row.sender))
            },
            sender: row.sender,
            summary: row.content.summary(),
        };
        (Some(context), Some(quoted))
    }

    fn forward_message(&mut self, from_chat: ChatId, message_id: String, to_chat: ChatId) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&to_chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        let Ok(Some(source)) = self.archive.message(&from_chat, &message_id) else {
            self.emit(Event::Error(
                "This message is not stored on this computer".to_owned(),
            ));
            return;
        };
        if matches!(
            source.content,
            Content::Revoked
                | Content::Unsupported { .. }
                | Content::Poll { .. }
                | Content::Buttons { .. }
                | Content::List { .. }
                | Content::Template { .. }
        ) {
            self.emit(Event::Error("This message cannot be forwarded".to_owned()));
            return;
        }
        let Ok(Some(raw)) = self.archive.raw(&from_chat, &message_id) else {
            self.emit(Event::Error(
                "The original message data is not available to forward".to_owned(),
            ));
            return;
        };
        let Ok(original) = wa::Message::decode_from_slice(&raw) else {
            self.emit(Event::Error(
                "The original message data could not be read".to_owned(),
            ));
            return;
        };
        // whatsapp-rust owns the forwarding rules: unwrap transient wrappers,
        // strip quote chains and secrets, and retain reusable media metadata.
        let (message, expiration) =
            outgoing_forward(&original, self.ephemeral_expiration(&to_chat));
        let id = client.generate_message_id();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let mentions = self.mentions_of(&mentioned_of(&message));
        let thumbnail = thumbnail_of(&message).or_else(|| source.thumbnail.clone());
        let row = forwarded_row(
            source,
            to_chat.clone(),
            self.me(),
            id.clone(),
            crate::util::now(),
            mentions,
            thumbnail,
        );
        self.store_message(row, Some(message.encode_to_vec()), None);
        self.forward_tails.retain(|_, send| !send.is_finished());
        let previous = self.forward_tails.remove(&to_chat);
        let destination = to_chat.clone();
        let send = send_outgoing(
            OutgoingSession {
                client,
                commands: self.commands.clone(),
                generation: session_generation,
                generation_shared: session_generation_shared,
            },
            to_chat,
            jid,
            id,
            message,
            expiration,
        );
        let tail = tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            send.await
        });
        self.forward_tails.insert(destination, tail);
    }

    fn mark_read(&mut self, chat: ChatId, receipts: bool) {
        let Ok(Some(row)) = self.archive.chat(&chat) else {
            return;
        };
        // Collect before advancing the archive's read position.
        let ids = if receipts {
            self.archive
                .unread_incoming(&chat, row.unread)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let _ = self.archive.mark_read(&chat);
        self.emit_chat(&chat);
        if row.unread == 0 {
            return;
        }
        let _ = self.archive.queue_read_sync(&chat);
        self.pump_read_sync();
        self.send_read_receipts(chat, ids);
    }

    fn pump_read_sync(&mut self) {
        if !matches!(self.status, LinkStatus::Connected) || !self.read_sync.ready(Instant::now()) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        for (chat, through) in self.archive.pending_reads().unwrap_or_default() {
            let Some(jid) = Self::jid_of(&chat) else {
                continue;
            };
            let Some(attempt_id) = self.read_sync.start(&chat, through, Instant::now()) else {
                break;
            };
            let client = client.clone();
            let commands = self.commands.clone();
            let session_generation = self.session_generation;
            tokio::spawn(async move {
                // This update is private to our devices, even with blue ticks
                // disabled. Keep the original position when retrying offline
                // reads, not the latest message received since the local read.
                let range = whatsapp_rust::message_range(through, None, Vec::new());
                // A timed-out patch may still have reached the phone. Keep the durable
                // position queued and retry the same-or-newer read range at least once.
                let outcome = bounded_read_sync(
                    READ_SYNC_TIMEOUT,
                    client
                        .chat_actions()
                        .mark_chat_as_read(&jid, true, Some(range)),
                )
                .await;
                match outcome {
                    ReadSyncOutcome::Success => {}
                    ReadSyncOutcome::Failed => {
                        log::debug!("chat read state not synced");
                    }
                    ReadSyncOutcome::TimedOut => {
                        log::warn!("chat read-state sync timed out; keeping position queued");
                    }
                }
                let _ = commands.send(Command::ReadSyncFinished {
                    session_generation,
                    attempt_id,
                    chat,
                    through,
                    success: outcome == ReadSyncOutcome::Success,
                });
            });
            // Every read-state write uses regular_low. A queue of spawned tasks
            // would each retry the same broken collection before we can back off.
            break;
        }
    }

    fn send_read_receipts(&self, chat: ChatId, ids: Vec<(String, String)>) {
        if ids.is_empty() {
            return;
        }
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            return;
        };
        let is_group = jid.is_group();
        let mut by_sender: HashMap<Option<String>, Vec<String>> = HashMap::new();
        for (id, sender) in ids {
            by_sender
                .entry(is_group.then_some(sender))
                .or_default()
                .push(id);
        }
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            if !receipts_allowed(&client, &jid, &commands, session_generation).await {
                return;
            }
            for (sender, ids) in by_sender {
                let sender = sender.and_then(|sender| sender.parse::<Jid>().ok());
                let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
                if let Err(error) = client.mark_as_read(&jid, sender.as_ref(), &ids).await {
                    log::debug!("read receipt not sent: {error}");
                }
            }
        });
    }

    /// Refreshes stored quote ids and names with current mappings.
    fn polish(&self, message: &mut Message) {
        self.polish_poll(message);
        // Stored names are the sender's push name; a saved contact wins.
        if !message.from_me
            && let Some(name) = self.saved_name(&message.sender)
        {
            message.sender_name = Some(name);
        }
        if let Some(quoted) = message.quoted.as_mut() {
            let sender = self.canonical_str(&quoted.sender);
            if sender != quoted.sender || quoted.sender_name.is_none() {
                quoted.sender_name = self.name_for(&sender);
                quoted.sender = sender;
            } else if quoted.sender_name.as_deref() != Some("You")
                && let Some(name) = self.saved_name(&quoted.sender)
            {
                quoted.sender_name = Some(name);
            }
            quoted.summary = self.pn_tokens(&quoted.summary);
            for mention in &mut quoted.mentions {
                mention.id = self.canonical_str(&mention.id);
            }
            if quoted.mentions.is_empty() {
                quoted.mentions = self.mention_tokens(&quoted.summary);
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
                        self.lid_to_pn.contains_key(&mention.user)
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
        let known = self.lid_to_pn.get(&mention.user);
        [
            Some(mention.id.clone()),
            Some(format!("{}@lid", mention.user)),
            known.map(|pn| format!("{pn}@s.whatsapp.net")),
        ]
        .into_iter()
        .flatten()
        .find_map(|id| {
            self.contacts
                .get(&id)?
                .push_name
                .clone()
                .filter(|name| !name.is_empty())
        })
    }

    fn load_chat(&mut self, chat: ChatId, before: Option<super::PageKey>) {
        self.send_page(&chat, before.clone());
        if before.is_none() && ChatKind::from_id(&chat) == ChatKind::Group {
            self.request_group_info(&chat, false);
        }
        if before.is_none()
            && ChatKind::from_id(&chat) == ChatKind::Direct
            && chat != self.me()
            && self.presence_subscribed.insert(chat.clone())
            && let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat))
        {
            tokio::spawn(async move {
                if let Err(error) = client.presence().subscribe(jid).await {
                    log::debug!("presence not subscribed: {error}");
                }
            });
        }
    }

    /// Sends one page of the archive, the newest one or the one before
    /// `before`.
    fn send_page(&mut self, chat: &ChatId, before: Option<super::PageKey>) {
        if self.withhold(WithheldPage::Page(chat.clone(), before.clone())) {
            return;
        }
        if self.archive_cleanup_failed {
            self.emit(Event::Error(
                "Local conversation cleanup must finish before chats can be opened.".to_owned(),
            ));
            return;
        }
        match self.archive.messages(
            chat,
            before.as_ref().map(|(time, id)| (*time, id.as_str())),
            PAGE + 1,
        ) {
            Ok(mut messages) => {
                let complete = messages.len() <= PAGE;
                if !complete {
                    messages.remove(0);
                }
                for message in &mut messages {
                    self.polish(message);
                }
                self.emit(Event::Messages {
                    chat: chat.clone(),
                    messages,
                    older: before.is_some(),
                    complete,
                });
            }
            Err(error) => self.emit(Event::Error(format!("Could not read the chat: {error}"))),
        }
    }

    /// Keeps a transcript read for later while private content is withheld.
    /// Its answer would be dropped, and the interface, having asked once, would
    /// wait for it forever: a chat opened then stayed empty until a new message
    /// arrived.
    fn withhold(&mut self, page: WithheldPage) -> bool {
        if self.privacy_ready {
            return false;
        }
        if !self.withheld_pages.contains(&page) {
            self.withheld_pages.push(page);
        }
        true
    }

    async fn retry_deferred_downloads(&mut self, now: Instant) {
        // Take one batch so a still-failing read cannot retry in a tight loop.
        let pending = std::mem::take(&mut self.deferred_downloads);
        for mut download in pending {
            if now < download.retry_at {
                self.deferred_downloads.push(download);
            } else if let Some(completion) = download.completion.take() {
                self.handle_command(completion).await;
            }
        }
    }

    fn download(&mut self, chat: ChatId, id: String) {
        if self.archive_cleanup_failed {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.emit(Event::Media {
                chat,
                message: id,
                result: Err("Not connected to WhatsApp".to_owned()),
            });
            return;
        };
        let raw = self.archive.raw(&chat, &id).ok().flatten();
        let Some(raw) = raw else {
            self.emit(Event::Media {
                chat,
                message: id,
                result: Err("Attachment download keys are missing".to_owned()),
            });
            return;
        };
        let raw_fingerprint = message_raw_fingerprint(&raw);
        let Some(message) = wa::Message::decode_from_slice(&raw).ok() else {
            self.emit(Event::Media {
                chat,
                message: id,
                result: Err("Attachment download keys are missing".to_owned()),
            });
            return;
        };
        let base = message.get_base_message().clone();
        let (downloadable, mime, file_name): (Box<dyn Downloadable>, String, Option<String>) =
            if let Some(image) = base.image_message.as_option() {
                (
                    Box::new(image.clone()),
                    image.mimetype.clone().unwrap_or_default(),
                    None,
                )
            } else if let Some(video) = base
                .video_message
                .as_option()
                .or(base.ptv_message.as_option())
            {
                (
                    Box::new(video.clone()),
                    video.mimetype.clone().unwrap_or_default(),
                    None,
                )
            } else if let Some(audio) = base.audio_message.as_option() {
                (
                    Box::new(audio.clone()),
                    audio.mimetype.clone().unwrap_or_default(),
                    None,
                )
            } else if let Some(document) = base.document_message.as_option() {
                (
                    Box::new(document.clone()),
                    document.mimetype.clone().unwrap_or_default(),
                    document.file_name.clone(),
                )
            } else if let Some(sticker) = base.sticker_message.as_option() {
                (
                    Box::new(sticker.clone()),
                    sticker.mimetype.clone().unwrap_or_default(),
                    None,
                )
            } else {
                self.emit(Event::Media {
                    chat,
                    message: id,
                    result: Err("This message has no downloadable file".to_owned()),
                });
                return;
            };
        // Keep metadata needed for one media re-upload request and retry.
        let media_key = base
            .image_message
            .as_option()
            .and_then(|media| media.media_key.clone())
            .or_else(|| {
                base.video_message
                    .as_option()
                    .or(base.ptv_message.as_option())
                    .and_then(|media| media.media_key.clone())
            })
            .or_else(|| {
                base.audio_message
                    .as_option()
                    .and_then(|media| media.media_key.clone())
            })
            .or_else(|| {
                base.document_message
                    .as_option()
                    .and_then(|media| media.media_key.clone())
            })
            .or_else(|| {
                base.sticker_message
                    .as_option()
                    .and_then(|media| media.media_key.clone())
            })
            .unwrap_or_default();
        let jid = Self::jid_of(&chat);
        let row = self.archive.message(&chat, &id).ok().flatten();
        let is_from_me = row.as_ref().is_some_and(|row| row.from_me);
        let participant = match (&jid, &row) {
            (Some(jid), Some(row)) if jid.is_group() => Self::jid_of(&row.sender),
            _ => None,
        };
        let mut fresh_base = base;
        let mut refreshed = move |direct: String| -> Option<Box<dyn Downloadable>> {
            if let Some(media) = fresh_base.image_message.as_option_mut() {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            if let Some(media) = fresh_base
                .video_message
                .as_option_mut()
                .or(fresh_base.ptv_message.as_option_mut())
            {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            if let Some(media) = fresh_base.audio_message.as_option_mut() {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            if let Some(media) = fresh_base.document_message.as_option_mut() {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            if let Some(media) = fresh_base.sticker_message.as_option_mut() {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            None
        };
        let dir = self.dirs.media_cache_dir();
        let destination = media_path(&dir, &chat, &id, &mime, file_name.as_deref());
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let session_cache_lock = self.session_cache_lock.clone();
        tokio::spawn(async move {
            let keep = |bytes: Vec<u8>| {
                let dir = dir.clone();
                let path = download_staging_path(&dir);
                let session_generation_shared = session_generation_shared.clone();
                let session_cache_lock = session_cache_lock.clone();
                async move {
                    write_session_cache_file(
                        &dir,
                        &path,
                        &bytes,
                        session_generation,
                        &session_generation_shared,
                        None,
                        &session_cache_lock,
                    )
                    .await
                }
            };
            let result = match client.download(&*downloadable).await {
                Ok(bytes) => keep(bytes).await,
                Err(error) => {
                    let text = error.to_string();
                    let expired = ["403", "404", "410"].iter().any(|code| text.contains(code));
                    match (&jid, expired && !media_key.is_empty()) {
                        (Some(jid), true) => {
                            // Ask the phone to re-upload expired media, then retry once.
                            let request = MediaReuploadRequest {
                                msg_id: &id,
                                chat_jid: jid,
                                media_key: &media_key,
                                is_from_me,
                                participant: participant.as_ref(),
                            };
                            match client.media_reupload().request(&request).await {
                                Ok(MediaRetryResult::Success { direct_path }) => {
                                    match refreshed(direct_path) {
                                        Some(again) => match client.download(&*again).await {
                                            Ok(bytes) => keep(bytes).await,
                                            Err(error) => Err(error.to_string()),
                                        },
                                        None => Err(text),
                                    }
                                }
                                Ok(_) => {
                                    Err("No longer available on WhatsApp's servers".to_owned())
                                }
                                Err(_error) => {
                                    log::info!("media re-upload was not granted");
                                    Err("No longer available on WhatsApp's servers".to_owned())
                                }
                            }
                        }
                        _ => Err(text),
                    }
                }
            };
            let _ = commands.send(Command::Downloaded {
                chat,
                id,
                session_generation,
                raw_fingerprint,
                destination,
                result,
            });
        });
    }

    /// Downloads missing recent and archived stickers for the picker.
    fn fetch_missing_stickers(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let phone = match self.archive.phone_stickers() {
            Ok(list) => list,
            Err(error) => {
                log::warn!("could not list the phone's stickers: {error}");
                Vec::new()
            }
        };
        let dir = self.dirs.sticker_cache_dir();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let session_cache_lock = self.session_cache_lock.clone();
        for sticker in phone.into_iter().filter(|sticker| sticker.path.is_none()) {
            if !self.sticker_fetches.insert(sticker.hash.clone()) {
                continue;
            }
            let Ok(meta) = wa::StickerMetadata::decode_from_slice(&sticker.raw) else {
                self.sticker_fetches.remove(&sticker.hash);
                continue;
            };
            let client = client.clone();
            let commands = self.commands.clone();
            let dir = dir.clone();
            let hash = sticker.hash;
            let session_generation_shared = session_generation_shared.clone();
            let session_cache_lock = session_cache_lock.clone();
            tokio::spawn(async move {
                let result = async {
                    if session_generation_shared.load(Ordering::Acquire) != session_generation {
                        return Err(
                            "The linked account changed during the sticker download".to_owned()
                        );
                    }
                    let bytes = client
                        .download(&PhoneSticker(meta))
                        .await
                        .map_err(|error| error.to_string())?;
                    let path = dir.join(format!("{hash}.webp"));
                    write_session_cache_file(
                        &dir,
                        &path,
                        &bytes,
                        session_generation,
                        &session_generation_shared,
                        None,
                        &session_cache_lock,
                    )
                    .await
                }
                .await;
                let _ = commands.send(Command::StickerFetched {
                    hash,
                    session_generation,
                    result,
                });
            });
        }
        match self.archive.stickers_without_file(STICKER_FETCH_LIMIT) {
            Ok(list) => {
                for (chat, id) in list {
                    let Some(raw) = self.archive.raw(&chat, &id).ok().flatten() else {
                        continue;
                    };
                    let fingerprint = message_raw_fingerprint(&raw);
                    if self
                        .sticker_downloads
                        .insert((chat.clone(), id.clone(), fingerprint))
                    {
                        self.download(chat, id);
                    }
                }
            }
            Err(error) => log::warn!("could not list unfetched stickers: {error}"),
        }
    }

    /// Returns distinct downloaded stickers by most recent use.
    fn emit_stickers(&mut self) {
        let mut seen = HashSet::new();
        let mut list: Vec<(i64, PathBuf)> = Vec::new();
        if let Ok(phone) = self.archive.phone_stickers() {
            for sticker in phone {
                if let Some(path) = sticker.path
                    && path.exists()
                    && seen.insert(sticker.hash)
                {
                    list.push((sticker.last_used, path));
                }
            }
        }
        match self.archive.recent_stickers(80) {
            Ok(rows) => {
                for sticker in rows {
                    let hash = sticker
                        .raw
                        .as_deref()
                        .and_then(|raw| wa::Message::decode_from_slice(raw).ok())
                        .and_then(|message| {
                            let base = message.get_base_message();
                            let sticker = base.sticker_message.as_option()?;
                            sticker_hash(
                                sticker.file_sha256.as_deref(),
                                sticker.file_enc_sha256.as_deref(),
                            )
                        })
                        .unwrap_or_else(|| sticker.path.display().to_string());
                    if seen.insert(hash) {
                        list.push((sticker.last_used, sticker.path));
                    }
                }
            }
            Err(error) => log::warn!("could not list stickers: {error}"),
        }
        list.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        self.emit(Event::Stickers {
            saved: self.saved_stickers(),
            packs: self.sticker_packs(),
            recent: list.into_iter().map(|(_, path)| path).collect(),
        });
    }

    /// Root directory for imported sticker packs.
    fn packs_dir(&self) -> PathBuf {
        self.dirs.saved_sticker_dir().join("packs")
    }

    /// Returns imported packs, newest first, with files in name order.
    fn sticker_packs(&self) -> Vec<crate::model::StickerPack> {
        let Ok(entries) = std::fs::read_dir(self.packs_dir()) else {
            return Vec::new();
        };
        let mut packs: Vec<(std::time::SystemTime, crate::model::StickerPack)> = entries
            .flatten()
            .filter_map(|entry| {
                let dir = entry.path();
                if !dir.is_dir() {
                    return None;
                }
                let mut stickers: Vec<PathBuf> = std::fs::read_dir(&dir)
                    .ok()?
                    .flatten()
                    .map(|file| file.path())
                    .filter(|path| {
                        path.extension()
                            .is_some_and(|extension| extension == "webp")
                    })
                    .collect();
                if stickers.is_empty() {
                    return None;
                }
                stickers.sort();
                let when = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                Some((
                    when,
                    crate::model::StickerPack {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        dir,
                        stickers,
                        local: true,
                    },
                ))
            })
            .collect();
        packs.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        packs.into_iter().map(|(_, pack)| pack).collect()
    }

    /// Returns saved sticker files, newest first.
    fn saved_stickers(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.dirs.saved_sticker_dir()) else {
            return Vec::new();
        };
        let mut saved: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                path.extension()
                    .is_some_and(|extension| extension == "webp")
                    .then(|| {
                        let when = entry
                            .metadata()
                            .and_then(|metadata| metadata.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        (when, path)
                    })
            })
            .collect();
        saved.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        saved.into_iter().map(|(_, path)| path).collect()
    }

    fn avatar_file(&self, id: &str, full: bool) -> PathBuf {
        self.dirs.avatar_file(id, full)
    }

    fn fetch_avatar(&mut self, id: String, full: bool) {
        let path = self.avatar_file(&id, full);
        if let Ok(metadata) = std::fs::metadata(&path)
            && metadata
                .modified()
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age < AVATAR_FRESH)
        {
            let path = (metadata.len() > 0).then_some(path);
            self.emit(Event::Avatar { id, full, path });
            return;
        }
        // Try both of our ids for our profile picture.
        let candidates: Vec<Jid> = if self.is_me(&id) || id == self.me() {
            [self.me_pn.clone(), self.me_lid.clone()]
                .into_iter()
                .flatten()
                .filter_map(|id| Self::jid_of(&id))
                .collect()
        } else {
            Self::jid_of(&id).into_iter().collect()
        };
        if candidates.is_empty() {
            self.emit(Event::Avatar {
                id,
                full,
                path: None,
            });
            return;
        }
        let connected = self
            .client
            .as_ref()
            .is_some_and(|client| client.is_connected());
        let Some(client) = self.client.clone().filter(|_| connected) else {
            // Defer profile-picture lookup until connected.
            self.pending_avatars.entry((id, full)).or_insert(0);
            return;
        };
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let avatar_generation_shared = self.avatar_generation(&id, full);
        let avatar_generation = avatar_generation_shared.load(Ordering::Acquire);
        let session_cache_lock = self.session_cache_lock.clone();
        tokio::spawn(async move {
            let fetched = async {
                let mut picture = None;
                let mut failed = false;
                'lookup: for jid in &candidates {
                    for preview in [!full, false] {
                        match client.contacts().get_profile_picture(jid, preview).await {
                            Ok(Some(found)) => {
                                picture = Some(found);
                                break 'lookup;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                log::debug!("picture lookup failed: {error}");
                                failed = true;
                            }
                        }
                    }
                }
                let Some(picture) = picture else {
                    return if failed {
                        Err("lookup failed".to_owned())
                    } else {
                        Ok(None)
                    };
                };
                let url = picture.url;
                let bytes = tokio::task::spawn_blocking(move || {
                    ureq::get(&url)
                        .call()
                        .and_then(|mut response| response.body_mut().read_to_vec())
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| error.to_string())??;
                if session_generation_shared.load(Ordering::Acquire) != session_generation {
                    return Err(
                        "The linked account changed during profile-picture lookup".to_owned()
                    );
                }
                let parent = path
                    .parent()
                    .ok_or_else(|| "Profile-picture cache path is invalid".to_owned())?;
                let path = write_session_cache_file(
                    parent,
                    &path,
                    &bytes,
                    session_generation,
                    &session_generation_shared,
                    Some((avatar_generation, &avatar_generation_shared)),
                    &session_cache_lock,
                )
                .await?;
                Ok::<Option<PathBuf>, String>(Some(path))
            }
            .await;
            match fetched {
                Ok(path) => {
                    let _ = commands.send(Command::AvatarFetched {
                        id,
                        full,
                        session_generation,
                        avatar_generation,
                        path,
                    });
                }
                Err(_error) => {
                    log::debug!("profile picture lookup failed");
                    let _ = commands.send(Command::AvatarFailed {
                        id,
                        full,
                        session_generation,
                        avatar_generation,
                    });
                }
            }
        });
    }

    fn avatar_generation(&mut self, id: &str, full: bool) -> Arc<AtomicU64> {
        self.avatar_generations
            .entry((id.to_owned(), full))
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone()
    }

    fn invalidate_avatar_generations(&mut self, id: &str) {
        for full in [false, true] {
            self.avatar_generation(id, full)
                .fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Retries deferred or failed profile-picture requests.
    fn retry_avatars(&mut self) {
        if !self
            .client
            .as_ref()
            .is_some_and(|client| client.is_connected())
        {
            return;
        }
        let due: Vec<(String, bool)> = self.pending_avatars.keys().cloned().collect();
        for (id, full) in due {
            let attempts = self
                .pending_avatars
                .remove(&(id.clone(), full))
                .unwrap_or(0);
            if attempts >= 3 {
                self.emit(Event::Avatar {
                    id,
                    full,
                    path: None,
                });
                continue;
            }
            self.fetch_avatar(id, full);
        }
    }

    fn load_until(&mut self, chat: ChatId, id: String, before: super::PageKey) {
        if self.withhold(WithheldPage::Until(
            chat.clone(),
            id.clone(),
            before.clone(),
        )) {
            return;
        }
        if self.archive_cleanup_failed {
            return;
        }
        let Ok(Some(target)) = self.archive.message(&chat, &id) else {
            self.emit(Event::Messages {
                chat: chat.clone(),
                messages: Vec::new(),
                older: true,
                complete: false,
            });
            self.emit(Event::Error(
                "This message is not stored on this computer".to_owned(),
            ));
            return;
        };
        match self
            .archive
            .messages_range(&chat, target.timestamp, (before.0, &before.1), 2000)
        {
            Ok(mut messages) => {
                for message in &mut messages {
                    self.polish(message);
                }
                self.emit(Event::Messages {
                    chat,
                    messages,
                    older: true,
                    complete: false,
                });
            }
            Err(error) => self.emit(Event::Error(format!("Could not read the chat: {error}"))),
        }
    }

    fn edit_text(&mut self, chat: ChatId, id: String, text: String, mentions: Vec<String>) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            self.emit(Event::Edited {
                chat,
                id,
                success: false,
            });
            return;
        };
        let content = Content::text(text.clone());
        let mention_rows = self.mentions_of(&mentions);
        let mut message = outgoing_text(text, None, &mentions);
        self.apply_ephemeral(&chat, &mut message);
        let commands = self.commands.clone();
        let events = self.events.clone();
        let waker = self.waker.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        tokio::spawn(async move {
            let result = client.edit_message(jid, id.clone(), message).await;
            let success = result.is_ok();
            let _ = commands.send(Command::Edited {
                chat: chat.clone(),
                id,
                session_generation,
                success,
                content,
                mentions: mention_rows,
            });
            if result.is_err()
                && session_generation_shared.load(Ordering::Acquire) == session_generation
            {
                let _ = events.send(Event::Error("Could not edit the message".to_owned()));
                waker.wake();
            }
        });
    }

    fn confirm_revoke(&mut self, chat: &str, id: &str) {
        // An authoritative phone update invalidates any later rollback.
        self.pending_revokes
            .remove(&(chat.to_owned(), id.to_owned()));
        if let Ok(true) = self.archive.set_content(chat, id, &Content::Revoked, false) {
            self.emit_message(chat, id);
            self.emit_chat(chat);
        }
    }

    fn begin_revoke(&mut self, chat: &str, id: &str) -> crate::archive::Result<Option<u64>> {
        let key = (chat.to_owned(), id.to_owned());
        if self.pending_revokes.contains_key(&key) {
            return Ok(None);
        }
        let previous = self
            .archive
            .message(chat, id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        if matches!(previous.content, Content::Revoked) {
            return Ok(None);
        }
        if !self
            .archive
            .set_content(chat, id, &Content::Revoked, false)?
        {
            return Ok(None);
        }
        self.next_revoke_attempt = self.next_revoke_attempt.wrapping_add(1);
        let attempt_id = self.next_revoke_attempt;
        self.pending_revokes.insert(
            key,
            PendingRevoke {
                attempt_id,
                content: previous.content,
                edited: previous.edited,
            },
        );
        self.emit_message(chat, id);
        self.emit_chat(chat);
        Ok(Some(attempt_id))
    }

    fn revoke(&mut self, chat: ChatId, id: String) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        let attempt_id = match self.begin_revoke(&chat, &id) {
            Ok(Some(attempt_id)) => attempt_id,
            Ok(None) => return,
            Err(_) => {
                self.emit(Event::Error(
                    "Could not delete the message for everyone".to_owned(),
                ));
                return;
            }
        };
        let session_generation = self.session_generation;
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let success = client
                .revoke_message(jid, &id, RevokeType::Sender)
                .await
                .is_ok();
            let _ = commands.send(Command::RevokeFinished {
                session_generation,
                attempt_id,
                chat,
                message: id,
                success,
            });
        });
    }

    /// Sends a played receipt for an incoming voice message.
    fn mark_played(&mut self, chat: ChatId, message: String, sender: String) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            return;
        };
        let sender = if jid.is_group() {
            sender.parse::<Jid>().ok()
        } else {
            None
        };
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            if !receipts_allowed(&client, &jid, &commands, session_generation).await {
                return;
            }
            if let Err(error) = client
                .mark_as_played(&jid, sender.as_ref(), &[message.as_str()])
                .await
            {
                log::debug!("played receipt not sent: {error}");
            }
        });
    }

    /// Archives and sends an uploaded attachment message.
    fn outbound(&mut self, chat: ChatId, session_generation: u64, row: Message, raw: Vec<u8>) {
        if session_generation != self.session_generation || self.archive_cleanup_failed {
            return;
        }
        // A sticker's pending row is already shown; fail it rather than
        // leave it pending forever.
        let fail = |worker: &mut Self, error: &str| {
            if let Ok(true) =
                worker
                    .archive
                    .set_status(&chat, &row.id, Delivery::Failed, crate::util::now())
            {
                worker.emit_message(&chat, &row.id);
            }
            worker.emit(Event::Error(error.to_owned()));
        };
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            fail(self, "Not connected to WhatsApp");
            return;
        };
        let Ok(mut message) = wa::Message::decode_from_slice(&raw) else {
            fail(self, "Could not encode the attachment");
            return;
        };
        let expiration = self.apply_ephemeral(&chat, &mut message);
        let raw = message.encode_to_vec();
        let id = row.id.clone();
        self.store_message(row, Some(raw), None);
        let session_generation_shared = self.session_generation_shared.clone();
        tokio::spawn(send_outgoing(
            OutgoingSession {
                client,
                commands: self.commands.clone(),
                generation: session_generation,
                generation_shared: session_generation_shared,
            },
            chat,
            jid,
            id,
            message,
            expiration,
        ));
    }

    fn outbound_batch(
        &mut self,
        chat: ChatId,
        session_generation: u64,
        row: Message,
        raw: Vec<u8>,
        sent: tokio::sync::mpsc::UnboundedSender<bool>,
    ) {
        if session_generation != self.session_generation || self.archive_cleanup_failed {
            let _ = sent.send(false);
            return;
        }
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            let _ = sent.send(false);
            return;
        };
        let Ok(mut message) = wa::Message::decode_from_slice(&raw) else {
            let _ = sent.send(false);
            return;
        };
        let expiration = self.apply_ephemeral(&chat, &mut message);
        let raw = message.encode_to_vec();
        let id = row.id.clone();
        self.store_message(row, Some(raw), None);
        let commands = self.commands.clone();
        let session_generation_shared = self.session_generation_shared.clone();
        tokio::spawn(async move {
            let success = send_outgoing(
                OutgoingSession {
                    client,
                    commands,
                    generation: session_generation,
                    generation_shared: session_generation_shared,
                },
                chat,
                jid,
                id,
                message,
                expiration,
            )
            .await;
            let _ = sent.send(success);
        });
    }

    fn react(&mut self, chat: ChatId, id: String, emoji: String) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        let Ok(Some(target)) = self.archive.message(&chat, &id) else {
            return;
        };
        let me = self.me();
        if let Ok(Some(updated)) = self.archive.set_reaction(&chat, &id, &me, true, &emoji) {
            self.emit(Event::MessageUpdated(Box::new(updated)));
        }
        let key = wa::MessageKey {
            remote_jid: Some(chat.clone()),
            from_me: Some(target.from_me),
            id: Some(id),
            participant: (jid.is_group() && !target.from_me).then(|| target.sender.clone()),
        };
        tokio::spawn(async move {
            if client.send_reaction(jid, key, &emoji).await.is_err() {
                log::warn!("could not send a reaction");
            }
        });
    }
}

// --- free helpers ----------------------------------------------------------

fn outgoing_forward(original: &wa::Message, expiration: Option<u32>) -> (wa::Message, Option<u32>) {
    let mut message = original.get_base_message().prepare_for_forward();
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
            commands
                .send(Command::GroupRecipients {
                    session_generation,
                    chat: chat.clone(),
                    id: id.clone(),
                    recipients,
                    lids,
                    stored,
                })
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
        Command::ContactSent {
            chat,
            id,
            session_generation,
            error,
        }
    } else {
        Command::Sent {
            chat,
            id,
            session_generation,
            error,
        }
    };
    let _ = commands.send(completion);
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
        .filter(|extension| !extension.is_empty() && extension.len() <= 8)
    {
        return extension.to_ascii_lowercase();
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

/// Finds the first web address when preview metadata omits its URL.
fn first_link(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| token.starts_with("http://") || token.starts_with("https://"))
        .map(|token| token.trim_end_matches(['.', ',', ')', ']']).to_owned())
}

/// Extracts the attachment or link-preview thumbnail.
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
    bytes.filter(|bytes| !bytes.is_empty())
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
