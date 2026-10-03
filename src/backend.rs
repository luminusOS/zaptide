//! Channel bridge between the UI and asynchronous runtime.
//!
//! A dedicated tokio runtime owns the WhatsApp connection, archive, and media
//! work. Commands and events cross channels, and events wake the UI.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::model::{Chat, ChatId, Contact, Content, MentionRef, Message, PollDraft, StickerPack};
use crate::paths::AppDirs;

mod read_sync;
mod worker;

/// Maximum backend events applied in one native main-context turn.
pub const EVENT_BATCH_LIMIT: usize = 256;

/// Delivers a backend event notification to the application shell.
pub trait Wake: Send + Sync {
    fn wake(&self);
}

/// Phone-link state.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkStatus {
    Starting,
    /// Waiting for QR scanning or pairing-code acceptance.
    Unlinked {
        qr: Option<String>,
        pair_code: Option<String>,
        pairing_phone: Option<String>,
    },
    Connecting,
    Connected,
    /// Connection dropped and automatic reconnection is active.
    Disconnected {
        reason: String,
    },
    /// Device unlinked by the phone.
    LoggedOut,
    Failed(String),
}

impl LinkStatus {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }

    /// Stable, non-sensitive description suitable for the desktop log.
    pub(crate) fn log_label(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Unlinked { .. } => "unlinked",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnected { .. } => "disconnected",
            Self::LoggedOut => "logged out",
            Self::Failed(_) => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LinkStatus;

    #[test]
    fn backend_waits_for_window_acknowledgement_before_touching_storage() {
        let directory = tempfile::tempdir().unwrap();
        let dirs = crate::paths::AppDirs::under(directory.path());
        let mut backend = super::Backend::spawn(dirs.clone(), super::Waker);
        assert!(!dirs.session_db().exists());
        assert!(!dirs.archive_db().exists());
        // Closing before a first frame must cancel startup without connecting
        // or hanging while joining the waiting worker.
        backend.shutdown();
        assert!(!dirs.session_db().exists());
        assert!(!dirs.archive_db().exists());
    }

    #[test]
    fn batch_poll_preserves_events_for_the_next_main_context_turn() {
        let (backend, events) = super::Backend::detached();
        for index in 0..=super::EVENT_BATCH_LIMIT {
            events.send(super::Event::Info(index.to_string())).unwrap();
        }

        let batch = backend.poll_batch(super::EVENT_BATCH_LIMIT);
        assert_eq!(batch.len(), super::EVENT_BATCH_LIMIT);
        assert!(matches!(batch.first(), Some(super::Event::Info(value)) if value == "0"));
        assert!(matches!(batch.last(), Some(super::Event::Info(value)) if value == "255"));
        assert!(
            matches!(backend.poll_batch(super::EVENT_BATCH_LIMIT).as_slice(), [super::Event::Info(value)] if value == "256")
        );
    }

    #[test]
    fn zero_limit_leaves_events_and_legacy_poll_drains_the_rest() {
        let (backend, events) = super::Backend::detached();
        events.send(super::Event::Info("first".into())).unwrap();
        events.send(super::Event::Info("second".into())).unwrap();

        assert!(backend.poll_batch(0).is_empty());
        assert!(
            matches!(backend.poll().as_slice(), [super::Event::Info(first), super::Event::Info(second)] if first == "first" && second == "second")
        );
    }

    #[test]
    fn oversized_limit_drains_all_available_events_in_order() {
        let (backend, events) = super::Backend::detached();
        for index in 0..3 {
            events.send(super::Event::Info(index.to_string())).unwrap();
        }

        let events = backend.poll_batch(super::EVENT_BATCH_LIMIT + 1);
        assert!(
            matches!(events.as_slice(), [super::Event::Info(first), super::Event::Info(second), super::Event::Info(third)] if first == "0" && second == "1" && third == "2")
        );
    }

    #[test]
    fn link_logs_redact_pairing_credentials() {
        let qr = "qr-payload-that-links-an-account";
        let code = "12345678";
        let phone = "573001234567";
        let status = LinkStatus::Unlinked {
            qr: Some(qr.into()),
            pair_code: Some(code.into()),
            pairing_phone: Some(phone.into()),
        };

        // The previous Debug formatting leaked every field into zaptide.log.
        let previous = format!("link: {status:?}");
        assert!(previous.contains(qr));
        assert!(previous.contains(code));
        assert!(previous.contains(phone));

        let current = format!("link: {}", status.log_label());
        assert_eq!(current, "link: unlinked");
        assert!(!current.contains(qr));
        assert!(!current.contains(code));
        assert!(!current.contains(phone));
    }
}

/// Oldest loaded message timestamp and id used as a page boundary.
pub type PageKey = (i64, String);

#[derive(Clone, Debug)]
pub struct CreatedPoll {
    pub id: String,
    pub secret: Vec<u8>,
    pub creator: String,
    pub recipients: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Command {
    PollHistoryFailed {
        session_generation: u64,
        chat: ChatId,
        message: String,
        requested: std::time::Instant,
    },
    CreatePoll {
        chat: ChatId,
        draft: PollDraft,
    },
    PollCreated {
        session_generation: u64,
        chat: ChatId,
        draft: PollDraft,
        result: Result<CreatedPoll, String>,
    },
    VotePoll {
        chat: ChatId,
        message: String,
        choices: Vec<usize>,
    },
    PollVoted {
        session_generation: u64,
        chat: ChatId,
        message: String,
        choices: Vec<usize>,
        at: i64,
        result: Result<String, String>,
    },
    PollDecoded {
        session_generation: u64,
        vote: crate::archive::PollVote,
        choices: Option<Vec<usize>>,
    },
    SendText {
        chat: ChatId,
        text: String,
        quoting: Option<String>,
        mentions: Vec<String>,
    },
    SendContact {
        chat: ChatId,
        contact: crate::contact_cards::ContactCard,
        quoting: Option<String>,
    },
    /// Contact sharing completes independently of the composer request.
    ContactSent {
        chat: ChatId,
        id: String,
        session_generation: u64,
        error: Option<String>,
    },
    /// Answers a quick-reply button message with one of its buttons.
    AnswerButton {
        chat: ChatId,
        message: String,
        button: String,
    },
    /// Answers a list message with one of its rows.
    AnswerListRow {
        chat: ChatId,
        message: String,
        row: String,
    },
    /// Forwards an archived message to another chat.
    Forward {
        from_chat: ChatId,
        message: String,
        to_chat: ChatId,
    },
    /// Updates our typing state in a chat.
    Composing {
        chat: ChatId,
        composing: bool,
    },
    /// Marks a visible chat read and optionally sends receipts.
    MarkRead {
        chat: ChatId,
        receipts: bool,
    },
    /// Result of a private read-state update to the other linked devices.
    ReadSyncFinished {
        session_generation: u64,
        attempt_id: u64,
        chat: ChatId,
        through: i64,
        success: bool,
    },
    /// Loads archived chat messages before an optional boundary.
    LoadChat {
        chat: ChatId,
        before: Option<PageKey>,
    },
    /// Requests messages before the archive's earliest message.
    FetchOlder(ChatId),
    Download {
        chat: ChatId,
        message: String,
    },
    /// Requests a profile picture; `full` selects the info-dialog size.
    FetchAvatar {
        id: String,
        full: bool,
    },
    /// Loads archived messages from `id` through the current page.
    LoadUntil {
        chat: ChatId,
        id: String,
        before: PageKey,
    },
    /// Internal result for a failed phone-history request.
    OlderFailed {
        session_generation: u64,
        request_id: u64,
        chat: ChatId,
        error: String,
    },
    /// Correlates the phone's history response with the local fetch attempt.
    OlderStarted {
        session_generation: u64,
        request_id: u64,
        chat: ChatId,
        protocol_id: String,
    },
    /// Completes one optimistic revoke attempt, unless the phone confirmed it first.
    RevokeFinished {
        session_generation: u64,
        attempt_id: u64,
        chat: ChatId,
        message: String,
        success: bool,
    },
    /// Internal group-metadata failure.
    GroupInfoFailed {
        session_generation: u64,
        chat: ChatId,
        /// Whether the server refusal is permanent.
        permanent: bool,
    },
    EditText {
        chat: ChatId,
        id: String,
        text: String,
        mentions: Vec<String>,
    },
    /// Internal completion of an edit request.
    Edited {
        chat: ChatId,
        id: String,
        session_generation: u64,
        success: bool,
        content: Content,
        mentions: Vec<MentionRef>,
    },
    Revoke {
        chat: ChatId,
        id: String,
    },
    DeleteLocal {
        chat: ChatId,
        id: String,
    },
    /// Sends files with the caption on the first. Paths in `documents` go
    /// as documents even when they are images, videos, or audio.
    SendFiles {
        chat: ChatId,
        paths: Vec<PathBuf>,
        documents: std::collections::HashSet<PathBuf>,
        caption: Option<String>,
        quoting: Option<String>,
        mentions: Vec<String>,
    },
    /// Sends a clipboard image as straight-alpha RGBA.
    SendImage {
        chat: ChatId,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        caption: Option<String>,
        quoting: Option<String>,
        mentions: Vec<String>,
    },
    /// Syncs chat mute state. `Some(0)` is indefinite and `None` unmutes.
    SetMuted(ChatId, Option<i64>),
    /// Normalizes, encodes, and sends mono 48 kHz push-to-talk audio.
    SendVoice {
        chat: ChatId,
        samples: Vec<f32>,
        quoting: Option<String>,
    },
    /// Sends a played receipt for a voice message.
    MarkPlayed {
        chat: ChatId,
        message: String,
        sender: String,
        receipts: bool,
    },
    /// Sends a WebP sticker.
    SendSticker {
        chat: ChatId,
        path: PathBuf,
    },
    /// Saves a name through contact sync. `first_name` is the short display
    /// name; `to_phone` also adds it to the phone's address book.
    SaveContact {
        session_generation: Option<u64>,
        id: String,
        full_name: String,
        first_name: Option<String>,
        to_phone: bool,
    },
    /// Internal contact-save result.
    ContactSaved {
        session_generation: u64,
        id: String,
        name: String,
        error: Option<String>,
    },
    /// Checks a number, optionally saves it, and opens its chat.
    NewContact {
        phone: String,
        full_name: Option<String>,
        first_name: Option<String>,
        to_phone: bool,
    },
    /// Internal number-lookup result.
    ContactChecked {
        session_generation: u64,
        phone: String,
        full_name: Option<String>,
        first_name: Option<String>,
        to_phone: bool,
        registered: bool,
    },
    /// Internal number-lookup failure; never treat an API error as registration.
    ContactCheckFailed {
        session_generation: u64,
    },
    /// Loads recent and saved stickers for the picker.
    RecentStickers,
    React {
        chat: ChatId,
        message: String,
        emoji: String,
    },
    SetArchived(ChatId, bool),
    SetPinned(ChatId, bool),
    PairWithPhone(String),
    /// Drops a phone-number pairing so the QR code links instead.
    CancelPhonePairing,
    /// Unlinks the device remotely and locally.
    Unlink,
    Reconnect,
    Shutdown,
    /// Internal send result.
    Sent {
        chat: ChatId,
        id: String,
        session_generation: u64,
        error: Option<String>,
    },
    /// Internal attachment-download result.
    Downloaded {
        chat: ChatId,
        id: String,
        session_generation: u64,
        raw_fingerprint: [u8; 32],
        destination: PathBuf,
        result: Result<PathBuf, String>,
    },
    /// Internal recent-sticker download result.
    StickerFetched {
        hash: String,
        session_generation: u64,
        result: Result<PathBuf, String>,
    },
    /// Internal profile-picture result.
    AvatarFetched {
        id: String,
        full: bool,
        session_generation: u64,
        avatar_generation: u64,
        path: Option<PathBuf>,
    },
    /// Internal retryable profile-picture failure.
    AvatarFailed {
        id: String,
        full: bool,
        session_generation: u64,
        avatar_generation: u64,
    },
    /// Fetches a contact's About text for the info dialog.
    ContactAbout {
        id: String,
    },
    /// Internal contact About-text result.
    ContactAboutFetched {
        session_generation: u64,
        id: String,
        about: Option<String>,
    },
    /// Internal account about-text result.
    MeInfo {
        session_generation: u64,
        about: Option<String>,
    },
    /// Internal uploaded attachment ready for archiving and sending.
    Outbound {
        chat: ChatId,
        session_generation: u64,
        row: Box<Message>,
        raw: Vec<u8>,
    },
    /// Internal ordered attachment send; completion arrives after WhatsApp accepts it.
    OutboundBatch {
        chat: ChatId,
        session_generation: u64,
        row: Box<Message>,
        raw: Vec<u8>,
        sent: tokio::sync::mpsc::UnboundedSender<bool>,
    },
    /// Internal upload result for one selected attachment.
    AttachmentCompleted {
        chat: ChatId,
        session_generation: u64,
        batch: u64,
        index: usize,
        total: usize,
        path: PathBuf,
        success: bool,
    },
    /// Internal send audience. The sender waits for it to be archived.
    GroupRecipients {
        session_generation: u64,
        chat: ChatId,
        id: String,
        recipients: Vec<String>,
        lids: Vec<(String, String)>,
        stored: tokio::sync::mpsc::UnboundedSender<bool>,
    },
    /// Internal group metadata result.
    GroupInfo {
        session_generation: u64,
        chat: ChatId,
        name: Option<String>,
        participants: Vec<String>,
        read_only: bool,
        ephemeral_expiration: Option<u32>,
        ephemeral_setting_timestamp: Option<i64>,
    },
    /// Internal pairing-code result.
    PairCode {
        request_id: u64,
        result: Result<String, String>,
    },
    /// Internal account read-receipt setting.
    ReceiptsPrivacy {
        session_generation: u64,
        disabled: bool,
    },
}

#[derive(Debug)]
pub enum Event {
    PollCreated {
        chat: ChatId,
        error: Option<String>,
    },
    PollVoted {
        chat: ChatId,
        message: String,
        error: Option<String>,
    },
    Link(LinkStatus),
    /// Full chat list, newest first.
    Chats(Vec<Chat>),
    ChatUpdated(Box<Chat>),
    /// Chat messages in ascending order. `older` prepends them; `complete`
    /// means the archive has no earlier rows.
    Messages {
        chat: ChatId,
        messages: Vec<Message>,
        older: bool,
        complete: bool,
    },
    MessageUpdated(Box<Message>),
    /// Result of an outgoing message edit, without protocol error details.
    Edited {
        chat: ChatId,
        id: String,
        success: bool,
    },
    /// Result of an outgoing message send, without protocol error details.
    Sent {
        chat: ChatId,
        success: bool,
    },
    /// Upload outcome for one staged attachment. Protocol error details are omitted.
    AttachmentCompleted {
        chat: ChatId,
        batch: u64,
        index: usize,
        total: usize,
        path: PathBuf,
        success: bool,
    },
    /// Live incoming message for desktop notification.
    Incoming {
        chat: ChatId,
        message: Box<Message>,
    },
    Contacts(Vec<Contact>),
    Typing {
        chat: ChatId,
        sender: String,
        composing: bool,
    },
    ContactAbout {
        id: String,
        about: Option<String>,
    },
    Presence {
        id: String,
        online: bool,
        last_seen: Option<i64>,
    },
    Avatar {
        id: String,
        full: bool,
        path: Option<PathBuf>,
    },
    MessageDeleted {
        chat: ChatId,
        id: String,
    },
    /// Saved stickers, imported packs, and recent stickers for the picker.
    Stickers {
        saved: Vec<PathBuf>,
        packs: Vec<StickerPack>,
        recent: Vec<PathBuf>,
    },
    Media {
        chat: ChatId,
        message: String,
        result: Result<PathBuf, String>,
    },
    /// Link-time history sync state.
    Syncing(bool),
    /// Phone-history result. `more` indicates whether another request may help.
    OlderFetched {
        chat: ChatId,
        more: bool,
    },
    /// Whether account privacy disables direct-chat read receipts.
    ReceiptsPrivacy {
        disabled: bool,
    },
    /// Number lookup succeeded and its chat can open.
    ContactReady {
        id: String,
        name: Option<String>,
    },
    /// Informational toast message.
    Info(String),
    Error(String),
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub struct Waker;

#[cfg(test)]
impl Wake for Waker {
    fn wake(&self) {}
}

impl Wake for crate::notifier::EventNotifier {
    fn wake(&self) {
        self.notify();
    }
}

/// UI handle to the backend runtime.
pub struct Backend {
    startup: Option<tokio::sync::oneshot::Sender<()>>,
    commands: mpsc::UnboundedSender<Command>,
    events: std::sync::mpsc::Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Backend {
    #[cfg(test)]
    pub fn spawn(dirs: AppDirs, waker: impl Wake + 'static) -> Self {
        Self::try_spawn(dirs, waker).expect("unable to start backend")
    }

    /// Starts the backend runtime without panicking on process-resource failure.
    pub fn try_spawn(dirs: AppDirs, waker: impl Wake + 'static) -> anyhow::Result<Self> {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("zaptide-runtime")
            .enable_all()
            .build()?;
        let worker_commands = command_tx.clone();
        let (startup, started) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("zaptide-backend".to_string())
            .spawn(move || {
                runtime.block_on(async move {
                    if started.await.is_ok() {
                        worker::run(dirs, event_tx, worker_commands, command_rx, Arc::new(waker))
                            .await;
                    }
                });
                runtime.shutdown_timeout(Duration::from_secs(3));
            })?;

        Ok(Self {
            startup: Some(startup),
            commands: command_tx,
            events: event_rx,
            thread: Some(thread),
        })
    }

    /// Creates a disconnected backend and event sender for tests.
    #[cfg(test)]
    pub fn detached() -> (Self, std::sync::mpsc::Sender<Event>) {
        let (command_tx, _command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        (
            Self {
                startup: None,
                commands: command_tx,
                events: event_rx,
                thread: None,
            },
            event_tx,
        )
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    #[cfg(test)]
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Drains at most `limit` events, preserving remaining events for a later turn.
    pub fn poll_batch(&self, limit: usize) -> Vec<Event> {
        self.events.try_iter().take(limit).collect()
    }

    /// Start database migrations only after the window is shown. Dropping this
    /// permit cancels startup.
    pub fn take_startup(&mut self) -> Option<tokio::sync::oneshot::Sender<()>> {
        self.startup.take()
    }

    pub fn shutdown(&mut self) {
        self.startup.take();
        self.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
