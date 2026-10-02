use super::*;

impl NativeApplication {
    pub(super) fn sync_components(&self, draft_empty_before: Option<(bool, bool)>) {
        if let Some((before, after)) = draft_empty_before {
            if composer_draft_state_changed(before, after) {
                self.sync_composer_view();
            }
        } else {
            self.sync_link_page();
            self.sync_transcript_view();
            self.sync_composer_view();
            self.sync_sidebar();
        }
    }

    fn composer_state(&self) -> ComposerState {
        let (context_title, context_preview) = self.composer_context();
        let has_context = self.reply_to.is_some() || self.editing.is_some();
        let chat = self
            .active_chat
            .as_deref()
            .and_then(|id| self.chat_snapshots.iter().find(|chat| chat.id == id));
        ComposerState {
            active: self.active_chat.is_some() && self.message_selection.is_none(),
            recording: self.recording_active(),
            recording_blink: self.recording_blink(),
            recording_time: self.recording_time(),
            recording_send_enabled: self.pending_send.is_none() && !self.audio.voice_send_pending,
            context: has_context.then_some((context_title, context_preview)),
            attachment_count: self.pending_attachment_count(),
            attachment_paths: self
                .active_chat
                .as_ref()
                .and_then(|chat| self.pending_attachments.get(chat))
                .cloned()
                .unwrap_or_default(),
            clipboard_preview: self
                .active_chat
                .as_ref()
                .and_then(|chat| self.pending_clipboard_images.get(chat))
                .map(|image| image.preview.clone()),
            can_attach: self.can_attach(),
            can_mention: chat.is_some_and(|chat| !chat.participants.is_empty()),
            mention_candidates: self.mention_candidates(chat),
            editable: !self.audio.voice_send_pending
                && chat.is_some_and(crate::model::Chat::can_send),
            editing: self.editing.is_some(),
            draft_empty: self.draft.trim().is_empty(),
            can_send_voice: !self.recording_active()
                && self.pending_send.is_none()
                && !self.audio.voice_send_pending
                && chat.is_some_and(crate::model::Chat::can_send),
            can_send: chat.is_some_and(crate::model::Chat::can_send),
        }
    }

    pub(super) fn sync_composer_view(&self) {
        let state = self.composer_state();
        if !self.composer_view.model().is_synced(&state) {
            self.composer_view.emit(ComposerViewInput::Sync(state));
        }
    }

    fn mention_candidates(
        &self,
        chat: Option<&crate::model::Chat>,
    ) -> Vec<crate::native_composer::MentionCandidate> {
        let (Some(chat), Some(query)) = (
            chat,
            crate::native_composer::active_mention_query(&self.draft),
        ) else {
            return Vec::new();
        };
        let needle = query.to_lowercase();
        let mut candidates: Vec<_> = self
            .mention_labels(chat)
            .into_iter()
            .filter(|candidate| candidate.label.to_lowercase().contains(&needle))
            .collect();
        candidates.sort_by_cached_key(|candidate| candidate.label.to_lowercase());
        candidates.truncate(8);
        candidates
    }

    /// Inserts `@label ` with the mention highlighted in the accent colour.
    pub(super) fn insert_mention(&self, at: &mut gtk::TextIter, label: &str) {
        let buffer = &self.composer_buffer;
        let tag = buffer.tag_table().lookup("mention").unwrap_or_else(|| {
            let tag = buffer
                .create_tag(Some("mention"), &[("weight", &700)])
                .expect("mention tag is new");
            let style = adw::StyleManager::default();
            let recolor = tag.clone();
            let paint = move |style: &adw::StyleManager| {
                recolor.set_foreground_rgba(Some(
                    &style.accent_color().to_standalone_rgba(style.is_dark()),
                ));
            };
            paint(&style);
            style.connect_dark_notify(paint.clone());
            style.connect_accent_color_notify(paint);
            tag
        });
        buffer.insert_with_tags(at, &format!("@{label}"), &[&tag]);
        buffer.insert(at, " ");
    }

    /// Visible mention label per participant. Duplicate names get the phone
    /// appended so each label resolves to one person when sending.
    pub(super) fn mention_labels(
        &self,
        chat: &crate::model::Chat,
    ) -> Vec<crate::native_composer::MentionCandidate> {
        let mut candidates: Vec<_> = chat
            .participants
            .iter()
            .enumerate()
            .map(|(index, id)| crate::native_composer::MentionCandidate {
                id: id.clone(),
                label: self.participant_label(id, index),
                avatar: self.avatars.get(id).cloned(),
            })
            .collect();
        let duplicated: Vec<bool> = candidates
            .iter()
            .map(|candidate| {
                candidates
                    .iter()
                    .filter(|other| other.label == candidate.label)
                    .count()
                    > 1
            })
            .collect();
        for (candidate, duplicated) in candidates.iter_mut().zip(duplicated) {
            if duplicated {
                let phone = crate::model::phone_of(&candidate.id)
                    .map(crate::util::phone)
                    .unwrap_or_else(|| candidate.id.clone());
                candidate.label = format!("{} ({phone})", candidate.label);
            }
        }
        candidates
    }

    fn sync_transcript_view(&self) {
        let state = TranscriptState {
            active: self.active_chat.is_some(),
            has_messages: !self.message_ids.is_empty(),
            history_complete: self.history_complete,
            loading_older: self.loading_older,
            recent_messages_pending: self.recent_messages_pending,
        };
        if !self.transcript_view.model().is_synced(&state) {
            self.transcript_view.emit(TranscriptViewInput::Sync(state));
        }
    }

    fn sync_sidebar(&self) {
        let state = SidebarState {
            search_resets: self.search_resets,
            filters: self.chat_filters,
            archived: self.showing_archived(),
            archived_unread: self.archived_unread_count(),
            refreshing: self.refreshing(),
            empty: self.chat_ids.is_empty(),
            empty_title: self.chat_list_empty_title(),
            empty_description: self.chat_list_empty_description(),
        };
        if !self.sidebar.model().is_synced(&state) {
            self.sidebar.emit(SidebarInput::Sync(state));
        }
    }

    fn sync_link_page(&self) {
        let state = LinkPageState {
            link: self.link.clone(),
            title: self.page_title.clone(),
            status: self.status.clone(),
            qr_texture: self.qr_texture.clone(),
            phone_linking: self.phone_linking,
            busy: self.link_busy(),
        };
        if !self.link_page.model().is_synced(&state) {
            self.link_page.emit(LinkPageInput::Sync(state));
        }
    }
}
