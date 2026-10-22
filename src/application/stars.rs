//! Starring messages and browsing the starred ones.

use super::dialogs::{StarredEntry, show_starred_dialog};
use super::presentation::sender_label;
use super::*;

impl NativeApplication {
    pub(super) fn toggle_selected_star(&mut self) {
        let Some((chat, id)) = self.active_chat.clone().zip(self.selected_message_id()) else {
            return;
        };
        let starred = !self.starred.contains(&id);
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::SetStarred { chat, id, starred });
        }
    }

    pub(super) fn star_changed(&mut self, chat: &str, id: String, starred: bool) {
        if self.active_chat.as_deref() != Some(chat) {
            return;
        }
        let changed = if starred {
            self.starred.insert(id.clone())
        } else {
            self.starred.remove(&id)
        };
        if changed {
            self.refresh_message_row(&id);
        }
    }

    pub(super) fn starred_ids_loaded(&mut self, chat: &str, ids: Vec<String>) {
        if self.active_chat.as_deref() != Some(chat) {
            return;
        }
        self.starred = ids.into_iter().collect();
        if !self.message_ids.is_empty() {
            self.rebuild_message_rows(false);
        }
    }

    pub(super) fn show_starred(&self) {
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::LoadStarred);
        }
    }

    pub(super) fn starred_list_loaded(
        &self,
        list: Vec<crate::archive::StarredMessage>,
        sender: &ComponentSender<Self>,
    ) {
        let entries = list
            .into_iter()
            .map(|starred| {
                let message = starred.message;
                let chat_name = self
                    .chat_snapshots
                    .iter()
                    .find(|chat| chat.id == starred.chat)
                    .map_or_else(|| gettext("Chat"), |chat| chat.name.clone());
                let from = if message.from_me {
                    gettext("You")
                } else {
                    sender_label(message.sender_name.as_deref(), &message.sender)
                };
                StarredEntry {
                    chat: starred.chat,
                    chat_name,
                    id: message.id.clone(),
                    sender: from,
                    summary: message.summary(),
                    when: crate::util::moment_stamp(message.timestamp),
                }
            })
            .collect();
        show_starred_dialog(&self.window, &dialog_action_callback(sender), entries);
    }

    /// Opens the chat, then loads older history until the message is there.
    pub(super) fn open_starred(
        &mut self,
        chat: String,
        id: String,
        sender: &ComponentSender<Self>,
    ) {
        self.handle_input(Input::OpenChatId(chat.clone()), sender.clone());
        if self.active_chat.as_deref() != Some(chat.as_str()) {
            return;
        }
        self.pending_quote_navigation = Some((chat, id));
        self.jump_needs_history = true;
    }

    /// Called after a page of the open chat arrived. Scrolls to the target of
    /// a jump, or asks for the history between it and the loaded page once.
    pub(super) fn continue_message_jump(&mut self, chat: &str) {
        let Some((target_chat, target_id)) = self.pending_quote_navigation.clone() else {
            return;
        };
        if target_chat != chat {
            return;
        }
        if self.message_ids.contains(&target_id) {
            self.pending_quote_navigation = None;
            self.jump_needs_history = false;
            self.scroll_message_into_view(&target_id);
            return;
        }
        if !std::mem::take(&mut self.jump_needs_history) {
            return;
        }
        let oldest = self
            .message_ids
            .first()
            .and_then(|id| self.message_snapshots.get(id))
            .map(|message| (message.timestamp, message.id.clone()));
        match (&self.backend, oldest) {
            (Some(backend), Some(before)) => backend.send(crate::backend::Command::LoadUntil {
                chat: target_chat,
                id: target_id,
                before,
            }),
            _ => self.pending_quote_navigation = None,
        }
    }
}
