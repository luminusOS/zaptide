use super::*;
use crate::application::conversation::{TranscriptChange, sender_in_window};
use crate::i18n::gettext;

pub(in crate::application) fn poll_error_message(event: &Event) -> Option<String> {
    match event {
        Event::PollCreated {
            error: Some(error), ..
        } if error == "The poll was sent, but its voting key could not be saved." => Some(gettext(
            "The poll was sent, but its voting key could not be saved.",
        )),
        Event::PollCreated { error: Some(_), .. } => {
            Some(gettext("Poll creation could not be completed."))
        }
        Event::PollVoted {
            error: Some(error), ..
        } if error == "Your vote was sent, but could not be saved locally." => Some(gettext(
            "Your vote was sent, but could not be saved locally.",
        )),
        Event::PollVoted { error: Some(_), .. } => Some(gettext("Could not send your vote.")),
        _ => None,
    }
}

pub(super) fn drain_events(backend: Option<&Backend>, notifier: &EventNotifier) -> Vec<Event> {
    let mut events = Vec::new();
    if let Some(backend) = backend {
        drain_backend_events(backend, notifier, |event| events.push(event));
    }
    events
}

pub(in crate::application) fn drain_hidden(
    session: &crate::application::accounts::AccountSession,
) -> Vec<Event> {
    drain_events(session.backend.as_ref(), &session.notifier)
}

pub(super) fn handle_backend_ready(
    app: &mut NativeApplication,
    account: crate::account::AccountId,
    sender: ComponentSender<NativeApplication>,
) {
    if account != app.active_account {
        app.handle_hidden_events(account, &sender);
        app.poll_theme_catalog();
        return;
    }
    let events = drain_events(app.backend.as_ref(), &app.notifier);
    for event in events {
        // A removal or a duplicate link can put another account on screen
        // mid-batch; the rest of the batch belongs to the one that left.
        if app.active_account != account {
            break;
        }
        if let Some(feedback) = poll_error_message(&event) {
            app.status = feedback.clone();
            app.toast(&feedback);
        }
        match event {
            Event::Link(link) => {
                if matches!(link, LinkStatus::LoggedOut) {
                    app.leave_screen();
                    clear_avatar_textures();
                    app.notifications.clear_account(account);
                    app.chat_snapshots.clear();
                    app.contacts.clear();
                    app.avatars.clear();
                    app.drafts.clear();
                    app.account_receipts_off = false;
                    app.avatar_requests.clear();
                    app.chats_dirty = true;
                    app.sync_chat_projection();
                    if app.removing == Some(account) {
                        app.finish_removal(&sender);
                        if app.active_account != account {
                            break;
                        }
                    }
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
                app.sync_translucent_sidebar();
            }
            Event::Profile { phone, name } => app.remember_profile(account, phone, name, &sender),
            Event::Syncing(syncing) => app.syncing = syncing,
            Event::Chats(chats) => {
                app.apply_chat_changes(vec![ChatChange::Snapshot(chats)]);
            }
            Event::ChatUpdated(chat) => {
                app.apply_chat_changes(vec![ChatChange::Update(*chat)]);
                app.read_open_chat();
            }

            Event::RefreshTranscript => {
                if let (Some(backend), Some(chat)) =
                    (&app.backend, app.open_chat.active_chat.clone())
                {
                    app.open_chat.message_ids.clear();
                    app.open_chat.message_snapshots.clear();
                    app.open_chat.editable_messages.clear();
                    app.messages.clear();
                    app.open_chat.transcript.clear();
                    app.loading_older = false;
                    app.history_complete = false;
                    let request = app.transcript_requests.reload();
                    backend.send(crate::backend::Command::LoadChat {
                        chat,
                        before: None,
                        request,
                    });
                }
            }
            // An empty list clears contacts on logout; otherwise
            // the backend sends the full set or single updates.
            Event::Contacts(contacts) => {
                app.apply_transcript_change(TranscriptChange::Contacts(contacts))
            }
            Event::Messages {
                chat,
                messages,
                older,
                complete,
                request,
            } => {
                if !app.transcript_requests.messages(
                    app.open_chat.active_chat.as_deref() == Some(&chat),
                    request,
                    older,
                    complete,
                    &mut app.history_complete,
                    &mut app.loading_older,
                ) {
                    continue;
                }
                app.apply_messages(chat.clone(), messages, older);
                app.continue_message_jump(&chat);
            }

            Event::OlderFetched {
                chat,
                more,
                request,
            } => {
                if app.transcript_requests.older_fetched(
                    app.open_chat.active_chat.as_deref() == Some(&chat),
                    request,
                    more,
                    &mut app.history_complete,
                    &mut app.loading_older,
                ) && !more
                {
                    app.status = gettext("No older messages available");
                }
            }
            Event::Stickers {
                saved,
                packs,
                recent,
            } => {
                let changed = saved != app.favorite_stickers
                    || packs != app.sticker_packs
                    || recent != app.recent_stickers;
                app.favorite_stickers = saved;
                app.sticker_packs = packs;
                app.recent_stickers = recent;
                if changed {
                    app.refresh_sticker_picker(&sender);
                }
            }
            Event::MessageUpdated(message) => {
                app.apply_transcript_change(TranscriptChange::Message(message))
            }
            Event::MessagesUpdated(messages) => {
                for message in messages {
                    app.apply_transcript_change(TranscriptChange::Message(Box::new(message)));
                }
            }
            Event::Edited { chat, id, success } => app.edited(chat, id, success),
            Event::Sent { chat, success } => {
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
            Event::AttachmentCompleted {
                chat,
                path,
                success,
                ..
            } => app.attachment_completed(chat, path, success),
            Event::Media {
                chat,
                message,
                result,
            } => app.media_completed(&chat, &message, result),
            Event::StarChanged {
                chat,
                id,
                starred,
                local,
            } => app.star_changed(&chat, id, starred, local),
            Event::StarredIds { chat, ids } => app.starred_ids_loaded(&chat, ids),
            Event::StarredList(list) => app.starred_list_loaded(list, &sender),
            Event::MessageDeleted { chat, id } => app.message_deleted(&chat, &id),
            Event::Calls { entries, seen_at } => app.calls_loaded(entries, seen_at),
            event @ (Event::Call(_)
            | Event::CallWaiting(_)
            | Event::CallVideo(_)
            | Event::CallFrames) => app.handle_call_event(account, event, &sender),
            Event::Incoming { chat, message } => {
                if app.settings.auto_download
                    && should_auto_download(message.content.media())
                    && let Some(backend) = &app.backend
                {
                    backend.send(crate::backend::Command::Download {
                        chat: chat.clone(),
                        message: message.id.clone(),
                    });
                    if let Some(media) = app
                        .open_chat
                        .message_snapshots
                        .get_mut(&message.id)
                        .and_then(|known| known.content.media_mut())
                    {
                        media.state = crate::model::MediaState::Downloading;
                        app.open_chat.pending_transcript.mark_rows();
                    }
                }
                let known = app
                    .chat_snapshots
                    .iter()
                    .find(|known| known.id == chat)
                    .map(std::rc::Rc::as_ref);
                if notification_should_show(
                    app.settings.notifications,
                    app.window.is_active(),
                    app.open_chat.active_chat.as_deref(),
                    &chat,
                    known,
                ) {
                    let (title, body) = super::super::accounts::notice_text(
                        known,
                        &message,
                        app.settings.notification_previews,
                    );
                    if let Err(error) = app.notifications.show(
                        account,
                        &chat,
                        &title,
                        &body,
                        app.avatars.get(&chat).map(std::path::PathBuf::as_path),
                    ) {
                        log::warn!("could not show a notification: {error}");
                    }
                }
            }
            Event::Typing {
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
                    let shown = app.open_chat.active_chat.as_deref() == Some(chat.as_str())
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
            Event::Presence {
                id,
                online,
                last_seen,
            } => {
                app.presence.insert(id, (online, last_seen));
            }
            Event::Avatar {
                id,
                full: false,
                path,
            } => {
                let previous = app.avatars.get(&id).cloned();
                if let Some(current) = &path {
                    app.avatars.insert(id.clone(), current.clone());
                } else {
                    app.avatars.remove(&id);
                }
                update_avatar_texture_reference(previous, path);
                app.chats_dirty = true;
                if sender_in_window(
                    &app.open_chat.message_ids,
                    &app.open_chat.message_snapshots,
                    &id,
                ) {
                    app.open_chat.pending_transcript.mark_rows();
                }
            }
            Event::ReceiptsPrivacy { disabled } => app.account_receipts_off = disabled,
            Event::ProfileSaved { error } => {
                if let Some(dialog) = &app.profile_dialog {
                    dialog.saved(error.as_deref());
                }
                if error.is_none() {
                    app.toast(&gettext("Profile saved"));
                }
            }
            Event::LinkPreviewsChanged(enabled) => {
                app.settings.link_previews = enabled;
                if app.save_settings().is_err() {
                    app.toast(&gettext("Could not save preferences"));
                }
            }
            Event::ContactAbout { id, about } => {
                if let Some(dialog) = &app.profile_dialog
                    && dialog.is_for(&id)
                {
                    dialog.set_about(about.as_deref().unwrap_or_default());
                }
                if let Some((chat, row)) = &app.info_about
                    && chat == &id
                    && let Some(about) = about
                {
                    row.set_subtitle(&about);
                    row.set_visible(true);
                }
            }
            Event::ContactReady { id, name } => {
                let display_name = name
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| crate::util::phone(&id));
                app.apply_chat_changes(vec![ChatChange::Update(crate::model::Chat::new(
                    id.clone(),
                    display_name,
                ))]);
                app.status = gettext("Contact is on WhatsApp");
                sender.input(Input::OpenChatId(id));
            }
            Event::Info(message) => {
                app.status = message.clone();
                app.toast(&message);
            }
            Event::Error(error) => {
                if error.contains("remains linked") {
                    // The removal did not happen; a later logout must not finish it.
                    app.removing = None;
                }
                app.loading_older = false;
                log::warn!("native backend operation failed");
                let feedback = sanitized_error_feedback(&error);
                app.status = feedback.clone();
                app.toast(&feedback);
            }
            _ => {}
        }
    }
    app.flush_transcript();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_errors_surface_without_exposing_details_or_misreporting_sent_results() {
        let created = |error: Option<&str>| Event::PollCreated {
            chat: "fixture".into(),
            error: error.map(str::to_owned),
        };
        let voted = |error: Option<&str>| Event::PollVoted {
            chat: "fixture".into(),
            message: "fixture".into(),
            error: error.map(str::to_owned),
        };
        assert_eq!(poll_error_message(&created(None)), None);
        assert_eq!(poll_error_message(&voted(None)), None);
        assert_eq!(
            poll_error_message(&created(Some("private detail"))),
            Some(gettext("Poll creation could not be completed."))
        );
        assert_eq!(
            poll_error_message(&voted(Some("private detail"))),
            Some(gettext("Could not send your vote."))
        );
        let sent_poll = "The poll was sent, but its voting key could not be saved.";
        let sent_vote = "Your vote was sent, but could not be saved locally.";
        assert_eq!(
            poll_error_message(&created(Some(sent_poll))),
            Some(gettext(sent_poll))
        );
        assert_eq!(
            poll_error_message(&voted(Some(sent_vote))),
            Some(gettext(sent_vote))
        );
    }
}
