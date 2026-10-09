//! The linked accounts that are not on screen.
//!
//! The window's fields always describe the active account. Every other
//! account keeps the same data here, updated from its own backend, and trades
//! places with the window's fields when it is switched to.

use super::*;

use crate::account::{AccountId, AccountSettings};

/// One account that is not on screen. The fields from `backend` on are the
/// ones that trade places with the window's when the account is switched to.
pub(super) struct AccountSession {
    pub(super) id: AccountId,
    pub(super) dirs: AppDirs,
    pub(super) settings: AccountSettings,
    pub(super) backend: Option<Backend>,
    pub(super) notifier: EventNotifier,
    pub(super) drain: Option<GlibEventDrain>,
    pub(super) link: LinkStatus,
    pub(super) syncing: bool,
    pub(super) chat_snapshots: Vec<crate::model::Chat>,
    pub(super) contacts: std::collections::HashMap<String, crate::model::Contact>,
    pub(super) avatars: std::collections::HashMap<String, std::path::PathBuf>,
    pub(super) avatar_requests: std::collections::HashSet<String>,
    pub(super) drafts: std::collections::HashMap<String, String>,
    pub(super) account_receipts_off: bool,
    pub(super) sticker_packs: Vec<crate::model::StickerPack>,
    pub(super) recent_stickers: Vec<std::path::PathBuf>,
    pub(super) favorite_stickers: Vec<std::path::PathBuf>,
    pub(super) sticker_emojis: std::collections::HashMap<std::path::PathBuf, Vec<String>>,
}

impl AccountSession {
    fn new(
        id: AccountId,
        dirs: AppDirs,
        settings: AccountSettings,
        backend: Option<Backend>,
        notifier: EventNotifier,
        drain: Option<GlibEventDrain>,
    ) -> Self {
        Self {
            id,
            dirs,
            settings,
            link: if backend.is_some() {
                LinkStatus::Starting
            } else {
                LinkStatus::Failed("Couldn't start".into())
            },
            backend,
            notifier,
            drain,
            syncing: false,
            chat_snapshots: Vec::new(),
            contacts: Default::default(),
            avatars: Default::default(),
            avatar_requests: Default::default(),
            drafts: Default::default(),
            account_receipts_off: false,
            sticker_packs: Vec::new(),
            recent_stickers: Vec::new(),
            favorite_stickers: Vec::new(),
            sticker_emojis: Default::default(),
        }
    }

    #[cfg(test)]
    pub(super) fn detached_for_tests(id: AccountId) -> Self {
        let (backend, _events) = Backend::detached();
        let (notifier, _drain) = EventNotifier::new();
        Self::new(
            id,
            AppDirs::under(&std::env::temp_dir().join("zaptide-detached-account")),
            AccountSettings::default(),
            Some(backend),
            notifier,
            None,
        )
    }
}

impl AccountSession {
    /// Opens a hidden account's folders and starts its backend's thread;
    /// it connects once the window asks the backends to start.
    pub(super) fn spawn(
        base: &AppDirs,
        id: AccountId,
        current: &crate::settings::Settings,
        sender: &ComponentSender<NativeApplication>,
    ) -> Self {
        let dirs = base.for_account(id);
        if let Err(error) = dirs.ensure() {
            log::error!("could not prepare account {id}'s folder: {error}");
        }
        let settings = AccountSettings::load_or(&dirs.account_settings_file(), current);
        let (notifier, drain) = EventNotifier::new();
        let input = sender.clone();
        let drain = GlibEventDrain::install(
            &notifier,
            drain,
            gtk::glib::MainContext::default(),
            move || input.input(Input::BackendReady(id)),
        );
        let backend = Backend::try_spawn(dirs.clone(), notifier.clone())
            .inspect_err(|error| log::error!("account {id}'s backend could not start: {error}"))
            .ok();
        Self::new(id, dirs, settings, backend, notifier, Some(drain))
    }
}

impl NativeApplication {
    pub(super) fn spawn_hidden_accounts(&mut self, sender: &ComponentSender<Self>) {
        if self.legacy_layout {
            return;
        }
        let ids: Vec<AccountId> = self
            .registry
            .accounts
            .iter()
            .map(|entry| entry.id)
            .filter(|id| *id != self.active_account)
            .collect();
        for id in ids {
            let session = AccountSession::spawn(&self.base_dirs, id, &self.settings, sender);
            self.accounts.insert(id, session);
        }
    }

    /// Saves the shared preferences, and the active account's own ones.
    pub(super) fn save_settings(&mut self) -> std::io::Result<()> {
        self.settings.last_chat = self.active_chat.clone();
        self.settings.save(&self.settings_path)?;
        if self.legacy_layout {
            return Ok(());
        }
        AccountSettings::from_settings(&self.settings)
            .save(&self.active_dirs.account_settings_file())
    }

    pub(super) fn save_registry(&self) {
        if self.legacy_layout {
            return;
        }
        if let Err(error) = self.registry.save(&self.base_dirs.accounts_file()) {
            log::warn!("could not save the accounts file: {error}");
        }
    }

    /// Records an account's own number and name; a number means it linked.
    pub(super) fn remember_profile(
        &mut self,
        id: AccountId,
        phone: Option<String>,
        name: Option<String>,
        sender: &ComponentSender<Self>,
    ) {
        if let (Some(phone), Some((pending, _))) = (phone.as_deref(), self.pending_account)
            && pending == id
        {
            if let Some(existing) = duplicate_of(&self.registry, id, phone) {
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::Unlink);
                }
                self.toast("This account is already added");
                self.cancel_add_account(existing, sender);
                return;
            }
            self.pending_account = None;
        }
        if id == self.active_account
            && let Some(phone) = phone.as_deref()
        {
            self.request_avatar(&own_jid(phone));
        }
        let Some(entry) = self.registry.entry_mut(id) else {
            return;
        };
        if record_profile(entry, phone, name) {
            self.save_registry();
        }
    }

    /// Drains a hidden account's backend into its session.
    pub(super) fn handle_hidden_events(&mut self, id: AccountId, sender: &ComponentSender<Self>) {
        let (notifications, previews) = (
            self.settings.notifications,
            self.settings.notification_previews,
        );
        let Some(session) = self.accounts.get_mut(&id) else {
            return;
        };
        let events = super::dispatch::drain_hidden(session);
        let mut profiles = Vec::new();
        let mut notices = Vec::new();
        let mut withdrawn = Vec::new();
        for event in events {
            if matches!(event, NativeEvent::Link(LinkStatus::LoggedOut)) {
                withdrawn.extend(session.chat_snapshots.iter().map(|chat| chat.id.clone()));
            }
            match event {
                NativeEvent::Profile { phone, name } => profiles.push((phone, name)),
                event => {
                    notices.extend(apply_hidden_event(session, event, notifications, previews))
                }
            }
        }
        for (phone, name) in profiles {
            self.remember_profile(id, phone, name, sender);
        }
        for chat in &withdrawn {
            self.notifications.clear_chat(chat);
        }
        if self.removing == Some(id)
            && (!withdrawn.is_empty()
                || matches!(session_link(self, id), Some(LinkStatus::LoggedOut)))
        {
            // Switched away before its logout arrived: finish removing it.
            self.removing = None;
            self.forget_account(id);
            self.sync_tray();
            return;
        }
        if !withdrawn.is_empty() || matches!(session_link(self, id), Some(LinkStatus::LoggedOut)) {
            notices.clear();
        }
        for notice in notices {
            if let Err(error) = self.notifications.show(
                &notice.chat,
                &notice.title,
                &notice.body,
                notice.avatar.as_deref(),
            ) {
                log::warn!("could not show a notification: {error}");
            }
        }
        self.sync_tray();
    }

    /// What the account button and its popover show.
    pub(super) fn switcher_state(&self) -> super::account_switcher::SwitcherState {
        use super::account_switcher::{RowState, SwitcherRow, SwitcherState};
        let row_state = |backend: bool, link: &LinkStatus| {
            if !backend {
                RowState::Failed
            } else if matches!(link, LinkStatus::LoggedOut) {
                RowState::SignedOut
            } else {
                RowState::Ready
            }
        };
        let own_avatar = |phone: &Option<String>| {
            phone
                .as_deref()
                .and_then(|phone| self.avatars.get(&own_jid(phone)).cloned())
        };
        let rows = self
            .registry
            .accounts
            .iter()
            .filter(|entry| entry.linked)
            .map(|entry| {
                let (unread, state, avatar) = if entry.id == self.active_account {
                    (
                        unread_chat_count(&self.chat_snapshots),
                        row_state(self.backend.is_some(), &self.link),
                        own_avatar(&entry.phone),
                    )
                } else if let Some(session) = self.accounts.get(&entry.id) {
                    (
                        unread_chat_count(&session.chat_snapshots),
                        row_state(session.backend.is_some(), &session.link),
                        None,
                    )
                } else {
                    (0, RowState::Failed, None)
                };
                SwitcherRow {
                    id: entry.id,
                    label: entry.label(),
                    phone: entry.phone.clone(),
                    avatar,
                    unread,
                    state,
                }
            })
            .collect();
        let active = self.registry.entry(self.active_account);
        SwitcherState {
            rows,
            active: Some(self.active_account),
            active_label: active.map(|entry| entry.label()).unwrap_or_default(),
            active_avatar: active.and_then(|entry| own_avatar(&entry.phone)),
            unread_elsewhere: self.unread_elsewhere(),
            can_add: !self.legacy_layout && self.pending_account.is_none(),
        }
    }

    /// Links another number: a new account comes on screen with the link page.
    pub(super) fn add_account(&mut self, sender: &ComponentSender<Self>) {
        if self.legacy_layout || self.pending_account.is_some() {
            return;
        }
        let id = self.registry.add();
        self.save_registry();
        let mut session = AccountSession::spawn(&self.base_dirs, id, &self.settings, sender);
        if let Some(startup) = session.backend.as_mut().and_then(Backend::take_startup) {
            let _ = startup.send(());
        }
        self.accounts.insert(id, session);
        self.pending_account = Some((id, self.active_account));
        self.switch_account(id, sender);
    }

    /// Drops the account being added and returns to `to`.
    pub(super) fn cancel_add_account(&mut self, to: AccountId, sender: &ComponentSender<Self>) {
        let Some((pending, previous)) = self.pending_account.take() else {
            return;
        };
        if pending == self.active_account
            && matches!(
                self.link,
                LinkStatus::Connecting | LinkStatus::Connected | LinkStatus::Disconnected { .. }
            )
            && let Some(backend) = &self.backend
        {
            // The phone already lists this device.
            backend.send(crate::backend::Command::Unlink);
        }
        let to = if self.accounts.contains_key(&to) {
            to
        } else {
            previous
        };
        self.switch_account(to, sender);
        self.forget_account(pending);
    }

    /// Stops a hidden account's backend and deletes everything it kept here.
    pub(super) fn forget_account(&mut self, id: AccountId) {
        if id == self.active_account {
            return;
        }
        let backend = self.accounts.remove(&id).and_then(|mut session| {
            if let Some(drain) = &session.drain {
                drain.close();
            }
            session.backend.take()
        });
        self.registry.remove(id);
        self.save_registry();
        // Joining waits for queued commands such as a logout; never on the
        // window's thread. Files go only once the backend let go of them.
        let base = self.base_dirs.clone();
        std::thread::spawn(move || {
            if let Some(mut backend) = backend {
                backend.shutdown();
            }
            crate::account::delete_account_data(&base, id);
        });
    }

    /// The account being removed has logged out: show another one and
    /// delete it, or keep the last one, reset, on the link page.
    pub(super) fn finish_removal(&mut self, sender: &ComponentSender<Self>) {
        let Some(removed) = self.removing.take() else {
            return;
        };
        let works = |id: AccountId| {
            self.accounts.get(&id).is_some_and(|session| {
                session.backend.is_some() && !matches!(session.link, LinkStatus::LoggedOut)
            })
        };
        match next_account_after_removal(&self.registry, removed, works) {
            Some(next) => {
                self.switch_account(next, sender);
                self.forget_account(removed);
            }
            None => {
                if self.backend.is_none() {
                    // Nothing holds its files open: delete them as promised.
                    crate::account::delete_account_data(&self.base_dirs, removed);
                }
                if let Some(entry) = self.registry.entry_mut(removed) {
                    entry.linked = false;
                    entry.name = None;
                    entry.phone = None;
                }
                self.save_registry();
            }
        }
    }

    /// Puts another linked account on screen.
    pub(super) fn switch_account(&mut self, id: AccountId, sender: &ComponentSender<Self>) {
        if id == self.active_account {
            return;
        }
        let Some(mut session) = self.accounts.remove(&id) else {
            return;
        };
        if self.save_settings().is_err() {
            log::warn!("could not save account {}'s settings", self.active_account);
        }
        self.leave_screen();
        std::mem::swap(&mut self.backend, &mut session.backend);
        std::mem::swap(&mut self.notifier, &mut session.notifier);
        std::mem::swap(&mut self.active_drain, &mut session.drain);
        std::mem::swap(&mut self.active_dirs, &mut session.dirs);
        std::mem::swap(&mut self.link, &mut session.link);
        std::mem::swap(&mut self.syncing, &mut session.syncing);
        std::mem::swap(&mut self.chat_snapshots, &mut session.chat_snapshots);
        std::mem::swap(&mut self.contacts, &mut session.contacts);
        std::mem::swap(&mut self.avatars, &mut session.avatars);
        std::mem::swap(&mut self.avatar_requests, &mut session.avatar_requests);
        std::mem::swap(&mut self.drafts, &mut session.drafts);
        std::mem::swap(
            &mut self.account_receipts_off,
            &mut session.account_receipts_off,
        );
        std::mem::swap(&mut self.sticker_packs, &mut session.sticker_packs);
        std::mem::swap(&mut self.recent_stickers, &mut session.recent_stickers);
        std::mem::swap(&mut self.favorite_stickers, &mut session.favorite_stickers);
        AVATAR_TEXTURES.with_borrow_mut(|cache| {
            AVATAR_TEXTURE_REFERENCES.with_borrow_mut(|references| {
                move_avatar_references(cache, references, &session.avatars, &self.avatars)
            })
        });
        self.composer = composer_with_drafts(&self.drafts);
        std::mem::swap(&mut self.sticker_emojis, &mut session.sticker_emojis);
        // `session` now holds the account that left the screen.
        let incoming_settings = std::mem::replace(
            &mut session.settings,
            AccountSettings::from_settings(&self.settings),
        );
        incoming_settings.apply_to(&mut self.settings);
        session.id = self.active_account;
        self.accounts.insert(session.id, session);
        self.active_account = id;
        self.registry.active = Some(id);
        self.save_registry();

        self.qr_texture = match &self.link {
            LinkStatus::Unlinked { qr: Some(qr), .. } => qr_texture(qr),
            _ => None,
        };
        (self.page_title, self.status) = link_page(&self.link);
        let chats = std::mem::take(&mut self.chat_snapshots);
        self.apply_chat_changes(vec![ChatChange::Snapshot(chats)]);
        self.flush_chats();
        self.refresh_sticker_picker(sender);
        if let Some(chat) = self.settings.last_chat.clone() {
            sender.input(Input::OpenChatId(chat));
        }
        self.apply_runtime_settings();
        self.sync_tray();
    }

    pub(super) fn rebuild_sticker_emojis(&mut self) {
        self.sticker_emojis = sticker_emojis(&self.sticker_packs);
    }

    /// Clears everything on screen that belongs to the open conversation, so
    /// nothing of one account shows or acts in another.
    pub(super) fn leave_screen(&mut self) {
        // Pickers answer by chat id only; an answer must not land in another account.
        self.cancel_portal_requests();
        // A dialog acts on whichever account is on screen when it is confirmed.
        // Stacked dialogs close one at a time.
        for _ in 0..8 {
            let Some(dialog) = self.window.visible_dialog() else {
                break;
            };
            dialog.force_close();
        }
        for window in gtk::Window::list_toplevels() {
            if let Ok(window) = window.downcast::<gtk::Window>()
                && window.transient_for().as_ref() == Some(self.window.upcast_ref())
            {
                window.close();
            }
        }
        self.preferences = None;
        if self.settings.send_typing
            && let Some(backend) = &self.backend
        {
            for chat in self.composing_until.keys() {
                backend.send(crate::backend::Command::Composing {
                    chat: chat.clone(),
                    composing: false,
                });
            }
        }
        self.contact_share_generation = self.contact_share_generation.wrapping_add(1);
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
        self.chat_ids.clear();
        self.reset_chat_filters(false);
        self.message_ids.clear();
        self.message_snapshots.clear();
        self.editable_messages.clear();
        self.messages.clear();
        self.presence.clear();
        self.typing.clear();
        self.typing_until.clear();
        self.composing_until.clear();
        self.composer = crate::native_composer::NativeComposerState::default();
        self.composer_buffer.set_text("");
        self.draft.clear();
        self.editing = None;
        self.reply_to = None;
        self.pending_edit = None;
        self.pending_composer_request = None;
        self.pending_send = None;
        self.audio.voice_send_pending = false;
        self.audio.played_voice.clear();
        self.pending_attachments.clear();
        self.document_attachments.clear();
        self.pending_clipboard_images.clear();
        self.audio.selected_voice = None;
        self.audio.selected_voice_message = None;
        self.pending_quote_navigation = None;
        self.message_selection = None;
        self.message_target = None;
        self.info_about = None;
        self.history_complete = false;
        self.loading_older = false;
        self.recent_messages_pending = false;
        self.transcript.clear();
        self.message_menu.popdown();
        if let Some((picker, _)) = &self.sticker_picker {
            picker.popdown();
        }
    }
}

/// A composer holding an account's saved drafts, so opening its chats
/// restores what was typed before the switch.
pub(super) fn composer_with_drafts(
    drafts: &std::collections::HashMap<String, String>,
) -> crate::native_composer::NativeComposerState {
    let mut composer = crate::native_composer::NativeComposerState::default();
    for (chat, draft) in drafts {
        composer.set_draft(chat, draft.clone());
    }
    composer
}

/// Moves picture-texture references from the account leaving the screen to
/// the one arriving, so the arriving account's pictures load and the leaving
/// one's textures are released.
fn move_avatar_references<T>(
    cache: &mut std::collections::HashMap<std::path::PathBuf, T>,
    references: &mut std::collections::HashMap<std::path::PathBuf, usize>,
    outgoing: &std::collections::HashMap<String, std::path::PathBuf>,
    incoming: &std::collections::HashMap<String, std::path::PathBuf>,
) {
    for path in incoming.values() {
        update_avatar_texture_references(cache, references, None, Some(path.clone()));
    }
    for path in outgoing.values() {
        update_avatar_texture_references(cache, references, Some(path.clone()), None);
    }
}

/// The emoji each sticker of an account's packs answers to.
fn sticker_emojis(
    packs: &[crate::model::StickerPack],
) -> std::collections::HashMap<std::path::PathBuf, Vec<String>> {
    let mut index = std::collections::HashMap::new();
    for sticker in packs.iter().flat_map(|pack| &pack.stickers) {
        if let Ok(bytes) = std::fs::read(sticker) {
            let emojis = crate::sticker_meta::emojis(&bytes);
            if !emojis.is_empty() {
                index.insert(sticker.clone(), emojis);
            }
        }
    }
    index
}

/// A suspend kills the socket of every linked account, not only the one on screen.
pub(super) fn reconnects_after_resume(link: &LinkStatus) -> bool {
    matches!(
        link,
        LinkStatus::Connecting | LinkStatus::Connected | LinkStatus::Disconnected { .. }
    )
}

fn session_link(app: &NativeApplication, id: AccountId) -> Option<&LinkStatus> {
    app.accounts.get(&id).map(|session| &session.link)
}

/// Another account already linked to `phone`, when `id` links to it again.
pub(super) fn duplicate_of(
    registry: &crate::account::Registry,
    id: AccountId,
    phone: &str,
) -> Option<AccountId> {
    registry
        .accounts
        .iter()
        .find(|entry| entry.id != id && entry.linked && entry.phone.as_deref() == Some(phone))
        .map(|entry| entry.id)
}

/// The linked account to show once `removed` is gone, if any is left,
/// preferring one that works over a signed-out or failed one.
pub(super) fn next_account_after_removal(
    registry: &crate::account::Registry,
    removed: AccountId,
    works: impl Fn(AccountId) -> bool,
) -> Option<AccountId> {
    let others: Vec<AccountId> = registry
        .linked_ids()
        .into_iter()
        .filter(|id| *id != removed)
        .collect();
    others
        .iter()
        .copied()
        .find(|id| works(*id))
        .or_else(|| others.first().copied())
}

/// Stores what an account says about itself; true if anything changed. A
/// logout says nothing, but the number stays known so linking it again is
/// recognised as the same account.
pub(super) fn record_profile(
    entry: &mut crate::account::AccountEntry,
    phone: Option<String>,
    name: Option<String>,
) -> bool {
    let phone = phone.or_else(|| entry.phone.clone());
    let linked = entry.linked || phone.is_some();
    if entry.phone == phone && entry.name == name && entry.linked == linked {
        return false;
    }
    entry.phone = phone;
    entry.name = name;
    entry.linked = linked;
    true
}

/// The WhatsApp id of a formatted phone number, for our own picture.
fn own_jid(phone: &str) -> String {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    format!("{digits}@s.whatsapp.net")
}

/// A notification a hidden account wants shown.
#[derive(Debug, PartialEq)]
pub(super) struct Notice {
    pub(super) chat: String,
    pub(super) title: String,
    pub(super) body: String,
    pub(super) avatar: Option<std::path::PathBuf>,
}

/// Unarchived, unmuted, unlocked chats with unread messages, as the tray counts them.
pub(super) fn unread_chat_count(chats: &[crate::model::Chat]) -> usize {
    let now = crate::util::now();
    chats
        .iter()
        .filter(|chat| chat.unread > 0 && !chat.archived && !chat.locked && !chat.muted(now))
        .count()
}

/// The title and body of a message notification.
pub(super) fn notice_text(
    known: Option<&crate::model::Chat>,
    message: &crate::model::Message,
    previews: bool,
) -> (String, String) {
    let title = known.map_or_else(
        || sender_label(message.sender_name.as_deref(), &message.sender),
        |known| known.name.clone(),
    );
    let body = if !previews {
        "New message".to_owned()
    } else if known.is_some_and(crate::model::Chat::is_group) {
        format!(
            "{}: {}",
            sender_label(message.sender_name.as_deref(), &message.sender),
            message.summary()
        )
    } else {
        message.summary()
    };
    (title, body)
}

/// Keeps a hidden account's data current. Nothing here may touch the screen
/// or send anything: a hidden account never marks messages read.
pub(super) fn apply_hidden_event(
    session: &mut AccountSession,
    event: NativeEvent,
    notifications: bool,
    previews: bool,
) -> Option<Notice> {
    match event {
        NativeEvent::Link(link) => {
            if matches!(link, LinkStatus::LoggedOut) {
                session.chat_snapshots.clear();
                session.contacts.clear();
                session.avatars.clear();
                session.avatar_requests.clear();
                session.drafts.clear();
                session.account_receipts_off = false;
                session.sticker_packs.clear();
                session.recent_stickers.clear();
                session.favorite_stickers.clear();
                session.sticker_emojis.clear();
            }
            session.link = link;
        }
        NativeEvent::Syncing(syncing) => session.syncing = syncing,
        NativeEvent::Chats(chats) => session.chat_snapshots = chats,
        NativeEvent::ChatUpdated(chat) => {
            session.chat_snapshots.retain(|known| known.id != chat.id);
            let index = session
                .chat_snapshots
                .partition_point(|known| known.last_activity >= chat.last_activity);
            session.chat_snapshots.insert(index, *chat);
        }
        NativeEvent::Contacts(contacts) => {
            if contacts.is_empty() {
                session.contacts.clear();
            }
            session.contacts.extend(
                contacts
                    .into_iter()
                    .map(|contact| (contact.id.clone(), contact)),
            );
        }
        NativeEvent::Avatar { id, path } => match path {
            Some(path) => {
                session.avatars.insert(id, path);
            }
            None => {
                session.avatars.remove(&id);
            }
        },
        NativeEvent::ReceiptsPrivacy { disabled } => session.account_receipts_off = disabled,
        NativeEvent::Stickers {
            saved,
            packs,
            recent,
        } => {
            session.favorite_stickers = saved;
            if packs != session.sticker_packs {
                session.sticker_emojis = sticker_emojis(&packs);
            }
            session.sticker_packs = packs;
            session.recent_stickers = recent;
        }
        NativeEvent::Incoming { chat, message } => {
            let known = session.chat_snapshots.iter().find(|known| known.id == chat);
            // Not on screen, so no chat counts as open.
            if notification_should_show(notifications, false, None, &chat, known) {
                let (title, body) = notice_text(known, &message, previews);
                return Some(Notice {
                    avatar: session.avatars.get(&chat).cloned(),
                    chat,
                    title,
                    body,
                });
            }
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(chat: &str, text: &str) -> Box<crate::model::Message> {
        Box::new(crate::model::Message {
            id: "message".into(),
            chat: chat.into(),
            sender: "alice@s.whatsapp.net".into(),
            sender_name: Some("Alice".into()),
            from_me: false,
            timestamp: 1,
            content: crate::model::Content::text(text),
            status: crate::model::Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        })
    }

    fn unread_chat(id: &str, name: &str) -> crate::model::Chat {
        let mut chat = crate::model::Chat::new(id.into(), name.into());
        chat.unread = 1;
        chat
    }

    #[test]
    fn hidden_account_messages_are_dropped() {
        let mut session = AccountSession::detached_for_tests(AccountId(2));
        let notice = apply_hidden_event(
            &mut session,
            NativeEvent::Messages {
                chat: "chat".into(),
                messages: vec![*message("chat", "Hello")],
                older: false,
                complete: true,
            },
            true,
            true,
        );
        assert!(notice.is_none());
        assert!(session.chat_snapshots.is_empty());
    }

    #[test]
    fn hidden_account_incoming_message_notifies_and_counts_unread() {
        let mut session = AccountSession::detached_for_tests(AccountId(2));
        apply_hidden_event(
            &mut session,
            NativeEvent::Chats(vec![unread_chat("chat", "Alice")]),
            true,
            true,
        );
        assert_eq!(unread_chat_count(&session.chat_snapshots), 1);
        let notice = apply_hidden_event(
            &mut session,
            NativeEvent::Incoming {
                chat: "chat".into(),
                message: message("chat", "Hello"),
            },
            true,
            true,
        )
        .expect("a hidden account still notifies");
        assert_eq!(notice.chat, "chat");
        assert_eq!(notice.title, "Alice");
        assert_eq!(notice.body, "Hello");
    }

    #[test]
    fn hidden_account_respects_locked_chats_and_hidden_previews() {
        let mut session = AccountSession::detached_for_tests(AccountId(2));
        let mut locked = unread_chat("locked", "Bob");
        locked.locked = true;
        apply_hidden_event(
            &mut session,
            NativeEvent::Chats(vec![locked, unread_chat("chat", "Alice")]),
            true,
            true,
        );
        let incoming = |chat: &str| NativeEvent::Incoming {
            chat: chat.into(),
            message: message(chat, "Hello"),
        };
        assert!(apply_hidden_event(&mut session, incoming("locked"), true, true).is_none());
        assert!(apply_hidden_event(&mut session, incoming("chat"), false, true).is_none());
        let hidden = apply_hidden_event(&mut session, incoming("chat"), true, false).unwrap();
        assert_eq!(hidden.body, "New message");
    }

    #[test]
    fn switching_carries_each_account_s_drafts_into_the_composer() {
        let drafts = std::collections::HashMap::from([("chat".to_owned(), "see you".to_owned())]);
        assert_eq!(composer_with_drafts(&drafts).draft("chat"), "see you");
    }

    #[test]
    fn avatar_references_follow_the_account_on_screen() {
        let shared = std::path::PathBuf::from("/synthetic/shared.jpg");
        let outgoing_only = std::path::PathBuf::from("/synthetic/outgoing.jpg");
        let incoming_only = std::path::PathBuf::from("/synthetic/incoming.jpg");
        let mut cache =
            std::collections::HashMap::from([(shared.clone(), ()), (outgoing_only.clone(), ())]);
        let mut references =
            std::collections::HashMap::from([(shared.clone(), 1), (outgoing_only.clone(), 1)]);
        let outgoing = std::collections::HashMap::from([
            ("a".to_owned(), shared.clone()),
            ("b".to_owned(), outgoing_only.clone()),
        ]);
        let incoming = std::collections::HashMap::from([
            ("a".to_owned(), shared.clone()),
            ("c".to_owned(), incoming_only.clone()),
        ]);
        move_avatar_references(&mut cache, &mut references, &outgoing, &incoming);
        assert_eq!(references.get(&shared), Some(&1));
        assert_eq!(references.get(&incoming_only), Some(&1));
        assert!(!references.contains_key(&outgoing_only));
        assert!(!cache.contains_key(&outgoing_only));
    }

    #[test]
    fn only_linked_accounts_reconnect_after_resume() {
        assert!(reconnects_after_resume(&LinkStatus::Connected));
        assert!(reconnects_after_resume(&LinkStatus::Connecting));
        assert!(!reconnects_after_resume(&LinkStatus::LoggedOut));
        assert!(!reconnects_after_resume(&LinkStatus::Starting));
    }

    #[test]
    fn a_hidden_account_that_logs_out_forgets_its_data() {
        let mut session = AccountSession::detached_for_tests(AccountId(2));
        apply_hidden_event(
            &mut session,
            NativeEvent::Chats(vec![unread_chat("chat", "Alice")]),
            true,
            true,
        );
        session.drafts.insert("chat".into(), "draft".into());
        session
            .avatars
            .insert("chat".into(), "/synthetic/a.jpg".into());
        apply_hidden_event(
            &mut session,
            NativeEvent::Link(LinkStatus::LoggedOut),
            true,
            true,
        );
        assert!(session.chat_snapshots.is_empty());
        assert!(session.drafts.is_empty());
        assert!(session.avatars.is_empty());
    }

    fn linked(registry: &mut crate::account::Registry, phone: &str) -> AccountId {
        let id = registry.add();
        let entry = registry.entry_mut(id).unwrap();
        entry.phone = Some(phone.into());
        entry.linked = true;
        id
    }

    #[test]
    fn duplicate_phone_is_detected() {
        let mut registry = crate::account::Registry::default();
        let first = linked(&mut registry, "+1 555 0100");
        let pending = registry.add();
        assert_eq!(duplicate_of(&registry, pending, "+1 555 0100"), Some(first));
        assert_eq!(duplicate_of(&registry, pending, "+1 555 0199"), None);
        assert_eq!(
            duplicate_of(&registry, first, "+1 555 0100"),
            None,
            "an account is not its own duplicate"
        );
    }

    #[test]
    fn remove_active_account_moves_to_another_linked_one() {
        let mut registry = crate::account::Registry::default();
        let first = linked(&mut registry, "+1 555 0100");
        let second = linked(&mut registry, "+1 555 0199");
        registry.add();
        assert_eq!(
            next_account_after_removal(&registry, second, |_| true),
            Some(first)
        );
        assert_eq!(
            next_account_after_removal(&registry, first, |_| true),
            Some(second)
        );
    }

    #[test]
    fn removal_prefers_an_account_that_works() {
        let mut registry = crate::account::Registry::default();
        let removed = linked(&mut registry, "+1 555 0100");
        let signed_out = linked(&mut registry, "+1 555 0101");
        let ready = linked(&mut registry, "+1 555 0102");
        assert_eq!(
            next_account_after_removal(&registry, removed, |id| id == ready),
            Some(ready)
        );
        assert_eq!(
            next_account_after_removal(&registry, removed, |_| false),
            Some(signed_out),
            "a signed-out account is still better than none"
        );
    }

    #[test]
    fn a_logout_keeps_the_known_number_for_duplicate_checks() {
        let mut entry = crate::account::AccountEntry {
            id: AccountId(1),
            name: Some("Alice".into()),
            phone: Some("+1 555 0100".into()),
            linked: true,
        };
        assert!(record_profile(&mut entry, None, None));
        assert_eq!(entry.phone.as_deref(), Some("+1 555 0100"));
        assert_eq!(entry.name, None);
        assert!(!record_profile(&mut entry, None, None));
    }

    #[test]
    fn remove_last_account_leaves_none() {
        let mut registry = crate::account::Registry::default();
        let only = linked(&mut registry, "+1 555 0100");
        registry.add();
        assert_eq!(next_account_after_removal(&registry, only, |_| true), None);
    }

    #[test]
    fn hidden_account_keeps_its_chat_list_and_contacts_current() {
        let mut session = AccountSession::detached_for_tests(AccountId(2));
        apply_hidden_event(
            &mut session,
            NativeEvent::Chats(vec![crate::model::Chat::new("a".into(), "A".into())]),
            true,
            true,
        );
        apply_hidden_event(
            &mut session,
            NativeEvent::ChatUpdated(Box::new(unread_chat("a", "A"))),
            true,
            true,
        );
        assert_eq!(session.chat_snapshots.len(), 1);
        assert_eq!(unread_chat_count(&session.chat_snapshots), 1);
        apply_hidden_event(
            &mut session,
            NativeEvent::Link(LinkStatus::LoggedOut),
            true,
            true,
        );
        assert!(matches!(session.link, LinkStatus::LoggedOut));
    }
}
