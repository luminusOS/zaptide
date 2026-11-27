//! Starring messages and browsing the starred ones.

use super::dialogs::{StarredEntry, show_starred_dialog};
use super::presentation::sender_label;
use super::*;

/// Only a star this user just added is confirmed; one the phone synced, or a
/// removal, changes the bubble silently.
pub(super) fn confirms_starring(changed: bool, starred: bool, local: bool) -> bool {
    changed && starred && local
}

fn starred_toast() -> adw::Toast {
    let text = gettext("Message starred");
    let title = gtk::Box::builder().spacing(8).build();
    title.append(&decorative_star(-1));
    title.append(&gtk::Label::new(Some(&text)));
    adw::Toast::builder()
        .title(text)
        .use_markup(false)
        .custom_title(&title)
        .build()
}

impl NativeApplication {
    pub(super) fn toggle_selected_star(&mut self) {
        let Some((chat, id)) = self
            .open_chat
            .active_chat
            .clone()
            .zip(self.selected_message_id())
        else {
            return;
        };
        let starred = !self.starred.contains(&id);
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::SetStarred { chat, id, starred });
        }
    }

    pub(super) fn star_changed(&mut self, chat: &str, id: String, starred: bool, local: bool) {
        if self.open_chat.active_chat.as_deref() != Some(chat) {
            return;
        }
        let changed = if starred {
            self.starred.insert(id.clone())
        } else {
            self.starred.remove(&id)
        };
        if confirms_starring(changed, starred, local) {
            self.add_toast(starred_toast());
        }
        if changed {
            self.refresh_message_row(&id);
        }
    }

    pub(super) fn starred_ids_loaded(&mut self, chat: &str, ids: Vec<String>) {
        if self.open_chat.active_chat.as_deref() != Some(chat) {
            return;
        }
        self.starred = ids.into_iter().collect();
        if !self.open_chat.message_ids.is_empty() {
            self.rebuild_message_rows(false);
        }
    }

    pub(super) fn keep_offered(&self, message: &crate::model::Message) -> bool {
        let can_change = self
            .open_chat
            .keepable
            .get(&message.id)
            .is_some_and(|deadline| {
                self.open_chat.kept.contains(&message.id)
                    || *deadline > jiff::Timestamp::now().as_second()
            });
        can_change
            && !matches!(
                message.content,
                crate::model::Content::Revoked | crate::model::Content::System { .. }
            )
            && !message.content.view_once()
    }

    pub(super) fn toggle_selected_keep(&mut self) {
        if !self
            .selected_message()
            .is_some_and(|message| self.keep_offered(message))
        {
            return;
        }
        let Some((chat, id)) = self
            .open_chat
            .active_chat
            .clone()
            .zip(self.selected_message_id())
        else {
            return;
        };
        let kept = !self.open_chat.kept.contains(&id);
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::SetKept { chat, id, kept });
        }
    }

    pub(super) fn kept_changed(
        &mut self,
        chat: &str,
        id: String,
        kept: bool,
        deadline: i64,
        local: bool,
        restored: Option<Box<crate::model::Message>>,
    ) {
        if self.open_chat.active_chat.as_deref() != Some(chat) {
            return;
        }
        let loaded = self.open_chat.message_snapshots.contains_key(&id);
        if !loaded && restored.is_none() {
            return;
        }
        self.open_chat.keepable.insert(id.clone(), deadline);
        let changed = if kept {
            self.open_chat.kept.insert(id.clone())
        } else {
            self.open_chat.kept.remove(&id)
        };
        if changed && local {
            self.add_toast(adw::Toast::new(&if kept {
                gettext("Message kept")
            } else {
                gettext("Message no longer kept")
            }));
        }
        if !loaded && let Some(message) = restored {
            if !self.restore_kept_message(*message) {
                self.open_chat.kept.remove(&id);
                self.open_chat.keepable.remove(&id);
            }
        } else if changed {
            self.refresh_message_row(&id);
        }
    }

    pub(super) fn keep_states_loaded(&mut self, states: Vec<(String, i64, bool)>) {
        for (id, deadline, kept) in states {
            if kept {
                self.open_chat.kept.insert(id.clone());
            } else {
                self.open_chat.kept.remove(&id);
            }
            self.open_chat.keepable.insert(id, deadline);
        }
    }

    pub(super) fn show_starred(&self, chat: String) {
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::LoadStarred(chat));
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
                let from = if message.from_me {
                    gettext("You")
                } else {
                    sender_label(message.sender_name.as_deref(), &message.sender)
                };
                StarredEntry {
                    chat: starred.chat,
                    id: message.id.clone(),
                    sender: from,
                    summary: message.summary(),
                    when: crate::util::moment_stamp(message.timestamp),
                }
            })
            .collect();
        show_starred_dialog(
            &self.window,
            self.active_account,
            &dialog_action_callback(sender),
            entries,
        );
    }

    /// Scrolls to a starred message of the open chat, loading older history
    /// first when it is not on screen.
    pub(super) fn open_starred(&mut self, chat: String, id: String) {
        if self.open_chat.active_chat.as_deref() != Some(chat.as_str()) {
            return;
        }
        self.pending_quote_navigation = Some((chat.clone(), id));
        self.jump_needs_history = true;
        if self.transcript_requests.initial.is_none() {
            self.continue_message_jump(&chat);
        }
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
        if self.open_chat.message_ids.contains(&target_id) {
            self.pending_quote_navigation = None;
            self.jump_needs_history = false;
            self.scroll_message_into_view(&target_id);
            return;
        }
        if !std::mem::take(&mut self.jump_needs_history) {
            return;
        }
        let oldest = self
            .open_chat
            .message_ids
            .first()
            .and_then(|id| self.open_chat.message_snapshots.get(id))
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
