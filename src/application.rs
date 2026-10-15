//! Relm4 root shell: link page, chat list, and conversation.

mod account_switcher;
mod accounts;
mod actions;
mod albums;
mod audio;
mod bubbles;
mod chats;
mod components;
mod composer;
mod contact_sharing;
mod conversation;
mod dialogs;
mod dispatch;
mod helpers;
mod icons;
mod link_page;
mod linking;
mod motion;
mod presentation;
mod profile_dialog;
mod rows;
mod sidebar;
mod transcript_view;

use crate::i18n::gettext;
use actions::*;
use albums::{AlbumRole, album_roles};
use bubbles::*;
use composer::{
    ComposerState, ComposerView, ComposerViewInit, ComposerViewInput, ComposerViewOutput,
};
use dialogs::{attach_tile, show_archive_confirmation, show_remove_account_confirmation};
use dialogs::{
    show_chat_info_dialog, show_forward_dialog, show_mention_dialog, show_new_chat_dialog,
    show_poll_dialog, sticker_picker_content,
};
use helpers::*;
use icons::*;
use link_page::{LinkPage, LinkPageInit, LinkPageInput, LinkPageOutput, LinkPageState};
use linking::{link_page, qr_texture};
use motion::*;
use presentation::*;
use rows::MessageRowWidgets;
use sidebar::{Sidebar, SidebarInit, SidebarInput, SidebarOutput, SidebarState};
use transcript_view::{
    TranscriptState, TranscriptView, TranscriptViewInit, TranscriptViewInput, TranscriptViewOutput,
};

use adw::prelude::*;
use relm4::{
    RelmApp,
    prelude::*,
    typed_view::list::{RelmListItem, TypedListView},
};
use relm4::{adw, gtk};

use crate::{
    backend::{Backend, Event, LinkStatus},
    event_drain::drain_backend_events,
    glib_notifier::GlibEventDrain,
    message_window::{ACTIVE_MESSAGE_LIMIT, trim_message_window},
    notifier::EventNotifier,
    paths::AppDirs,
};

fn dialog_action_callback(
    sender: &ComponentSender<NativeApplication>,
) -> dialogs::DialogActionCallback {
    let sender = sender.clone();
    std::rc::Rc::new(move |action| {
        sender.input(match action {
            dialogs::DialogAction::ArchiveChat(id) => Input::ArchiveChat(id),
            dialogs::DialogAction::UnlinkConfirmed => Input::UnlinkConfirmed,
            dialogs::DialogAction::CreatePoll(draft) => Input::CreatePoll(draft),
            dialogs::DialogAction::StartChat { id, name } => Input::StartChat { id, name },
            dialogs::DialogAction::NewContact { phone, name } => Input::NewContact { phone, name },
            dialogs::DialogAction::InsertMentionId(id) => Input::InsertMentionId(id),
            dialogs::DialogAction::ForwardSelected(ids) => Input::ForwardSelected(ids),
            dialogs::DialogAction::SendSticker(path) => Input::SendSticker(path),
        });
    })
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ChatRow {
    id: String,
    last_activity: i64,
    name: String,
    preview: String,
    unread: Option<String>,
    avatar: Option<std::path::PathBuf>,
    pinned: bool,
    muted: bool,
    /// Muted and archived chats count unread messages in grey.
    quiet: bool,
    /// Delivery of our own last message; `None` for incoming.
    delivery: crate::model::Delivery,
    /// The chat open beside the list. Selection follows the pointer in a
    /// single-click list, so the open chat needs a mark of its own.
    open: bool,
}

thread_local! {
    /// Decoded avatars by file. Rows rebind constantly while scrolling.
    static AVATAR_TEXTURES: std::cell::RefCell<
        std::collections::HashMap<std::path::PathBuf, gtk::gdk::Texture>,
    > = std::cell::RefCell::default();
    static AVATAR_TEXTURE_REFERENCES: std::cell::RefCell<
        std::collections::HashMap<std::path::PathBuf, usize>,
    > = std::cell::RefCell::default();
}

fn clear_avatar_textures() {
    AVATAR_TEXTURES.with_borrow_mut(|cache| cache.clear());
    AVATAR_TEXTURE_REFERENCES.with_borrow_mut(|references| references.clear());
}

fn update_avatar_texture_references<T>(
    cache: &mut std::collections::HashMap<std::path::PathBuf, T>,
    references: &mut std::collections::HashMap<std::path::PathBuf, usize>,
    previous: Option<std::path::PathBuf>,
    current: Option<std::path::PathBuf>,
) {
    if previous == current {
        return;
    }
    if let Some(previous) = previous
        && let Some(count) = references.get_mut(&previous)
    {
        if *count > 1 {
            *count -= 1;
        } else {
            references.remove(&previous);
            cache.remove(&previous);
        }
    }
    if let Some(current) = current {
        *references.entry(current).or_default() += 1;
    }
}

fn update_avatar_texture_reference(
    previous: Option<std::path::PathBuf>,
    current: Option<std::path::PathBuf>,
) {
    AVATAR_TEXTURES.with_borrow_mut(|cache| {
        AVATAR_TEXTURE_REFERENCES.with_borrow_mut(|references| {
            update_avatar_texture_references(cache, references, previous, current);
        });
    });
}

fn avatar_texture_is_referenced(path: &std::path::Path) -> bool {
    AVATAR_TEXTURE_REFERENCES.with_borrow(|references| references.contains_key(path))
}

enum ChatChange {
    Snapshot(Vec<crate::model::Chat>),
    Update(crate::model::Chat),
}

enum NativeEvent {
    Profile {
        phone: Option<String>,
        name: Option<String>,
    },
    Link(LinkStatus),
    Syncing(bool),
    Chats(Vec<crate::model::Chat>),
    ChatUpdated(Box<crate::model::Chat>),
    Contacts(Vec<crate::model::Contact>),
    Messages {
        chat: String,
        messages: Vec<crate::model::Message>,
        older: bool,
        complete: bool,
    },
    MessageUpdated(Box<crate::model::Message>),
    Edited {
        chat: String,
        id: String,
        success: bool,
    },
    Sent {
        chat: String,
        success: bool,
    },
    AttachmentCompleted {
        chat: String,
        path: std::path::PathBuf,
        success: bool,
    },
    Media {
        chat: String,
        message: String,
        result: Result<std::path::PathBuf, String>,
    },
    ReceiptsPrivacy {
        disabled: bool,
    },
    ContactReady {
        id: String,
        name: Option<String>,
    },
    ContactAbout {
        id: String,
        about: Option<String>,
    },
    ProfileSaved {
        error: Option<String>,
    },
    Info(String),
    Error(String),
    Typing {
        chat: String,
        sender: String,
        composing: bool,
    },
    Presence {
        id: String,
        online: bool,
        last_seen: Option<i64>,
    },
    Avatar {
        id: String,
        path: Option<std::path::PathBuf>,
    },
    MessageDeleted {
        chat: String,
        id: String,
    },
    Incoming {
        chat: String,
        message: Box<crate::model::Message>,
    },
    OlderFetched {
        chat: String,
        more: bool,
    },
    Stickers {
        saved: Vec<std::path::PathBuf>,
        packs: Vec<crate::model::StickerPack>,
        recent: Vec<std::path::PathBuf>,
    },
}

struct PendingSend {
    chat: String,
    text: String,
    reply: Option<String>,
    attachments: Vec<std::path::PathBuf>,
    failed_attachments: Vec<std::path::PathBuf>,
    clipboard_image: bool,
    remaining: usize,
    failed: bool,
}

#[derive(Clone)]
pub struct ClipboardPixels {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    preview: gtk::gdk::Texture,
}

impl std::fmt::Debug for ClipboardPixels {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClipboardPixels")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixels", &"[REDACTED]")
            .field("preview", &"[REDACTED]")
            .finish()
    }
}

impl PendingSend {
    fn complete(&mut self, success: bool) -> bool {
        self.remaining = self.remaining.saturating_sub(1);
        self.failed |= !success;
        self.remaining == 0
    }

    fn complete_attachment(&mut self, path: std::path::PathBuf, success: bool) -> bool {
        if !success {
            self.failed_attachments.push(path);
        }
        self.complete(success)
    }
}

struct MessageRow {
    id: String,
    /// Day and unread separators shown above the row.
    separator: String,
    sender: String,
    sender_class: &'static str,
    avatar: Option<std::path::PathBuf>,
    quote: String,
    /// Selectable text: the message body, or a caption.
    body: String,
    /// People mentioned in `body`, shown by name.
    mentions: Vec<crate::safety::MentionLabel>,
    footer: String,
    accessible_label: String,
    show_sender: bool,
    show_timestamp: bool,
    pointer_sender: ComponentSender<NativeApplication>,
    message: crate::model::Message,
    /// Photos sent together, drawn as one grid by the first of them. Empty
    /// for any other row.
    album: Vec<crate::model::Message>,
    /// A later photo of an album: its row stays in the list, drawn empty.
    collapsed: bool,
    audio: Option<crate::native_voice::VoiceMessage>,
    audio_registry: AudioRegistry,
    /// `None` outside selection mode; otherwise whether the row is picked.
    selected: Option<bool>,
}

impl MessageRow {
    /// Whether rebinding `other` would draw exactly this row.
    fn renders_like(&self, other: &Self) -> bool {
        self.message == other.message
            && self.album == other.album
            && self.collapsed == other.collapsed
            && self.audio == other.audio
            && self.mentions == other.mentions
            && self.separator == other.separator
            && self.avatar == other.avatar
            && self.show_sender == other.show_sender
            && self.show_timestamp == other.show_timestamp
            && self.selected == other.selected
    }
}

impl PartialEq for MessageRow {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for MessageRow {}

impl PartialOrd for MessageRow {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MessageRow {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

type AudioRegistry = std::rc::Rc<
    std::cell::RefCell<
        std::collections::HashMap<String, crate::native_media_widgets::AudioControls>,
    >,
>;

struct AudioState {
    selected_voice: Option<crate::native_voice::VoiceMessage>,
    selected_voice_message: Option<String>,
    media: crate::services::media::MediaService,
    audio_waveforms: std::collections::HashMap<(String, String), Vec<u8>>,
    waveform_queue: std::collections::VecDeque<(String, String, std::path::PathBuf)>,
    waveform_busy: bool,
    waveform_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    waveform_attempted: std::collections::HashSet<(String, String)>,
    playing_audio: Option<String>,
    audio_errors: std::collections::HashMap<(String, String), String>,
    audio_registry: AudioRegistry,
    voice_send_pending: bool,
    recording_meter: crate::native_media_widgets::RecordingMeter,
    played_voice: std::collections::HashSet<(String, String)>,
}

/// True when `b` would render the same media as `a`. A sent sticker moves
/// from the picker file to the uploaded cache copy; the picture is the same,
/// so keep the decoded one while the old file still exists.
fn same_media(a: &crate::model::Message, b: &crate::model::Message) -> bool {
    use crate::model::Content;
    let same_content = match (&a.content, &b.content) {
        (Content::Sticker { media: old, .. }, Content::Sticker { media: new, .. }) => {
            old == new
                || (new.path.is_some() && old.path.as_ref().is_some_and(|path| path.is_file()))
        }
        (old, new) => old == new,
    };
    a.id == b.id
        && a.chat == b.chat
        && a.from_me == b.from_me
        && same_content
        && a.thumbnail == b.thumbnail
}

/// Express a row-local click in the list's coordinates without sending GTK objects across threads.
fn message_menu_position(row: &gtk::Widget, x: f64, y: f64) -> Option<gtk::graphene::Point> {
    let list = row.ancestor(gtk::ListView::static_type())?;
    row.compute_point(&list, &gtk::graphene::Point::new(x as f32, y as f32))
}

pub struct Init {
    pub dirs: AppDirs,
}

/// How long the resume spinner may show if the link never reports back.
const RESUME_REFRESH_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

/// Whether a check that saw `wall` pass on the wall clock and `monotonic` on
/// the monotonic clock woke from a suspend: only the wall clock keeps
/// counting while the computer sleeps.
fn slept_through(wall: std::time::Duration, monotonic: std::time::Duration) -> bool {
    wall.saturating_sub(monotonic) > std::time::Duration::from_secs(20)
}

pub struct NativeApplication {
    window: adw::ApplicationWindow,
    backend: Option<Backend>,
    notifications: crate::native_notifications::NativeNotifications,
    notifier: EventNotifier,
    /// The active account's event drain; hidden accounts keep their own.
    active_drain: Option<GlibEventDrain>,
    /// Linked accounts that are not on screen.
    accounts: std::collections::HashMap<crate::account::AccountId, accounts::AccountSession>,
    active_account: crate::account::AccountId,
    active_dirs: AppDirs,
    base_dirs: AppDirs,
    registry: crate::account::Registry,
    /// The single-account files could not move into their folder yet.
    legacy_layout: bool,
    account_switcher: account_switcher::AccountSwitcher,
    /// The same, on the link page of an account that cannot be used.
    link_switcher: account_switcher::AccountSwitcher,
    /// An account being linked, and the one to return to if it is cancelled.
    pending_account: Option<(crate::account::AccountId, crate::account::AccountId)>,
    /// The account whose removal waits for its logout.
    removing: Option<crate::account::AccountId>,
    shutdown_started: bool,
    chats: TypedListView<ChatRow, gtk::SingleSelection>,
    chat_projection: crate::native_chat_list::ChatListProjection,
    chat_filters: crate::native_chat_list::ChatListFilters,
    chat_ids: Vec<String>,
    chat_targets: std::rc::Rc<std::cell::RefCell<chats::ChatTargets>>,
    chat_snapshots: Vec<crate::model::Chat>,
    contacts: std::collections::HashMap<String, crate::model::Contact>,
    contact_share_generation: u64,
    /// Header menu for the open chat, relabelled by `sync_chat_menu`.
    chat_menu: gtk::gio::Menu,
    /// (group, pinned, muted, archived) the chat menu was last labelled for.
    chat_menu_state: Option<(bool, bool, bool, bool)>,
    tray: Option<crate::native_tray::TrayHandle>,
    tray_state: crate::native_tray::TrayState,
    /// Whether a tray host shows the icon, so closing can hide the window.
    tray_shown: bool,
    avatars: std::collections::HashMap<String, std::path::PathBuf>,
    avatar_requests: std::collections::HashSet<String>,
    /// Who is typing per chat: display name and sender ID.
    typing: std::collections::HashMap<String, (String, String)>,
    typing_until: std::collections::HashMap<String, std::time::Instant>,
    composing_until: std::collections::HashMap<String, std::time::Instant>,
    presence: std::collections::HashMap<String, (bool, Option<i64>)>,
    messages: TypedListView<MessageRow, gtk::NoSelection>,
    qr_texture: Option<gtk::gdk::Texture>,
    link_page: relm4::Controller<LinkPage>,
    transcript_view: relm4::Controller<TranscriptView>,
    composer_view: relm4::Controller<ComposerView>,
    history_complete: bool,
    loading_older: bool,
    message_ids: Vec<String>,
    message_snapshots: std::collections::HashMap<String, crate::model::Message>,
    pending_quote_navigation: Option<(String, String)>,
    pointer_sender: ComponentSender<NativeApplication>,
    editable_messages: std::collections::HashMap<String, String>,
    transcript: Vec<crate::native_transcript::TranscriptRow>,
    audio: AudioState,
    /// About row of the open contact-info dialog, filled when the fetch returns.
    info_about: Option<(String, adw::ActionRow)>,
    message_target: Option<String>,
    /// Unread count when the chat was opened, until the first page pins it
    /// to `unread_marker`, so live arrivals never move the "Unread" line.
    opened_unread: usize,
    unread_marker: Option<String>,
    recent_messages_pending: bool,
    /// Messages picked for a batch forward, oldest first; `None` outside
    /// selection mode.
    message_selection: Option<Vec<String>>,
    message_menu: gtk::PopoverMenu,
    poll_choice: usize,
    sticker_packs: Vec<crate::model::StickerPack>,
    recent_stickers: Vec<std::path::PathBuf>,
    favorite_stickers: Vec<std::path::PathBuf>,
    sticker_emojis: std::collections::HashMap<std::path::PathBuf, Vec<String>>,
    active_chat: Option<String>,
    draft: String,
    drafts: std::collections::HashMap<String, String>,
    composer: crate::native_composer::NativeComposerState,
    composer_buffer: gtk::TextBuffer,
    /// The open sticker picker and its page stack, refreshed as lists arrive.
    sticker_picker: Option<(gtk::Popover, gtk::Stack)>,
    pending_composer_request: Option<crate::native_composer::ComposerRequest>,
    pending_attachments: std::collections::HashMap<String, Vec<std::path::PathBuf>>,
    /// Staged paths picked through Files, sent as documents. The last pick of
    /// a path wins, so it survives a failed send and re-queue.
    document_attachments: std::collections::HashSet<(String, std::path::PathBuf)>,
    pending_clipboard_images: std::collections::HashMap<String, ClipboardPixels>,
    pending_send: Option<PendingSend>,
    pending_edit: Option<(String, String, String)>,
    reply_to: Option<(String, String)>,
    editing: Option<(String, String)>,
    account_receipts_off: bool,
    link: LinkStatus,
    syncing: bool,
    /// Reconnecting after a suspend, until the link is back.
    resuming: bool,
    /// The link left `Connected` since the resume began.
    resume_dropped: bool,
    theme_catalog: crate::theme::custom::Catalog,
    page_title: String,
    status: String,
    settings: crate::settings::Settings,
    settings_path: std::path::PathBuf,
    portals: crate::native_portals::NativePortals,
    portal_requests: std::rc::Rc<
        std::cell::RefCell<std::collections::HashSet<crate::native_portals::RequestId>>,
    >,
    preferences: Option<crate::native_preferences::NativePreferencesDialog>,
    profile_dialog: Option<profile_dialog::ProfileDialog>,
    sidebar: relm4::Controller<Sidebar>,
    sidebar_visible: bool,
    split_view: Option<adw::NavigationSplitView>,
    phone_linking: bool,
    /// Chat snapshots changed since the list widget was last rebuilt.
    chats_dirty: bool,
    search_resets: u64,
    chats_flush_scheduled: bool,
    zoom_provider: gtk::CssProvider,
    custom_theme_provider: gtk::CssProvider,
    enter_sends: std::rc::Rc<std::cell::Cell<bool>>,
}

#[derive(Debug)]
pub enum Input {
    WindowMapped,
    WindowActivated,
    /// The split view moved between the chat list and the conversation itself,
    /// as its back button does.
    SidebarShown(bool),
    StartBackend,
    BackendReady(crate::account::AccountId),
    SwitchAccount(crate::account::AccountId),
    AddAccount,
    ShowProfile,
    SaveProfile(crate::backend::ProfileEdit),
    CancelAddAccount,
    ConfirmRemoveAccount,
    SelectChat(crate::account::AccountId, String),
    HighlightChat(u32),
    OpenChatId(String),
    OpenAccountChat(crate::account::AccountId, String),
    ScrollToRecentMessages,
    TranscriptAtEnd,
    LoadOlder,
    SearchChats(String),
    SetUnreadFilter(bool),
    SetPinnedFilter(bool),
    SetChatKindFilter(crate::native_chat_list::ChatKindFilter),
    SetArchivedFilter(bool),
    SetMutedFilter(bool),
    Reconnect,
    /// The computer woke from suspend: the connection may be dead.
    Resumed,
    /// Resume refresh still showing after its time limit.
    ResumeSettled,
    SelectMessage(u32),
    ShowMessageMenu {
        id: String,
        x: f32,
        y: f32,
    },
    OpenQuoted,
    OpenQuotedOf(String),
    ReplySelected,
    EditSelected,
    CancelReply,
    CancelEdit,
    DraftChanged(String),
    /// Gallery picks images and videos sent as media; Files sends anything
    /// as a document.
    PickAttachments {
        gallery: bool,
    },
    AttachmentsPicked {
        chat: String,
        paths: Vec<std::path::PathBuf>,
        documents: bool,
    },
    ClearAttachments,
    RemoveAttachment(std::path::PathBuf),
    SendText(String),
    CopyTranscript,
    ActivateVoice,
    CycleVoiceSpeed,
    SeekVoice(f64),
    AudioControl {
        id: String,
        intent: crate::native_voice::VoiceIntent,
    },
    AudioWaveformReady {
        chat: String,
        id: String,
        bars: Option<Vec<u8>>,
    },
    ActivateSelectedAttachment,
    ReactSelected(String),
    ReactTo {
        id: String,
        emoji: String,
    },
    CopySelectedText,
    ShowForward,
    ForwardSelected(Vec<String>),
    StartSelection,
    CancelSelection,
    DeleteSelected(bool),
    VoteOption(usize),
    CreatePoll(crate::model::PollDraft),
    Recording(crate::native_voice::RecordingIntent),
    PollVoice,
    ShowStickerPicker,
    ShowPollCreator,
    ShowContactPicker,
    ImportContact(contact_sharing::ShareTarget),
    ContactFileReady {
        target: contact_sharing::ShareTarget,
        result: Result<Vec<crate::contact_cards::ContactCard>, String>,
    },
    SendContact {
        target: contact_sharing::ShareTarget,
        contact: crate::contact_cards::ContactCard,
    },
    ContactActionFinished(Result<(), String>),
    SendSticker(std::path::PathBuf),
    ClearTyping(String),
    StopComposing(String),
    InsertEmoji(String),
    InsertMention,
    InsertMentionId(String),
    SelectMention(crate::native_composer::MentionCandidate),
    PasteClipboardImage,
    AttachDropped(Vec<std::path::PathBuf>),
    ClipboardImageReady {
        chat: String,
        pixels: ClipboardPixels,
    },
    PortalActionFinished(String),
    OpenSelectedUri,
    PortalUriFinished(bool),
    ShowSelectedInFolder,
    SaveSelectedAttachment,
    SaveAttachmentFinished(bool),
    ToggleSelectedPin,
    ToggleSelectedArchive,
    /// Flips this chat's archive state; archiving is confirmed first.
    ArchiveChat(String),
    ToggleSelectedMute,
    Close,
    Quit,
    ShutdownComplete,
    MediaAction(crate::native_media::NativeMediaAction),
    ShowPreferences,
    OpenThemesFolder,
    ThemesFolderPrepared(bool),
    ThemesFolderFinished(bool),
    SplitCollapsed(bool),
    ApplyPreferences,
    ShowAbout,
    ShowShortcuts,
    ConfirmDelete(bool),
    UnlinkConfirmed,
    ShowChatInfo,
    ShowNewChat,
    Tray(crate::native_tray::TrayAction),
    /// The window was shown or hidden outside the tray; `update` resyncs it.
    WindowVisibilityChanged,
    /// Opens the chat with a contact picked in New Chat, creating it if new.
    StartChat {
        id: String,
        name: String,
    },
    PairWithPhone(String),
    TogglePhoneLinking,
    CopyPairCode,
    CopyCode(String),
    FlushChats,
    NewContact {
        phone: String,
        name: Option<String>,
    },
}

#[relm4::component(pub)]
impl SimpleComponent for NativeApplication {
    type Init = Init;
    type Input = Input;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some("ZapTide"),
            set_default_size: (960, 640),
            set_size_request: (420, 480),

            connect_map[sender] => move |_| sender.input(Input::WindowMapped),

            connect_is_active_notify[sender] => move |window| {
                if window.is_active() {
                    sender.input(Input::WindowActivated);
                }
            },

            connect_close_request[sender] => move |_| {
                sender.input(Input::Close);
                gtk::glib::Propagation::Stop
            },

            #[wrap(Some)]
            set_content = &adw::ToastOverlay {
                #[wrap(Some)]
                #[name = "page_stack"]
                set_child = &gtk::Stack {
                    set_transition_type: gtk::StackTransitionType::Crossfade,

                    add_named[Some("link")] = model.link_page.widget(),

                    #[name = "main_split_view"]
                    add_named[Some("chats")] = &adw::NavigationSplitView {
                        // Collapsed, the chat list and the conversation are two
                        // pages; the conversation's header gets a back button.
                        // libadwaita's 180-280sp at 25% truncates chat names, so the
                        // sidebar keeps scaling with the window from a wider floor.
                        set_min_sidebar_width: 260.0,
                        set_max_sidebar_width: 360.0,
                        set_sidebar_width_fraction: 0.3,
                        #[watch]
                        set_show_content: !model.sidebar_visible,

                        set_sidebar: Some(&adw::NavigationPage::new(model.sidebar.widget(), "Chats")),

                        #[wrap(Some)]
                        set_content = &adw::NavigationPage {
                            set_title: &gettext("Chat"),
                            #[wrap(Some)]
                            set_child = &adw::ToolbarView {
                            add_top_bar = &adw::HeaderBar {
                                // The title opens the chat's details, as in GNOME's
                                // chat apps.
                                #[wrap(Some)]
                                set_title_widget = &gtk::Button {
                                    add_css_class: "flat",
                                    set_tooltip_text: Some(&gettext("Chat details")),
                                    #[watch]
                                    set_can_target: model.active_chat.is_some(),
                                    #[watch]
                                    set_can_focus: model.active_chat.is_some(),
                                    connect_clicked => Input::ShowChatInfo,
                                    #[wrap(Some)]
                                    #[name = "conversation_title"]
                                    set_child = &adw::WindowTitle {
                                        #[watch]
                                        set_title: if model.active_chat.is_some() { model.page_title.as_str() } else { "" },
                                        #[watch]
                                        set_subtitle: &model.header_subtitle(),
                                    },
                                },
                                pack_end = &gtk::MenuButton {
                                    set_icon_name: "view-more-symbolic",
                                    set_tooltip_text: Some(&gettext("Chat menu")),
                                    #[watch]
                                    set_visible: model.active_chat.is_some(),
                                    set_menu_model: Some(&model.chat_menu),
                                },
                            },
                            add_top_bar = &adw::Banner {
                                set_title: &gettext("Connection lost"),
                                set_button_label: Some("Reconnect"),
                                #[watch]
                                set_revealed: matches!(model.link, LinkStatus::Failed(_) | LinkStatus::Disconnected { .. }),
                                connect_button_clicked => Input::Reconnect,
                            },

                                #[wrap(Some)]
                                set_content = &gtk::Overlay {
                                    #[name = "conversation_body"]
                                    #[wrap(Some)]
                                    set_child = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        add_css_class: "zaptide-conversation",

                                         append = model.transcript_view.widget(),
                                         append = &gtk::Revealer {
                                             // Laid out like an incoming message row so it
                                             // reads as the last item of the conversation.
                                             set_transition_type: gtk::RevealerTransitionType::Crossfade,
                                             set_margin_start: 18,
                                             set_margin_top: 2,
                                             set_margin_bottom: 6,
                                             set_halign: gtk::Align::Start,
                                             // Crossfade keeps the child's height while hidden,
                                             // so the row is removed from layout when idle.
                                             #[watch]
                                             set_visible: model.active_chat.as_ref().is_some_and(|chat| model.typing.contains_key(chat)),
                                             #[watch]
                                             set_reveal_child: model.active_chat.as_ref().is_some_and(|chat| model.typing.contains_key(chat)),
                                             #[wrap(Some)]
                                             set_child = &gtk::Box {
                                                 set_spacing: 8,
                                                 append = &adw::Avatar {
                                                     set_size: 36,
                                                     set_show_initials: true,
                                                     set_valign: gtk::Align::End,
                                                     #[watch]
                                                     set_text: Some(&model.typing_name()),
                                                     #[watch]
                                                     set_custom_image: model.typing_avatar().as_ref(),
                                                 },
                                                 append = &gtk::Box {
                                                     add_css_class: "zaptide-bubble",
                                                     add_css_class: "incoming",
                                                     add_css_class: "zaptide-typing-bubble",
                                                     set_spacing: 4,
                                                     set_accessible_role: gtk::AccessibleRole::Status,
                                                     #[watch]
                                                     set_tooltip_text: Some(&model.typing_label()),
                                                     #[watch]
                                                     update_property: &[gtk::accessible::Property::Label(&model.typing_label())],
                                                     append = &gtk::Box { add_css_class: "zaptide-typing-dot", set_valign: gtk::Align::Center },
                                                     append = &gtk::Box { add_css_class: "zaptide-typing-dot", set_valign: gtk::Align::Center },
                                                     append = &gtk::Box { add_css_class: "zaptide-typing-dot", set_valign: gtk::Align::Center },
                                                 },
                                             },
                                         },
                                         append = model.composer_view.widget(),
                                         // Replaces the composer while messages are picked.
                                         append = &gtk::CenterBox {
                                             add_css_class: "toolbar",
                                             set_margin_start: 6,
                                             set_margin_end: 6,
                                             set_margin_top: 6,
                                             set_margin_bottom: 6,
                                             #[watch]
                                             set_visible: model.message_selection.is_some(),
                                             #[wrap(Some)]
                                             set_start_widget = &gtk::Button {
                                                 set_label: &gettext("Cancel"),
                                                 add_css_class: "flat",
                                                 connect_clicked => Input::CancelSelection,
                                             },
                                             #[wrap(Some)]
                                             set_center_widget = &gtk::Label {
                                                 add_css_class: "heading",
                                                 #[watch]
                                                 set_label: &selection_title(model.selected_rows()),
                                             },
                                             #[wrap(Some)]
                                             set_end_widget = &gtk::Button {
                                                 add_css_class: "suggested-action",
                                                 set_tooltip_text: Some(&gettext("Forward selected messages")),
                                                 #[wrap(Some)]
                                                 set_child = &adw::ButtonContent {
                                                     set_icon_name: "mail-forward-symbolic",
                                                     set_label: &gettext("Forward"),
                                                 },
                                                 #[watch]
                                                 set_sensitive: model.message_selection.as_ref().is_some_and(|selection| !selection.is_empty()),
                                                 connect_clicked => Input::ShowForward,
                                             },
                                         },
                                    },
                                    // Shown while files are dragged over the
                                    // conversation; it never takes the drag itself.
                                    #[name = "drop_hint"]
                                    add_overlay = &gtk::Revealer {
                                        set_transition_type: gtk::RevealerTransitionType::Crossfade,
                                        set_can_target: false,
                                        #[wrap(Some)]
                                        set_child = &gtk::Box {
                                            add_css_class: "zaptide-drop-scrim",
                                            append = &adw::Clamp {
                                                set_maximum_size: 400,
                                                set_hexpand: true,
                                                set_valign: gtk::Align::Center,
                                                #[wrap(Some)]
                                                set_child = &adw::StatusPage {
                                                    add_css_class: "compact",
                                                    add_css_class: "zaptide-drop-card",
                                                    set_icon_name: Some("mail-attachment-symbolic"),
                                                    set_title: &gettext("Drop Files to Attach"),
                                                    set_description: Some("Photos and files are added to your message, so you can write a caption before sending"),
                                                },
                                            },
                                        },
                                    },
                                },
                            },
                        },
                    },

                    #[watch]
                    set_visible_child_name: if model.is_linked() { "chats" } else { "link" },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let icon_dir = init.dirs.icon_dir();
        let base_dirs = init.dirs.clone();
        let prepared = crate::account::prepare(
            &base_dirs,
            &crate::settings::Settings::load(&base_dirs.settings_file()),
        );
        let legacy_layout = prepared.legacy;
        let registry = prepared.registry;
        let active_account = registry.active.unwrap_or(crate::account::AccountId::FIRST);
        let active_dirs = if legacy_layout {
            base_dirs.clone()
        } else {
            base_dirs.for_account(active_account)
        };
        if let Err(error) = active_dirs.ensure() {
            log::error!("could not prepare the account folder: {error}");
        }
        let (notifier, drain) = EventNotifier::new();
        let input = sender.clone();
        let event_drain = GlibEventDrain::install(
            &notifier,
            drain,
            gtk::glib::MainContext::default(),
            move || input.input(Input::BackendReady(active_account)),
        );
        let notification_sender = sender.clone();
        let application: gtk::Application = relm4::main_application().upcast();
        let notifications = crate::native_notifications::NativeNotifications::new(
            &application,
            move |account, chat| notification_sender.input(Input::OpenAccountChat(account, chat)),
        );
        let chats: TypedListView<ChatRow, gtk::SingleSelection> = TypedListView::new();
        let chat_targets = std::rc::Rc::new(std::cell::RefCell::new(chats::ChatTargets::default()));
        let chat_view = &chats.view.clone();
        // Nothing is highlighted until a chat is opened or picked with the keys.
        chats.selection_model.set_autoselect(false);
        chats.selection_model.set_can_unselect(true);
        let selection_sender = sender.clone();
        let observed_selection = chats.selection_model.clone();
        chats
            .selection_model
            .connect_selection_changed(move |_, _, _| {
                let position = observed_selection.selected();
                if position != gtk::INVALID_LIST_POSITION {
                    selection_sender.input(Input::HighlightChat(position));
                }
            });
        let chat_keys = gtk::EventControllerKey::new();
        chat_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let chat_selection = chats.selection_model.clone();
        let chat_key_sender = sender.clone();
        let key_targets = chat_targets.clone();
        chat_keys.connect_key_pressed(move |_, key, _, _| {
            let selected = chat_selection.selected();
            let count = chat_selection.n_items();
            match key {
                gtk::gdk::Key::Up if count > 0 => {
                    let current = if selected == gtk::INVALID_LIST_POSITION {
                        count - 1
                    } else {
                        selected.saturating_sub(1)
                    };
                    chat_selection.set_selected(current);
                    chat_key_sender.input(Input::HighlightChat(current));
                    gtk::glib::Propagation::Stop
                }
                gtk::gdk::Key::Down if count > 0 => {
                    let current = if selected == gtk::INVALID_LIST_POSITION {
                        0
                    } else {
                        (selected + 1).min(count - 1)
                    };
                    chat_selection.set_selected(current);
                    chat_key_sender.input(Input::HighlightChat(current));
                    gtk::glib::Propagation::Stop
                }
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter if count > 0 => {
                    let current = if selected == gtk::INVALID_LIST_POSITION {
                        0
                    } else {
                        selected.min(count - 1)
                    };
                    if let Some((account, chat)) = key_targets.borrow().get(current) {
                        chat_key_sender.input(Input::SelectChat(account, chat));
                    }
                    gtk::glib::Propagation::Stop
                }
                _ => gtk::glib::Propagation::Proceed,
            }
        });
        chat_view.add_controller(chat_keys);
        // Single-click activation makes GTK select whichever row the pointer
        // last crossed, leaving a second highlight beside the open chat.
        // Selection follows the open chat and the arrow keys only.
        chat_view.action_set_enabled("list.select-item", false);
        let messages: TypedListView<MessageRow, gtk::NoSelection> = TypedListView::new();
        let composer_buffer = gtk::TextBuffer::new(None);
        let enter_sends = std::rc::Rc::new(std::cell::Cell::new(true));
        let settings_path = init.dirs.settings_file();
        let mut settings = crate::settings::Settings::load(&settings_path);
        if !legacy_layout {
            crate::account::AccountSettings::load_or(
                &active_dirs.account_settings_file(),
                &settings,
            )
            .apply_to(&mut settings);
        }
        enter_sends.set(settings.enter_sends);
        let mut theme_catalog = crate::theme::custom::Catalog::default();
        theme_catalog.enable_desktop_themes();
        theme_catalog.start(
            settings_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("themes"),
            settings.custom_theme.clone(),
            &notifier,
        );
        let mut media = crate::services::media::MediaService::default();
        media.set_speed(settings.voice_speed);
        let (backend, page_title, status): (Option<Backend>, String, String) =
            match Backend::try_spawn(active_dirs.clone(), notifier.clone()) {
                Ok(backend) => (
                    Some(backend),
                    "Starting ZapTide".into(),
                    "Waiting for first native frame".into(),
                ),
                Err(_) => {
                    log::error!("native backend could not start");
                    (
                        None,
                        "ZapTide needs attention".into(),
                        "Backend unavailable".into(),
                    )
                }
            };
        let link_menu = gtk::gio::Menu::new();
        link_menu.append(Some(&gettext("_Remove Account…")), Some("win.unlink"));
        link_menu.append(Some(&gettext("_Preferences")), Some("win.preferences"));
        link_menu.append(Some(&gettext("_About ZapTide")), Some("win.about"));
        link_menu.append(Some(&gettext("_Quit")), Some("win.quit"));
        let (switch_sender, add_sender, profile_sender) =
            (sender.clone(), sender.clone(), sender.clone());
        let link_switcher = account_switcher::AccountSwitcher::new(
            move |account| switch_sender.input(Input::SwitchAccount(account)),
            move || add_sender.input(Input::AddAccount),
            move || profile_sender.input(Input::ShowProfile),
        );
        link_switcher.button.set_visible(false);
        let link_page = LinkPage::builder()
            .launch(LinkPageInit {
                account_button: link_switcher.button.clone(),
                state: LinkPageState {
                    link: LinkStatus::Starting,
                    title: page_title.clone(),
                    status: status.clone(),
                    qr_texture: None,
                    phone_linking: false,
                    busy: true,
                    can_cancel: false,
                },
                menu: link_menu,
            })
            .forward(sender.input_sender(), |output| match output {
                LinkPageOutput::PairWithPhone(phone) => Input::PairWithPhone(phone),
                LinkPageOutput::TogglePhoneLinking => Input::TogglePhoneLinking,
                LinkPageOutput::CopyPairCode => Input::CopyPairCode,
                LinkPageOutput::Reconnect => Input::Reconnect,
                LinkPageOutput::Cancel => Input::CancelAddAccount,
            });
        let primary_menu = gtk::gio::Menu::new();
        let section = gtk::gio::Menu::new();
        section.append(Some(&gettext("_New Chat")), Some("win.new-chat"));
        section.append(Some(&gettext("_Remove Account…")), Some("win.unlink"));
        primary_menu.append_section(None, &section);
        let section = gtk::gio::Menu::new();
        section.append(Some(&gettext("_Preferences")), Some("win.preferences"));
        section.append(Some(&gettext("_Keyboard Shortcuts")), Some("win.shortcuts"));
        section.append(Some(&gettext("_About ZapTide")), Some("win.about"));
        section.append(Some(&gettext("_Quit")), Some("win.quit"));
        primary_menu.append_section(None, &section);
        let (switch_sender, add_sender, profile_sender) =
            (sender.clone(), sender.clone(), sender.clone());
        let account_switcher = account_switcher::AccountSwitcher::new(
            move |account| switch_sender.input(Input::SwitchAccount(account)),
            move || add_sender.input(Input::AddAccount),
            move || profile_sender.input(Input::ShowProfile),
        );
        let sidebar = Sidebar::builder()
            .launch(SidebarInit {
                account_button: account_switcher.button.clone(),
                state: SidebarState::default(),
                chat_view: chats.view.clone(),
                chat_targets: chat_targets.clone(),
                menu: primary_menu.clone(),
            })
            .forward(sender.input_sender(), |output| match output {
                SidebarOutput::SearchChats(query) => Input::SearchChats(query),
                SidebarOutput::SetUnreadFilter(active) => Input::SetUnreadFilter(active),
                SidebarOutput::SetPinnedFilter(active) => Input::SetPinnedFilter(active),
                SidebarOutput::SetChatKindFilter(kind) => Input::SetChatKindFilter(kind),
                SidebarOutput::SetArchivedFilter(active) => Input::SetArchivedFilter(active),
                SidebarOutput::SetMutedFilter(active) => Input::SetMutedFilter(active),
                SidebarOutput::SelectChat(account, chat) => Input::SelectChat(account, chat),
                SidebarOutput::NewChat => Input::ShowNewChat,
            });
        let transcript_view = TranscriptView::builder()
            .launch(TranscriptViewInit {
                state: TranscriptState {
                    active: false,
                    has_messages: false,
                    history_complete: false,
                    loading_older: false,
                    recent_messages_pending: false,
                },
                message_view: messages.view.clone(),
            })
            .forward(sender.input_sender(), |output| match output {
                TranscriptViewOutput::LoadOlder => Input::LoadOlder,
                TranscriptViewOutput::SelectMessage(position) => Input::SelectMessage(position),
                TranscriptViewOutput::AtEnd => Input::TranscriptAtEnd,
                TranscriptViewOutput::ScrollToRecentMessages => Input::ScrollToRecentMessages,
            });
        let recording_meter = crate::native_media_widgets::RecordingMeter::default();
        let composer_view = ComposerView::builder()
            .launch(ComposerViewInit {
                state: ComposerState::default(),
                buffer: composer_buffer.clone(),
                enter_sends: enter_sends.clone(),
                recording_meter_area: recording_meter.area.clone(),
            })
            .forward(sender.input_sender(), |output| match output {
                ComposerViewOutput::DraftChanged(text) => Input::DraftChanged(text),
                ComposerViewOutput::SendText(text) => Input::SendText(text),
                ComposerViewOutput::PickAttachments { gallery } => {
                    Input::PickAttachments { gallery }
                }
                ComposerViewOutput::ClearAttachments => Input::ClearAttachments,
                ComposerViewOutput::RemoveAttachment(path) => Input::RemoveAttachment(path),
                ComposerViewOutput::CancelReply => Input::CancelReply,
                ComposerViewOutput::CancelEdit => Input::CancelEdit,
                ComposerViewOutput::Recording(intent) => Input::Recording(intent),
                ComposerViewOutput::InsertMention => Input::InsertMention,
                ComposerViewOutput::SelectMention(candidate) => Input::SelectMention(candidate),
                ComposerViewOutput::ShowStickerPicker => Input::ShowStickerPicker,
                ComposerViewOutput::ShowPollCreator => Input::ShowPollCreator,
                ComposerViewOutput::ShowContactPicker => Input::ShowContactPicker,
                ComposerViewOutput::InsertEmoji(emoji) => Input::InsertEmoji(emoji),
            });
        let mut model = Self {
            window: root.clone(),
            backend,
            notifications,
            notifier,
            active_drain: Some(event_drain),
            accounts: std::collections::HashMap::new(),
            active_account,
            active_dirs,
            base_dirs,
            registry,
            legacy_layout,
            account_switcher,
            link_switcher,
            pending_account: None,
            removing: None,
            shutdown_started: false,
            chats,
            chat_projection: crate::native_chat_list::ChatListProjection::default(),
            chat_filters: crate::native_chat_list::ChatListFilters::default(),
            chat_ids: Vec::new(),
            chat_targets,
            chat_snapshots: Vec::new(),
            contacts: std::collections::HashMap::new(),
            contact_share_generation: 0,
            chat_menu: gtk::gio::Menu::new(),
            chat_menu_state: None,
            tray: None,
            tray_state: crate::native_tray::TrayState::default(),
            tray_shown: false,
            avatars: std::collections::HashMap::new(),
            avatar_requests: std::collections::HashSet::new(),
            typing: std::collections::HashMap::new(),
            typing_until: std::collections::HashMap::new(),
            composing_until: std::collections::HashMap::new(),
            presence: std::collections::HashMap::new(),
            messages,
            qr_texture: None,
            link_page,
            transcript_view,
            composer_view,
            history_complete: false,
            loading_older: false,
            message_ids: Vec::new(),
            message_snapshots: std::collections::HashMap::new(),
            pending_quote_navigation: None,
            pointer_sender: sender.clone(),
            editable_messages: std::collections::HashMap::new(),
            transcript: Vec::new(),
            audio: AudioState {
                selected_voice: None,
                selected_voice_message: None,
                media,
                audio_waveforms: Default::default(),
                waveform_queue: Default::default(),
                waveform_busy: false,
                waveform_cancel: None,
                waveform_attempted: Default::default(),
                playing_audio: None,
                audio_errors: Default::default(),
                audio_registry: Default::default(),
                voice_send_pending: false,
                recording_meter,
                played_voice: Default::default(),
            },
            info_about: None,
            message_target: None,
            opened_unread: 0,
            unread_marker: None,
            recent_messages_pending: false,
            message_selection: None,
            message_menu: gtk::PopoverMenu::from_model(None::<&gtk::gio::MenuModel>),
            poll_choice: 0,
            sticker_packs: Vec::new(),
            recent_stickers: Vec::new(),
            favorite_stickers: Vec::new(),
            sticker_emojis: std::collections::HashMap::new(),
            active_chat: None,
            draft: String::new(),
            drafts: std::collections::HashMap::new(),
            composer: crate::native_composer::NativeComposerState::default(),
            composer_buffer,
            sticker_picker: None,
            pending_composer_request: None,
            pending_attachments: std::collections::HashMap::new(),
            document_attachments: std::collections::HashSet::new(),
            pending_clipboard_images: std::collections::HashMap::new(),
            pending_send: None,
            pending_edit: None,
            reply_to: None,
            editing: None,
            account_receipts_off: false,
            link: LinkStatus::Starting,
            syncing: false,
            resuming: false,
            resume_dropped: false,
            theme_catalog,
            page_title,
            status,
            settings,
            settings_path,
            portals: crate::native_portals::NativePortals::default(),
            portal_requests: std::rc::Rc::default(),
            preferences: None,
            profile_dialog: None,
            sidebar,
            sidebar_visible: true,
            split_view: None,
            phone_linking: false,
            chats_dirty: false,
            search_resets: 0,
            chats_flush_scheduled: false,
            zoom_provider: gtk::CssProvider::new(),
            custom_theme_provider: gtk::CssProvider::new(),
            enter_sends,
        };
        model.spawn_hidden_accounts(&sender);
        install_window_actions(&root, &sender);
        install_icons(&icon_dir);
        let (tray_actions, tray_receiver) = relm4::channel();
        let tray_sender = sender.clone();
        gtk::glib::spawn_future_local(async move {
            while let Some(action) = tray_receiver.recv().await {
                tray_sender.input(Input::Tray(action));
            }
        });
        model.tray = crate::native_tray::TrayHandle::spawn(
            &icon_dir,
            model.tray_state.clone(),
            tray_actions,
        );
        model.tray_shown = model.tray.is_some();
        // The tray icon follows the theme; any input ends in `sync_tray`.
        let scheme_sender = sender.clone();
        adw::StyleManager::default()
            .connect_dark_notify(move |_| scheme_sender.input(Input::WindowVisibilityChanged));
        let visibility_sender = sender.clone();
        root.connect_visible_notify(move |_| {
            visibility_sender.input(Input::WindowVisibilityChanged)
        });
        let widgets = view_output!();
        widgets
            .page_stack
            .set_visible_child_name(if model.is_linked() { "chats" } else { "link" });
        model.message_menu.set_parent(&widgets.conversation_body);
        model.message_menu.set_has_arrow(false);
        model.message_menu.set_halign(gtk::Align::Start);
        install_message_actions(&root, &sender);
        let selection_keys = gtk::EventControllerKey::new();
        let selection_sender = sender.clone();
        selection_keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                selection_sender.input(Input::CancelSelection);
            }
            gtk::glib::Propagation::Proceed
        });
        widgets.conversation_body.add_controller(selection_keys);
        widgets
            .conversation_title
            .connect_notify_local(Some("subtitle"), |title, _| {
                let text = title.subtitle();
                if !text.is_empty() {
                    title
                        .upcast_ref::<gtk::Widget>()
                        .announce(&text, gtk::AccessibleAnnouncementPriority::Medium);
                }
            });
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            640.0,
            adw::LengthUnit::Sp,
        ));
        breakpoint.add_setter(
            &widgets.main_split_view,
            "collapsed",
            Some(&true.to_value()),
        );
        root.add_breakpoint(breakpoint);
        model.split_view = Some(widgets.main_split_view.clone());
        // Collapsed, start on the chat list.
        model.sidebar_visible = true;
        let split_sender = sender.clone();
        widgets
            .main_split_view
            .connect_notify_local(Some("collapsed"), move |split, _| {
                split_sender.input(Input::SplitCollapsed(split.is_collapsed()));
            });
        // The back button changes the page without telling the model; the next
        // view update would then undo it.
        let shown_sender = sender.clone();
        widgets
            .main_split_view
            .connect_show_content_notify(move |split| {
                shown_sender.input(Input::SidebarShown(!split.shows_content()));
            });
        model.sync_translucent_sidebar();
        // The whole conversation accepts dropped files, not only the composer.
        // Capture runs before the text view's own drop handling.
        let (mut last_wall, mut last_monotonic) =
            (std::time::SystemTime::now(), std::time::Instant::now());
        let resume_sender = sender.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_secs(5), move || {
            let (wall, monotonic) = (std::time::SystemTime::now(), std::time::Instant::now());
            if slept_through(
                wall.duration_since(last_wall).unwrap_or_default(),
                monotonic.duration_since(last_monotonic),
            ) {
                resume_sender.input(Input::Resumed);
            }
            (last_wall, last_monotonic) = (wall, monotonic);
            gtk::glib::ControlFlow::Continue
        });
        let drop_target = gtk::DropTarget::new(
            gtk::gdk::FileList::static_type(),
            gtk::gdk::DragAction::COPY,
        );
        drop_target.set_propagation_phase(gtk::PropagationPhase::Capture);
        let drop_hint = widgets.drop_hint.clone();
        drop_target.connect_enter(move |_, _, _| {
            drop_hint.set_reveal_child(true);
            gtk::gdk::DragAction::COPY
        });
        let drop_hint = widgets.drop_hint.clone();
        drop_target.connect_leave(move |_| drop_hint.set_reveal_child(false));
        let drop_hint = widgets.drop_hint.clone();
        let drop_sender = sender.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            drop_hint.set_reveal_child(false);
            let Ok(files) = value.get::<gtk::gdk::FileList>() else {
                return false;
            };
            let paths: Vec<_> = files
                .files()
                .into_iter()
                .filter_map(|file| file.path())
                .collect();
            if paths.is_empty() {
                return false;
            }
            drop_sender.input(Input::AttachDropped(paths));
            true
        });
        widgets.conversation_body.add_controller(drop_target);
        // Ctrl+V of copied files or of an image attaches them. Text copied
        // from office apps also carries a picture of itself, so an image
        // offered alongside text pastes as text.
        let paste_sender = sender.clone();
        model
            .composer_view
            .model()
            .text_view()
            .connect_paste_clipboard(move |view| {
                let clipboard = view.clipboard();
                let formats = clipboard.formats();
                if formats.contains_type(gtk::gdk::FileList::static_type()) {
                    view.stop_signal_emission_by_name("paste-clipboard");
                    let input = paste_sender.clone();
                    gtk::glib::spawn_future_local(async move {
                        let Ok(value) = clipboard
                            .read_value_future(
                                gtk::gdk::FileList::static_type(),
                                gtk::glib::Priority::DEFAULT,
                            )
                            .await
                        else {
                            return;
                        };
                        let paths: Vec<_> = value
                            .get::<gtk::gdk::FileList>()
                            .map(|files| files.files())
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|file| file.path())
                            .collect();
                        if !paths.is_empty() {
                            input.input(Input::AttachDropped(paths));
                        }
                    });
                } else if formats.contains_type(gtk::gdk::Texture::static_type())
                    && !formats.contains_type(String::static_type())
                {
                    view.stop_signal_emission_by_name("paste-clipboard");
                    paste_sender.input(Input::PasteClipboardImage);
                }
            });
        model.install_zoom_provider();
        model.apply_runtime_settings();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, input: Self::Input, sender: ComponentSender<Self>) {
        let draft_empty_before = match &input {
            Input::DraftChanged(text) => {
                Some((self.draft.trim().is_empty(), text.trim().is_empty()))
            }
            _ => None,
        };
        let draft_changed = matches!(&input, Input::DraftChanged(_));
        self.handle_input(input, sender);
        if draft_changed {
            self.sync_composer_view();
        } else {
            self.sync_components(draft_empty_before);
        }
        self.sync_chat_menu();
        self.sync_tray();
    }
}

fn apply_theme(settings: &crate::settings::Settings, theme_provider: &gtk::CssProvider) {
    let palette = settings.cached_palette();
    let scheme = palette.map_or_else(
        || match settings.theme {
            crate::settings::ThemeChoice::Dark => adw::ColorScheme::ForceDark,
            crate::settings::ThemeChoice::Light => adw::ColorScheme::ForceLight,
            crate::settings::ThemeChoice::System => adw::ColorScheme::Default,
        },
        |palette| {
            if palette.dark {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::ForceLight
            }
        },
    );
    adw::StyleManager::default().set_color_scheme(scheme);
    let mut css = include_str!("../data/theme-defaults.css").to_owned();
    if let Some(palette) = palette {
        css.push_str(&crate::native_theme::css_for_palette(&palette));
    }
    css.push_str(include_str!("../data/style.css"));
    theme_provider.load_from_string(&css);
}

impl NativeApplication {
    fn header_subtitle(&self) -> String {
        let Some(chat) = &self.active_chat else {
            return String::new();
        };
        if let Some((name, _)) = self.typing.get(chat) {
            return format!("{name} is typing…");
        }
        match self.presence.get(chat) {
            Some((true, _)) => "Online".into(),
            Some((false, Some(at))) => format!("Last seen {}", crate::util::moment_stamp(*at)),
            _ => self.status.clone(),
        }
    }

    fn typing_label(&self) -> String {
        self.active_chat
            .as_ref()
            .and_then(|chat| self.typing.get(chat))
            .map(|(name, _)| format!("{name} is typing"))
            .unwrap_or_else(|| "Participant is typing".into())
    }

    fn typing_name(&self) -> String {
        self.active_chat
            .as_ref()
            .and_then(|chat| self.typing.get(chat))
            .map(|(name, _)| name.clone())
            .unwrap_or_default()
    }

    fn typing_avatar(&self) -> Option<gtk::gdk::Texture> {
        let (_, sender) = self.typing.get(self.active_chat.as_ref()?)?;
        dialogs::cached_texture(self.avatars.get(sender)?)
    }

    fn focus_composer(&self) {
        self.composer_view.model().focus_text_view();
    }

    fn themes_directory(&self) -> std::path::PathBuf {
        self.settings_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("themes")
    }

    fn theme_choice_filenames(&self) -> Vec<String> {
        self.theme_catalog
            .picker_themes()
            .map(|theme| theme.filename.clone())
            .collect()
    }

    fn sync_custom_theme_selection(&mut self) {
        if let Some(filename) = self.settings.custom_theme.clone() {
            if let Some(theme) = self.theme_catalog.find(&filename).cloned() {
                self.settings.custom_theme_cache = Some(theme);
            } else {
                self.theme_catalog
                    .start(self.themes_directory(), Some(filename), &self.notifier);
            }
        } else {
            self.settings.custom_theme_cache = None;
        }
        self.apply_runtime_settings();
    }

    fn poll_theme_catalog(&mut self) {
        if self.theme_catalog.needs_reload() {
            self.theme_catalog.start(
                self.themes_directory(),
                self.settings.custom_theme.clone(),
                &self.notifier,
            );
        }
        if !self.theme_catalog.poll() {
            return;
        }

        let previous_palette = self.settings.cached_palette();
        if let Some(filename) = self.settings.custom_theme.as_deref()
            && let Some(theme) = self.theme_catalog.find(filename).cloned()
        {
            self.settings.custom_theme_cache = Some(theme);
        }

        if self.settings.cached_palette() != previous_palette {
            self.apply_runtime_settings();
            if self.save_settings().is_err() {
                self.status = gettext("Could not save theme preference");
            }
        }
        let choices = self.theme_choice_filenames();
        let selected = self.settings.custom_theme.clone();
        if let Some(dialog) = self.preferences.as_mut() {
            dialog.set_custom_theme_choices(&choices, selected.as_deref());
        }
    }

    fn install_zoom_provider(&self) {
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &self.zoom_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            gtk::style_context_add_provider_for_display(
                &display,
                &self.custom_theme_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
    }

    fn apply_runtime_settings(&mut self) {
        apply_theme(&self.settings, &self.custom_theme_provider);
        self.zoom_provider.load_from_string(&format!(
            "window {{ font-size: {:.0}%; }}",
            100.0 * self.settings.zoom
        ));
        self.enter_sends.set(self.settings.enter_sends);
        self.audio.media.set_speed(self.settings.voice_speed);
        self.sync_translucent_sidebar();
    }

    pub(super) fn sync_translucent_sidebar(&self) {
        let collapsed = self
            .split_view
            .as_ref()
            .is_some_and(adw::NavigationSplitView::is_collapsed);
        let active = translucent_sidebar_active(&self.settings, self.is_linked(), collapsed);
        if active {
            self.window.add_css_class("translucent-sidebar");
        } else {
            self.window.remove_css_class("translucent-sidebar");
        }
    }

    fn cancel_portal_requests(&mut self) {
        let ids: Vec<_> = self.portal_requests.borrow_mut().drain().collect();
        for id in ids {
            self.portals.cancel(id);
        }
    }

    /// Scrolls to a message and selects it: the row takes focus, with its
    /// ring forced visible, and becomes the target of reply/copy actions.
    fn scroll_message_into_view(&mut self, id: &str) {
        let Some(position) = self.message_ids.iter().position(|message| message == id) else {
            return;
        };
        self.message_target = Some(id.to_owned());
        let view = self.messages.view.clone();
        let id = id.to_owned();
        gtk::glib::idle_add_local_once(move || {
            if let Some(window) = view.root().and_downcast::<gtk::Window>() {
                window.set_focus_visible(true);
            }
            view.scroll_to(position as u32, gtk::ListScrollFlags::FOCUS, None);
            // The row may not exist until the scroll lands; each row's root is
            // named after its message id.
            let id = id.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                let mut child = view.first_child();
                while let Some(item) = child {
                    if let Some(root) = item.first_child()
                        && root.widget_name() == id
                    {
                        root.add_css_class("zaptide-message-flash");
                        gtk::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(1800),
                            move || root.remove_css_class("zaptide-message-flash"),
                        );
                        return;
                    }
                    child = item.next_sibling();
                }
            });
        });
    }

    fn show_message_menu(
        &mut self,
        id: String,
        x: f32,
        y: f32,
        sender: &ComponentSender<NativeApplication>,
    ) {
        // Selection mode acts on the picked set, not one message.
        if self.message_selection.is_some() {
            return;
        }
        if !self.message_ids.contains(&id) {
            return;
        }
        let Some(parent) = self.message_menu.parent() else {
            return;
        };
        let Some(point) = self
            .messages
            .view
            .compute_point(&parent, &gtk::graphene::Point::new(x, y))
        else {
            return;
        };
        self.message_target = Some(id);
        let rect = gtk::gdk::Rectangle::new(point.x() as i32, point.y() as i32, 1, 1);
        self.message_menu
            .set_menu_model(Some(&self.message_menu_model()));
        let current = self.selected_message().and_then(|message| {
            message
                .reactions
                .iter()
                .find(|reaction| reaction.from_me)
                .map(|reaction| reaction.emoji.clone())
        });
        self.message_menu.add_child(
            &reaction_bar(&self.message_menu, &parent, rect, current, sender),
            "reactions",
        );
        self.message_menu.set_pointing_to(Some(&rect));
        self.message_menu.popup();
    }

    /// Actions for the right-clicked message, grouped as GNOME menus are.
    fn message_menu_model(&self) -> gtk::gio::Menu {
        let menu = gtk::gio::Menu::new();
        let Some(message) = self.selected_message() else {
            return menu;
        };
        let open = gtk::gio::Menu::new();
        if self.attachment_action_available() {
            let downloaded = message
                .content
                .media()
                .is_some_and(|media| media.path.is_some());
            open.append(
                Some(if downloaded {
                    "Open Attachment"
                } else {
                    "Download Attachment"
                }),
                Some("message.attachment"),
            );
            if downloaded {
                open.append(Some(&gettext("Show in Folder")), Some("message.folder"));
                open.append(Some(&gettext("Save Attachment…")), Some("message.save"));
            }
        }
        if matches!(
            &message.content,
            crate::model::Content::Text {
                preview: Some(_),
                ..
            }
        ) {
            open.append(Some(&gettext("Open Link")), Some("message.open-link"));
        }
        if message.quoted.is_some() {
            open.append(
                Some(&gettext("Go to Quoted Message")),
                Some("message.quoted"),
            );
        }
        let reactions = gtk::gio::Menu::new();
        let item = gtk::gio::MenuItem::new(None, None);
        item.set_attribute_value("custom", Some(&"reactions".to_variant()));
        reactions.append_item(&item);
        let respond = gtk::gio::Menu::new();
        if message_text(message).is_some() {
            respond.append(Some(&gettext("Copy Text")), Some("message.copy"));
        }
        respond.append(Some(&gettext("Reply")), Some("message.reply"));
        if self.editable_messages.contains_key(&message.id) {
            respond.append(Some(&gettext("Edit")), Some("message.edit"));
        }
        respond.append(Some(&gettext("Forward…")), Some("message.forward"));
        respond.append(Some(&gettext("Select")), Some("message.select"));
        let vote = gtk::gio::Menu::new();
        if self.selected_poll_can_vote() {
            for (index, option) in self
                .selected_poll_options()
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                let item = gtk::gio::MenuItem::new(Some(&format!("Vote: {option}")), None);
                item.set_action_and_target_value(
                    Some("message.vote"),
                    Some(&(index as u32).to_variant()),
                );
                vote.append_item(&item);
            }
        }
        let delete = gtk::gio::Menu::new();
        delete.append(Some(&gettext("Delete for Me")), Some("message.delete"));
        if message.from_me {
            delete.append(
                Some(&gettext("Delete for Everyone")),
                Some("message.delete-everyone"),
            );
        }
        for section in [reactions, open, respond, vote, delete] {
            if section.n_items() > 0 {
                menu.append_section(None, &section);
            }
        }
        menu
    }

    fn selected_message_id(&self) -> Option<String> {
        self.message_target.clone()
    }

    fn participant_label(&self, id: &str, index: usize) -> String {
        let name = self
            .contacts
            .get(id)
            .and_then(crate::model::Contact::display_name)
            .map(str::to_owned)
            .or_else(|| crate::model::phone_of(id).map(crate::util::phone))
            .unwrap_or_else(|| id.split('@').next().unwrap_or_default().to_owned());
        if name.is_empty() {
            format!("Participant {}", index + 1)
        } else {
            name
        }
    }

    fn selected_message(&self) -> Option<&crate::model::Message> {
        self.selected_message_id()
            .and_then(|id| self.message_snapshots.get(&id))
    }

    fn selected_attachment(&self) -> Option<crate::native_attachments::AttachmentPresentation> {
        self.selected_message()
            .and_then(crate::native_attachments::project)
    }

    fn attachment_action_available(&self) -> bool {
        self.selected_attachment().is_some_and(|attachment| {
            attachment.control != crate::native_attachments::AttachmentControl::Downloading
        })
    }

    fn selected_poll_options(&self) -> Option<&[String]> {
        match &self.selected_message()?.content {
            crate::model::Content::Poll { options, .. } => Some(options),
            _ => None,
        }
    }

    fn selected_poll_can_vote(&self) -> bool {
        matches!(&self.selected_message().map(|message| &message.content),
            Some(crate::model::Content::Poll { state, .. }) if state.can_vote)
    }

    fn recording_active(&self) -> bool {
        self.audio.media.is_recording()
    }

    fn recording_time(&self) -> String {
        self.audio
            .media
            .recording_elapsed()
            .map(|elapsed| crate::util::duration(elapsed.as_secs().min(u64::from(u32::MAX)) as u32))
            .unwrap_or_default()
    }

    /// Opacity of the record dot; it pulses once a second.
    fn recording_blink(&self) -> f64 {
        match self.audio.media.recording_elapsed() {
            Some(elapsed) if elapsed.subsec_millis() >= 500 => 0.3,
            _ => 1.0,
        }
    }

    fn can_send_voice(&self) -> bool {
        !self.recording_active()
            && self.pending_send.is_none()
            && !self.audio.voice_send_pending
            && self
                .active_chat
                .as_deref()
                .and_then(|id| self.chat_snapshots.iter().find(|chat| chat.id == id))
                .is_some_and(crate::model::Chat::can_send)
    }

    fn activate_attachment(&mut self, id: &str) {
        let Some(action) = self
            .message_snapshots
            .get(id)
            .and_then(crate::native_attachments::project)
            .and_then(|attachment| {
                attachment.action(crate::native_attachments::AttachmentIntent::Activate)
            })
        else {
            return;
        };
        match action {
            crate::model::Action::Download { chat, message } => {
                if let Some(media) = self
                    .message_snapshots
                    .get_mut(&message)
                    .and_then(|message| message.content.media_mut())
                {
                    media.state = crate::model::MediaState::Downloading;
                }
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::Download {
                        chat,
                        message: message.clone(),
                    });
                    self.status = gettext("Downloading attachment");
                }
                self.refresh_message_row(&message);
            }
            crate::model::Action::OpenFile(path) => {
                let file = gtk::gio::File::for_path(path);
                if gtk::gio::AppInfo::launch_default_for_uri(
                    &file.uri(),
                    None::<&gtk::gio::AppLaunchContext>,
                )
                .is_err()
                {
                    self.status = gettext("Could not open attachment");
                }
            }
            _ => {}
        }
    }

    fn paste_clipboard_image(&mut self, sender: &ComponentSender<Self>) {
        let Some(chat) = self.active_chat.clone().filter(|_| self.can_attach()) else {
            return;
        };
        if !self.portal_requests.borrow().is_empty() {
            self.status = gettext("Finish the active portal action first");
            return;
        }
        let Some(display) = gtk::gdk::Display::default() else {
            self.status = gettext("Clipboard is unavailable");
            return;
        };
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        let request = self
            .portals
            .read_clipboard_image(&display.clipboard(), move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                let pixels = result
                    .map_err(|_| "Clipboard image is unavailable".to_owned())
                    .and_then(|texture| {
                        let Some(texture) = texture else {
                            return Err("Clipboard does not contain an image".into());
                        };
                        let width = texture.width() as u32;
                        let height = texture.height() as u32;
                        let Some(length) = (width as usize)
                            .checked_mul(height as usize)
                            .and_then(|pixels| pixels.checked_mul(4))
                        else {
                            return Err("Clipboard image is too large".into());
                        };
                        if width == 0
                            || height == 0
                            || u64::from(width) * u64::from(height) > 16_777_216
                        {
                            return Err("Clipboard image is too large".into());
                        }
                        let mut rgba = vec![0; length];
                        texture.download(&mut rgba, width as usize * 4);
                        convert_premultiplied_bgra_to_rgba(&mut rgba);
                        Ok(ClipboardPixels {
                            width,
                            height,
                            preview: gtk::gdk::MemoryTexture::new(
                                width as i32,
                                height as i32,
                                gtk::gdk::MemoryFormat::R8g8b8a8,
                                &gtk::glib::Bytes::from_owned(rgba.clone()),
                                width as usize * 4,
                            )
                            .into(),
                            rgba,
                        })
                    });
                match pixels {
                    Ok(pixels) => input.input(Input::ClipboardImageReady { chat, pixels }),
                    Err(error) => input.input(Input::PortalActionFinished(error)),
                }
            });
        if let Some(id) = request {
            request_id.set(Some(id));
            self.portal_requests.borrow_mut().insert(id);
            self.status = gettext("Reading clipboard image");
        } else {
            self.status = gettext("Could not read clipboard image");
        }
    }

    fn stage_clipboard_image(&mut self, chat: String, pixels: ClipboardPixels) {
        if self.active_chat.as_deref() != Some(&chat) || !self.can_attach() {
            self.status = gettext("Choose an available conversation before pasting an image");
            return;
        }
        if self.pending_send.is_some()
            || self.audio.voice_send_pending
            || self
                .pending_attachments
                .get(&chat)
                .is_some_and(|paths| !paths.is_empty())
            || self
                .pending_clipboard_images
                .keys()
                .any(|staged_chat| staged_chat != &chat)
        {
            self.status = gettext("Send or clear the staged attachment before pasting an image");
            return;
        }
        self.pending_clipboard_images.insert(chat, pixels);
        self.status = gettext("Clipboard image ready");
    }

    fn open_selected_uri(&mut self, sender: &ComponentSender<Self>) {
        let Some(uri) = self
            .selected_message()
            .and_then(|message| match &message.content {
                crate::model::Content::Text {
                    preview: Some(preview),
                    ..
                } => Some(preview.url.clone()),
                _ => None,
            })
        else {
            return;
        };
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        match self.portals.open_uri(
            crate::native_portals::UserAction::for_explicit_user_action(),
            &uri,
            move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                input.input(Input::PortalUriFinished(result.is_ok()));
            },
        ) {
            Ok(id) => {
                request_id.set(Some(id));
                self.portal_requests.borrow_mut().insert(id);
                self.status = gettext("Opening link");
            }
            Err(_) => self.status = gettext("Link is not supported"),
        }
    }

    fn prepare_themes_folder(&mut self, sender: &ComponentSender<Self>) {
        let Some(config_dir) = self.settings_path.parent() else {
            self.status = gettext("Could not open themes folder");
            self.toast(&gettext("Could not open themes folder"));
            return;
        };
        let folder = config_dir.join("themes");
        let sender = sender.clone();
        std::thread::spawn(move || {
            let success = std::fs::create_dir_all(folder).is_ok();
            sender.input(Input::ThemesFolderPrepared(success));
        });
        self.status = gettext("Preparing themes folder");
    }

    fn launch_themes_folder(&mut self, sender: &ComponentSender<Self>) {
        let Some(config_dir) = self.settings_path.parent() else {
            self.status = gettext("Could not open themes folder");
            self.toast(&gettext("Could not open themes folder"));
            return;
        };
        let folder = config_dir.join("themes");
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        match self.portals.open_folder(
            crate::native_portals::UserAction::for_explicit_user_action(),
            &folder,
            move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                input.input(Input::ThemesFolderFinished(result.is_ok()));
            },
        ) {
            Ok(id) => {
                request_id.set(Some(id));
                self.portal_requests.borrow_mut().insert(id);
                self.status = gettext("Opening themes folder");
            }
            Err(_) => {
                self.status = gettext("Could not open themes folder");
                self.toast(&gettext("Could not open themes folder"));
            }
        }
    }

    fn save_selected_attachment(&mut self, sender: &ComponentSender<Self>) {
        let Some(path) = self
            .selected_message()
            .and_then(|message| message.content.media())
            .and_then(|media| media.path.clone())
        else {
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let callback_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let id_slot = callback_id.clone();
        let source = gtk::gio::File::for_path(path);
        let request = self.portals.save_copy(
            Some(&self.window),
            &source,
            "Save attachment",
            &name,
            move |result| {
                if let Some(id) = id_slot.get() {
                    requests.borrow_mut().remove(&id);
                }
                input.input(Input::SaveAttachmentFinished(result.is_ok()));
            },
        );
        if let Some(id) = request {
            callback_id.set(Some(id));
            self.portal_requests.borrow_mut().insert(id);
            self.status = gettext("Choose save location");
        } else {
            self.status = gettext("Could not open save dialog");
        }
    }

    fn refresh_message_row(&mut self, id: &str) {
        if self.message_ids.iter().any(|known| known == id) {
            self.rebuild_message_rows(false);
        }
    }

    fn react_selected(&mut self, emoji: String) {
        let Some((chat, message)) = self.active_chat.clone().zip(self.selected_message_id()) else {
            return;
        };
        let action = crate::native_actions::react(chat, message, emoji);
        if let crate::model::Action::React {
            chat,
            message,
            emoji,
        } = action
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::React {
                chat,
                message,
                emoji,
            });
        }
    }

    /// Picked rows, counting an album once.
    fn selected_rows(&self) -> usize {
        (0..self.messages.len())
            .filter_map(|position| self.messages.get(position))
            .filter(|item| {
                let row = item.borrow();
                row.selected == Some(true) && !row.collapsed
            })
            .count()
    }

    /// Picks or drops the message at `position`; an album goes as a whole.
    fn toggle_message_selection(&mut self, position: u32) {
        let Some(item) = self.messages.get(position) else {
            return;
        };
        let ids: Vec<String> = {
            let row = item.borrow();
            if row.album.is_empty() {
                vec![row.id.clone()]
            } else {
                row.album.iter().map(|message| message.id.clone()).collect()
            }
        };
        let Some(selection) = self.message_selection.as_mut() else {
            return;
        };
        if selection.contains(&ids[0]) {
            selection.retain(|id| !ids.contains(id));
        } else {
            selection.extend(ids);
            let order = &self.message_ids;
            selection.sort_by_cached_key(|id| order.iter().position(|known| known == id));
        }
        self.rebuild_message_rows(false);
    }

    fn forward_selected(&mut self, destinations: Vec<String>) {
        let Some(source) = self.active_chat.clone() else {
            return;
        };
        let messages = match self.message_selection.as_ref() {
            Some(selection) => selection.clone(),
            None => self.selected_message_id().into_iter().collect(),
        };
        if messages.is_empty() {
            return;
        }
        let destinations: Vec<_> = destinations
            .into_iter()
            .filter(|destination| {
                *destination != source
                    && self
                        .chat_snapshots
                        .iter()
                        .any(|chat| chat.id == *destination && !chat.archived && !chat.locked)
            })
            .collect();
        if destinations.is_empty() {
            self.status = gettext("Choose another available conversation");
            return;
        }
        let Some(backend) = &self.backend else {
            return;
        };
        for destination in &destinations {
            for message in &messages {
                if let crate::model::Action::Forward {
                    from_chat,
                    message,
                    to_chat,
                } = crate::native_actions::forward(
                    source.clone(),
                    message.clone(),
                    destination.clone(),
                ) {
                    backend.send(crate::backend::Command::Forward {
                        from_chat,
                        message,
                        to_chat,
                    });
                }
            }
        }
        let rows = if self.message_selection.is_some() {
            self.selected_rows()
        } else {
            1
        };
        let what = match rows {
            1 => "message".to_owned(),
            count => format!("{count} messages"),
        };
        self.status = match destinations.len() {
            1 => format!("Forwarding {what}"),
            chats => format!("Forwarding {what} to {chats} chats"),
        };
        if self.message_selection.take().is_some() {
            self.rebuild_message_rows(false);
        }
    }

    fn delete_selected(&mut self, everyone: bool) {
        let Some(message) = self.selected_message().cloned() else {
            return;
        };
        let action = if everyone {
            if !message.from_me {
                return;
            }
            crate::native_actions::delete_for_everyone(message.chat.clone(), message.id.clone())
        } else {
            crate::native_actions::delete_for_me(message.chat.clone(), message.id.clone())
        };
        if let Some(backend) = &self.backend {
            match action {
                crate::model::Action::DeleteForEveryone { chat, message } => {
                    backend.send(crate::backend::Command::Revoke { chat, id: message });
                    self.status = gettext("Deleting message for everyone");
                }
                crate::model::Action::DeleteForMe { chat, message } => {
                    backend.send(crate::backend::Command::DeleteLocal { chat, id: message });
                    self.status = gettext("Deleting message");
                }
                _ => {}
            }
        }
    }

    fn vote_selected(&mut self) {
        let Some(message) = self.selected_message() else {
            return;
        };
        let crate::model::Content::Poll { options, state, .. } = &message.content else {
            return;
        };
        let choice = self.poll_choice;
        if choice >= options.len() || !state.can_vote {
            return;
        }
        let action = crate::native_actions::vote_poll(
            message.chat.clone(),
            message.id.clone(),
            vec![choice],
        );
        if let crate::model::Action::VotePoll {
            chat,
            message,
            choices,
        } = action
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::VotePoll {
                chat,
                message,
                choices,
            });
        }
    }

    fn create_poll(&mut self, draft: crate::model::PollDraft) {
        let Some(chat) = self.active_chat.clone() else {
            return;
        };
        let Ok(draft) = draft.validated() else {
            self.status = gettext("Enter a question and two different poll options");
            return;
        };
        let action = crate::native_actions::create_poll(chat, draft);
        if let crate::model::Action::CreatePoll { chat, draft } = action
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::CreatePoll { chat, draft });
            self.status = gettext("Creating poll");
        }
    }

    fn request_avatar(&mut self, id: &str) {
        if self.avatar_requests.insert(id.to_owned())
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::FetchAvatar {
                id: id.to_owned(),
                full: false,
            });
        }
    }

    /// Rebuilds the open picker from the latest own and saved stickers.
    fn refresh_sticker_picker(&mut self, sender: &ComponentSender<NativeApplication>) {
        let Some((popover, stack)) = &self.sticker_picker else {
            return;
        };
        if !popover.is_visible() {
            self.sticker_picker = None;
            return;
        }
        let scroll = |stack: &gtk::Stack| {
            stack
                .visible_child()
                .and_downcast::<gtk::ScrolledWindow>()
                .map(|scroller| scroller.vadjustment())
        };
        let offset = scroll(stack).map(|adjustment| adjustment.value());
        let had_focus = popover.focus_child().is_some();
        let (content, new_stack) = sticker_picker_content(
            &self.recent_stickers,
            &self.favorite_stickers,
            &dialog_action_callback(sender),
        );
        popover.set_child(Some(&content));
        if had_focus {
            content.child_focus(gtk::DirectionType::TabForward);
        }
        // The rebuilt grid has no height yet; restore the scroll once it is
        // tall enough to hold the old position.
        if let (Some(offset), Some(adjustment)) = (offset, scroll(&new_stack))
            && offset > 0.0
        {
            let handler = std::rc::Rc::new(std::cell::Cell::new(None));
            let slot = handler.clone();
            let id = adjustment.connect_upper_notify(move |adjustment| {
                if adjustment.upper() - adjustment.page_size() >= offset {
                    adjustment.set_value(offset);
                    if let Some(id) = slot.take() {
                        adjustment.disconnect(id);
                    }
                }
            });
            handler.set(Some(id));
        }
        self.sticker_picker = Some((popover.clone(), new_stack));
    }

    /// Stickers are small and meaningless as placeholders, so loaded history
    /// fetches them the way live messages are fetched.
    fn download_missing_stickers(&mut self) {
        let Some(backend) = self
            .backend
            .as_ref()
            .filter(|_| self.settings.auto_download)
        else {
            return;
        };
        for message in self.message_snapshots.values_mut() {
            if let crate::model::Content::Sticker { media, .. } = &mut message.content
                && should_auto_download(Some(media))
            {
                backend.send(crate::backend::Command::Download {
                    chat: message.chat.clone(),
                    message: message.id.clone(),
                });
                media.state = crate::model::MediaState::Downloading;
            }
        }
    }

    fn media_completed(
        &mut self,
        chat: &str,
        id: &str,
        result: Result<std::path::PathBuf, String>,
    ) {
        if self.active_chat.as_deref() != Some(chat) {
            return;
        }
        let Some(media) = self
            .message_snapshots
            .get_mut(id)
            .and_then(|message| message.content.media_mut())
        else {
            return;
        };
        match result {
            Ok(path) => {
                media.path = Some(path);
                media.state = crate::model::MediaState::Idle;
            }
            Err(error) => {
                let notice = if error.contains("403") || error.contains("404") {
                    "No longer available on WhatsApp's servers".to_owned()
                } else {
                    error
                };
                media.state = crate::model::MediaState::Failed(notice);
            }
        }
        self.refresh_message_row(id);
        self.refresh_selected_voice();
        self.queue_waveforms();
    }

    fn message_deleted(&mut self, chat: &str, id: &str) {
        if self.active_chat.as_deref() != Some(chat) {
            return;
        }
        if self.audio.playing_audio.as_deref() == Some(id) {
            self.audio.media.stop_playback();
            self.audio.playing_audio = None;
        }
        self.audio
            .audio_waveforms
            .remove(&(chat.to_owned(), id.to_owned()));
        self.audio
            .audio_errors
            .remove(&(chat.to_owned(), id.to_owned()));
        self.audio.audio_registry.borrow_mut().remove(id);
        let Some(position) = self.message_ids.iter().position(|known| known == id) else {
            return;
        };
        self.message_ids.remove(position);
        if let Some(selection) = self.message_selection.as_mut() {
            selection.retain(|known| known != id);
        }
        self.message_snapshots.remove(id);
        self.editable_messages.remove(id);
        self.rebuild_message_rows(false);
        if self
            .editing
            .as_ref()
            .is_some_and(|(_, editing_id)| editing_id == id)
        {
            self.editing = None;
            self.pending_edit = None;
        }
        if self
            .reply_to
            .as_ref()
            .is_some_and(|(_, reply_id)| reply_id == id)
        {
            self.reply_to = None;
        }
        self.sync_transcript();
        if self.selected_message_id().is_none() {
            self.audio.selected_voice = None;
            self.audio.selected_voice_message = None;
        } else {
            self.refresh_selected_voice();
        }
        self.status = gettext("Message deleted");
    }

    fn clear_active_chat(&mut self) {
        self.audio.media.stop_playback();
        self.audio.playing_audio = None;
        self.audio.waveform_queue.clear();
        if let Some(cancel) = self.audio.waveform_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        self.audio.audio_waveforms.clear();
        self.audio.waveform_attempted.clear();
        self.audio.audio_errors.clear();
        self.audio.audio_registry.borrow_mut().clear();
        self.active_chat = None;
        self.pending_send = None;
        self.pending_edit = None;
        self.reply_to = None;
        self.editing = None;
        self.message_ids.clear();
        self.message_snapshots.clear();
        self.editable_messages.clear();
        self.transcript.clear();
        self.audio.selected_voice = None;
        self.audio.selected_voice_message = None;
        self.messages.clear();
        self.page_title = "Conversation unavailable".into();
        self.status = gettext("This chat is no longer available.");
    }

    /// File names of the files staged for the open chat.
    fn pending_attachment_names(&self) -> Vec<String> {
        self.active_chat
            .as_ref()
            .and_then(|chat| self.pending_attachments.get(chat))
            .into_iter()
            .flatten()
            .map(|path| {
                path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }

    fn pending_attachment_count(&self) -> usize {
        let files = self
            .active_chat
            .as_ref()
            .and_then(|chat| self.pending_attachments.get(chat))
            .map_or(0, Vec::len);
        let image = self
            .active_chat
            .as_ref()
            .is_some_and(|chat| self.pending_clipboard_images.contains_key(chat));
        files + usize::from(image)
    }

    /// Whether messages are being brought up to date: syncing with the phone,
    /// reconnecting, or refreshing after a suspend.
    fn refreshing(&self) -> bool {
        self.syncing
            || self.resuming
            || matches!(
                self.link,
                LinkStatus::Connecting | LinkStatus::Disconnected { .. }
            )
    }

    fn request_shutdown(&mut self, sender: ComponentSender<Self>) {
        if std::mem::replace(&mut self.shutdown_started, true) {
            return;
        }
        if let Some(drain) = &self.active_drain {
            drain.close();
        }
        for session in self.accounts.values() {
            if let Some(drain) = &session.drain {
                drain.close();
            }
        }
        self.portals.cancel_all();
        if self.save_settings().is_err() {
            self.status = gettext("Could not save preferences");
        }
        self.status = gettext("Shutdown requested");
        let mut backends: Vec<Backend> = self.backend.take().into_iter().collect();
        backends.extend(
            self.accounts
                .values_mut()
                .filter_map(|session| session.backend.take()),
        );
        // ponytail: one account after another; join in parallel if many accounts slow quitting
        std::thread::spawn(move || {
            for mut backend in backends {
                backend.shutdown();
            }
            sender.input(Input::ShutdownComplete);
        });
    }

    /// Closing the window hides it when something can bring it back (a tray or
    /// the Background Apps list), and otherwise asks before quitting.
    fn finish_close(&mut self, sender: ComponentSender<Self>) {
        if !self.settings.keep_running_in_background {
            self.request_shutdown(sender);
        } else if self.tray_shown || crate::native_background::sandboxed() {
            self.window.set_visible(false);
        } else {
            self.present_quit_confirmation_dialog(sender);
        }
    }

    fn present_quit_confirmation_dialog(&self, sender: ComponentSender<Self>) {
        use libadwaita::prelude::*;

        let alert = adw::AlertDialog::new(
            Some("Quit ZapTide?"),
            Some(
                "ZapTide stays in the background when the desktop shows tray \
                 icons, and none is shown now. Closing the window will quit the app.",
            ),
        );
        alert.add_response("cancel", &gettext("Cancel"));
        alert.add_response("quit", &gettext("Quit"));
        alert.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
        alert.set_close_response("cancel");
        alert.set_default_response(Some("cancel"));
        alert.connect_response(None, move |_, response| {
            if response == "quit" {
                sender.input(Input::Quit);
            }
        });
        alert.present(Some(&self.window));
    }

    fn toast(&self, text: &str) {
        if let Some(child) = gtk::prelude::GtkWindowExt::child(&self.window)
            && let Ok(overlay) = child.downcast::<adw::ToastOverlay>()
        {
            overlay.add_toast(adw::Toast::builder().title(text).use_markup(false).build());
        }
    }
}

/// Runs the native shell and starts the backend only after the first main-context turn.
pub fn run(dirs: AppDirs) {
    let application_id =
        std::env::var("FLATPAK_ID").unwrap_or_else(|_| "dev.luminusos.ZapTide".into());
    RelmApp::new(&application_id).run::<NativeApplication>(Init { dirs });
}

#[cfg(test)]
#[path = "application/tests.rs"]
mod tests;
