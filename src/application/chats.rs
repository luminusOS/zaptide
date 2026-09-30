//! Chat-list filtering, projection, and snapshot synchronization.

use super::*;

fn chat_row(chat: crate::model::Chat, avatar: Option<std::path::PathBuf>, open: bool) -> ChatRow {
    let unread = (chat.unread != 0).then(|| chat.unread.to_string());
    let muted = chat.muted(crate::util::now());
    let delivery = chat
        .last
        .as_ref()
        .filter(|last| last.from_me)
        .map_or(crate::model::Delivery::None, |last| last.status);
    let preview = chat
        .last
        .as_ref()
        .map(|last| last.summary.clone())
        .unwrap_or_else(|| "No messages yet".into());
    ChatRow {
        id: chat.id,
        last_activity: chat.last_activity,
        name: chat.name.clone(),
        preview,
        unread,
        avatar,
        pinned: chat.pinned,
        muted,
        quiet: muted || chat.archived,
        delivery,
        open,
    }
}

impl NativeApplication {
    /// Clears search and filters; the sidebar is synchronized after input handling.
    pub(super) fn reset_chat_filters(&mut self, archived: bool) {
        self.chat_filters = crate::native_chat_list::ChatListFilters {
            archive: if archived {
                crate::native_chat_list::ArchiveFilter::Only
            } else {
                crate::native_chat_list::ArchiveFilter::Exclude
            },
            ..Default::default()
        };
        self.chat_projection.set_query("");
        self.search_resets += 1;
        self.chat_projection.set_filters(self.chat_filters);
    }

    /// Sends the tray what changed: window visibility, unread chats that are
    /// not muted or archived, and whether notifications are on.
    pub(super) fn sync_tray(&mut self) {
        let Some(tray) = &self.tray else {
            return;
        };
        let now = crate::util::now();
        let state = crate::native_tray::TrayState {
            unread_chats: self
                .chat_snapshots
                .iter()
                .filter(|chat| {
                    chat.unread > 0 && !chat.archived && !chat.locked && !chat.muted(now)
                })
                .count(),
            notifications: self.settings.notifications,
        };
        if state != self.tray_state {
            tray.update(state.clone());
            self.tray_state = state;
        }
    }

    /// Relabels the chat menu for the open chat. Rebuilt only when a label
    /// changes, so an open menu is left alone by unrelated updates.
    pub(super) fn sync_chat_menu(&mut self) {
        let state = self.selected_chat().map(|chat| {
            (
                chat.is_group(),
                chat.pinned,
                chat.muted(crate::util::now()),
                chat.archived,
            )
        });
        if state == self.chat_menu_state {
            return;
        }
        self.chat_menu_state = state;
        self.chat_menu.remove_all();
        let Some((group, pinned, muted, archived)) = state else {
            return;
        };
        let section = gtk::gio::Menu::new();
        let info = if group {
            "_Group Info"
        } else {
            "_Contact Info"
        };
        section.append(Some(info), Some("win.chat-info"));
        self.chat_menu.append_section(None, &section);
        let section = gtk::gio::Menu::new();
        let pin = if pinned { "Un_pin Chat" } else { "_Pin Chat" };
        section.append(Some(pin), Some("win.chat-pin"));
        let mute = if muted {
            "_Unmute Notifications"
        } else {
            "_Mute Notifications"
        };
        section.append(Some(mute), Some("win.chat-mute"));
        let archive = if archived {
            "Un_archive Chat"
        } else {
            "_Archive Chat"
        };
        section.append(Some(archive), Some("win.chat-archive"));
        self.chat_menu.append_section(None, &section);
        let section = gtk::gio::Menu::new();
        section.append(Some("Copy _Transcript"), Some("win.copy-transcript"));
        self.chat_menu.append_section(None, &section);
    }

    /// Archived chats with unread messages, as counted on the phone.
    pub(super) fn archived_unread_count(&self) -> usize {
        self.chat_snapshots
            .iter()
            .filter(|chat| chat.archived && !chat.locked && chat.unread > 0)
            .count()
    }

    pub(super) fn showing_archived(&self) -> bool {
        self.chat_filters.archive == crate::native_chat_list::ArchiveFilter::Only
    }

    fn leaves_section(&self, chat: &crate::model::Chat) -> bool {
        chat.locked || chat.archived != self.showing_archived()
    }

    /// Whether a search or filter pill, rather than an empty section, hides chats.
    fn chat_list_narrowed(&self) -> bool {
        let filters = self.chat_filters;
        !self.chat_projection.query().is_empty()
            || filters.unread_only
            || filters.pinned_only
            || filters.private_only
            || filters.groups_only
            || filters.muted == crate::native_chat_list::MutedFilter::Only
    }

    pub(super) fn chat_list_empty_title(&self) -> &'static str {
        match (!self.chat_list_narrowed(), self.showing_archived()) {
            (false, _) => "No Results",
            (true, true) => "No Archived Chats",
            (true, false) => "No Chats Yet",
        }
    }

    pub(super) fn chat_list_empty_description(&self) -> &'static str {
        match (!self.chat_list_narrowed(), self.showing_archived()) {
            (false, _) => "No chats match this search or filter.",
            (true, true) => "Archived conversations appear here.",
            (true, false) => "Conversations appear here as WhatsApp syncs.",
        }
    }

    pub(super) fn selected_chat(&self) -> Option<&crate::model::Chat> {
        // The projection refreshes on a throttled flush and hides filtered-out
        // chats, so it can still name the previous chat right after opening one.
        self.active_chat
            .as_ref()
            .and_then(|id| self.chat_snapshots.iter().find(|chat| &chat.id == id))
    }

    pub(super) fn apply_chat_changes(&mut self, changes: Vec<ChatChange>) {
        for change in changes {
            match change {
                ChatChange::Snapshot(chats) => {
                    for chat in &chats {
                        self.request_avatar(&chat.id);
                    }
                    self.reset_chats(chats)
                }
                ChatChange::Update(chat) => {
                    self.request_avatar(&chat.id);
                    self.update_chat(chat)
                }
            }
        }
    }

    fn reset_chats(&mut self, chats: Vec<crate::model::Chat>) {
        self.chat_snapshots = chats;
        self.chats_dirty = true;
        if self.active_chat.as_deref().is_some_and(|id| {
            self.chat_snapshots
                .iter()
                .find(|chat| chat.id == id)
                .is_none_or(|chat| self.leaves_section(chat))
        }) {
            self.clear_active_chat();
        }
    }

    fn update_chat(&mut self, chat: crate::model::Chat) {
        if let Some(index) = self
            .chat_snapshots
            .iter()
            .position(|known| known.id == chat.id)
        {
            self.chat_snapshots.remove(index);
        }
        let index = self
            .chat_snapshots
            .partition_point(|known| known.last_activity >= chat.last_activity);
        if self.leaves_section(&chat) && self.active_chat.as_deref() == Some(&chat.id) {
            self.clear_active_chat();
        }
        self.chat_snapshots.insert(index, chat);
        self.chats_dirty = true;
    }

    /// Rebuilds the chat list widget from the snapshots, once per burst of
    /// backend changes. History sync sends thousands of chat and avatar
    /// updates; rebuilding per event froze the interface.
    pub(super) fn flush_chats(&mut self) {
        if self.chats_dirty {
            self.sync_chat_projection();
        }
    }

    /// Messages arriving in the chat on screen are read as they come in,
    /// but only while the window has focus, as on the phone.
    pub(super) fn read_open_chat(&self) {
        if !self.window.is_active() {
            return;
        }
        let Some(chat) = self.active_chat.as_deref() else {
            return;
        };
        if self
            .chat_snapshots
            .iter()
            .any(|known| known.id == chat && known.unread > 0)
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::MarkRead {
                chat: chat.to_owned(),
                receipts: self.settings.send_read_receipts && !self.account_receipts_off,
            });
        }
    }

    /// Moves the open-chat mark without rebuilding the list: rows between
    /// the old and new open chat would be recreated, losing scroll and focus.
    pub(super) fn mark_open_chat(&self) {
        let open = self.active_chat.as_deref();
        for (position, id) in self.chat_ids.iter().enumerate() {
            if let Some(item) = self.chats.get(position as u32) {
                let is_open = open == Some(id.as_str());
                if item.borrow().open != is_open {
                    item.borrow_mut().open = is_open;
                }
            }
        }
        let mut row = self.chats.view.first_child();
        while let Some(current) = row {
            if let Some(root) = current.first_child() {
                if open == Some(root.widget_name().as_str()) {
                    root.add_css_class("zaptide-chat-open");
                } else {
                    root.remove_css_class("zaptide-chat-open");
                }
            }
            row = current.next_sibling();
        }
    }

    pub(super) fn sync_chat_projection(&mut self) {
        // New activity inserts rows above the visible ones, and the list keeps
        // its anchor on the old first row; stay pinned to the newest chat.
        let at_top = self
            .chats
            .view
            .vadjustment()
            .is_none_or(|adjustment| adjustment.value() <= 0.0);
        if std::mem::take(&mut self.chats_dirty) {
            self.chat_projection
                .replace_snapshot(self.chat_snapshots.clone());
            self.chat_projection.set_filters(self.chat_filters);
        }
        let selected = self.chat_projection.selected_id().map(str::to_owned);
        self.chat_ids.clear();
        let mut rows = Vec::new();
        for chat in self.chat_projection.visible() {
            self.chat_ids.push(chat.id.clone());
            let open = self.active_chat.as_deref() == Some(chat.id.as_str());
            rows.push(chat_row(
                chat.clone(),
                self.avatars.get(&chat.id).cloned(),
                open,
            ));
        }
        // Replace only the changed middle so scrolling and a click in progress
        // survive the frequent small reorders of history sync.
        let old_len = self.chats.len() as usize;
        let (prefix, removed, inserted) = changed_span(old_len, &rows, |position, row| {
            self.chats
                .get(position as u32)
                .is_some_and(|item| *item.borrow() == *row)
        });
        if removed + inserted > old_len.max(rows.len()) / 2 {
            self.chats.clear();
            self.chats.extend_from_iter(rows);
        } else {
            for _ in 0..removed {
                self.chats.remove(prefix as u32);
            }
            for (offset, row) in rows.into_iter().skip(prefix).take(inserted).enumerate() {
                self.chats.insert((prefix + offset) as u32, row);
            }
        }
        if let Some(position) = selected
            .as_deref()
            .and_then(|id| self.chat_ids.iter().position(|known| known == id))
        {
            self.chats.selection_model.set_selected(position as u32);
        }
        if at_top && !self.chat_ids.is_empty() {
            self.chats
                .view
                .scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
    }
}
