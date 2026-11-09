use super::*;
use crate::i18n::gettext;

/// What Escape does in the conversation, most specific first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EscapeAction {
    CancelSelection,
    CancelEdit,
    CancelReply,
    CloseChat,
    Nothing,
}

impl EscapeAction {
    pub(super) fn for_state(
        selecting: bool,
        editing: bool,
        replying: bool,
        chat_open: bool,
    ) -> Self {
        match (selecting, editing, replying, chat_open) {
            (true, ..) => Self::CancelSelection,
            (_, true, ..) => Self::CancelEdit,
            (_, _, true, _) => Self::CancelReply,
            (.., true) => Self::CloseChat,
            _ => Self::Nothing,
        }
    }
}

impl NativeApplication {
    pub(super) fn press_escape(&mut self) -> EscapeAction {
        let action = EscapeAction::for_state(
            self.message_selection.is_some(),
            self.open_chat.editing.is_some(),
            self.open_chat.reply_to.is_some(),
            self.open_chat.active_chat.is_some(),
        );
        match action {
            EscapeAction::CancelSelection => {
                self.message_selection = None;
                self.rebuild_message_rows(false);
                self.focus_composer();
            }
            EscapeAction::CancelEdit => self.cancel_edit(),
            EscapeAction::CancelReply => self.cancel_reply(),
            EscapeAction::CloseChat => self.close_chat(),
            EscapeAction::Nothing => {}
        }
        action
    }

    /// Returns to the empty home screen. The draft stays with the chat and
    /// nothing is marked read, since reading only follows the open chat.
    pub(super) fn close_chat(&mut self) {
        self.close_media_viewer();
        let Some(chat) = self.open_chat.active_chat.clone() else {
            return;
        };
        self.cancel_edit();
        self.cancel_reply();
        self.cancel_portal_requests();
        self.clear_open_chat_state();
        self.composer.cancel_context(&chat);
        self.chat_projection.clear_selection();
        self.chats
            .selection_model
            .set_selected(gtk::INVALID_LIST_POSITION);
        self.mark_open_chat();
        self.sidebar_visible = true;
        self.opened_unread = 0;
        self.unread_marker = None;
        self.recent_messages_pending = false;
        self.history_complete = false;
        self.loading_older = false;
        self.message_target = None;
        self.message_selection = None;
        self.page_title = String::new();
        self.status = String::new();
    }

    /// Jumps to the message `message` quotes, loading older history if needed.
    pub(super) fn open_quoted(&mut self, message: crate::model::Message) {
        let Some(quoted_id) = message.quoted.map(|quoted| quoted.id) else {
            return;
        };
        if self.open_chat.message_ids.contains(&quoted_id) {
            self.scroll_message_into_view(&quoted_id);
        } else if let Some(backend) = &self.backend {
            self.pending_quote_navigation = Some((message.chat.clone(), quoted_id.clone()));
            backend.send(crate::backend::Command::LoadUntil {
                chat: message.chat,
                id: quoted_id,
                before: (message.timestamp, message.id),
            });
            self.status = gettext("Loading quoted message");
        }
    }

    pub(super) fn reply_selected(&mut self) {
        let Some((chat, message)) = self
            .open_chat
            .active_chat
            .clone()
            .zip(self.selected_message_id())
        else {
            return;
        };
        self.composer.begin_reply(&chat, message.clone());
        self.open_chat.reply_to = Some((chat, message));
        self.open_chat.editing = None;
        self.status = gettext("Replying to selected message");
        self.focus_composer();
    }

    pub(super) fn edit_selected(&mut self) {
        if self.open_chat.pending_edit.is_some() {
            self.status = gettext("Saving edit");
            return;
        }
        let Some((chat, message)) = self
            .open_chat
            .active_chat
            .clone()
            .zip(self.selected_message_id())
        else {
            return;
        };
        if let Some(text) = self.open_chat.editable_messages.get(&message).cloned() {
            self.open_chat.editing = Some((chat.clone(), message.clone()));
            self.open_chat.reply_to = None;
            self.draft = text;
            self.composer.begin_edit(&chat, message, self.draft.clone());
            self.composer_buffer.set_text(&self.draft);
            self.status = gettext("Editing selected message");
            self.focus_composer();
        } else {
            self.open_chat.reply_to = Some((chat.clone(), message.clone()));
            self.composer.begin_reply(&chat, message);
            self.composer.begin_reply(
                &self.open_chat.reply_to.as_ref().unwrap().0,
                self.open_chat.reply_to.as_ref().unwrap().1.clone(),
            );
            self.status = gettext("Replying to selected message");
        }
    }

    pub(super) fn cancel_reply(&mut self) {
        if let Some((chat, _)) = self.open_chat.reply_to.take() {
            self.composer.cancel_context(&chat);
        }
    }

    pub(super) fn cancel_edit(&mut self) {
        if self.open_chat.editing.take().is_some()
            && let Some(chat) = &self.open_chat.active_chat
        {
            self.draft = self.drafts.get(chat).cloned().unwrap_or_default();
            self.composer.cancel_context(chat);
            self.composer.set_draft(chat, self.draft.clone());
            self.composer_buffer.set_text(&self.draft);
        }
        self.open_chat.pending_edit = None;
    }

    pub(super) fn draft_changed(&mut self, text: String, sender: &ComponentSender<Self>) {
        self.draft = text;
        if let Some(chat) = &self.open_chat.active_chat {
            self.composer.set_draft(chat, self.draft.clone());
            if self.open_chat.editing.is_none() {
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

    pub(super) fn send_text(
        &mut self,
        text: String,
        link_preview: Option<crate::model::LinkPreview>,
    ) {
        // The draft keeps visible labels; only the backend gets `@<user>` tokens.
        let (wire, mentions) = self
            .open_chat
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
        if let Some(chat) = &self.open_chat.active_chat {
            self.composer.set_draft(chat, self.draft.clone());
        }
        if self.pending_send.is_some() || self.voice_send_pending() {
            self.status = gettext("Waiting for previous message.");
            return;
        }
        let Some(chat) = self.open_chat.active_chat.clone() else {
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
            .is_some_and(|chat| chat.can_send());
        if !writable {
            self.status = gettext("This conversation is read-only.");
            return;
        }
        if let Some(backend) = &self.backend {
            if attachment_count > 0 && self.open_chat.editing.is_some() {
                self.status = gettext("Finish editing before sending attachments.");
                return;
            }
            if let Some((edit_chat, id)) = self.open_chat.editing.as_ref()
                && edit_chat == &chat
            {
                if self.open_chat.pending_edit.is_some() {
                    self.status = gettext("Saving edit");
                    return;
                }
                self.pending_composer_request = self.composer.submit(&chat);
                self.open_chat.pending_edit = Some((chat.clone(), id.clone(), self.draft.clone()));
                backend.send(crate::backend::Command::EditText {
                    chat,
                    id: id.clone(),
                    text: wire.clone(),
                    mentions: mentions.clone(),
                });
                self.status = gettext("Saving edit");
                return;
            }
            let quote = self
                .open_chat
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
                self.status = gettext("Sending attachments");
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
                self.status = gettext("Sending clipboard image");
            } else {
                backend.send(crate::backend::Command::SendText {
                    chat,
                    text: wire.clone(),
                    quoting: quote,
                    mentions,
                    link_preview: self
                        .settings
                        .link_previews
                        .then_some(link_preview)
                        .flatten(),
                });
                self.status = gettext("Sending message");
            }
            // The pending row shows the message now; a failure restores it.
            self.draft.clear();
            self.composer_buffer.set_text("");
        } else {
            self.status = gettext("Backend unavailable. Draft kept.");
        }
    }

    pub(super) fn apply_messages(
        &mut self,
        chat: String,
        messages: Vec<crate::model::Message>,
        older: bool,
    ) {
        if self.open_chat.active_chat.as_deref() != Some(&chat) {
            return;
        }
        // Deferred updates land first, so this page changes only its own
        // rows, as when every update was drawn on arrival.
        self.flush_transcript();
        let anchor = older
            .then(|| self.open_chat.message_ids.first().cloned())
            .flatten();
        let mut page_ids = std::collections::HashSet::new();
        let messages = messages
            .into_iter()
            .filter(|message| {
                page_ids.insert(message.id.clone())
                    && !self.open_chat.message_snapshots.contains_key(&message.id)
            })
            .collect::<Vec<_>>();
        let live_messages_appended = !older && !messages.is_empty();
        if older {
            // One splice keeps a page of history linear instead of shifting
            // the whole window per message.
            self.open_chat
                .message_ids
                .splice(0..0, messages.iter().map(|message| message.id.clone()));
            for message in messages {
                if let Some(text) = editable_text(&message) {
                    self.open_chat
                        .editable_messages
                        .insert(message.id.clone(), text);
                }
                self.open_chat
                    .message_snapshots
                    .insert(message.id.clone(), message);
            }
        } else {
            for message in messages {
                if let Some(text) = editable_text(&message) {
                    self.open_chat
                        .editable_messages
                        .insert(message.id.clone(), text);
                }
                self.open_chat.message_ids.push(message.id.clone());
                self.open_chat
                    .message_snapshots
                    .insert(message.id.clone(), message.clone());
            }
        }
        for id in trim_message_window(&mut self.open_chat.message_ids, older, ACTIVE_MESSAGE_LIMIT)
        {
            self.open_chat.editable_messages.remove(&id);
            self.open_chat.message_snapshots.remove(&id);
        }
        self.download_missing_stickers();
        self.queue_waveforms();
        self.rebuild_message_rows(live_messages_appended);
        self.sync_transcript();
        if older
            && let Some(anchor) = anchor
            && let Some(position) = self
                .open_chat
                .message_ids
                .iter()
                .position(|id| id == &anchor)
        {
            let view = self.messages.view.clone();
            gtk::glib::idle_add_local_once(move || {
                view.scroll_to(position as u32, gtk::ListScrollFlags::NONE, None);
            });
        }
        self.status = gettext("Conversation loaded");
    }

    /// Applies a message update or contact change from a backend batch.
    pub(super) fn apply_transcript_change(&mut self, change: TranscriptChange) {
        let message_changed = absorb_transcript_change(
            &mut OpenTranscript {
                active_chat: self.open_chat.active_chat.as_deref(),
                ids: &self.open_chat.message_ids,
                snapshots: &mut self.open_chat.message_snapshots,
                editable: &mut self.open_chat.editable_messages,
                contacts: &mut self.contacts,
                pending: &mut self.open_chat.pending_transcript,
            },
            change,
        );
        if message_changed {
            self.refresh_selected_voice();
        }
    }

    /// Applies the transcript work a backend batch deferred. A rebuild for
    /// any other reason already covers it.
    pub(super) fn flush_transcript(&mut self) {
        if self.open_chat.pending_transcript.rows {
            self.rebuild_message_rows(false);
        }
    }

    pub(super) fn edited(&mut self, chat: String, id: String, success: bool) {
        if !is_edit_completion(self.open_chat.pending_edit.as_ref(), &chat, &id) {
            return;
        }
        self.open_chat.pending_edit = None;
        if success {
            if let Some(request) = self.pending_composer_request.take() {
                self.composer.complete_edit(&request);
            }
            if self
                .open_chat
                .editing
                .as_ref()
                .is_some_and(|editing| editing == &(chat.clone(), id.clone()))
            {
                self.open_chat.editing = None;
                self.draft = self.drafts.get(&chat).cloned().unwrap_or_default();
                self.composer.cancel_context(&chat);
                self.composer.set_draft(&chat, self.draft.clone());
                self.composer_buffer.set_text(&self.draft);
                self.status = gettext("Message updated");
            }
        } else {
            if let Some(request) = self.pending_composer_request.take() {
                let text = request.text().to_owned();
                self.composer.complete_edit(&request);
                self.composer.set_draft(&chat, text);
            }
            self.status = gettext("Edit could not be saved. Draft kept.");
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
            if unchanged
                && self.open_chat.active_chat.as_deref() == Some(&chat)
                && self.draft == pending.text
            {
                self.draft.clear();
                self.composer_buffer.set_text("");
            }
            if unchanged {
                self.drafts.remove(&chat);
            }
            if self
                .open_chat
                .reply_to
                .as_ref()
                .is_some_and(|reply| Some(&reply.1) == pending.reply.as_ref())
            {
                self.open_chat.reply_to = None;
            }
            self.status = gettext("Message sent");
        } else {
            // Put the cleared text back; if the user already typed something
            // new, keep the failed text ahead of it instead of dropping it.
            let merge = |typed: &str| match (pending.text.is_empty(), typed.is_empty()) {
                (true, _) => typed.to_owned(),
                (false, true) => pending.text.clone(),
                (false, false) => format!("{}\n{typed}", pending.text),
            };
            if self.open_chat.active_chat.as_deref() == Some(&chat) {
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
            self.status = gettext("Message could not be sent. Draft kept.");
        }
    }

    /// Title and one-line preview for the bar above the composer: the message
    /// being edited, or the one being replied to and who wrote it.
    pub(super) fn composer_context(&self) -> (String, String) {
        if let Some((_, id)) = &self.open_chat.editing {
            let preview = self
                .open_chat
                .message_snapshots
                .get(id)
                .map(|m| m.summary());
            return ("Editing message".into(), preview.unwrap_or_default());
        }
        let Some(message) = self
            .open_chat
            .reply_to
            .as_ref()
            .and_then(|(_, id)| self.open_chat.message_snapshots.get(id))
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
        self.open_chat.editing.is_none()
            && self.pending_send.is_none()
            && self
                .open_chat
                .active_chat
                .as_deref()
                .and_then(|id| self.chat_snapshots.iter().find(|chat| chat.id == id))
                .is_some_and(|chat| chat.can_send())
    }

    pub(super) fn sync_transcript(&mut self) {
        self.refresh_transcript_text();
        self.clear_missing_selected_voice();
    }

    fn refresh_transcript_text(&mut self) {
        self.open_chat.transcript = transcript_rows(
            &self.open_chat.message_ids,
            &self.open_chat.message_snapshots,
            &self.contacts,
        );
        self.open_chat.pending_transcript.text = false;
    }

    pub(super) fn rebuild_message_rows(&mut self, live_messages_appended: bool) {
        self.open_chat.pending_transcript.rows = false;
        let len = self.open_chat.message_ids.len();
        let mut just_opened = false;
        if self.opened_unread > 0 && len > 0 {
            just_opened = true;
            let first_unread = len - self.opened_unread.min(len);
            self.unread_marker = Some(self.open_chat.message_ids[first_unread].clone());
            self.opened_unread = 0;
        }
        let unread = self
            .unread_marker
            .as_ref()
            .and_then(|marker| {
                self.open_chat
                    .message_ids
                    .iter()
                    .position(|id| id == marker)
            })
            .map_or(0, |position| len - position);
        let messages = self
            .open_chat
            .message_ids
            .iter()
            .filter_map(|id| self.open_chat.message_snapshots.get(id).cloned())
            .collect::<Vec<_>>();
        let voice = |message: &crate::model::Message| self.project_voice(message);
        let (rows, unread_row) = message_rows(
            messages,
            unread,
            RowInputs {
                contacts: &self.contacts,
                avatars: &self.avatars,
                starred: &self.starred,
                selection: self.message_selection.as_deref(),
                link_previews: self.settings.link_previews,
                pointer_sender: &self.pointer_sender,
                audio_registry: &self.audio_registry(),
                voice: &voice,
            },
        );
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
            && old_last.as_ref() != self.open_chat.message_ids.last()
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
                    // A batch of updates spans every row between its first
                    // and last change; the rows in between keep their widget.
                    let unchanged = self
                        .messages
                        .get(position as u32)
                        .is_some_and(|item| item.borrow().renders_like(&row));
                    if !unchanged {
                        self.rebind_message(position, row);
                    }
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
                match unread_row.filter(|_| just_opened) {
                    Some(at) => scroll_to_unread(
                        &self.messages.view,
                        at as u32,
                        self.open_chat.message_ids[at].clone(),
                    ),
                    None => scroll_to_end(&self.messages.view),
                }
            } else {
                glide_to_end(&self.messages.view);
                // Only a new last message fades: older pages, separators and
                // reactions leave the tail unchanged.
                if let Some(id) = self.open_chat.message_ids.last()
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
        if self.open_chat.pending_transcript.text {
            self.refresh_transcript_text();
        }
        if let Some(text) = crate::native_transcript::copied_text(&self.open_chat.transcript) {
            crate::native_portals::NativePortals::write_clipboard_text(
                &self.window.clipboard(),
                &text,
            );
            self.status = gettext("Transcript copied");
        }
    }
}

/// Transcript work a backend batch defers to its end, so a burst of receipts
/// or push names rebuilds the open conversation once instead of per event.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct PendingTranscript {
    rows: bool,
    text: bool,
}

impl PendingTranscript {
    pub(super) fn mark(&mut self) {
        self.rows = true;
        self.text = true;
    }

    /// For changes that alter rows but not the copied text, such as media
    /// state or avatars.
    pub(super) fn mark_rows(&mut self) {
        self.rows = true;
    }

    pub(super) fn clear(&mut self) {
        self.rows = false;
        self.text = false;
    }
}

pub(super) enum TranscriptChange {
    Message(Box<crate::model::Message>),
    Contacts(Vec<crate::model::Contact>),
}

/// The open conversation's state that message updates and contact changes
/// write to.
pub(super) struct OpenTranscript<'a> {
    pub(super) active_chat: Option<&'a str>,
    pub(super) ids: &'a [String],
    pub(super) snapshots: &'a mut std::collections::HashMap<String, crate::model::Message>,
    pub(super) editable: &'a mut std::collections::HashMap<String, String>,
    pub(super) contacts: &'a mut std::collections::HashMap<String, crate::model::Contact>,
    pub(super) pending: &'a mut PendingTranscript,
}

/// Applies one change from a backend batch and marks what the end of the
/// batch has to redraw. Returns whether a message of the open window changed.
pub(super) fn absorb_transcript_change(
    open: &mut OpenTranscript<'_>,
    change: TranscriptChange,
) -> bool {
    match change {
        TranscriptChange::Message(message) => {
            let changed =
                absorb_message_update(open.active_chat, open.snapshots, open.editable, *message);
            if changed {
                open.pending.mark();
            }
            changed
        }
        // An empty list clears contacts on logout; otherwise the backend
        // sends the full set or single updates.
        TranscriptChange::Contacts(contacts) => {
            if contacts_touch_window(open.ids, open.snapshots, &contacts) {
                open.pending.mark();
            }
            if contacts.is_empty() {
                open.contacts.clear();
            }
            open.contacts.extend(
                contacts
                    .into_iter()
                    .map(|contact| (contact.id.clone(), contact)),
            );
            false
        }
    }
}

/// Whether someone else's messages in the open window show `id`'s avatar.
pub(super) fn sender_in_window(
    ids: &[String],
    snapshots: &std::collections::HashMap<String, crate::model::Message>,
    id: &str,
) -> bool {
    ids.iter()
        .filter_map(|known| snapshots.get(known))
        .any(|message| !message.from_me && message.sender == id)
}

/// Stores an update to a message of the open window. Returns whether it
/// changed the window; updates for other chats or unseen rows are ignored.
fn absorb_message_update(
    active_chat: Option<&str>,
    snapshots: &mut std::collections::HashMap<String, crate::model::Message>,
    editable: &mut std::collections::HashMap<String, String>,
    message: crate::model::Message,
) -> bool {
    if active_chat != Some(message.chat.as_str()) || !snapshots.contains_key(&message.id) {
        return false;
    }
    match editable_text(&message) {
        Some(text) => {
            editable.insert(message.id.clone(), text);
        }
        None => {
            editable.remove(&message.id);
        }
    }
    snapshots.insert(message.id.clone(), message);
    true
}

/// Whether new contact details can change the open window. Rows and copied
/// text only read contacts for the saved names of mentioned people.
fn contacts_touch_window(
    ids: &[String],
    snapshots: &std::collections::HashMap<String, crate::model::Message>,
    contacts: &[crate::model::Contact],
) -> bool {
    if ids.is_empty() {
        return false;
    }
    if contacts.is_empty() {
        return true;
    }
    let changed = contacts
        .iter()
        .map(|contact| contact.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    ids.iter()
        .filter_map(|id| snapshots.get(id))
        .flat_map(|message| &message.mentions)
        .any(|mention| changed.contains(mention.id.as_str()))
}

pub(super) fn transcript_rows(
    ids: &[String],
    snapshots: &std::collections::HashMap<String, crate::model::Message>,
    contacts: &std::collections::HashMap<String, crate::model::Contact>,
) -> Vec<crate::native_transcript::TranscriptRow> {
    ids.iter()
        .filter_map(|id| snapshots.get(id))
        .map(|message| transcript_row(message, contacts))
        .collect()
}

/// What message rows read besides the messages themselves.
pub(super) struct RowInputs<'a, S> {
    pub(super) contacts: &'a std::collections::HashMap<String, crate::model::Contact>,
    pub(super) avatars: &'a std::collections::HashMap<String, std::path::PathBuf>,
    pub(super) starred: &'a std::collections::HashSet<String>,
    pub(super) selection: Option<&'a [String]>,
    pub(super) link_previews: bool,
    pub(super) pointer_sender: &'a S,
    pub(super) audio_registry: &'a AudioRegistry,
    pub(super) voice:
        &'a dyn Fn(&crate::model::Message) -> Option<crate::native_voice::VoiceMessage>,
}

/// Rows for the open window, and the position of the unread divider.
pub(super) fn message_rows<S: Clone>(
    messages: Vec<crate::model::Message>,
    unread: usize,
    inputs: RowInputs<'_, S>,
) -> (Vec<MessageRow<S>>, Option<usize>) {
    let timeline = messages
        .iter()
        .map(|message| (message.timestamp, message.from_me))
        .collect::<Vec<_>>();
    let prefixes = conversation_prefixes(&timeline, unread);
    let unread_row = prefixes
        .iter()
        .position(|prefix| prefix.contains(UNREAD_MARKER));
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
            let audio = (inputs.voice)(&message);
            let (show_sender, mut show_timestamp) = boundaries[index];
            if !inputs.link_previews
                && let crate::model::Content::Text { preview, .. } = &mut message.content
            {
                *preview = None;
            }
            let album = std::mem::take(&mut albums[index]);
            // The album's delivery mark and spacing follow its last photo.
            if let Some(last) = album.last() {
                message.status = last.status;
                show_timestamp = boundaries[index + album.len() - 1].1;
            }
            let avatar = (!message.from_me)
                .then(|| inputs.avatars.get(&message.sender).cloned())
                .flatten();
            let starred = inputs.starred.contains(&message.id);
            let mut row = message_row(
                message,
                inputs.contacts,
                inputs.pointer_sender.clone(),
                &prefixes[index],
                avatar,
                RowFlags {
                    show_sender,
                    show_timestamp,
                    starred,
                },
                inputs.audio_registry.clone(),
            );
            row.audio = audio;
            row.album = album;
            row.collapsed = roles[index] == AlbumRole::Follower;
            row.selected = inputs
                .selection
                .map(|selection| selection.contains(&row.id));
            row
        })
        .collect::<Vec<_>>();
    (rows, unread_row)
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

#[cfg(test)]
mod escape_tests {
    use super::EscapeAction::{self, *};

    #[test]
    fn escape_dismisses_the_most_specific_state_before_closing_the_chat() {
        for (state, expected) in [
            ((true, true, true, true), CancelSelection),
            ((false, true, true, true), CancelEdit),
            ((false, false, true, true), CancelReply),
            ((false, false, false, true), CloseChat),
            ((false, false, false, false), Nothing),
        ] {
            let (selecting, editing, replying, chat_open) = state;
            assert_eq!(
                EscapeAction::for_state(selecting, editing, replying, chat_open),
                expected,
                "{state:?}"
            );
        }
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use crate::model::{Contact, Delivery, MentionRef, Message, Reaction};
    use std::collections::{HashMap, HashSet};

    const CHAT: &str = "120363000000000001@g.us";

    fn person(index: usize) -> String {
        format!("1666{index:07}@s.whatsapp.net")
    }

    fn window_message(index: usize) -> Message {
        let from_me = index.is_multiple_of(2);
        let mut message = crate::archive::tests::message(
            CHAT,
            &format!("M{index:05}"),
            1_700_000_000 + index as i64 * 40,
            from_me,
        );
        if !from_me {
            message.sender = person(index % 60);
            message.sender_name = Some(format!("Pushed {index}"));
        }
        if index.is_multiple_of(5) {
            let user = format!("1666{:07}", index % 20);
            message.content = crate::model::Content::text(format!("hi @{user}"));
            message.mentions = vec![MentionRef {
                user,
                id: person(index % 20),
                name: None,
            }];
        }
        message
    }

    /// 200 message updates (receipts and reactions, plus a few for rows
    /// outside the window) and 50 push-name or saved-name changes, as one
    /// history or receipt burst delivers them.
    fn burst(len: usize) -> Vec<TranscriptChange> {
        let mut changes = Vec::new();
        for n in 0..200 {
            let index = (n * 7) % len;
            let mut message = window_message(index);
            if n % 50 == 49 {
                message.chat = "other@s.whatsapp.net".into();
            } else if message.from_me {
                message.status = Delivery::Read;
            } else {
                message.reactions.push(Reaction {
                    sender: person(1),
                    from_me: false,
                    emoji: "👍".into(),
                });
            }
            changes.push(TranscriptChange::Message(Box::new(message)));
            if n % 4 == 3 {
                let who = n / 4;
                changes.push(TranscriptChange::Contacts(vec![Contact {
                    id: person(who),
                    full_name: who.is_multiple_of(2).then(|| format!("Saved {who}")),
                    push_name: Some(format!("Pushed {who}")),
                }]));
            }
        }
        changes
    }

    /// The open conversation's state, driven the way `handle_backend_ready`
    /// drives `NativeApplication`, without a window.
    struct Window {
        ids: Vec<String>,
        snapshots: HashMap<String, Message>,
        editable: HashMap<String, String>,
        contacts: HashMap<String, Contact>,
        pending: PendingTranscript,
        avatars: HashMap<String, std::path::PathBuf>,
        starred: HashSet<String>,
        registry: AudioRegistry,
        rows: Vec<MessageRow<()>>,
        text: Vec<crate::native_transcript::TranscriptRow>,
        rebuilds: usize,
    }

    impl Window {
        fn open(len: usize) -> Self {
            let messages = (0..len).map(window_message).collect::<Vec<_>>();
            let mut window = Self {
                ids: messages.iter().map(|message| message.id.clone()).collect(),
                editable: messages
                    .iter()
                    .filter_map(|message| Some((message.id.clone(), editable_text(message)?)))
                    .collect(),
                snapshots: messages
                    .into_iter()
                    .map(|message| (message.id.clone(), message))
                    .collect(),
                contacts: HashMap::new(),
                pending: PendingTranscript::default(),
                avatars: HashMap::new(),
                starred: HashSet::new(),
                registry: AudioRegistry::default(),
                rows: Vec::new(),
                text: Vec::new(),
                rebuilds: 0,
            };
            window.rebuild();
            window.sync_text();
            window.rebuilds = 0;
            window
        }

        fn rebuild(&mut self) {
            self.pending.rows = false;
            self.rebuilds += 1;
            let messages = self
                .ids
                .iter()
                .filter_map(|id| self.snapshots.get(id).cloned())
                .collect();
            self.rows = message_rows(
                messages,
                0,
                RowInputs {
                    contacts: &self.contacts,
                    avatars: &self.avatars,
                    starred: &self.starred,
                    selection: None,
                    link_previews: true,
                    pointer_sender: &(),
                    audio_registry: &self.registry,
                    voice: &|_| None,
                },
            )
            .0;
        }

        fn sync_text(&mut self) {
            self.text = transcript_rows(&self.ids, &self.snapshots, &self.contacts);
            self.pending.text = false;
        }

        /// The same entry point `handle_backend_ready` uses.
        fn absorb(&mut self, change: TranscriptChange) -> bool {
            absorb_transcript_change(
                &mut OpenTranscript {
                    active_chat: Some(CHAT),
                    ids: &self.ids,
                    snapshots: &mut self.snapshots,
                    editable: &mut self.editable,
                    contacts: &mut self.contacts,
                    pending: &mut self.pending,
                },
                change,
            )
        }

        /// The handlers before batching: every message update redrew the
        /// window, and so did every contact change while a chat was open.
        fn apply_per_event(&mut self, changes: Vec<TranscriptChange>) {
            for change in changes {
                let contacts = matches!(change, TranscriptChange::Contacts(_));
                let message_changed = self.absorb(change);
                if message_changed || (contacts && !self.ids.is_empty()) {
                    self.rebuild();
                    self.sync_text();
                }
            }
        }

        /// What `flush_transcript` and `copy_transcript` do after a batch.
        fn apply_batch(&mut self, changes: Vec<TranscriptChange>) {
            for change in changes {
                self.absorb(change);
            }
            if self.pending.rows {
                self.rebuild();
            }
            if self.pending.text {
                self.sync_text();
            }
        }
    }

    fn assert_same_window(batched: &Window, per_event: &Window) {
        assert_eq!(batched.rows.len(), per_event.rows.len());
        for (batched, per_event) in batched.rows.iter().zip(&per_event.rows) {
            assert_eq!(batched.id, per_event.id);
            assert!(batched.renders_like(per_event), "row {}", batched.id);
            assert_eq!(batched.accessible_label, per_event.accessible_label);
        }
        assert_eq!(batched.text, per_event.text);
        assert_eq!(batched.snapshots, per_event.snapshots);
        assert_eq!(batched.editable, per_event.editable);
    }

    #[test]
    fn a_burst_of_updates_and_names_rebuilds_the_window_once_with_the_same_rows() {
        let mut per_event = Window::open(300);
        per_event.apply_per_event(burst(300));
        let mut batched = Window::open(300);
        batched.apply_batch(burst(300));

        assert_eq!(per_event.rebuilds, 246);
        assert_eq!(batched.rebuilds, 1);
        assert_same_window(&batched, &per_event);
        assert!(
            batched
                .rows
                .iter()
                .any(|row| row.mentions.iter().any(|label| label.label == "Saved 0")),
            "a saved name reaches a mention in the window"
        );
    }

    #[test]
    fn a_saved_name_for_someone_mentioned_rebuilds_the_window_once() {
        let saved = || {
            vec![TranscriptChange::Contacts(vec![Contact {
                id: person(5),
                full_name: Some("Saved 5".into()),
                push_name: None,
            }])]
        };
        let mut per_event = Window::open(40);
        per_event.apply_per_event(saved());
        let mut batched = Window::open(40);
        batched.apply_batch(saved());

        assert_eq!(batched.rebuilds, 1);
        assert_same_window(&batched, &per_event);
        assert!(
            batched
                .rows
                .iter()
                .any(|row| row.mentions.iter().any(|label| label.label == "Saved 5"))
        );
        assert!(batched.text.iter().any(|row| row.text.contains("Saved 5")));
    }

    #[test]
    fn names_of_people_not_mentioned_in_the_window_leave_it_alone() {
        let unrelated = || {
            vec![TranscriptChange::Contacts(vec![Contact {
                id: person(59),
                full_name: Some("Saved 59".into()),
                push_name: Some("Pushed 59".into()),
            }])]
        };
        let mut per_event = Window::open(40);
        per_event.apply_per_event(unrelated());
        let mut batched = Window::open(40);
        batched.apply_batch(unrelated());

        assert_eq!(per_event.rebuilds, 1);
        assert_eq!(batched.rebuilds, 0);
        assert_same_window(&batched, &per_event);
        assert_eq!(
            batched.contacts[&person(59)].full_name.as_deref(),
            Some("Saved 59")
        );
    }

    #[test]
    fn clearing_contacts_rebuilds_an_open_window_only() {
        let mut open = Window::open(40);
        open.apply_batch(vec![TranscriptChange::Contacts(Vec::new())]);
        assert_eq!(open.rebuilds, 1, "an empty list clears every contact");
        let mut closed = Window::open(0);
        closed.apply_batch(vec![TranscriptChange::Contacts(Vec::new())]);
        assert_eq!(closed.rebuilds, 0);
    }

    #[test]
    fn an_avatar_redraws_the_window_only_for_someone_shown_in_it() {
        let window = Window::open(40);
        assert!(sender_in_window(&window.ids, &window.snapshots, &person(1)));
        assert!(!sender_in_window(
            &window.ids,
            &window.snapshots,
            &person(59)
        ));
        let me = window.snapshots["M00000"].sender.clone();
        assert!(
            !sender_in_window(&window.ids, &window.snapshots, &me),
            "own messages show no avatar"
        );
    }

    #[test]
    fn rows_without_mentions_read_nothing_from_contacts() {
        let mut message = window_message(1);
        message.quoted = Some(crate::model::Quoted {
            id: "M00000".into(),
            sender: person(2),
            sender_name: Some("Pushed 2".into()),
            summary: "earlier".into(),
            mentions: Vec::new(),
        });
        message.reactions.push(Reaction {
            sender: person(3),
            from_me: false,
            emoji: "👍".into(),
        });
        assert!(message.mentions.is_empty());
        let everyone = [
            CHAT.to_owned(),
            message.sender.clone(),
            person(2),
            person(3),
        ]
        .into_iter()
        .map(|id| {
            (
                id.clone(),
                Contact {
                    full_name: Some(format!("Saved {id}")),
                    push_name: Some(format!("Pushed {id}")),
                    id,
                },
            )
        })
        .collect::<HashMap<_, _>>();
        let row = |contacts: &HashMap<String, Contact>| {
            message_row(
                message.clone(),
                contacts,
                (),
                "",
                None,
                RowFlags {
                    show_sender: true,
                    show_timestamp: true,
                    starred: false,
                },
                AudioRegistry::default(),
            )
        };
        let (bare, known) = (row(&HashMap::new()), row(&everyone));
        assert!(bare.renders_like(&known));
        for (bare, known) in [
            (&bare.sender, &known.sender),
            (&bare.quote, &known.quote),
            (&bare.body, &known.body),
            (&bare.notes, &known.notes),
            (&bare.accessible_label, &known.accessible_label),
        ] {
            assert_eq!(bare, known);
        }
        assert_eq!(
            transcript_row(&message, &HashMap::new()),
            transcript_row(&message, &everyone)
        );
    }

    /// Run with `cargo test --locked --lib transcript_batch_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark"]
    fn transcript_batch_timing() {
        let len = crate::message_window::ACTIVE_MESSAGE_LIMIT;
        let mut per_event = Window::open(len);
        let started = std::time::Instant::now();
        per_event.apply_per_event(burst(len));
        let before = started.elapsed();
        let mut batched = Window::open(len);
        let started = std::time::Instant::now();
        batched.apply_batch(burst(len));
        let after = started.elapsed();
        assert_same_window(&batched, &per_event);
        eprintln!(
            "{len} rows, 200 updates and 50 names: per event {before:.1?} ({} rebuilds), \
             batched {after:.1?} ({} rebuild)",
            per_event.rebuilds, batched.rebuilds
        );
    }
}
