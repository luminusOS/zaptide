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
            active: self.active_chat.is_some(),
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

    fn sync_composer_view(&self) {
        let state = self.composer_state();
        if !self.composer_view.model().is_synced(&state) {
            self.composer_view.emit(ComposerViewInput::Sync(state));
        }
    }

    fn sync_transcript_view(&self) {
        let state = TranscriptState {
            active: self.active_chat.is_some(),
            has_messages: !self.message_ids.is_empty(),
            history_complete: self.history_complete,
            loading_older: self.loading_older,
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
