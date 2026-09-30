use super::*;

mod attachments;
mod backend_events;

impl NativeApplication {
    pub(super) fn handle_input(&mut self, input: Input, sender: ComponentSender<Self>) {
        match input {
            Input::WindowActivated => self.read_open_chat(),
            Input::WindowMapped => {
                gtk::glib::idle_add_local_once(move || sender.input(Input::StartBackend));
            }
            Input::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Input::SplitCollapsed(collapsed) => self.sidebar_visible = !collapsed,
            Input::ShowPreferences => {
                let preferences =
                    crate::native_preferences::NativePreferences::from(&self.settings);
                let theme_choices: Vec<_> = self
                    .theme_catalog
                    .picker_themes()
                    .map(|theme| theme.filename.clone())
                    .collect();
                let dialog = crate::native_preferences::NativePreferencesDialog::new(
                    &preferences,
                    &theme_choices,
                );
                dialog.present(&self.window);
                let input_sender = sender.clone();
                connect_widget_changes(dialog.dialog().upcast_ref(), &input_sender);
                let input_sender = sender.clone();
                dialog
                    .open_themes_folder_button()
                    .connect_clicked(move |_| input_sender.input(Input::OpenThemesFolder));
                self.preferences = Some(dialog);
            }
            Input::OpenThemesFolder => self.prepare_themes_folder(&sender),
            Input::ThemesFolderPrepared(success) => {
                if success {
                    self.launch_themes_folder(&sender);
                } else {
                    self.status = "Could not open themes folder".into();
                    self.toast("Could not open themes folder");
                }
            }
            Input::ThemesFolderFinished(success) => {
                self.status = if success {
                    "Opened themes folder".into()
                } else {
                    "Could not open themes folder".into()
                };
                if !success {
                    self.toast("Could not open themes folder");
                }
            }
            Input::ShowAbout => {
                let about = adw::AboutDialog::builder()
                    .application_name("ZapTide")
                    .application_icon("dev.luminusos.ZapTide")
                    .version(env!("CARGO_PKG_VERSION"))
                    .developer_name("LuminusOS")
                    .build();
                about.present(Some(&self.window));
            }
            Input::ShowShortcuts => {
                let (send, newline) = if self.enter_sends.get() {
                    ("Return", "<Shift>Return")
                } else {
                    ("<Control>Return", "Return")
                };
                let dialog = adw::ShortcutsDialog::new();
                for (title, items) in [
                    (
                        "Chat List",
                        &[
                            ("Previous chat", "Up"),
                            ("Next chat", "Down"),
                            ("Open chat", "Return"),
                        ][..],
                    ),
                    (
                        "Composer",
                        &[("Send message", send), ("New line", newline)][..],
                    ),
                    (
                        "Messages",
                        &[
                            ("Open message menu", "Menu <Shift>F10"),
                            ("Copy selected text", "<Control>c"),
                        ][..],
                    ),
                ] {
                    let section = adw::ShortcutsSection::new(Some(title));
                    for (item, accelerator) in items {
                        section.add(adw::ShortcutsItem::new(item, accelerator));
                    }
                    dialog.add(section);
                }
                dialog.present(Some(&self.window));
            }
            Input::ApplyPreferences => {
                if let Some(dialog) = &self.preferences {
                    let changes = dialog.take_changes();
                    let errors = dialog.take_errors();
                    let custom_theme_changed = changes.iter().any(|change| {
                        change.settings_field()
                            == crate::native_preferences::SettingsField::CustomTheme
                    });
                    for change in changes {
                        if change.apply(&mut self.settings).is_ok() {
                            if let Err(_error) = self.settings.save(&self.settings_path) {
                                self.status = "Could not save preferences".into();
                                self.toast("Could not save preferences");
                            } else {
                                self.status = "Preferences saved".into();
                            }
                        } else {
                            self.status = "Preference value is invalid".into();
                            self.toast("Preference value is invalid");
                        }
                    }
                    if custom_theme_changed {
                        self.sync_custom_theme_selection();
                        if self.settings.save(&self.settings_path).is_err() {
                            self.status = "Could not save preferences".into();
                            self.toast("Could not save preferences");
                        }
                    }
                    if !errors.is_empty() {
                        self.status = "Preference value is invalid".into();
                        self.toast("Preference value is invalid");
                    }
                    self.apply_runtime_settings();
                }
            }
            Input::StartBackend => {
                if let Some(startup) = self.backend.as_mut().and_then(Backend::take_startup) {
                    let _ = startup.send(());
                    self.status = "Starting backend".into();
                }
            }
            Input::BackendReady => {
                let events =
                    backend_events::drain_and_convert(self.backend.as_ref(), &self.notifier);
                for event in events {
                    match event {
                        NativeEvent::Link(link) => {
                            if matches!(link, LinkStatus::LoggedOut) {
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
                                for chat in &self.chat_snapshots {
                                    self.notifications.clear_chat(&chat.id);
                                }
                                self.active_chat = None;
                                self.chat_snapshots.clear();
                                self.chat_ids.clear();
                                self.reset_chat_filters(false);
                                self.message_ids.clear();
                                self.message_snapshots.clear();
                                self.editable_messages.clear();
                                self.messages.clear();
                                self.contacts.clear();
                                self.avatars.clear();
                                self.presence.clear();
                                self.typing.clear();
                                self.typing_until.clear();
                                self.composing_until.clear();
                                self.drafts.clear();
                                self.composer =
                                    crate::native_composer::NativeComposerState::default();
                                self.composer_buffer.set_text("");
                                self.draft.clear();
                                self.editing = None;
                                self.reply_to = None;
                                self.pending_edit = None;
                                self.pending_composer_request = None;
                                self.pending_send = None;
                                self.audio.voice_send_pending = false;
                                self.account_receipts_off = false;
                                self.audio.played_voice.clear();
                                self.avatar_requests.clear();
                                self.pending_attachments.clear();
                                self.document_attachments.clear();
                                self.pending_clipboard_images.clear();
                                self.audio.selected_voice = None;
                                self.audio.selected_voice_message = None;
                                self.pending_quote_navigation = None;
                                self.history_complete = false;
                                self.loading_older = false;
                                self.transcript.clear();
                                self.chats_dirty = true;
                                self.sync_chat_projection();
                            }
                            self.qr_texture = match &link {
                                LinkStatus::Unlinked { qr: Some(qr), .. } => qr_texture(qr),
                                _ => None,
                            };
                            if self.resuming {
                                if link.is_connected() {
                                    self.resuming = !self.resume_dropped;
                                } else {
                                    self.resume_dropped = true;
                                }
                            }
                            self.link = link;
                            (self.page_title, self.status) = link_page(&self.link);
                        }
                        NativeEvent::Syncing(syncing) => self.syncing = syncing,
                        NativeEvent::Chats(chats) => {
                            self.apply_chat_changes(vec![ChatChange::Snapshot(chats)]);
                        }
                        NativeEvent::ChatUpdated(chat) => {
                            self.apply_chat_changes(vec![ChatChange::Update(*chat)]);
                            self.read_open_chat();
                        }
                        // An empty list clears contacts on logout; otherwise
                        // the backend sends the full set or single updates.
                        NativeEvent::Contacts(contacts) => {
                            if contacts.is_empty() {
                                self.contacts.clear();
                            }
                            self.contacts.extend(
                                contacts
                                    .into_iter()
                                    .map(|contact| (contact.id.clone(), contact)),
                            );
                            if !self.message_ids.is_empty() {
                                self.sync_transcript();
                                self.rebuild_message_rows();
                            }
                        }
                        NativeEvent::Messages {
                            chat,
                            messages,
                            older,
                            complete,
                        } => {
                            if self.active_chat.as_deref() == Some(&chat) {
                                self.history_complete = complete;
                                self.loading_older = false;
                            }
                            self.apply_messages(chat.clone(), messages, older);
                            if let Some((target_chat, target_id)) =
                                self.pending_quote_navigation.clone()
                                && target_chat == chat
                                && self.message_ids.contains(&target_id)
                            {
                                self.pending_quote_navigation = None;
                                self.scroll_message_into_view(&target_id);
                            }
                        }
                        NativeEvent::OlderFetched { chat, more } => {
                            if self.active_chat.as_deref() == Some(&chat) {
                                self.loading_older = false;
                                self.history_complete = !more;
                                if !more {
                                    self.status = "No older messages available".into();
                                }
                            }
                        }
                        NativeEvent::Stickers {
                            saved,
                            packs,
                            recent,
                        } => {
                            let changed = saved != self.favorite_stickers
                                || packs != self.sticker_packs
                                || recent != self.recent_stickers;
                            let packs_changed = packs != self.sticker_packs;
                            self.favorite_stickers = saved;
                            self.sticker_packs = packs;
                            self.recent_stickers = recent;
                            if changed {
                                self.refresh_sticker_picker(&sender);
                            }
                            if !packs_changed {
                                continue;
                            }
                            self.sticker_emojis.clear();
                            for pack in &self.sticker_packs {
                                for sticker in &pack.stickers {
                                    if let Ok(bytes) = std::fs::read(sticker) {
                                        let emojis = crate::sticker_meta::emojis(&bytes);
                                        if !emojis.is_empty() {
                                            self.sticker_emojis.insert(sticker.clone(), emojis);
                                        }
                                    }
                                }
                            }
                        }
                        NativeEvent::MessageUpdated(message) => self.message_updated(*message),
                        NativeEvent::Edited { chat, id, success } => self.edited(chat, id, success),
                        NativeEvent::Sent { chat, success } => {
                            if self.audio.voice_send_pending {
                                self.audio.voice_send_pending = false;
                                self.status = if success {
                                    "Voice message sent".into()
                                } else {
                                    "Voice message could not be sent".into()
                                };
                            } else {
                                self.sent(chat, success);
                            }
                        }
                        NativeEvent::AttachmentCompleted {
                            chat,
                            path,
                            success,
                        } => self.attachment_completed(chat, path, success),
                        NativeEvent::Media {
                            chat,
                            message,
                            result,
                        } => self.media_completed(&chat, &message, result),
                        NativeEvent::MessageDeleted { chat, id } => {
                            self.message_deleted(&chat, &id)
                        }
                        NativeEvent::Incoming { chat, message } => {
                            if self.settings.auto_download
                                && should_auto_download(message.content.media())
                                && let Some(backend) = &self.backend
                            {
                                backend.send(crate::backend::Command::Download {
                                    chat: chat.clone(),
                                    message: message.id.clone(),
                                });
                                if let Some(media) = self
                                    .message_snapshots
                                    .get_mut(&message.id)
                                    .and_then(|known| known.content.media_mut())
                                {
                                    media.state = crate::model::MediaState::Downloading;
                                }
                            }
                            let known = self.chat_snapshots.iter().find(|known| known.id == chat);
                            if notification_should_show(
                                self.settings.notifications,
                                self.window.is_active(),
                                self.active_chat.as_deref(),
                                &chat,
                                known,
                            ) {
                                let title = known.map_or_else(
                                    || {
                                        sender_label(
                                            message.sender_name.as_deref(),
                                            &message.sender,
                                        )
                                    },
                                    |known| known.name.clone(),
                                );
                                let body = if !self.settings.notification_previews {
                                    "New message".to_owned()
                                } else if known.is_some_and(crate::model::Chat::is_group) {
                                    format!(
                                        "{}: {}",
                                        sender_label(
                                            message.sender_name.as_deref(),
                                            &message.sender
                                        ),
                                        message.summary()
                                    )
                                } else {
                                    message.summary()
                                };
                                if let Err(error) = self.notifications.show(
                                    &chat,
                                    &title,
                                    &body,
                                    self.avatars.get(&chat).map(std::path::PathBuf::as_path),
                                ) {
                                    log::warn!("could not show a notification: {error}");
                                }
                            }
                        }
                        NativeEvent::Typing {
                            chat,
                            sender: typing_sender,
                            composing,
                        } => {
                            if composing {
                                let name = self
                                    .chat_snapshots
                                    .iter()
                                    .find(|known| known.id == typing_sender)
                                    .map(|known| known.name.clone())
                                    .unwrap_or_else(|| "Someone".into());
                                self.typing.insert(chat.clone(), name);
                                self.typing_until.insert(
                                    chat.clone(),
                                    std::time::Instant::now() + std::time::Duration::from_secs(5),
                                );
                                let input = sender.clone();
                                gtk::glib::timeout_add_local_once(
                                    std::time::Duration::from_secs(5),
                                    move || {
                                        input.input(Input::ClearTyping(chat));
                                    },
                                );
                            } else {
                                self.typing.remove(&chat);
                                self.typing_until.remove(&chat);
                            }
                        }
                        NativeEvent::Presence {
                            id,
                            online,
                            last_seen,
                        } => {
                            self.presence.insert(id, (online, last_seen));
                        }
                        NativeEvent::Avatar { id, path } => {
                            if let Some(path) = path {
                                self.avatars.insert(id, path);
                            } else {
                                self.avatars.remove(&id);
                            }
                            self.chats_dirty = true;
                        }
                        NativeEvent::ReceiptsPrivacy { disabled } => {
                            self.account_receipts_off = disabled
                        }
                        NativeEvent::ContactAbout { id, about } => {
                            if let Some((chat, row)) = &self.info_about
                                && chat == &id
                                && let Some(about) = about
                            {
                                row.set_subtitle(&about);
                                row.set_visible(true);
                            }
                        }
                        NativeEvent::ContactReady { id, name } => {
                            let display_name = name
                                .filter(|name| !name.trim().is_empty())
                                .unwrap_or_else(|| crate::util::phone(&id));
                            self.apply_chat_changes(vec![ChatChange::Update(
                                crate::model::Chat::new(id.clone(), display_name),
                            )]);
                            self.status = "Contact is on WhatsApp".into();
                            sender.input(Input::OpenChatId(id));
                        }
                        NativeEvent::Info(message) => {
                            self.status = message.clone();
                            self.toast(&message);
                        }
                        NativeEvent::Error(error) => {
                            self.loading_older = false;
                            log::warn!("native backend operation failed");
                            let feedback = sanitized_error_feedback(&error);
                            self.status = feedback.into();
                            self.toast(feedback);
                        }
                    }
                }
                if self.chats_dirty && self.chat_ids.is_empty() {
                    self.flush_chats();
                } else if self.chats_dirty && !self.chats_flush_scheduled {
                    // ponytail: fixed 200 ms coalescing window; make it adaptive if
                    // very large archives still stutter during sync.
                    self.chats_flush_scheduled = true;
                    let flush = sender.clone();
                    gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(200),
                        move || flush.input(Input::FlushChats),
                    );
                }
                self.poll_theme_catalog();
            }
            Input::SelectChat(position) => {
                let Some(chat) = self.chat_ids.get(position as usize).cloned() else {
                    return;
                };
                if self
                    .split_view
                    .as_ref()
                    .is_some_and(adw::OverlaySplitView::is_collapsed)
                {
                    self.sidebar_visible = false;
                }
                if self.active_chat.as_deref() != Some(&chat) {
                    self.cancel_portal_requests();
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
                }
                self.notifications.clear_chat(&chat);
                if let Some(previous) = self.active_chat.as_deref() {
                    if self
                        .editing
                        .as_ref()
                        .is_some_and(|(editing_chat, _)| editing_chat == previous)
                    {
                        self.composer.set_draft(
                            previous,
                            self.drafts.get(previous).cloned().unwrap_or_default(),
                        );
                    }
                    self.composer.cancel_context(previous);
                }
                self.chat_projection.select(chat.clone());
                self.active_chat = Some(chat.clone());
                self.mark_open_chat();
                self.reply_to = None;
                self.editing = None;
                self.pending_edit = None;
                self.draft = self.composer.draft(&chat).to_owned();
                self.composer_buffer.set_text(&self.draft);
                self.messages.clear();
                self.message_target = None;
                self.opened_unread = self
                    .chat_snapshots
                    .iter()
                    .find(|known| known.id == chat)
                    .map_or(0, |known| known.unread as usize);
                self.unread_marker = None;
                self.history_complete = false;
                self.loading_older = false;
                self.message_ids.clear();
                self.message_snapshots.clear();
                self.editable_messages.clear();
                self.transcript.clear();
                self.audio.selected_voice = None;
                self.audio.selected_voice_message = None;
                self.page_title = self
                    .chat_snapshots
                    .iter()
                    .find(|known| known.id == chat)
                    .map(|known| known.name.clone())
                    .unwrap_or_else(|| "Conversation".into());
                self.status = "Loading messages".into();
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::MarkRead {
                        chat: chat.clone(),
                        receipts: self.settings.send_read_receipts && !self.account_receipts_off,
                    });
                    backend.send(crate::backend::Command::LoadChat { chat, before: None });
                }
                self.focus_composer();
            }
            Input::HighlightChat(position) => {
                if let Some(chat) = self.chat_ids.get(position as usize) {
                    self.chat_projection.select(chat.clone());
                }
            }
            Input::LoadOlder => {
                let Some(chat) = self.active_chat.clone() else {
                    return;
                };
                if self.loading_older {
                    return;
                }
                let Some(backend) = &self.backend else {
                    self.status = "Backend unavailable".into();
                    return;
                };
                self.loading_older = true;
                if self.history_complete {
                    backend.send(crate::backend::Command::FetchOlder(chat.clone()));
                } else if let Some(oldest) = self
                    .message_snapshots
                    .values()
                    .filter(|message| message.chat == chat)
                    .min_by_key(|message| (message.timestamp, message.id.as_str()))
                {
                    backend.send(crate::backend::Command::LoadChat {
                        chat: chat.clone(),
                        before: Some((oldest.timestamp, oldest.id.clone())),
                    });
                } else {
                    self.loading_older = false;
                }
            }
            Input::OpenChatId(chat) => {
                self.window.present();
                let archived = self
                    .chat_snapshots
                    .iter()
                    .any(|known| known.id == chat && known.archived);
                self.reset_chat_filters(archived);
                self.sync_chat_projection();
                if let Some(position) = self.chat_ids.iter().position(|id| id == &chat) {
                    sender.input(Input::SelectChat(position as u32));
                }
            }
            Input::SearchChats(query) => {
                self.chat_projection.set_query(query);
                self.sync_chat_projection();
            }
            Input::SetUnreadFilter(enabled) => {
                self.chat_filters.unread_only = enabled;
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::SetPinnedFilter(enabled) => {
                self.chat_filters.pinned_only = enabled;
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::SetArchivedFilter(enabled) => {
                self.chat_filters.archive = if enabled {
                    crate::native_chat_list::ArchiveFilter::Only
                } else {
                    crate::native_chat_list::ArchiveFilter::Exclude
                };
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::SetMutedFilter(enabled) => {
                self.chat_filters.muted = if enabled {
                    crate::native_chat_list::MutedFilter::Only
                } else {
                    crate::native_chat_list::MutedFilter::All
                };
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::SetChatKindFilter(kind) => {
                kind.apply(&mut self.chat_filters);
                self.chat_projection.set_filters(self.chat_filters);
                self.sync_chat_projection();
            }
            Input::ToggleSelectedPin => {
                if let Some(chat) = self.selected_chat().cloned()
                    && let Some(backend) = &self.backend
                {
                    backend.send(crate::backend::Command::SetPinned(chat.id, !chat.pinned));
                }
            }
            Input::ToggleSelectedArchive => {
                if let Some(chat) = self.selected_chat() {
                    if chat.archived {
                        sender.input(Input::ArchiveChat(chat.id.clone()));
                    } else {
                        show_archive_confirmation(
                            &self.window,
                            &dialog_action_callback(&sender),
                            chat,
                        );
                    }
                }
            }
            Input::ArchiveChat(id) => {
                if let Some(chat) = self.chat_snapshots.iter().find(|chat| chat.id == id)
                    && let Some(backend) = &self.backend
                {
                    backend.send(crate::backend::Command::SetArchived(id, !chat.archived));
                }
            }
            Input::ToggleSelectedMute => {
                if let Some(chat) = self.selected_chat().cloned()
                    && let Some(backend) = &self.backend
                {
                    let muted = chat.muted(crate::util::now());
                    backend.send(crate::backend::Command::SetMuted(
                        chat.id,
                        (!muted).then_some(i64::MAX),
                    ));
                }
            }
            Input::Reconnect => {
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::Reconnect);
                    self.status = "Reconnecting to WhatsApp".into();
                }
            }
            Input::Resumed => {
                let linked = matches!(
                    self.link,
                    LinkStatus::Connecting
                        | LinkStatus::Connected
                        | LinkStatus::Disconnected { .. }
                );
                if let (true, Some(backend)) = (linked, &self.backend) {
                    // The socket died with the suspend but may not know yet;
                    // reconnecting now brings the missed messages in.
                    backend.send(crate::backend::Command::Reconnect);
                    self.resuming = true;
                    self.resume_dropped = false;
                    self.status = "Refreshing messages".into();
                    let input = sender.clone();
                    gtk::glib::timeout_add_local_once(RESUME_REFRESH_LIMIT, move || {
                        input.input(Input::ResumeSettled)
                    });
                }
            }
            Input::ResumeSettled => self.resuming = false,
            Input::UnlinkConfirmed => {
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::Unlink);
                    self.status = "Unlinking this computer".into();
                } else {
                    self.status = "Backend unavailable".into();
                }
            }
            Input::ShowChatInfo => {
                if let Some(chat) = self
                    .active_chat
                    .as_ref()
                    .and_then(|id| self.chat_snapshots.iter().find(|chat| &chat.id == id))
                {
                    self.info_about = show_chat_info_dialog(
                        &self.window,
                        chat,
                        &self.contacts,
                        self.avatars.get(&chat.id).map(std::path::PathBuf::as_path),
                        self.presence.get(&chat.id).copied(),
                        &self.chat_snapshots,
                        &self.avatars,
                    );
                    if self.info_about.is_some()
                        && let Some(backend) = &self.backend
                    {
                        backend.send(crate::backend::Command::ContactAbout {
                            id: chat.id.clone(),
                        });
                    }
                }
            }
            Input::ShowNewChat => show_new_chat_dialog(
                &self.window,
                &dialog_action_callback(&sender),
                new_chat_contacts(&self.contacts),
            ),
            Input::StartChat { id, name } => {
                if !self.chat_snapshots.iter().any(|chat| chat.id == id) {
                    self.apply_chat_changes(vec![ChatChange::Update(crate::model::Chat::new(
                        id.clone(),
                        name,
                    ))]);
                }
                sender.input(Input::OpenChatId(id));
            }
            Input::Tray(action) => {
                use crate::native_tray::TrayAction;
                match action {
                    TrayAction::ToggleWindow if self.window.is_visible() => {
                        self.window.set_visible(false)
                    }
                    TrayAction::ToggleWindow => self.window.present(),
                    TrayAction::NewChat => {
                        self.window.present();
                        sender.input(Input::ShowNewChat);
                    }
                    TrayAction::ToggleNotifications => {
                        self.settings.notifications = !self.settings.notifications;
                        if self.settings.save(&self.settings_path).is_err() {
                            self.status = "Could not save preferences".into();
                        }
                    }
                    TrayAction::Preferences => {
                        self.window.present();
                        sender.input(Input::ShowPreferences);
                    }
                    TrayAction::Quit => sender.input(Input::Quit),
                    TrayAction::Shown(shown) => {
                        self.tray_shown = shown;
                        // Without a tray there is no way back to a hidden window.
                        if !shown {
                            self.window.present();
                        }
                    }
                }
            }
            Input::WindowVisibilityChanged => {}
            Input::TogglePhoneLinking => {
                // Leaving a requested or shown code goes back to the QR code.
                if self.pairing_requested() || self.pair_code().is_some() {
                    self.phone_linking = false;
                    if let Some(backend) = &self.backend {
                        backend.send(crate::backend::Command::CancelPhonePairing);
                    }
                } else {
                    self.phone_linking = !self.phone_linking;
                }
            }
            Input::CopyPairCode => {
                if let Some(code) = self.pair_code().map(str::to_owned) {
                    crate::native_portals::NativePortals::write_clipboard_text(
                        &self.window.clipboard(),
                        &code,
                    );
                    self.toast("Code copied");
                }
            }
            Input::FlushChats => {
                self.chats_flush_scheduled = false;
                self.flush_chats();
            }
            Input::PairWithPhone(phone) => {
                let Some(digits) = normalized_phone(&phone) else {
                    self.status = "Enter valid phone number with country code".into();
                    self.toast("Enter valid phone number with country code");
                    return;
                };
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::PairWithPhone(digits));
                    self.status = "Requesting a pairing code from WhatsApp".into();
                } else {
                    self.status = "Backend unavailable".into();
                }
            }
            Input::NewContact { phone, name } => {
                let Some(digits) = normalized_phone(&phone) else {
                    self.status = "Enter valid phone number with country code".into();
                    self.toast("Enter valid phone number with country code");
                    return;
                };
                let name = name.filter(|name| !name.trim().is_empty());
                // A phone number in a message: an existing chat needs no lookup.
                let id = format!("{digits}@s.whatsapp.net");
                if name.is_none() && self.chat_snapshots.iter().any(|chat| chat.id == id) {
                    sender.input(Input::OpenChatId(id));
                    return;
                }
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::NewContact {
                        phone: digits,
                        first_name: name.clone(),
                        full_name: name,
                        to_phone: self.settings.save_contacts_to_phone,
                    });
                    self.status = "Checking contact on WhatsApp".into();
                } else {
                    self.status = "Backend unavailable".into();
                }
            }
            Input::ShowMessageMenu { id, x, y } => self.show_message_menu(id, x, y, &sender),
            Input::SelectMessage(position) => {
                if self.active_chat.is_none() {
                    return;
                }
                let Some(message) = self.message_ids.get(position as usize) else {
                    return;
                };
                self.audio.selected_voice_message = self
                    .message_snapshots
                    .get(message)
                    .and_then(|message| self.project_voice(message).map(|_| message.id.clone()));
                self.audio.selected_voice = self
                    .message_snapshots
                    .get(message)
                    .and_then(|message| self.project_voice(message));
            }
            Input::OpenQuoted => {
                let Some(message) = self.selected_message().cloned() else {
                    return;
                };
                let Some(quoted_id) = message.quoted.map(|quoted| quoted.id) else {
                    return;
                };
                if self.message_ids.contains(&quoted_id) {
                    self.scroll_message_into_view(&quoted_id);
                } else if let Some(backend) = &self.backend {
                    self.pending_quote_navigation = Some((message.chat.clone(), quoted_id.clone()));
                    backend.send(crate::backend::Command::LoadUntil {
                        chat: message.chat,
                        id: quoted_id,
                        before: (message.timestamp, message.id),
                    });
                    self.status = "Loading quoted message".into();
                }
            }
            Input::ReplySelected => self.reply_selected(),
            Input::EditSelected => self.edit_selected(),
            Input::CancelReply => self.cancel_reply(),
            Input::CancelEdit => self.cancel_edit(),
            Input::DraftChanged(text) => self.draft_changed(text, &sender),
            Input::ClearTyping(chat) => {
                if self
                    .typing_until
                    .get(&chat)
                    .is_some_and(|until| *until <= std::time::Instant::now())
                {
                    self.typing.remove(&chat);
                    self.typing_until.remove(&chat);
                }
            }
            Input::StopComposing(chat) => {
                if self
                    .composing_until
                    .get(&chat)
                    .is_some_and(|until| *until <= std::time::Instant::now())
                {
                    self.composing_until.remove(&chat);
                    if self.settings.send_typing
                        && let Some(backend) = &self.backend
                    {
                        backend.send(crate::backend::Command::Composing {
                            chat,
                            composing: false,
                        });
                    }
                }
            }
            Input::InsertEmoji(emoji) => {
                let mut end = self.composer_buffer.end_iter();
                self.composer_buffer.insert(&mut end, &emoji);
            }
            Input::ShowStickerPicker => {
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::RecentStickers);
                }
                let anchor = self.composer_view.model().sticker_button();
                // Anchored to the button, the popover flips above it near screen edges.
                let popover = gtk::Popover::builder()
                    .position(gtk::PositionType::Top)
                    .css_classes(["zaptide-sticker-picker"])
                    .build();
                popover.connect_closed(|popover| {
                    let popover = popover.clone();
                    gtk::glib::idle_add_local_once(move || popover.unparent());
                });
                let (content, stack) = sticker_picker_content(
                    &self.recent_stickers,
                    &self.favorite_stickers,
                    &dialog_action_callback(&sender),
                );
                popover.set_child(Some(&content));
                popover.set_parent(&anchor);
                popover.popup();
                self.sticker_picker = Some((popover, stack));
            }
            Input::ShowPollCreator => {
                show_poll_dialog(&self.window, &dialog_action_callback(&sender))
            }
            Input::SendSticker(path) => {
                if let Some((popover, _)) = self.sticker_picker.take() {
                    popover.popdown();
                }
                if let (Some(chat), Some(backend)) = (&self.active_chat, &self.backend) {
                    backend.send(crate::backend::Command::SendSticker {
                        chat: chat.clone(),
                        path,
                    });
                }
            }
            Input::InsertMention => {
                let participants = self
                    .active_chat
                    .as_deref()
                    .and_then(|chat| self.chat_snapshots.iter().find(|known| known.id == chat))
                    .map(|chat| chat.participants.clone())
                    .unwrap_or_default();
                if participants.is_empty() {
                    return;
                }
                let mut people: Vec<_> = participants
                    .into_iter()
                    .enumerate()
                    .map(|(index, id)| {
                        let name = self.participant_label(&id, index);
                        let phone = crate::model::phone_of(&id).map(crate::util::phone);
                        (id, name, phone)
                    })
                    .collect();
                people.sort_by_cached_key(|(_, name, _)| name.to_lowercase());
                show_mention_dialog(
                    &self.window,
                    &dialog_action_callback(&sender),
                    people,
                    &self.avatars,
                );
            }
            Input::InsertMentionId(participant) => {
                let known = self
                    .active_chat
                    .as_deref()
                    .and_then(|chat| self.chat_snapshots.iter().find(|known| known.id == chat))
                    .is_some_and(|chat| chat.participants.contains(&participant));
                if known {
                    let token = participant.split('@').next().unwrap_or_default();
                    let mut end = self.composer_buffer.end_iter();
                    self.composer_buffer.insert(&mut end, &format!("@{token} "));
                }
            }
            Input::PasteClipboardImage => self.paste_clipboard_image(&sender),
            Input::AttachDropped(paths) => attachments::attach_dropped(self, paths, &sender),
            Input::ClipboardImageReady { chat, pixels } => {
                self.stage_clipboard_image(chat, pixels);
                self.show_attachment_preview(&sender);
            }
            Input::OpenSelectedUri => self.open_selected_uri(&sender),
            Input::PortalUriFinished(success) => {
                self.status = if success {
                    "Link opened".into()
                } else {
                    "Could not open link".into()
                };
            }
            Input::SaveSelectedAttachment => self.save_selected_attachment(&sender),
            Input::ShowSelectedInFolder => {
                if let Some(path) = self
                    .selected_message()
                    .and_then(|message| message.content.media())
                    .and_then(|media| media.path.clone())
                {
                    crate::native_media_widgets::show_in_folder(&self.window, &path);
                }
            }
            Input::SaveAttachmentFinished(success) => {
                self.status = if success {
                    "Attachment saved".into()
                } else {
                    "Could not save attachment".into()
                };
                if !success {
                    self.toast("Could not save attachment");
                }
            }
            Input::PortalActionFinished(error) => {
                self.status = error;
                self.toast("Clipboard or portal action failed");
            }
            Input::PickAttachments { gallery } => {
                attachments::pick_attachments(self, gallery, &sender)
            }
            Input::AttachmentsPicked {
                chat,
                paths,
                documents,
            } => attachments::attachments_picked(self, chat, paths, documents, &sender),
            Input::ClearAttachments => attachments::clear_attachments(self),
            Input::CopyTranscript => self.copy_transcript(),
            Input::CopySelectedText => {
                if let Some(text) = self.selected_message().and_then(message_text) {
                    crate::native_portals::NativePortals::write_clipboard_text(
                        &self.window.clipboard(),
                        &text,
                    );
                    self.toast("Copied");
                }
            }
            Input::ActivateVoice => self.activate_voice(&sender),
            Input::CycleVoiceSpeed => self.cycle_voice_speed(),
            Input::SeekVoice(fraction) => self.seek_voice(fraction, &sender),
            Input::AudioControl { id, intent } => self.audio_control(&id, intent, &sender),
            Input::AudioWaveformReady { chat, id, bars } => {
                self.waveform_ready(chat, id, bars, &sender);
            }
            Input::MediaAction(action) => match action {
                crate::native_media::NativeMediaAction::Download { message, .. } => {
                    self.activate_attachment(&message)
                }
                crate::native_media::NativeMediaAction::AnswerButton {
                    chat,
                    message,
                    button,
                } => {
                    if let Some(backend) = &self.backend {
                        backend.send(crate::backend::Command::AnswerButton {
                            chat,
                            message,
                            button,
                        });
                    } else {
                        self.status = "Backend unavailable".into();
                    }
                }
                crate::native_media::NativeMediaAction::AnswerListRow { chat, message, row } => {
                    if let Some(backend) = &self.backend {
                        backend.send(crate::backend::Command::AnswerListRow { chat, message, row });
                    } else {
                        self.status = "Backend unavailable".into();
                    }
                }
                crate::native_media::NativeMediaAction::Open(path) => {
                    if let Some(id) = self
                        .message_snapshots
                        .values()
                        .find(|message| {
                            message
                                .content
                                .media()
                                .and_then(|media| media.path.as_ref())
                                == Some(&path)
                        })
                        .map(|message| message.id.clone())
                    {
                        self.activate_attachment(&id);
                    }
                }
            },
            Input::ActivateSelectedAttachment => {
                if let Some(id) = self.selected_message_id() {
                    self.activate_attachment(&id);
                }
            }
            Input::ReactSelected(emoji) => self.react_selected(emoji),
            Input::ReactTo { id, emoji } => {
                self.message_target = Some(id);
                self.react_selected(emoji);
            }
            Input::ShowForward => {
                let mut chats: Vec<_> = self
                    .chat_snapshots
                    .iter()
                    .filter(|chat| {
                        forwardable_chat(chat)
                            && !chat.archived
                            && self.active_chat.as_ref() != Some(&chat.id)
                    })
                    .cloned()
                    .collect();
                chats.sort_by_key(|chat| std::cmp::Reverse(chat.last_activity));
                let summary = self
                    .selected_message()
                    .map(crate::model::Message::summary)
                    .unwrap_or_default();
                show_forward_dialog(
                    &self.window,
                    &dialog_action_callback(&sender),
                    &summary,
                    chats,
                    &self.avatars,
                );
            }
            Input::ForwardSelected(destination) => self.forward_selected(destination),
            Input::DeleteSelected(everyone) => {
                let dialog = adw::AlertDialog::builder()
                    .heading("Delete message?")
                    .body(if everyone {
                        "Delete this message for everyone?"
                    } else {
                        "Delete this message for you?"
                    })
                    .build();
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("delete", "Delete");
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                let input = sender.clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "delete" {
                        input.input(Input::ConfirmDelete(everyone));
                    }
                });
                dialog.present(Some(&self.window));
            }
            Input::ConfirmDelete(everyone) => self.delete_selected(everyone),
            Input::VoteOption(choice) => {
                self.poll_choice = choice;
                self.vote_selected();
            }
            Input::CreatePoll(draft) => self.create_poll(draft),
            Input::Recording(intent) => {
                self.recording_action(intent);
                self.audio
                    .recording_meter
                    .set_levels(&self.audio.media.recent_recording_levels(crate::voice::BARS));
                self.schedule_voice_poll(&sender);
            }
            Input::PollVoice => {
                self.poll_voice(&sender);
            }
            Input::SendText(text) => self.send_text(text),
            Input::Close => {
                self.cancel_portal_requests();
                if self.settings.keep_running_in_background && self.tray_shown {
                    self.window.set_visible(false);
                } else if self.settings.keep_running_in_background {
                    self.present_quit_confirmation_dialog(sender);
                } else {
                    self.request_shutdown(sender);
                }
            }
            Input::Quit => {
                self.cancel_portal_requests();
                self.request_shutdown(sender);
            }
            Input::ShutdownComplete => {
                self.status = "Shutdown complete".into();
                relm4::main_application().quit();
            }
        }
    }
}
