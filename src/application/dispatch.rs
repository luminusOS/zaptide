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
            Input::SidebarShown(shown) => self.sidebar_visible = shown,
            // Collapsing keeps an open conversation on screen; widening resets.
            Input::SplitCollapsed(collapsed) => {
                self.sidebar_visible = !collapsed || self.active_chat.is_none()
            }
            Input::ShowPreferences => {
                let preferences =
                    crate::native_preferences::NativePreferences::from(&self.settings);
                let theme_choices: Vec<_> = self
                    .theme_catalog
                    .picker_themes()
                    .map(|theme| theme.filename.clone())
                    .collect();
                let input_sender = sender.clone();
                let dialog = crate::native_preferences::NativePreferencesDialog::new(
                    &preferences,
                    &theme_choices,
                    move || input_sender.input(Input::ApplyPreferences),
                );
                dialog.present(&self.window);
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
            Input::BackendReady => backend_events::handle_backend_ready(self, sender),
            Input::SelectChat(position) => self.select_chat(position),
            Input::ScrollToRecentMessages => {
                self.recent_messages_pending = false;
                scroll_to_end(&self.messages.view);
            }
            Input::TranscriptAtEnd => self.recent_messages_pending = false,
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
                        // Without a tray there is no way back to a hidden window,
                        // unless the portal lists the app under Background Apps.
                        // GNOME turns the tray extension off while the screen is
                        // locked, which must not pull a hidden window back up.
                        if !shown && !crate::native_background::sandboxed() {
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
            Input::CopyCode(code) => {
                crate::native_portals::NativePortals::write_clipboard_text(
                    &self.window.clipboard(),
                    &code,
                );
                self.toast("Code copied");
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
                if self.message_selection.is_some() {
                    self.toggle_message_selection(position);
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
                if let Some(message) = self.selected_message().cloned() {
                    self.open_quoted(message);
                }
            }
            Input::OpenQuotedOf(id) => {
                if let Some(message) = self.message_snapshots.get(&id).cloned() {
                    self.open_quoted(message);
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
            Input::ShowContactPicker => self.show_contact_picker(&sender),
            Input::ImportContact(target) => self.import_contact(target, &sender),
            Input::ContactFileReady { target, result } => {
                self.contact_file_ready(target, result, &sender)
            }
            Input::SendContact { target, contact } => self.share_contact(target, contact),
            Input::ContactActionFinished(result) => match result {
                Ok(()) => self.toast("Contact opened"),
                Err(error) => {
                    self.status = error.clone();
                    self.toast(&error);
                }
            },
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
                let label = self
                    .active_chat
                    .as_deref()
                    .and_then(|chat| self.chat_snapshots.iter().find(|known| known.id == chat))
                    .and_then(|chat| {
                        self.mention_labels(chat)
                            .into_iter()
                            .find(|candidate| candidate.id == participant)
                    })
                    .map(|candidate| candidate.label);
                if let Some(label) = label {
                    let mut end = self.composer_buffer.end_iter();
                    self.insert_mention(&mut end, &label);
                    self.composer_buffer.place_cursor(&end);
                    self.focus_composer();
                }
            }
            Input::SelectMention(candidate) => {
                let known = self
                    .active_chat
                    .as_deref()
                    .and_then(|chat| self.chat_snapshots.iter().find(|known| known.id == chat))
                    .is_some_and(|chat| chat.participants.contains(&candidate.id));
                let cursor = self.composer_buffer.iter_at_mark(
                    &self
                        .composer_buffer
                        .mark("insert")
                        .expect("insert mark exists"),
                );
                let before = self
                    .composer_buffer
                    .text(&self.composer_buffer.start_iter(), &cursor, true)
                    .to_string();
                let after = self
                    .composer_buffer
                    .text(&cursor, &self.composer_buffer.end_iter(), true)
                    .to_string();
                if known && let Some(query) = crate::native_composer::active_mention_query(&before)
                {
                    let prefix = &before[..before.len() - query.len() - 1];
                    let mut start = self.composer_buffer.start_iter();
                    start.forward_chars(prefix.chars().count() as i32);
                    let mut end = cursor;
                    end.forward_chars(
                        after
                            .chars()
                            .take_while(|character| !character.is_whitespace())
                            .count() as i32,
                    );
                    self.composer_buffer.delete(&mut start, &mut end);
                    self.insert_mention(&mut start, &candidate.label);
                    self.composer_buffer.place_cursor(&start);
                    self.focus_composer();
                }
            }
            Input::PasteClipboardImage => self.paste_clipboard_image(&sender),
            Input::AttachDropped(paths) => attachments::attach_dropped(self, paths),
            Input::ClipboardImageReady { chat, pixels } => {
                self.stage_clipboard_image(chat, pixels);
                self.composer_view.model().focus_text_view();
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
            } => attachments::attachments_picked(self, chat, paths, documents),
            Input::ClearAttachments => attachments::clear_attachments(self),
            Input::RemoveAttachment(path) => attachments::remove_attachment(self, &path),
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
                crate::native_media::NativeMediaAction::CopyContactPhone(number) => {
                    crate::native_portals::NativePortals::write_clipboard_text(
                        &self.window.clipboard(),
                        &number,
                    );
                    self.toast("Phone number copied");
                }
                crate::native_media::NativeMediaAction::OpenContact(contact) => {
                    self.open_contact(contact, &sender)
                }
                crate::native_media::NativeMediaAction::MessageContact { id, name } => {
                    if crate::contact_cards::ContactCard::from_saved(&id, &name).is_some() {
                        sender.input(Input::StartChat { id, name });
                    }
                }
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
                let summary = match self
                    .message_selection
                    .as_ref()
                    .map(|_| self.selected_rows())
                {
                    Some(1) => self
                        .message_selection
                        .as_ref()
                        .and_then(|selection| self.message_snapshots.get(&selection[0]))
                        .map(crate::model::Message::summary)
                        .unwrap_or_default(),
                    Some(count) => format!("{count} messages"),
                    None => self
                        .selected_message()
                        .map(crate::model::Message::summary)
                        .unwrap_or_default(),
                };
                show_forward_dialog(
                    &self.window,
                    &dialog_action_callback(&sender),
                    &summary,
                    chats,
                    &self.avatars,
                );
            }
            Input::ForwardSelected(destination) => self.forward_selected(destination),
            Input::StartSelection => {
                let position = self
                    .selected_message_id()
                    .and_then(|id| self.message_ids.iter().position(|known| *known == id));
                self.message_selection = Some(Vec::new());
                match position {
                    Some(position) => self.toggle_message_selection(position as u32),
                    None => self.rebuild_message_rows(false),
                }
            }
            Input::CancelSelection => {
                if self.message_selection.take().is_some() {
                    self.rebuild_message_rows(false);
                    self.focus_composer();
                }
            }
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
                self.finish_close(sender);
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
