//! Worker command routing.

use super::*;

impl Worker {
    pub(super) async fn handle_command(&mut self, command: Command) {
        let contact_completion = matches!(&command, Command::ContactSent { .. });
        let destination = match &command {
            Command::SendText { chat, .. }
            | Command::SendContact { chat, .. }
            | Command::SendVoice { chat, .. }
            | Command::SendFiles { chat, .. }
            | Command::SendImage { chat, .. }
            | Command::SendSticker { chat, .. }
            | Command::AnswerButton { chat, .. }
            | Command::AnswerListRow { chat, .. }
            | Command::CreatePoll { chat, .. } => Some(chat),
            Command::Forward { to_chat, .. } => Some(to_chat),
            _ => None,
        };
        if let Some(chat) = destination {
            let writable = self.privacy_ready
                && match self.archive.chat(chat) {
                    Ok(Some(chat)) => chat.can_send(),
                    Ok(None) => ChatKind::from_id(chat) != ChatKind::Broadcast,
                    Err(_) => false,
                };
            if !writable {
                let error = "This conversation is read-only in ZapTide".to_owned();
                if matches!(&command, Command::CreatePoll { .. }) {
                    self.emit(Event::PollCreated {
                        chat: chat.clone(),
                        error: Some(error),
                    });
                } else {
                    self.emit(Event::Error(error));
                    if matches!(&command, Command::SendText { .. }) {
                        self.emit(Event::Sent {
                            chat: chat.clone(),
                            success: false,
                        });
                    }
                }
                return;
            }
        }
        match command {
            Command::PollHistoryFailed {
                session_generation,
                chat,
                message,
                requested,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                self.poll_history
                    .fail(&chat, &message, requested, Instant::now());
                self.emit_message(&chat, &message);
                self.pump_poll_history();
            }
            Command::CreatePoll { chat, draft } => self.create_poll(chat, draft),
            Command::PollCreated {
                session_generation,
                chat,
                draft,
                result,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                self.poll_created(chat, draft, result)
            }
            Command::VotePoll {
                chat,
                message,
                choices,
            } => self.vote_poll(chat, message, choices),
            Command::PollVoted {
                session_generation,
                chat,
                message,
                choices,
                at,
                result,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                self.poll_voted(chat, message, choices, at, result)
            }
            Command::PollDecoded {
                session_generation,
                vote,
                choices,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                self.poll_decoded(vote, choices)
            }
            Command::SendText {
                chat,
                text,
                quoting,
                mentions,
            } => self.send_text(chat, text, quoting, mentions),
            Command::SendContact {
                chat,
                contact,
                quoting,
            } => self.send_contact(chat, contact, quoting),
            Command::AnswerButton {
                chat,
                message,
                button,
            } => self.answer_choice(chat, message, button),
            Command::AnswerListRow { chat, message, row } => self.answer_choice(chat, message, row),
            Command::Forward {
                from_chat,
                message,
                to_chat,
            } => self.forward_message(from_chat, message, to_chat),
            Command::Composing { chat, composing } => {
                let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
                    return;
                };
                tokio::spawn(async move {
                    let result = if composing {
                        client.chatstate().send_composing(&jid).await
                    } else {
                        client.chatstate().send_paused(&jid).await
                    };
                    if let Err(error) = result {
                        log::debug!("chat state not sent: {error}");
                    }
                });
            }
            Command::MarkRead { chat, receipts } => self.mark_read(chat, receipts),
            Command::ReadSyncFinished {
                session_generation,
                attempt_id,
                chat,
                through,
                success,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                if !self
                    .read_sync
                    .finish(attempt_id, &chat, through, success, Instant::now())
                {
                    return;
                }
                if success {
                    let _ = self.archive.finish_read_sync(&chat, through);
                    self.pump_read_sync();
                }
            }
            Command::LoadChat { chat, before } => self.load_chat(chat, before),
            Command::FetchOlder(chat) => self.fetch_older(chat),
            Command::LoadUntil { chat, id, before } => self.load_until(chat, id, before),
            Command::Download { chat, message } => self.download(chat, message),
            Command::FetchAvatar { id, full } => self.fetch_avatar(id, full),
            Command::EditText {
                chat,
                id,
                text,
                mentions,
            } => self.edit_text(chat, id, text, mentions),
            Command::Revoke { chat, id } => self.revoke(chat, id),
            Command::DeleteLocal { chat, id } => {
                if let Ok(true) = self.archive.delete_message(&chat, &id) {
                    self.emit(Event::MessageDeleted {
                        chat: chat.clone(),
                        id,
                    });
                    self.emit_chat(&chat);
                }
            }
            Command::AttachmentCompleted {
                chat,
                session_generation,
                batch,
                index,
                total,
                path,
                success,
            } => {
                if session_generation == self.session_generation {
                    self.emit(Event::AttachmentCompleted {
                        chat,
                        batch,
                        index,
                        total,
                        path,
                        success,
                    });
                }
            }
            Command::SendFiles {
                chat,
                paths,
                documents,
                caption,
                quoting,
                mentions,
            } => {
                self.send_files(chat, paths, documents, caption, quoting, mentions);
            }
            Command::SendImage {
                chat,
                width,
                height,
                rgba,
                caption,
                quoting,
                mentions,
            } => self.send_pasted_image(
                chat,
                PastedImageRequest {
                    width,
                    height,
                    rgba,
                    caption,
                    quoting,
                    mentions,
                },
            ),
            Command::Outbound {
                chat,
                session_generation,
                row,
                raw,
            } => self.outbound(chat, session_generation, *row, raw),
            Command::OutboundBatch {
                chat,
                session_generation,
                row,
                raw,
                sent,
            } => self.outbound_batch(chat, session_generation, *row, raw, sent),
            Command::SendSticker { chat, path } => self.send_sticker(chat, path),
            Command::SaveContact {
                session_generation,
                id,
                full_name,
                first_name,
                to_phone,
            } => {
                if !self.privacy_ready
                    || self.archive_cleanup_failed
                    || session_generation
                        .is_some_and(|generation| generation != self.session_generation)
                {
                    self.emit(Event::Error(
                        "Contact cannot be saved until account sync is ready.".to_owned(),
                    ));
                    return;
                }
                let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&id)) else {
                    self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
                    return;
                };
                let commands = self.commands.clone();
                let session_generation = self.session_generation;
                tokio::spawn(async move {
                    let error = client
                        .chat_actions()
                        .save_contact(&jid, Some(full_name.clone()), first_name, to_phone)
                        .await
                        .err()
                        .map(|error| error.to_string());
                    let _ = commands.send(Command::ContactSaved {
                        session_generation,
                        id,
                        name: full_name,
                        error,
                    });
                });
            }
            Command::ContactSaved {
                session_generation,
                id,
                name,
                error,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                if let Some(error) = error {
                    self.emit(Event::Error(format!("Could not save contact: {error}")));
                    return;
                }
                let contact = Contact {
                    id: id.clone(),
                    full_name: Some(name.clone()),
                    push_name: None,
                };
                if let Err(error) = self.archive.upsert_contact(&contact) {
                    log::warn!("could not store the contact: {error}");
                }
                // Preserve the stored push name during contact updates.
                let stored = self.archive.contact(&id).ok().flatten().unwrap_or(contact);
                self.emit(Event::Contacts(vec![stored]));
                self.emit(Event::Info(format!("Added {name} to contacts")));
                self.emit_chat(&id);
            }
            Command::NewContact {
                phone,
                full_name,
                first_name,
                to_phone,
            } => {
                if !self.privacy_ready || self.archive_cleanup_failed {
                    self.emit(Event::Error(
                        "Wait for account sync to finish before adding a contact.".to_owned(),
                    ));
                    return;
                }
                let Some(client) = self.client.clone() else {
                    self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
                    return;
                };
                let commands = self.commands.clone();
                let jid = Jid::pn(&phone);
                let session_generation = self.session_generation;
                tokio::spawn(async move {
                    // Use WhatsApp's registration check before opening the chat.
                    match client.contacts().is_on_whatsapp(&[jid]).await {
                        Ok(results) => {
                            let registered = results.iter().any(|result| result.is_registered);
                            let _ = commands.send(Command::ContactChecked {
                                session_generation,
                                phone,
                                full_name,
                                first_name,
                                to_phone,
                                registered,
                            });
                        }
                        Err(_) => {
                            log::debug!("number registration check failed");
                            let _ =
                                commands.send(Command::ContactCheckFailed { session_generation });
                        }
                    }
                });
            }
            Command::ContactCheckFailed { session_generation } => {
                if session_generation == self.session_generation {
                    self.emit(Event::Error(
                        "Could not verify this phone number. Try again later.".to_owned(),
                    ));
                }
            }
            Command::ContactChecked {
                session_generation,
                phone,
                full_name,
                first_name,
                to_phone,
                registered,
            } => {
                if session_generation != self.session_generation
                    || !self.privacy_ready
                    || self.archive_cleanup_failed
                {
                    return;
                }
                if !registered {
                    self.emit(Event::Error(format!(
                        "{} is not on WhatsApp",
                        crate::util::phone(&phone)
                    )));
                    return;
                }
                let id = format!("{phone}@s.whatsapp.net");
                if let Some(full_name) = full_name.clone() {
                    let _ = self.commands.send(Command::SaveContact {
                        session_generation: Some(session_generation),
                        id: id.clone(),
                        full_name,
                        first_name,
                        to_phone,
                    });
                }
                self.emit(Event::ContactReady {
                    id,
                    name: full_name,
                });
            }
            Command::SendVoice {
                chat,
                samples,
                quoting,
            } => self.send_voice(chat, samples, quoting),
            Command::MarkPlayed {
                chat,
                message,
                sender,
                receipts,
            } => {
                if receipts {
                    self.mark_played(chat, message, sender);
                }
            }
            Command::ReceiptsPrivacy {
                session_generation,
                disabled,
            } => {
                if session_generation == self.session_generation {
                    self.emit(Event::ReceiptsPrivacy { disabled });
                }
            }
            Command::RecentStickers => {
                self.fetch_missing_stickers();
                self.emit_stickers();
            }
            Command::StickerFetched {
                hash,
                session_generation,
                result,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                match result {
                    Ok(path) => {
                        self.sticker_fetches.remove(&hash);
                        if self.archive.set_sticker_path(&hash, &path).is_err() {
                            log::warn!("could not file a sticker");
                        }
                    }
                    // Expired phone stickers fail every time; keep the hash
                    // in flight so reopening the picker does not refetch it.
                    Err(_error) => log::warn!("could not fetch a sticker"),
                }
                self.emit_stickers();
            }
            Command::ContactAbout { id } => {
                let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&id)) else {
                    return;
                };
                let commands = self.commands.clone();
                let session_generation = self.session_generation;
                tokio::spawn(async move {
                    let about = match client
                        .contacts()
                        .get_user_info(std::slice::from_ref(&jid))
                        .await
                    {
                        Ok(info) => info
                            .get(&jid)
                            .and_then(|info| info.status.clone())
                            .filter(|about| !about.is_empty()),
                        Err(error) => {
                            log::debug!("contact info not fetched: {error}");
                            None
                        }
                    };
                    let _ = commands.send(Command::ContactAboutFetched {
                        session_generation,
                        id,
                        about,
                    });
                });
            }
            Command::ContactAboutFetched {
                session_generation,
                id,
                about,
            } => {
                if session_generation == self.session_generation {
                    self.emit(Event::ContactAbout { id, about });
                }
            }
            Command::MeInfo {
                session_generation,
                about,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                self.me_about = about;
                match &self.me_about {
                    Some(about) => {
                        let _ = self.archive.set_meta("me_about", about);
                    }
                    None => {
                        let _ = self.archive.set_meta("me_about", "");
                    }
                }
            }
            Command::React {
                chat,
                message,
                emoji,
            } => self.react(chat, message, emoji),
            Command::SetArchived(chat, archived) => {
                let _ = self.archive.set_archived(&chat, archived);
                self.emit_chat(&chat);
                self.tell_phone(&chat, move |client, jid| async move {
                    if archived {
                        client.chat_actions().archive_chat(&jid, None).await
                    } else {
                        client.chat_actions().unarchive_chat(&jid, None).await
                    }
                    .map_err(|error| error.to_string())
                });
            }
            Command::SetPinned(chat, pinned) => {
                let _ = self.archive.set_pinned(&chat, pinned);
                self.emit_chat(&chat);
                self.tell_phone(&chat, move |client, jid| async move {
                    if pinned {
                        client.chat_actions().pin_chat(&jid).await
                    } else {
                        client.chat_actions().unpin_chat(&jid).await
                    }
                    .map_err(|error| error.to_string())
                });
            }
            Command::SetMuted(chat, until) => {
                let _ = self.archive.set_muted(&chat, until);
                self.emit_chat(&chat);
                self.tell_phone(&chat, move |client, jid| async move {
                    match until {
                        None => client.chat_actions().unmute_chat(&jid).await,
                        Some(0) => client.chat_actions().mute_chat(&jid).await,
                        Some(seconds) => {
                            client
                                .chat_actions()
                                .mute_chat_until(&jid, seconds * 1000)
                                .await
                        }
                    }
                    .map_err(|error| error.to_string())
                });
            }
            Command::PairWithPhone(phone) => {
                let Some(client) = self.client.clone() else {
                    self.emit(Event::Error("Not connected to WhatsApp yet".to_owned()));
                    return;
                };
                self.pairing_phone = Some(phone.clone());
                self.pair_code = None;
                self.pair_request_id = self.pair_request_id.wrapping_add(1);
                let request_id = self.pair_request_id;
                let status = self.unlinked();
                self.set_status(status);
                let commands = self.commands.clone();
                tokio::spawn(async move {
                    let result = client
                        .pair_with_code(PairCodeOptions {
                            phone_number: phone,
                            ..Default::default()
                        })
                        .await
                        .map_err(|error| error.to_string());
                    let _ = commands.send(Command::PairCode { request_id, result });
                });
            }
            Command::CancelPhonePairing => {
                // A later answer to the abandoned request is ignored.
                self.pair_request_id = self.pair_request_id.wrapping_add(1);
                self.pairing_phone = None;
                self.pair_code = None;
                let status = self.unlinked();
                self.set_status(status);
            }
            Command::PairCode { request_id, result } => {
                if request_id != self.pair_request_id {
                    return;
                }
                match result {
                    Ok(code) => {
                        self.pair_code = Some(code);
                        let status = self.unlinked();
                        self.set_status(status);
                    }
                    Err(error) => {
                        self.pairing_phone = None;
                        self.emit(Event::Error(format!(
                            "Could not link by phone number: {error}"
                        )));
                        let status = self.unlinked();
                        self.set_status(status);
                    }
                }
            }
            Command::Unlink => {
                if let Some(client) = self.client.clone() {
                    let markers = archive_cleanup_markers(&self.dirs);
                    if !persist_archive_cleanup_marker(&markers) {
                        self.emit(Event::Error(
                            "Could not prepare safe conversation cleanup. This device remains linked."
                                .to_owned(),
                        ));
                        return;
                    }
                    client.logout().await;
                } else {
                    self.on_logged_out().await;
                }
            }
            Command::Reconnect => {
                if self.archive_cleanup_failed {
                    self.emit(Event::Error(
                        "Local conversations could not be cleared; restart is blocked to protect data."
                            .to_owned(),
                    ));
                    return;
                }
                if let Some(client) = self.client.clone() {
                    tokio::spawn(async move { client.reconnect_immediately().await });
                } else {
                    self.start_bot().await;
                }
            }
            Command::Shutdown => {}
            Command::OlderStarted {
                session_generation,
                request_id,
                chat,
                protocol_id,
            } => {
                if session_generation == self.session_generation {
                    self.older_request_started(chat, request_id, protocol_id);
                }
            }
            Command::OlderFailed {
                session_generation,
                request_id,
                chat,
                error,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                if !self
                    .pending_older
                    .get(&chat)
                    .is_some_and(|request| request.request_id == request_id)
                {
                    return;
                }
                self.pending_older.remove(&chat);
                self.emit(Event::OlderFetched { chat, more: true });
                self.emit(Event::Error(error));
            }
            Command::RevokeFinished {
                session_generation,
                attempt_id,
                chat,
                message,
                success,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                let key = (chat.clone(), message.clone());
                if !self
                    .pending_revokes
                    .get(&key)
                    .is_some_and(|pending| pending.attempt_id == attempt_id)
                {
                    return;
                }
                let pending = self
                    .pending_revokes
                    .remove(&key)
                    .expect("matching revoke attempt");
                if success {
                    return;
                }
                let still_optimistically_revoked = self
                    .archive
                    .message(&chat, &message)
                    .ok()
                    .flatten()
                    .is_some_and(|row| matches!(row.content, Content::Revoked));
                if still_optimistically_revoked
                    && self
                        .archive
                        .set_content(&chat, &message, &pending.content, pending.edited)
                        .is_ok_and(|updated| updated)
                {
                    self.emit_message(&chat, &message);
                    self.emit_chat(&chat);
                }
                self.emit(Event::Error(
                    "Could not delete the message for everyone".to_owned(),
                ));
            }
            Command::GroupInfoFailed {
                session_generation,
                chat,
                permanent,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                self.handle_failed_group(chat, permanent);
            }
            Command::Edited {
                chat,
                id,
                session_generation,
                success,
                content,
                mentions,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                if success
                    && let Ok(true) = self
                        .archive
                        .set_edited_text(&chat, &id, &content, &mentions)
                {
                    self.emit_message(&chat, &id);
                    self.emit_chat(&chat);
                }
                self.emit(Event::Edited { chat, id, success });
            }
            Command::Sent {
                chat,
                id,
                session_generation,
                error,
            }
            | Command::ContactSent {
                chat,
                id,
                session_generation,
                error,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                if id.is_empty() {
                    // No message row can be updated, but native pending sends
                    // still need one sanitized terminal result.
                    if error.is_some() {
                        self.emit(Event::Error(sanitized_send_error().to_owned()));
                    }
                    if !contact_completion {
                        self.emit(Event::Sent {
                            chat,
                            success: false,
                        });
                    }
                    return;
                }
                // Server-confirmed echo wins over a late transport error.
                if error.is_some()
                    && self.answer_sends.contains_key(&id)
                    && self
                        .archive
                        .message(&chat, &id)
                        .ok()
                        .flatten()
                        .is_some_and(|row| {
                            matches!(
                                row.status,
                                Delivery::Sent
                                    | Delivery::Delivered
                                    | Delivery::Read
                                    | Delivery::Played
                            )
                        })
                {
                    self.answer_sends.remove(&id);
                    return;
                }
                let status = match &error {
                    Some(_) => Delivery::Failed,
                    None => Delivery::Sent,
                };
                if let Some((chat, original)) = self.answer_sends.remove(&id) {
                    if error.is_some() {
                        let _ = self.archive.delete_message(&chat, &id);
                        self.emit(Event::MessageDeleted {
                            chat: chat.clone(),
                            id: id.clone(),
                        });
                    } else if let Ok(Some(parent)) = self.archive.message(&chat, &original)
                        && let Some(choice) = answer_choice_id(&self.archive, &chat, &id)
                        && parent.content.choice(&choice).is_some()
                        && let Some(marked) = parent.content.with_answer(Some(choice))
                        && let Ok(true) =
                            self.archive
                                .set_content(&chat, &original, &marked, parent.edited)
                    {
                        self.emit_message(&chat, &original);
                    }
                    let _ = self
                        .archive
                        .set_status(&chat, &id, status, crate::util::now());
                    if error.is_none() {
                        self.emit_message(&chat, &id);
                    }
                    self.emit_chat(&chat);
                } else {
                    let _ = self
                        .archive
                        .set_status(&chat, &id, status, crate::util::now());
                    self.emit_message(&chat, &id);
                    self.emit_chat(&chat);
                    // Contact sharing is independent of the text/attachment draft.
                    // Its completion must not clear a simultaneous composer send.
                    if contact_completion {
                        if error.is_none() {
                            self.emit(Event::Info("Contact sent".to_owned()));
                        }
                    } else {
                        self.emit(Event::Sent {
                            chat: chat.clone(),
                            success: error.is_none(),
                        });
                    }
                }
                if let Some(error) = error {
                    self.emit(Event::Error(format!("Message not sent: {error}")));
                }
            }
            Command::Downloaded {
                chat,
                id,
                session_generation,
                raw_fingerprint,
                destination,
                mut result,
            } => {
                if session_generation != self.session_generation {
                    if let Ok(path) = &result {
                        let _ = std::fs::remove_file(path);
                    }
                    return;
                }
                let current_raw = match self.archive.raw(&chat, &id) {
                    Ok(raw) => raw,
                    Err(_) => {
                        log::warn!(
                            "attachment validation deferred because the archive could not be read"
                        );
                        self.deferred_downloads.push(DeferredDownload {
                            retry_at: Instant::now() + DOWNLOAD_VALIDATION_RETRY,
                            completion: Some(Command::Downloaded {
                                chat,
                                id,
                                session_generation,
                                raw_fingerprint,
                                destination,
                                result,
                            }),
                        });
                        return;
                    }
                };
                let for_picker =
                    self.sticker_downloads
                        .remove(&(chat.clone(), id.clone(), raw_fingerprint));
                if !current_raw
                    .as_deref()
                    .is_some_and(|raw| super::message_raw_fingerprint(raw) == raw_fingerprint)
                {
                    if let Ok(path) = &result {
                        let _ = std::fs::remove_file(path);
                    }
                    // This completion belongs to an obsolete payload. Do not fail
                    // its replacement or release that payload's picker request.
                    return;
                }
                if let Ok(staged) = &result {
                    match std::fs::rename(staged, &destination) {
                        Ok(()) => {
                            let _ = self.archive.set_media_path(&chat, &id, &destination);
                            result = Ok(destination);
                        }
                        Err(_error) => {
                            let _ = std::fs::remove_file(staged);
                            result = Err("The attachment could not be saved".to_owned());
                        }
                    }
                }
                self.emit(Event::Media {
                    chat,
                    message: id,
                    result,
                });
                if for_picker {
                    self.emit_stickers();
                }
            }
            Command::AvatarFetched {
                id,
                full,
                session_generation,
                avatar_generation,
                path,
            } => {
                if session_generation == self.session_generation
                    && self
                        .avatar_generations
                        .get(&(id.clone(), full))
                        .is_some_and(|generation| {
                            avatar_generation == generation.load(Ordering::Acquire)
                        })
                {
                    self.emit(Event::Avatar { id, full, path });
                }
            }
            Command::AvatarFailed {
                id,
                full,
                session_generation,
                avatar_generation,
            } => {
                if session_generation == self.session_generation
                    && self
                        .avatar_generations
                        .get(&(id.clone(), full))
                        .is_some_and(|generation| {
                            avatar_generation == generation.load(Ordering::Acquire)
                        })
                {
                    *self.pending_avatars.entry((id, full)).or_insert(0) += 1;
                }
            }
            Command::GroupRecipients {
                session_generation,
                chat,
                id,
                recipients,
                lids,
                stored,
            } => {
                if session_generation != self.session_generation {
                    let _ = stored.send(false);
                } else {
                    for (lid, pn) in lids {
                        self.learn_lid(&lid, &pn);
                    }
                    let saved = self.save_group_recipients(&chat, &id, &recipients);
                    let _ = stored.send(saved);
                }
            }
            Command::GroupInfo {
                session_generation,
                chat,
                name,
                participants,
                read_only,
                ephemeral_expiration,
                ephemeral_setting_timestamp,
            } => {
                if session_generation != self.session_generation {
                    return;
                }
                self.group_info_tries.remove(&chat);
                let _ =
                    self.archive
                        .set_group_info(&chat, name.as_deref(), &participants, read_only);
                if let Some(expiration) = ephemeral_expiration {
                    let _ = self.archive.set_ephemeral(
                        &chat,
                        expiration,
                        ephemeral_setting_timestamp.unwrap_or_default(),
                    );
                }
                self.emit_chat(&chat);
            }
        }
    }
}
