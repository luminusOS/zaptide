use super::*;
use crate::i18n::gettext;

/// Captures the displayed account and chat ID before a GTK activation is queued.
#[derive(Default)]
pub(super) struct ChatTargets {
    account: Option<crate::account::AccountId>,
    ids: Vec<String>,
}

impl ChatTargets {
    fn sync(&mut self, account: crate::account::AccountId, ids: &[String]) {
        self.account = Some(account);
        ids.clone_into(&mut self.ids);
    }

    pub(super) fn get(&self, position: u32) -> Option<(crate::account::AccountId, String)> {
        Some((self.account?, self.ids.get(position as usize)?.clone()))
    }

    pub(super) fn position(&self, account: crate::account::AccountId, chat: &str) -> Option<u32> {
        if self.account != Some(account) {
            return None;
        }
        self.ids
            .iter()
            .position(|id| id == chat)
            .map(|position| position as u32)
    }
}

fn chat_row(chat: &crate::model::Chat, avatar: Option<std::path::PathBuf>, open: bool) -> ChatRow {
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
        id: chat.id.clone(),
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

fn update_chat_rows(
    chats: &mut TypedListView<ChatRow, gtk::SingleSelection>,
    rows: Vec<ChatRow>,
    selected: Option<&str>,
    at_top: bool,
) {
    use super::sidebar_coordinates::RestorePlan;
    let old_len = chats.len() as usize;
    let (prefix, removed, inserted) = changed_span(old_len, &rows, |position, row| {
        chats
            .get(position as u32)
            .is_some_and(|item| *item.borrow() == *row)
    });
    let coordinates = chats
        .view
        .parent()
        .and_downcast::<super::sidebar_coordinates::SidebarCoordinates>();
    let changed = removed != 0 || inserted != 0;
    let destination = if changed && !at_top {
        let ids: Vec<_> = rows.iter().map(|row| row.id.clone()).collect();
        coordinates
            .as_ref()
            .map_or(RestorePlan::NoGeometry, |bridge| bridge.begin_restore(&ids))
    } else {
        if at_top
            && changed
            && let Some(bridge) = coordinates.as_ref()
        {
            bridge.cancel_restore();
        }
        RestorePlan::NoGeometry
    };
    let selected_position = selected.and_then(|id| rows.iter().position(|row| row.id == id));
    if removed + inserted > old_len.max(rows.len()) / 2 {
        chats.clear();
        chats.extend_from_iter(rows);
    } else {
        for _ in 0..removed {
            chats.remove(prefix as u32);
        }
        for (offset, row) in rows.into_iter().skip(prefix).take(inserted).enumerate() {
            chats.insert((prefix + offset) as u32, row);
        }
    }
    chats
        .selection_model
        .set_selected(selected_position.map_or(gtk::INVALID_LIST_POSITION, |i| i as u32));
    if let RestorePlan::Anchor(position) = destination {
        chats
            .view
            .scroll_to(position, gtk::ListScrollFlags::NONE, None);
    } else if (at_top || matches!(destination, RestorePlan::NoSurvivor)) && !chats.is_empty() {
        chats.view.scroll_to(0, gtk::ListScrollFlags::NONE, None);
    }
}

impl NativeApplication {
    pub(super) fn select_chat(&mut self, position: u32) {
        if let Some(bridge) = self
            .chats
            .view
            .parent()
            .and_downcast::<super::sidebar_coordinates::SidebarCoordinates>()
        {
            bridge.cancel_restore();
        }
        self.close_media_viewer();
        let Some(chat) = self.chat_ids.get(position as usize).cloned() else {
            return;
        };
        self.calls.showing_live = false;
        if self
            .split_view
            .as_ref()
            .is_some_and(adw::NavigationSplitView::is_collapsed)
        {
            self.sidebar_visible = false;
        }
        if self.open_chat.active_chat.as_deref() != Some(&chat) {
            self.cancel_portal_requests();
            self.audio.media.stop_playback();
            self.audio.playing_audio = None;
            self.audio.waveform_queue.clear();
            if let Some(cancel) = self.audio.waveform_cancel.take() {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            self.audio.audio_waveforms.clear();
            self.audio.waveform_attempted.clear();
            self.audio.audio_errors.clear();
            self.audio.audio_registry.borrow_mut().clear();
        }
        self.notifications.clear_chat(self.active_account, &chat);
        if let Some(previous) = self.open_chat.active_chat.as_deref() {
            if self
                .open_chat
                .editing
                .as_ref()
                .is_some_and(|(editing_chat, _)| editing_chat == previous)
            {
                self.composer.set_draft(
                    previous,
                    self.drafts.get(previous).cloned().unwrap_or_default(),
                );
            }
            self.composer.cancel_context(previous);
        }
        self.chat_projection.select(chat.clone());
        self.chats.selection_model.set_selected(position);
        self.open_chat.active_chat = Some(chat.clone());
        self.mark_open_chat();
        self.open_chat.reply_to = None;
        self.open_chat.editing = None;
        self.open_chat.pending_edit = None;
        self.draft = self.composer.draft(&chat).to_owned();
        self.composer_buffer.set_text(&self.draft);
        self.messages.clear();
        self.message_target = None;
        self.message_target_selection = None;
        self.opened_unread = self
            .chat_snapshots
            .iter()
            .find(|known| known.id == chat)
            .map_or(0, |known| known.unread as usize);
        self.unread_marker = None;
        self.recent_messages_pending = false;
        self.history_complete = false;
        self.loading_older = false;
        self.open_chat.message_ids.clear();
        self.message_selection = None;
        self.starred.clear();
        self.open_chat.message_snapshots.clear();
        self.open_chat.editable_messages.clear();
        self.open_chat.transcript.clear();
        self.open_chat.pending_transcript.clear();
        self.audio.selected_voice = None;
        self.audio.selected_voice_message = None;
        self.page_title = self
            .chat_snapshots
            .iter()
            .find(|known| known.id == chat)
            .map(|known| known.name.clone())
            .unwrap_or_else(|| "Conversation".into());
        self.status = gettext("Loading messages");
        let request = self.transcript_requests.reload();
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::MarkRead {
                chat: chat.clone(),
                receipts: self.settings.send_read_receipts && !self.account_receipts_off,
            });
            backend.send(crate::backend::Command::LoadChat {
                chat,
                before: None,
                request,
            });
        }
        self.focus_composer();
    }

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

    /// Unread chats in the accounts that are not on screen.
    pub(super) fn unread_elsewhere(&self) -> usize {
        self.accounts
            .values()
            .map(|session| super::accounts::unread_chat_count(&session.chat_snapshots))
            .sum()
    }

    /// Sends the tray what changed: window visibility, unread chats that are
    /// not muted or archived, and whether notifications are on.
    pub(super) fn sync_tray(&mut self) {
        let state = crate::native_tray::TrayState {
            unread_chats: super::accounts::unread_chat_count(&self.chat_snapshots)
                + self.unread_elsewhere(),
            notifications: self.settings.notifications,
        };
        if state == self.tray_state {
            return;
        }
        if let Some(tray) = &self.tray {
            tray.update(state.clone());
        }
        if crate::portal::sandboxed() {
            crate::native_background::set_status(
                &crate::native_tray::unread_label(state.unread_chats)
                    .unwrap_or_else(|| "No unread chats".into()),
            );
        }
        self.tray_state = state;
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
        section.append(Some(&gettext("_Close Chat")), Some("win.chat-close"));
        self.chat_menu.append_section(None, &section);
        let section = gtk::gio::Menu::new();
        section.append(
            Some(&gettext("Copy _Transcript")),
            Some("win.copy-transcript"),
        );
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
        self.open_chat
            .active_chat
            .as_ref()
            .and_then(|id| self.chat_snapshots.iter().find(|chat| &chat.id == id))
            .map(std::rc::Rc::as_ref)
    }

    pub(super) fn apply_chat_changes(&mut self, changes: Vec<ChatChange>) {
        for change in changes {
            match change {
                ChatChange::Snapshot(chats) => self.reset_chats(chats),
                ChatChange::Update(chat) => {
                    self.request_avatar(&chat.id);
                    self.update_chat(chat)
                }
            }
        }
    }

    fn reset_chats(&mut self, chats: Vec<crate::model::Chat>) {
        self.reset_shared_chats(chats.into_iter().map(std::rc::Rc::new).collect());
    }

    pub(super) fn reset_shared_chats(&mut self, chats: Vec<std::rc::Rc<crate::model::Chat>>) {
        for chat in &chats {
            self.request_avatar(&chat.id);
        }
        self.chat_snapshots = chats;
        self.chats_dirty = true;
        if self.open_chat.active_chat.as_deref().is_some_and(|id| {
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
        if self.leaves_section(&chat) && self.open_chat.active_chat.as_deref() == Some(&chat.id) {
            self.clear_active_chat();
        }
        self.chat_snapshots.insert(index, std::rc::Rc::new(chat));
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
        let Some(chat) = self.open_chat.active_chat.as_deref() else {
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
        let open = self.open_chat.active_chat.as_deref();
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
            .parent()
            .and_downcast::<super::sidebar_coordinates::SidebarCoordinates>()
            .is_none_or(|bridge| bridge.at_top());
        if std::mem::take(&mut self.chats_dirty) {
            self.chat_projection
                .replace_shared_snapshot(self.chat_snapshots.iter().cloned());
            self.chat_projection.set_filters(self.chat_filters);
        }
        let selected = self.chat_projection.selected_id().map(str::to_owned);
        self.chat_ids.clear();
        let mut rows = Vec::new();
        for chat in self.chat_projection.visible() {
            self.chat_ids.push(chat.id.clone());
            let open = self.open_chat.active_chat.as_deref() == Some(chat.id.as_str());
            rows.push(chat_row(chat, self.avatars.get(&chat.id).cloned(), open));
        }
        self.chat_targets
            .borrow_mut()
            .sync(self.active_account, &self.chat_ids);
        update_chat_rows(&mut self.chats, rows, selected.as_deref(), at_top);
    }
}

#[cfg(test)]
mod account_selection_tests {
    use super::*;

    #[derive(Default)]
    struct ReorderCase {
        remove_anchor: bool,
        repeat: bool,
        remove_all: bool,
        input: bool,
        lose_target: bool,
        no_geometry: bool,
        early_restore: bool,
        delayed_attempts: u32,
    }

    fn verify_reordered_chat_rows(case: ReorderCase) {
        let ReorderCase {
            remove_anchor,
            repeat,
            remove_all,
            input,
            lose_target,
            no_geometry,
            early_restore,
            delayed_attempts,
        } = case;
        gtk::init().unwrap();
        adw::init().unwrap();
        let mut chats: TypedListView<ChatRow, gtk::SingleSelection> = TypedListView::new();
        let row = |index: usize| ChatRow {
            id: format!("synthetic-{index}"),
            last_activity: index as i64,
            name: format!("Chat {index}"),
            preview: "Synthetic preview".into(),
            unread: None,
            avatar: None,
            pinned: false,
            muted: false,
            quiet: false,
            delivery: crate::model::Delivery::None,
            open: false,
        };
        chats.extend_from_iter((0..300).map(row));
        let bridge = super::super::sidebar_coordinates::SidebarCoordinates::new(&chats.view);
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_child(Some(&bridge));
        let window = gtk::Window::builder()
            .default_width(320)
            .default_height(400)
            .child(&scrolled)
            .build();
        window.present();
        let context = gtk::glib::MainContext::default();
        let settle = || {
            for _ in 0..100 {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        settle();
        chats.view.scroll_to(130, gtk::ListScrollFlags::NONE, None);
        settle();
        if no_geometry {
            chats.selection_model.set_selected(123);
            settle();
        }
        let visible = || {
            let mut result = Vec::new();
            let mut item = chats.view.first_child();
            while let Some(current) = item {
                if let Some(root) = current.first_child()
                    && current.is_mapped()
                    && let Some(bounds) = root.compute_bounds(&chats.view)
                    && bounds.y() >= 0.0
                    && bounds.y() < chats.view.height() as f32
                {
                    result.push((root.widget_name().to_string(), bounds.y()));
                }
                item = current.next_sibling();
            }
            result
        };
        let visible = visible();
        let removed_id = remove_anchor.then(|| visible[0].0.clone());
        let (id, before) = visible[usize::from(remove_anchor)].clone();
        let proxy = scrolled.vadjustment();
        let original_proxy = proxy.value();
        if input {
            proxy.set_value(proxy.value() + 5.0);
        }
        let accepted_before_commit = proxy.value() - original_proxy;
        if input {
            assert_eq!(accepted_before_commit, 5.0);
        }
        let proxy_before = proxy.value();
        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = snapshots.clone();
        let observed_id = id.clone();
        let weak_bridge = bridge.downgrade();
        bridge.observe_snapshot(move |view| {
            let mut item = view.first_child();
            let mut y = None;
            while let Some(current) = item {
                if current.is_mapped()
                    && let Some(root) = current.first_child()
                    && root.widget_name() == observed_id
                    && let Some(bounds) = root.compute_bounds(view)
                {
                    y = Some(bounds.y());
                    break;
                }
                item = current.next_sibling();
            }
            observed
                .borrow_mut()
                .push((y, weak_bridge.upgrade().unwrap().restore_pending()));
        });
        if no_geometry {
            bridge.forget_geometry();
        }
        if delayed_attempts > 0 {
            bridge.delay_materialization(delayed_attempts);
        }
        let indices: Vec<_> = if no_geometry {
            (0..300).collect()
        } else {
            (0..300).rev().collect()
        };
        let mut rows: Vec<_> = indices
            .into_iter()
            .map(row)
            .filter(|row| removed_id.as_ref() != Some(&row.id))
            .collect();
        if no_geometry {
            rows[299].preview = "Updated synthetic preview".into();
        }
        let selected = rows.iter_mut().find(|row| row.id == id).unwrap();
        selected.open = !no_geometry;
        update_chat_rows(&mut chats, rows, Some(&id), false);
        if early_restore {
            chats.view.scroll_to(0, gtk::ListScrollFlags::NONE, None);
            chats.view.queue_allocate();
            chats
                .view
                .allocate(chats.view.width(), chats.view.height(), -1, None);
            bridge.attempt_restore_before_layout(&chats.view);
            assert!(
                bridge.restore_pending(),
                "existing but unmaterialized anchor must remain pending"
            );
        }
        if repeat {
            let rows: Vec<_> = (0..300)
                .rev()
                .map(|i| {
                    let mut item = row(i);
                    item.open = item.id == id;
                    item
                })
                .collect();
            update_chat_rows(&mut chats, rows, Some(&id), false);
        }
        assert_eq!(
            scrolled.vadjustment().value().to_bits(),
            proxy_before.to_bits()
        );
        let before_second_input = proxy.value();
        if input {
            proxy.set_value(proxy.value() + 5.0);
        }
        let expected_proxy = proxy.value();
        let accepted_after_commit = expected_proxy - before_second_input;
        if input {
            assert_eq!(accepted_after_commit, 5.0);
        }
        let expected_y = before - (accepted_before_commit + accepted_after_commit) as f32;
        assert!(snapshots.borrow().is_empty());
        assert_eq!(
            chats.get(0).unwrap().borrow().id,
            if no_geometry {
                "synthetic-0"
            } else {
                "synthetic-299"
            }
        );
        let selected = chats.selection_model.selected();
        assert_eq!(chats.get(selected).unwrap().borrow().id, id);
        assert_eq!(chats.get(selected).unwrap().borrow().open, !no_geometry);
        if remove_all {
            let rows = (0..300)
                .map(|i| {
                    let mut item = row(i);
                    item.id = format!("replacement-{i}");
                    item
                })
                .collect();
            if lose_target {
                chats.clear();
                chats.extend_from_iter(rows);
            } else {
                update_chat_rows(&mut chats, rows, None, false);
            }
            assert_eq!(chats.get(0).unwrap().borrow().id, "replacement-0");
            assert!(snapshots.borrow().is_empty());
            settle();
            let (old_anchor, pending) = snapshots.borrow().first().copied().unwrap();
            assert_eq!(old_anchor, None);
            assert!(!pending);
            let proxy = scrolled.vadjustment();
            let real = chats.view.vadjustment().unwrap();
            assert_eq!(real.value(), real.lower());
            proxy.set_value(proxy.value() + 100.0);
            assert!(real.value() > real.lower());
            window.close();
            return;
        }
        settle();
        if delayed_attempts > 0 {
            assert_eq!(bridge.materialization_attempts(), 3);
            assert!(!bridge.restore_pending());
            window.close();
            return;
        }
        assert_eq!(
            scrolled.vadjustment().value().to_bits(),
            expected_proxy.to_bits()
        );
        let (first_snapshot, pending) = snapshots.borrow().first().copied().unwrap();
        assert!(!pending, "first snapshot must not paint a pending restore");
        assert!(
            first_snapshot.is_some_and(|y| (y - expected_y).abs() <= 2.0),
            "{id}: expected={expected_y}, first snapshot={first_snapshot:?}"
        );
        let mut item = chats.view.first_child();
        let mut after = None;
        while let Some(current) = item {
            if let Some(root) = current.first_child()
                && current.is_mapped()
                && root.widget_name() == id
                && let Some(bounds) = root.compute_bounds(&chats.view)
            {
                after = Some(bounds.y());
                break;
            }
            item = current.next_sibling();
        }
        assert!(
            after.is_some_and(|y| (y - expected_y).abs() <= 2.0),
            "{id}: expected={expected_y}, after={after:?}"
        );
        window.close();
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn reordered_chat_rows_keep_visible_anchor() {
        verify_reordered_chat_rows(ReorderCase::default());
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn removed_anchor_uses_visible_survivor() {
        verify_reordered_chat_rows(ReorderCase {
            remove_anchor: true,
            ..Default::default()
        });
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn unchanged_update_keeps_pending_restore() {
        verify_reordered_chat_rows(ReorderCase {
            repeat: true,
            ..Default::default()
        });
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn chained_removal_falls_back_before_first_snapshot() {
        verify_reordered_chat_rows(ReorderCase {
            remove_all: true,
            ..Default::default()
        });
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn proxy_input_before_and_after_commit_is_preserved() {
        verify_reordered_chat_rows(ReorderCase {
            input: true,
            ..Default::default()
        });
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn missing_materialized_target_falls_back_before_snapshot() {
        verify_reordered_chat_rows(ReorderCase {
            remove_all: true,
            lose_target: true,
            ..Default::default()
        });
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn missing_geometry_does_not_force_top() {
        verify_reordered_chat_rows(ReorderCase {
            no_geometry: true,
            ..Default::default()
        });
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn existing_unmaterialized_anchor_is_retried() {
        verify_reordered_chat_rows(ReorderCase {
            early_restore: true,
            ..Default::default()
        });
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn artificial_materialization_exhaustion_cancels_restore() {
        verify_reordered_chat_rows(ReorderCase {
            delayed_attempts: 4,
            ..Default::default()
        });
    }

    #[test]
    fn queued_selection_is_rejected_after_an_account_switch() {
        let mut targets = ChatTargets::default();
        targets.sync(crate::account::AccountId(1), &["shared".into()]);
        let (account, chat) = targets.get(0).unwrap();
        targets.sync(crate::account::AccountId(2), &["shared".into()]);
        assert_eq!(targets.position(account, &chat), None);
    }

    #[test]
    fn queued_selection_follows_chat_identity_after_rows_reorder() {
        let mut targets = ChatTargets::default();
        let account = crate::account::AccountId(1);
        targets.sync(account, &["one".into(), "two".into()]);
        let (_, chat) = targets.get(0).unwrap();
        targets.sync(account, &["two".into(), "one".into()]);
        assert_eq!(targets.position(account, &chat), Some(1));
        assert_eq!(targets.get(99), None);
        targets.sync(account, &["two".into()]);
        assert_eq!(targets.position(account, &chat), None);
    }
}
