use super::*;

impl NativeApplication {
    /// Jumps to the message `message` quotes, loading older history if needed.
    pub(super) fn open_quoted(&mut self, message: crate::model::Message) {
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

    pub(super) fn reply_selected(&mut self) {
        let Some((chat, message)) = self.active_chat.clone().zip(self.selected_message_id()) else {
            return;
        };
        self.composer.begin_reply(&chat, message.clone());
        self.reply_to = Some((chat, message));
        self.editing = None;
        self.status = "Replying to selected message".into();
        self.focus_composer();
    }

    pub(super) fn edit_selected(&mut self) {
        if self.pending_edit.is_some() {
            self.status = "Saving edit".into();
            return;
        }
        let Some((chat, message)) = self.active_chat.clone().zip(self.selected_message_id()) else {
            return;
        };
        if let Some(text) = self.editable_messages.get(&message).cloned() {
            self.editing = Some((chat.clone(), message.clone()));
            self.reply_to = None;
            self.draft = text;
            self.composer.begin_edit(&chat, message, self.draft.clone());
            self.composer_buffer.set_text(&self.draft);
            self.status = "Editing selected message".into();
            self.focus_composer();
        } else {
            self.reply_to = Some((chat.clone(), message.clone()));
            self.composer.begin_reply(&chat, message);
            self.composer.begin_reply(
                &self.reply_to.as_ref().unwrap().0,
                self.reply_to.as_ref().unwrap().1.clone(),
            );
            self.status = "Replying to selected message".into();
        }
    }

    pub(super) fn cancel_reply(&mut self) {
        if let Some((chat, _)) = self.reply_to.take() {
            self.composer.cancel_context(&chat);
        }
    }

    pub(super) fn cancel_edit(&mut self) {
        if self.editing.take().is_some()
            && let Some(chat) = &self.active_chat
        {
            self.draft = self.drafts.get(chat).cloned().unwrap_or_default();
            self.composer.cancel_context(chat);
            self.composer.set_draft(chat, self.draft.clone());
            self.composer_buffer.set_text(&self.draft);
        }
        self.pending_edit = None;
    }

    pub(super) fn draft_changed(&mut self, text: String, sender: &ComponentSender<Self>) {
        self.draft = text;
        if let Some(chat) = &self.active_chat {
            self.composer.set_draft(chat, self.draft.clone());
            if self.editing.is_none() {
                self.drafts.insert(chat.clone(), self.draft.clone());
            }
            if self.settings.send_typing
                && let Some(backend) = &self.backend
            {
                backend.send(crate::backend::Command::Composing {
                    chat: chat.clone(),
                    composing: true,
                });
            }
            self.composing_until.insert(
                chat.clone(),
                std::time::Instant::now() + std::time::Duration::from_secs(3),
            );
            let input = sender.clone();
            let chat = chat.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
                input.input(Input::StopComposing(chat));
            });
        }
    }

    pub(super) fn send_text(&mut self, text: String) {
        // The draft keeps visible labels; only the backend gets `@<user>` tokens.
        let (wire, mentions) = self
            .active_chat
            .as_deref()
            .and_then(|chat| self.chat_snapshots.iter().find(|known| known.id == chat))
            .map_or_else(
                || (text.clone(), Vec::new()),
                |known| {
                    crate::native_composer::encode_mentions(
                        &text,
                        &known.participants,
                        &self.mention_labels(known),
                    )
                },
            );
        self.draft = text;
        if let Some(chat) = &self.active_chat {
            self.composer.set_draft(chat, self.draft.clone());
        }
        if self.pending_send.is_some() || self.voice_send_pending() {
            self.status = "Waiting for previous message.".into();
            return;
        }
        let Some(chat) = self.active_chat.clone() else {
            return;
        };
        let attachment_count = self.pending_attachment_count();
        if self.draft.trim().is_empty() && attachment_count == 0 {
            return;
        }
        let writable = self
            .chat_snapshots
            .iter()
            .find(|known| known.id == chat)
            .is_some_and(crate::model::Chat::can_send);
        if !writable {
            self.status = "This conversation is read-only.".into();
            return;
        }
        if let Some(backend) = &self.backend {
            if attachment_count > 0 && self.editing.is_some() {
                self.status = "Finish editing before sending attachments.".into();
                return;
            }
            if let Some((edit_chat, id)) = self.editing.as_ref()
                && edit_chat == &chat
            {
                if self.pending_edit.is_some() {
                    self.status = "Saving edit".into();
                    return;
                }
                self.pending_composer_request = self.composer.submit(&chat);
                self.pending_edit = Some((chat.clone(), id.clone(), self.draft.clone()));
                backend.send(crate::backend::Command::EditText {
                    chat,
                    id: id.clone(),
                    text: wire.clone(),
                    mentions: mentions.clone(),
                });
                self.status = "Saving edit".into();
                return;
            }
            let quote = self
                .reply_to
                .as_ref()
                .filter(|(reply_chat, _)| reply_chat == &chat)
                .map(|(_, message)| message.clone());
            self.pending_composer_request = self.composer.submit(&chat);
            let attachments = self.pending_attachments.remove(&chat).unwrap_or_default();
            let has_clipboard_image = self.pending_clipboard_images.contains_key(&chat);
            self.pending_send = Some(PendingSend {
                chat: chat.clone(),
                text: self.draft.clone(),
                reply: quote.clone(),
                attachments,
                failed_attachments: Vec::new(),
                clipboard_image: has_clipboard_image,
                remaining: attachment_count.max(1),
                failed: false,
            });
            if let Some(pending) = &self.pending_send
                && !pending.attachments.is_empty()
            {
                let documents = pending
                    .attachments
                    .iter()
                    .filter(|path| {
                        self.document_attachments
                            .contains(&(chat.clone(), (*path).clone()))
                    })
                    .cloned()
                    .collect();
                backend.send(crate::backend::Command::SendFiles {
                    chat,
                    paths: pending.attachments.clone(),
                    documents,
                    caption: caption(&wire),
                    quoting: quote,
                    mentions: mentions.clone(),
                });
                self.status = "Sending attachments".into();
            } else if let Some(image) = self.pending_clipboard_images.get(&chat) {
                backend.send(crate::backend::Command::SendImage {
                    chat,
                    width: image.width,
                    height: image.height,
                    rgba: image.rgba.clone(),
                    caption: caption(&wire),
                    quoting: quote,
                    mentions: mentions.clone(),
                });
                self.status = "Sending clipboard image".into();
            } else {
                backend.send(crate::backend::Command::SendText {
                    chat,
                    text: wire.clone(),
                    quoting: quote,
                    mentions,
                });
                self.status = "Sending message".into();
            }
            // The pending row shows the message now; a failure restores it.
            self.draft.clear();
            self.composer_buffer.set_text("");
        } else {
            self.status = "Backend unavailable. Draft kept.".into();
        }
    }

    pub(super) fn apply_messages(
        &mut self,
        chat: String,
        messages: Vec<crate::model::Message>,
        older: bool,
    ) {
        if self.active_chat.as_deref() != Some(&chat) {
            return;
        }
        let anchor = older.then(|| self.message_ids.first().cloned()).flatten();
        let mut page_ids = std::collections::HashSet::new();
        let messages = messages
            .into_iter()
            .filter(|message| {
                page_ids.insert(message.id.clone())
                    && !self.message_snapshots.contains_key(&message.id)
            })
            .collect::<Vec<_>>();
        let live_messages_appended = !older && !messages.is_empty();
        if older {
            // One splice keeps a page of history linear instead of shifting
            // the whole window per message.
            self.message_ids
                .splice(0..0, messages.iter().map(|message| message.id.clone()));
            for message in messages {
                if let Some(text) = editable_text(&message) {
                    self.editable_messages.insert(message.id.clone(), text);
                }
                self.message_snapshots.insert(message.id.clone(), message);
            }
        } else {
            for message in messages {
                if let Some(text) = editable_text(&message) {
                    self.editable_messages.insert(message.id.clone(), text);
                }
                self.message_ids.push(message.id.clone());
                self.message_snapshots
                    .insert(message.id.clone(), message.clone());
            }
        }
        for id in trim_message_window(&mut self.message_ids, older, ACTIVE_MESSAGE_LIMIT) {
            self.editable_messages.remove(&id);
            self.message_snapshots.remove(&id);
        }
        self.download_missing_stickers();
        self.queue_waveforms();
        self.rebuild_message_rows(live_messages_appended);
        self.sync_transcript();
        if older
            && let Some(anchor) = anchor
            && let Some(position) = self.message_ids.iter().position(|id| id == &anchor)
        {
            let view = self.messages.view.clone();
            gtk::glib::idle_add_local_once(move || {
                view.scroll_to(position as u32, gtk::ListScrollFlags::NONE, None);
            });
        }
        self.status = "Conversation loaded".into();
    }

    pub(super) fn message_updated(&mut self, message: crate::model::Message) {
        let text = editable_text(&message);
        if self.active_chat.as_deref() == Some(&message.chat)
            && self.message_snapshots.contains_key(&message.id)
        {
            if let Some(text) = &text {
                self.editable_messages
                    .insert(message.id.clone(), text.clone());
            } else {
                self.editable_messages.remove(&message.id);
            }
            self.message_snapshots
                .insert(message.id.clone(), message.clone());
            self.rebuild_message_rows(false);
            self.sync_transcript();
            self.refresh_selected_voice();
        }
    }

    pub(super) fn edited(&mut self, chat: String, id: String, success: bool) {
        if !is_edit_completion(self.pending_edit.as_ref(), &chat, &id) {
            return;
        }
        self.pending_edit = None;
        if success {
            if let Some(request) = self.pending_composer_request.take() {
                self.composer.complete_edit(&request);
            }
            if self
                .editing
                .as_ref()
                .is_some_and(|editing| editing == &(chat.clone(), id.clone()))
            {
                self.editing = None;
                self.draft = self.drafts.get(&chat).cloned().unwrap_or_default();
                self.composer.cancel_context(&chat);
                self.composer.set_draft(&chat, self.draft.clone());
                self.composer_buffer.set_text(&self.draft);
                self.status = "Message updated".into();
            }
        } else {
            if let Some(request) = self.pending_composer_request.take() {
                let text = request.text().to_owned();
                self.composer.complete_edit(&request);
                self.composer.set_draft(&chat, text);
            }
            self.status = "Edit could not be saved. Draft kept.".into();
        }
    }

    pub(super) fn sent(&mut self, chat: String, success: bool) {
        if self
            .pending_send
            .as_ref()
            .is_some_and(|pending| !pending.attachments.is_empty())
        {
            return;
        }
        self.send_completed(chat, success);
    }

    pub(super) fn attachment_completed(
        &mut self,
        chat: String,
        path: std::path::PathBuf,
        success: bool,
    ) {
        let Some(pending) = self.pending_send.as_mut() else {
            return;
        };
        if pending.chat != chat || !pending.attachments.contains(&path) {
            return;
        }
        if pending.complete_attachment(path, success) {
            self.finish_pending_send(chat);
        }
    }

    fn send_completed(&mut self, chat: String, success: bool) {
        let Some(pending) = self.pending_send.as_mut() else {
            return;
        };
        if pending.chat != chat {
            return;
        }
        if !pending.complete(success) {
            return;
        }
        self.finish_pending_send(chat);
    }

    fn finish_pending_send(&mut self, chat: String) {
        let Some(pending) = self.pending_send.take() else {
            return;
        };
        let request = self.pending_composer_request.take();
        if let Some(request) = request {
            let text = request.text().to_owned();
            self.composer.complete_send(&request);
            if pending.failed && self.composer.draft(&chat).is_empty() {
                self.composer.set_draft(&chat, text);
            }
        }
        if !pending.failed {
            if pending.clipboard_image {
                self.pending_clipboard_images.remove(&chat);
            }
            let unchanged = self.drafts.get(&chat) == Some(&pending.text);
            if unchanged && self.active_chat.as_deref() == Some(&chat) && self.draft == pending.text
            {
                self.draft.clear();
                self.composer_buffer.set_text("");
            }
            if unchanged {
                self.drafts.remove(&chat);
            }
            if self
                .reply_to
                .as_ref()
                .is_some_and(|reply| Some(&reply.1) == pending.reply.as_ref())
            {
                self.reply_to = None;
            }
            self.status = "Message sent".into();
        } else {
            // Put the cleared text back; if the user already typed something
            // new, keep the failed text ahead of it instead of dropping it.
            let merge = |typed: &str| match (pending.text.is_empty(), typed.is_empty()) {
                (true, _) => typed.to_owned(),
                (false, true) => pending.text.clone(),
                (false, false) => format!("{}\n{typed}", pending.text),
            };
            if self.active_chat.as_deref() == Some(&chat) {
                if !pending.text.is_empty() {
                    let merged = merge(&self.draft);
                    self.composer_buffer.set_text(&merged);
                }
            } else {
                let merged = merge(self.drafts.get(&chat).map_or("", String::as_str));
                if !merged.is_empty() {
                    self.drafts.insert(chat.clone(), merged);
                }
            }
            if !pending.failed_attachments.is_empty() {
                self.pending_attachments
                    .entry(chat.clone())
                    .or_default()
                    .splice(0..0, pending.failed_attachments);
            }
            self.status = "Message could not be sent. Draft kept.".into();
        }
    }

    /// Title and one-line preview for the bar above the composer: the message
    /// being edited, or the one being replied to and who wrote it.
    pub(super) fn composer_context(&self) -> (String, String) {
        if let Some((_, id)) = &self.editing {
            let preview = self.message_snapshots.get(id).map(|m| m.summary());
            return ("Editing message".into(), preview.unwrap_or_default());
        }
        let Some(message) = self
            .reply_to
            .as_ref()
            .and_then(|(_, id)| self.message_snapshots.get(id))
        else {
            return ("Replying to message".into(), String::new());
        };
        let who = if message.from_me {
            "yourself".to_owned()
        } else {
            sender_label(message.sender_name.as_deref(), &message.sender)
        };
        (format!("Replying to {who}"), message.summary())
    }

    pub(super) fn can_attach(&self) -> bool {
        self.editing.is_none()
            && self.pending_send.is_none()
            && self
                .active_chat
                .as_deref()
                .and_then(|id| self.chat_snapshots.iter().find(|chat| chat.id == id))
                .is_some_and(crate::model::Chat::can_send)
    }

    pub(super) fn sync_transcript(&mut self) {
        let rows = self
            .message_ids
            .iter()
            .filter_map(|id| self.message_snapshots.get(id))
            .map(|message| transcript_row(message, &self.contacts))
            .collect();
        self.transcript = rows;
        self.clear_missing_selected_voice();
    }

    pub(super) fn rebuild_message_rows(&mut self, live_messages_appended: bool) {
        let len = self.message_ids.len();
        if self.opened_unread > 0 && len > 0 {
            let first_unread = len - self.opened_unread.min(len);
            self.unread_marker = Some(self.message_ids[first_unread].clone());
            self.opened_unread = 0;
        }
        let unread = self
            .unread_marker
            .as_ref()
            .and_then(|marker| self.message_ids.iter().position(|id| id == marker))
            .map_or(0, |position| len - position);
        let messages = self
            .message_ids
            .iter()
            .filter_map(|id| self.message_snapshots.get(id).cloned())
            .collect::<Vec<_>>();
        let timeline = messages
            .iter()
            .map(|message| (message.timestamp, message.from_me))
            .collect::<Vec<_>>();
        let prefixes = conversation_prefixes(&timeline, unread);
        let boundaries = message_group_boundaries(&messages);
        let separated = prefixes
            .iter()
            .map(|prefix| !prefix.is_empty())
            .collect::<Vec<_>>();
        let roles = album_roles(&messages, &separated);
        let mut albums = roles
            .iter()
            .enumerate()
            .map(|(index, role)| match role {
                AlbumRole::Leader(count) => messages[index..index + count].to_vec(),
                _ => Vec::new(),
            })
            .collect::<Vec<_>>();
        let rows = messages
            .into_iter()
            .enumerate()
            .map(|(index, mut message)| {
                let audio = self.project_voice(&message);
                let (show_sender, mut show_timestamp) = boundaries[index];
                let album = std::mem::take(&mut albums[index]);
                // The album's delivery mark and spacing follow its last photo.
                if let Some(last) = album.last() {
                    message.status = last.status;
                    show_timestamp = boundaries[index + album.len() - 1].1;
                }
                let avatar = (!message.from_me)
                    .then(|| self.avatars.get(&message.sender).cloned())
                    .flatten();
                let mut row = message_row(
                    message,
                    &self.contacts,
                    self.pointer_sender.clone(),
                    &prefixes[index],
                    avatar,
                    (show_sender, show_timestamp),
                    self.audio_registry(),
                );
                row.audio = audio;
                row.album = album;
                row.collapsed = roles[index] == AlbumRole::Follower;
                row.selected = self
                    .message_selection
                    .as_ref()
                    .map(|selection| selection.contains(&row.id));
                row
            })
            .collect::<Vec<_>>();
        // A glide still under way counts as the end: a covered window gets no
        // frames, so the glide waits there and finishes once it is shown.
        let at_bottom = gliding()
            || self.messages.view.vadjustment().is_none_or(|adjustment| {
                adjustment.value() + adjustment.page_size() >= adjustment.upper() - 48.0
            });
        // Rebinding only the changed span keeps the reader's scroll position
        // through receipts, reactions, and incoming messages.
        let old_len = self.messages.len() as usize;
        let old_last = old_len
            .checked_sub(1)
            .and_then(|last| self.messages.get(last as u32))
            .map(|item| item.borrow().id.clone());
        let (prefix, removed, inserted) = changed_span(old_len, &rows, |position, row| {
            self.messages
                .get(position as u32)
                .is_some_and(|item| item.borrow().renders_like(row))
        });
        let count = rows.len();
        let new_tail_arrived = old_len > 0
            && old_last.as_ref() != self.message_ids.last()
            && new_messages_need_notice(at_bottom, live_messages_appended);
        // Rows that only changed (delivery, reactions, grouping) keep their
        // widget; replacing them re-creates it, which blanks media and shifts
        // the scroll anchor.
        let in_place = removed.min(inserted);
        let reusable = (0..in_place).all(|offset| {
            self.messages
                .get((prefix + offset) as u32)
                .is_some_and(|item| item.borrow().id == rows[prefix + offset].id)
        });
        if !reusable && removed + inserted > old_len.max(count) / 2 {
            self.messages.clear();
            self.messages.extend_from_iter(rows);
        } else {
            let mut span = rows.into_iter().skip(prefix).take(inserted);
            let mut position = prefix;
            if reusable {
                for row in span.by_ref().take(in_place) {
                    self.rebind_message(position, row);
                    position += 1;
                }
                for _ in in_place..removed {
                    self.messages.remove(position as u32);
                }
            } else {
                for _ in 0..removed {
                    self.messages.remove(prefix as u32);
                }
            }
            for row in span {
                self.messages.insert(position as u32, row);
                position += 1;
            }
        }
        if new_tail_arrived {
            self.recent_messages_pending = true;
        }
        // Follow the conversation only when the reader is already at its end.
        if at_bottom && count > 0 && (removed > 0 || inserted > 0) {
            if old_len == 0 {
                scroll_to_end(&self.messages.view);
            } else {
                glide_to_end(&self.messages.view);
                // Only a new last message fades: older pages, separators and
                // reactions leave the tail unchanged.
                if let Some(id) = self.message_ids.last()
                    && old_last.as_ref() != Some(id)
                {
                    mark_arriving(id.clone());
                }
            }
        }
    }

    /// Swaps the row at `position` and redraws its widget in place, if shown.
    fn rebind_message(&mut self, position: usize, row: MessageRow) {
        let Some(item) = self.messages.get(position as u32) else {
            return;
        };
        let id = row.id.clone();
        *item.borrow_mut() = row;
        let mut child = self.messages.view.first_child();
        while let Some(current) = child {
            if let Some(mut root) = current.first_child().and_downcast::<gtk::Box>()
                && root.widget_name() == id.as_str()
            {
                // relm4's list factory keeps each row's widgets under this key.
                if let Some(mut widgets) =
                    unsafe { root.steal_data::<MessageRowWidgets>("widgets") }
                {
                    item.borrow_mut().bind(&mut widgets, &mut root);
                    unsafe { root.set_data("widgets", widgets) };
                }
                return;
            }
            child = current.next_sibling();
        }
    }

    pub(super) fn copy_transcript(&mut self) {
        if let Some(text) = crate::native_transcript::copied_text(&self.transcript) {
            crate::native_portals::NativePortals::write_clipboard_text(
                &self.window.clipboard(),
                &text,
            );
            self.status = "Transcript copied".into();
        }
    }
}

fn new_messages_need_notice(at_bottom: bool, live_messages_appended: bool) -> bool {
    !at_bottom && live_messages_appended
}

#[cfg(test)]
mod tests {
    use super::new_messages_need_notice;

    #[test]
    fn only_new_tail_messages_away_from_end_need_notice() {
        assert!(new_messages_need_notice(false, true));
        assert!(!new_messages_need_notice(true, true));
        assert!(!new_messages_need_notice(false, false));
    }
}
