use super::*;

pub(super) fn drain_and_convert(
    backend: Option<&Backend>,
    notifier: &EventNotifier,
) -> Vec<NativeEvent> {
    let mut events = Vec::new();
    if let Some(backend) = backend {
        drain_backend_events(backend, notifier, |event| match event {
            Event::Link(status) => events.push(NativeEvent::Link(status)),
            Event::Syncing(syncing) => events.push(NativeEvent::Syncing(syncing)),
            Event::Chats(rows) => events.push(NativeEvent::Chats(rows)),
            Event::ChatUpdated(chat) => events.push(NativeEvent::ChatUpdated(chat)),
            Event::Contacts(contacts) => events.push(NativeEvent::Contacts(contacts)),
            Event::Messages {
                chat,
                messages,
                older,
                complete,
            } => events.push(NativeEvent::Messages {
                chat,
                messages,
                older,
                complete,
            }),
            Event::OlderFetched { chat, more } => {
                events.push(NativeEvent::OlderFetched { chat, more })
            }
            Event::Stickers {
                saved,
                packs,
                recent,
            } => events.push(NativeEvent::Stickers {
                saved,
                packs,
                recent,
            }),
            Event::MessageUpdated(message) => events.push(NativeEvent::MessageUpdated(message)),
            Event::Edited { chat, id, success } => {
                events.push(NativeEvent::Edited { chat, id, success })
            }
            Event::Sent { chat, success } => events.push(NativeEvent::Sent { chat, success }),
            Event::AttachmentCompleted {
                chat,
                path,
                success,
                ..
            } => events.push(NativeEvent::AttachmentCompleted {
                chat,
                path,
                success,
            }),
            Event::Media {
                chat,
                message,
                result,
            } => events.push(NativeEvent::Media {
                chat,
                message,
                result,
            }),
            Event::ReceiptsPrivacy { disabled } => {
                events.push(NativeEvent::ReceiptsPrivacy { disabled })
            }
            Event::ContactReady { id, name } => events.push(NativeEvent::ContactReady { id, name }),
            Event::ContactAbout { id, about } => {
                events.push(NativeEvent::ContactAbout { id, about })
            }
            Event::Info(message) => events.push(NativeEvent::Info(message)),
            Event::MessageDeleted { chat, id } => {
                events.push(NativeEvent::MessageDeleted { chat, id })
            }
            Event::Incoming { chat, message } => {
                events.push(NativeEvent::Incoming { chat, message })
            }
            Event::Typing {
                chat,
                sender,
                composing,
            } => events.push(NativeEvent::Typing {
                chat,
                sender,
                composing,
            }),
            Event::Presence {
                id,
                online,
                last_seen,
            } => events.push(NativeEvent::Presence {
                id,
                online,
                last_seen,
            }),
            Event::Avatar {
                id,
                full: false,
                path,
            } => events.push(NativeEvent::Avatar { id, path }),
            Event::Error(error) => events.push(NativeEvent::Error(error)),
            _ => {}
        });
    }
    events
}

pub(super) fn handle_backend_ready(
    app: &mut NativeApplication,
    sender: ComponentSender<NativeApplication>,
) {
    let events = drain_and_convert(app.backend.as_ref(), &app.notifier);
    for event in events {
        match event {
            NativeEvent::Link(link) => {
                if matches!(link, LinkStatus::LoggedOut) {
                    clear_avatar_textures();
                    app.audio.media.stop_playback();
                    app.audio.playing_audio = None;
                    app.audio.waveform_queue.clear();
                    if let Some(cancel) = app.audio.waveform_cancel.take() {
                        cancel.store(true, std::sync::atomic::Ordering::Release);
                    }
                    app.audio.audio_waveforms.clear();
                    app.audio.waveform_attempted.clear();
                    app.audio.audio_errors.clear();
                    app.audio.audio_registry.borrow_mut().clear();
                    for chat in &app.chat_snapshots {
                        app.notifications.clear_chat(&chat.id);
                    }
                    app.active_chat = None;
                    app.chat_snapshots.clear();
                    app.chat_ids.clear();
                    app.reset_chat_filters(false);
                    app.message_ids.clear();
                    app.message_snapshots.clear();
                    app.editable_messages.clear();
                    app.messages.clear();
                    app.contacts.clear();
                    app.avatars.clear();
                    app.presence.clear();
                    app.typing.clear();
                    app.typing_until.clear();
                    app.composing_until.clear();
                    app.drafts.clear();
                    app.composer = crate::native_composer::NativeComposerState::default();
                    app.composer_buffer.set_text("");
                    app.draft.clear();
                    app.editing = None;
                    app.reply_to = None;
                    app.pending_edit = None;
                    app.pending_composer_request = None;
                    app.pending_send = None;
                    app.audio.voice_send_pending = false;
                    app.account_receipts_off = false;
                    app.audio.played_voice.clear();
                    app.avatar_requests.clear();
                    app.pending_attachments.clear();
                    app.document_attachments.clear();
                    app.pending_clipboard_images.clear();
                    app.audio.selected_voice = None;
                    app.audio.selected_voice_message = None;
                    app.pending_quote_navigation = None;
                    app.history_complete = false;
                    app.loading_older = false;
                    app.recent_messages_pending = false;
                    app.transcript.clear();
                    app.chats_dirty = true;
                    app.sync_chat_projection();
                }
                app.qr_texture = match &link {
                    LinkStatus::Unlinked { qr: Some(qr), .. } => qr_texture(qr),
                    _ => None,
                };
                if app.resuming {
                    if link.is_connected() {
                        app.resuming = !app.resume_dropped;
                    } else {
                        app.resume_dropped = true;
                    }
                }
                app.link = link;
                (app.page_title, app.status) = link_page(&app.link);
            }
            NativeEvent::Syncing(syncing) => app.syncing = syncing,
            NativeEvent::Chats(chats) => {
                app.apply_chat_changes(vec![ChatChange::Snapshot(chats)]);
            }
            NativeEvent::ChatUpdated(chat) => {
                app.apply_chat_changes(vec![ChatChange::Update(*chat)]);
                app.read_open_chat();
            }
            // An empty list clears contacts on logout; otherwise
            // the backend sends the full set or single updates.
            NativeEvent::Contacts(contacts) => {
                if contacts.is_empty() {
                    app.contacts.clear();
                }
                app.contacts.extend(
                    contacts
                        .into_iter()
                        .map(|contact| (contact.id.clone(), contact)),
                );
                if !app.message_ids.is_empty() {
                    app.sync_transcript();
                    app.rebuild_message_rows(false);
                }
            }
            NativeEvent::Messages {
                chat,
                messages,
                older,
                complete,
            } => {
                if app.active_chat.as_deref() == Some(&chat) {
                    app.history_complete = complete;
                    app.loading_older = false;
                }
                app.apply_messages(chat.clone(), messages, older);
                if let Some((target_chat, target_id)) = app.pending_quote_navigation.clone()
                    && target_chat == chat
                    && app.message_ids.contains(&target_id)
                {
                    app.pending_quote_navigation = None;
                    app.scroll_message_into_view(&target_id);
                }
            }
            NativeEvent::OlderFetched { chat, more } => {
                if app.active_chat.as_deref() == Some(&chat) {
                    app.loading_older = false;
                    app.history_complete = !more;
                    if !more {
                        app.status = "No older messages available".into();
                    }
                }
            }
            NativeEvent::Stickers {
                saved,
                packs,
                recent,
            } => {
                let changed = saved != app.favorite_stickers
                    || packs != app.sticker_packs
                    || recent != app.recent_stickers;
                let packs_changed = packs != app.sticker_packs;
                app.favorite_stickers = saved;
                app.sticker_packs = packs;
                app.recent_stickers = recent;
                if changed {
                    app.refresh_sticker_picker(&sender);
                }
                if !packs_changed {
                    continue;
                }
                app.sticker_emojis.clear();
                for pack in &app.sticker_packs {
                    for sticker in &pack.stickers {
                        if let Ok(bytes) = std::fs::read(sticker) {
                            let emojis = crate::sticker_meta::emojis(&bytes);
                            if !emojis.is_empty() {
                                app.sticker_emojis.insert(sticker.clone(), emojis);
                            }
                        }
                    }
                }
            }
            NativeEvent::MessageUpdated(message) => app.message_updated(*message),
            NativeEvent::Edited { chat, id, success } => app.edited(chat, id, success),
            NativeEvent::Sent { chat, success } => {
                if app.audio.voice_send_pending {
                    app.audio.voice_send_pending = false;
                    app.status = if success {
                        "Voice message sent".into()
                    } else {
                        "Voice message could not be sent".into()
                    };
                } else {
                    app.sent(chat, success);
                }
            }
            NativeEvent::AttachmentCompleted {
                chat,
                path,
                success,
            } => app.attachment_completed(chat, path, success),
            NativeEvent::Media {
                chat,
                message,
                result,
            } => app.media_completed(&chat, &message, result),
            NativeEvent::MessageDeleted { chat, id } => app.message_deleted(&chat, &id),
            NativeEvent::Incoming { chat, message } => {
                if app.settings.auto_download
                    && should_auto_download(message.content.media())
                    && let Some(backend) = &app.backend
                {
                    backend.send(crate::backend::Command::Download {
                        chat: chat.clone(),
                        message: message.id.clone(),
                    });
                    if let Some(media) = app
                        .message_snapshots
                        .get_mut(&message.id)
                        .and_then(|known| known.content.media_mut())
                    {
                        media.state = crate::model::MediaState::Downloading;
                    }
                }
                let known = app.chat_snapshots.iter().find(|known| known.id == chat);
                if notification_should_show(
                    app.settings.notifications,
                    app.window.is_active(),
                    app.active_chat.as_deref(),
                    &chat,
                    known,
                ) {
                    let title = known.map_or_else(
                        || sender_label(message.sender_name.as_deref(), &message.sender),
                        |known| known.name.clone(),
                    );
                    let body = if !app.settings.notification_previews {
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
                    if let Err(error) = app.notifications.show(
                        &chat,
                        &title,
                        &body,
                        app.avatars.get(&chat).map(std::path::PathBuf::as_path),
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
                    let name = app
                        .chat_snapshots
                        .iter()
                        .find(|known| known.id == chat)
                        .and_then(|known| {
                            known
                                .participants
                                .iter()
                                .position(|id| id == &typing_sender)
                                .map(|index| app.participant_label(&typing_sender, index))
                        })
                        .or_else(|| {
                            app.contacts
                                .get(&typing_sender)
                                .and_then(crate::model::Contact::display_name)
                                .map(str::to_owned)
                        })
                        .or_else(|| {
                            app.chat_snapshots
                                .iter()
                                .find(|known| known.id == chat)
                                .map(|known| known.name.clone())
                        })
                        .unwrap_or_else(|| "Someone".into());
                    let shown = app.active_chat.as_deref() == Some(chat.as_str())
                        && !app.typing.contains_key(&chat);
                    app.typing
                        .insert(chat.clone(), (name, typing_sender.clone()));
                    // Keep the bubble in view, like a new message, when already at the end.
                    if shown
                        && let Some(adjustment) = app.messages.view.vadjustment()
                        && adjustment.value() + adjustment.page_size() >= adjustment.upper() - 48.0
                    {
                        super::super::scroll_to_end(&app.messages.view);
                    }
                    app.typing_until.insert(
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
                    app.typing.remove(&chat);
                    app.typing_until.remove(&chat);
                }
            }
            NativeEvent::Presence {
                id,
                online,
                last_seen,
            } => {
                app.presence.insert(id, (online, last_seen));
            }
            NativeEvent::Avatar { id, path } => {
                let previous = app.avatars.get(&id).cloned();
                if let Some(current) = &path {
                    app.avatars.insert(id.clone(), current.clone());
                } else {
                    app.avatars.remove(&id);
                }
                update_avatar_texture_reference(previous, path);
                app.chats_dirty = true;
            }
            NativeEvent::ReceiptsPrivacy { disabled } => app.account_receipts_off = disabled,
            NativeEvent::ContactAbout { id, about } => {
                if let Some((chat, row)) = &app.info_about
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
                app.apply_chat_changes(vec![ChatChange::Update(crate::model::Chat::new(
                    id.clone(),
                    display_name,
                ))]);
                app.status = "Contact is on WhatsApp".into();
                sender.input(Input::OpenChatId(id));
            }
            NativeEvent::Info(message) => {
                app.status = message.clone();
                app.toast(&message);
            }
            NativeEvent::Error(error) => {
                app.loading_older = false;
                log::warn!("native backend operation failed");
                let feedback = sanitized_error_feedback(&error);
                app.status = feedback.into();
                app.toast(feedback);
            }
        }
    }
    if app.chats_dirty && app.chat_ids.is_empty() {
        app.flush_chats();
    } else if app.chats_dirty && !app.chats_flush_scheduled {
        app.chats_flush_scheduled = true;
        let flush = sender.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
            flush.input(Input::FlushChats)
        });
    }
    app.poll_theme_catalog();
}
