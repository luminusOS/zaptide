//! Voice and video calls: the call window, the history list and its details.

use super::*;
use crate::account::AccountId;
use crate::calls::ringtone::{Ring, Ringer};
use crate::calls::{RgbaFrame, VideoSlots};
use crate::i18n::gettext;
use crate::model::call::{
    CallEntry, CallKind, CallPhase, CallSession, call_summary, direction_label, unseen_missed,
    with_detail,
};
use crate::native_notifications::{CallBanner, CallChoice};
use std::sync::Arc;

#[cfg(test)]
mod render;
mod screen;
#[cfg(test)]
mod tests;
mod waiting;
mod window;

pub use screen::CallAction;
use screen::{Screen, incoming_label, screen, starts_day, when};
use waiting::Waiting;
use window::{CallWindow, Look};

fn texture(frame: RgbaFrame) -> Option<gtk::gdk::Texture> {
    let (width, height) = (frame.width as usize, frame.height as usize);
    if width == 0 || height == 0 || frame.data.len() != width * height * 4 {
        return None;
    }
    Some(
        gtk::gdk::MemoryTexture::new(
            width as i32,
            height as i32,
            gtk::gdk::MemoryFormat::R8g8b8a8,
            &gtk::glib::Bytes::from_owned(frame.data),
            width * 4,
        )
        .upcast(),
    )
}

fn avatar_image(path: Option<&std::path::Path>) -> Option<gtk::gdk::Texture> {
    gtk::gdk::Texture::from_filename(path?).ok()
}

fn avatar_for(
    avatars: &std::collections::HashMap<String, std::path::PathBuf>,
    chat: &str,
) -> Option<gtk::gdk::Texture> {
    avatar_image(avatars.get(chat).map(std::path::PathBuf::as_path))
}

struct LiveCall {
    account: AccountId,
    session: CallSession,
    window: CallWindow,
    avatar: Option<gtk::gdk::Texture>,
    tick: Option<gtk::glib::SourceId>,
}

impl LiveCall {
    fn is_active(&self) -> bool {
        !self.session.phase.is_finished()
    }

    /// Calling, connecting or talking. A ringing call does not count, so the
    /// phone keeps its notification until it is answered here.
    fn holds_presence(&self) -> bool {
        matches!(
            self.session.phase,
            CallPhase::Calling | CallPhase::Connecting | CallPhase::Connected { .. }
        )
    }

    fn stop_timer(&mut self) {
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
    }
}

impl Drop for LiveCall {
    fn drop(&mut self) {
        self.stop_timer();
        self.window.window.destroy();
    }
}

struct Detail {
    stack: gtk::Stack,
    avatar: adw::Avatar,
    name: gtk::Label,
    summary: gtk::Label,
    moment: gtk::Label,
    call_back: gtk::Button,
}

/// The Calls tab: the list in the sidebar, the details beside the chat, and
/// the call in progress.
pub(super) struct CallsUi {
    pub(super) list: gtk::ListBox,
    pub(super) details: adw::ToolbarView,
    detail: Detail,
    pub(super) tab: bool,
    entries: Vec<CallEntry>,
    seen_at: i64,
    selected: Option<usize>,
    live: Option<LiveCall>,
    slots: Option<(AccountId, Arc<VideoSlots>)>,
    ringer: Ringer,
    waiting: Option<Waiting>,
    /// The account whose call was closed here and has not reported its end yet.
    abandoned: Option<(AccountId, String)>,
}

impl CallsUi {
    pub(super) fn new(
        on_select: impl Fn(usize) + 'static,
        on_open_chat: impl Fn() + 'static,
        on_call_back: impl Fn() + 'static,
    ) -> Self {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        list.connect_row_activated(move |_, row| {
            if let Ok(index) = usize::try_from(row.index()) {
                on_select(index);
            }
        });

        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("call-start-symbolic"));
        empty.set_title(&gettext("No Call Selected"));
        empty.set_description(Some(&gettext("Pick a call to see its details")));

        let avatar = adw::Avatar::new(128, None, true);
        let name = gtk::Label::new(None);
        name.add_css_class("title-1");
        name.set_wrap(true);
        let summary = gtk::Label::new(None);
        summary.add_css_class("title-4");
        let moment = gtk::Label::new(None);
        moment.add_css_class("dim-label");
        let open_chat = gtk::Button::with_label(&gettext("Open chat"));
        open_chat.add_css_class("pill");
        let call_back = gtk::Button::with_label(&gettext("Call back"));
        call_back.add_css_class("pill");
        call_back.add_css_class("suggested-action");
        call_back.add_css_class("call-primary");
        open_chat.connect_clicked(move |_| on_open_chat());
        call_back.connect_clicked(move |_| on_call_back());
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        actions.set_halign(gtk::Align::Center);
        actions.set_margin_top(12);
        actions.append(&call_back);
        actions.append(&open_chat);
        let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.set_valign(gtk::Align::Center);
        page.set_margin_start(24);
        page.set_margin_end(24);
        for child in [
            avatar.upcast_ref::<gtk::Widget>(),
            name.upcast_ref(),
            summary.upcast_ref(),
            moment.upcast_ref(),
            actions.upcast_ref(),
        ] {
            page.append(child);
        }

        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&page, Some("detail"));
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new(&gettext("Call details"), "")));
        let details = adw::ToolbarView::new();
        details.add_top_bar(&header);
        details.set_content(Some(&stack));

        Self {
            list,
            details,
            detail: Detail {
                stack,
                avatar,
                name,
                summary,
                moment,
                call_back,
            },
            tab: false,
            entries: Vec::new(),
            seen_at: 0,
            selected: None,
            live: None,
            slots: None,
            ringer: Ringer::new(),
            waiting: None,
            abandoned: None,
        }
    }

    pub(super) fn unseen(&self) -> usize {
        unseen_missed(&self.entries, self.seen_at)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn holds_presence(&self, account: AccountId) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.account == account && live.holds_presence())
    }

    /// Whether `account` has a call that has not finished, ringing included.
    pub(super) fn is_busy_for(&self, account: AccountId) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.account == account && live.is_active())
    }

    fn busy_for_other(&self, account: AccountId) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.account != account && live.is_active())
    }

    pub(super) fn in_call(&self) -> bool {
        self.live.as_ref().is_some_and(LiveCall::is_active)
    }

    pub(super) fn stop_ring(&mut self) {
        self.ringer.stop();
    }

    pub(super) fn forget_account(&mut self, account: AccountId) {
        if self
            .live
            .as_ref()
            .is_some_and(|live| live.account == account)
        {
            self.live = None;
        }
        if self
            .waiting
            .as_ref()
            .is_some_and(|waiting| waiting.account == account)
        {
            self.waiting = None;
        }
    }

    pub(super) fn reset(&mut self) {
        self.entries.clear();
        self.seen_at = 0;
        self.selected = None;
        self.refill(&[], &Default::default());
    }

    fn refill(
        &self,
        chats: &[crate::model::Chat],
        avatars: &std::collections::HashMap<String, std::path::PathBuf>,
    ) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let days: Vec<i64> = self.entries.iter().map(|entry| entry.timestamp).collect();
        self.list.set_header_func(move |row, before| {
            let index = usize::try_from(row.index()).ok();
            let timestamp = index.and_then(|index| days.get(index)).copied();
            let previous = before
                .and_then(|before| usize::try_from(before.index()).ok())
                .and_then(|index| days.get(index))
                .copied();
            match timestamp {
                Some(timestamp) if starts_day(previous, timestamp) => {
                    row.set_header(Some(&day_heading(timestamp)));
                }
                _ => row.set_header(None::<&gtk::Widget>),
            }
        });
        for entry in &self.entries {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&chat_label(
                chats,
                &entry.chat,
            )));
            row.set_subtitle(&entry.history_line(&crate::util::clock(entry.timestamp)));
            let avatar = adw::Avatar::new(40, Some(&chat_label(chats, &entry.chat)), true);
            avatar.set_custom_image(avatar_for(avatars, &entry.chat).as_ref());
            row.add_prefix(&avatar);
            let icon = gtk::Image::from_icon_name(if entry.video {
                "camera-video-symbolic"
            } else {
                "call-start-symbolic"
            });
            icon.add_css_class("dim-label");
            row.add_suffix(&icon);
            if entry.kind() == CallKind::Missed {
                row.add_css_class("zaptide-call-missed");
            }
            self.list.append(&row);
        }
        match self.selected {
            Some(index) => {
                if let Some(row) = self.list.row_at_index(index as i32) {
                    self.list.select_row(Some(&row));
                }
            }
            None => self.list.unselect_all(),
        }
    }

    fn show_selected(
        &self,
        chats: &[crate::model::Chat],
        avatars: &std::collections::HashMap<String, std::path::PathBuf>,
        busy: bool,
    ) {
        let Some(entry) = self.selected.and_then(|index| self.entries.get(index)) else {
            self.detail.stack.set_visible_child_name("empty");
            return;
        };
        let name = chat_label(chats, &entry.chat);
        self.detail.avatar.set_text(Some(&name));
        self.detail
            .avatar
            .set_custom_image(avatar_for(avatars, &entry.chat).as_ref());
        self.detail.name.set_label(&name);
        self.detail.summary.set_label(&call_summary(
            entry.video,
            entry.outgoing,
            entry.outcome,
            entry.seconds,
        ));
        self.detail.moment.set_label(&with_detail(
            &direction_label(entry.outgoing, entry.video),
            &when(entry.timestamp),
        ));
        self.detail.call_back.set_sensitive(!busy);
        self.detail.stack.set_visible_child_name("detail");
    }
}

/// The ring a call in `phase` makes. `incoming_allowed` is asked only for an
/// incoming call, the one sound the person may have turned off.
fn ring_for(session: &CallSession, incoming_allowed: impl FnOnce() -> bool) -> Option<Ring> {
    match session.phase {
        CallPhase::Ringing => incoming_allowed().then_some(Ring::Incoming),
        CallPhase::Calling => Some(Ring::Outgoing),
        _ => None,
    }
}

/// What a notification button does to a call in `phase`: answering or
/// declining only makes sense while it rings.
fn choice_command(choice: CallChoice, phase: CallPhase) -> Option<crate::backend::Command> {
    use crate::backend::Command;
    match (choice, phase) {
        (CallChoice::Answer, CallPhase::Ringing) => Some(Command::AnswerCall),
        (CallChoice::Decline, CallPhase::Ringing) => Some(Command::DeclineCall),
        _ => None,
    }
}

/// Whether the desktop is set not to show banners, its do-not-disturb switch.
fn banners_hidden() -> bool {
    use gtk::gio::prelude::*;
    const SCHEMA: &str = "org.gnome.desktop.notifications";
    gtk::gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(SCHEMA, true))
        .is_some_and(|_| !gtk::gio::Settings::new(SCHEMA).boolean("show-banners"))
}

fn day_heading(timestamp: i64) -> gtk::Label {
    let heading = gtk::Label::new(Some(&crate::util::day_label(timestamp)));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    heading.set_margin_top(12);
    heading.set_margin_bottom(4);
    heading.set_margin_start(12);
    heading.set_margin_end(12);
    heading
}

fn chat_label(chats: &[crate::model::Chat], id: &str) -> String {
    chats
        .iter()
        .find(|chat| chat.id == id)
        .map(|chat| chat.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| crate::util::phone(id.split('@').next().unwrap_or(id)))
}

impl NativeApplication {
    pub(super) fn calls_details(&self) -> &adw::ToolbarView {
        &self.calls.details
    }

    pub(super) fn can_offer_call(&self) -> bool {
        self.active_chat.as_ref().is_some_and(|id| {
            self.chat_snapshots
                .iter()
                .find(|chat| &chat.id == id)
                .is_some_and(|chat| chat.kind == crate::model::ChatKind::Direct)
        })
    }

    pub(super) fn can_call(&self) -> bool {
        self.link.is_connected() && !self.calls.in_call() && self.can_offer_call()
    }

    fn call_backend(&self, account: AccountId) -> Option<&Backend> {
        if account == self.active_account {
            self.backend.as_ref()
        } else {
            self.accounts.get(&account)?.backend.as_ref()
        }
    }

    fn chats_of(&self, account: AccountId) -> &[crate::model::Chat] {
        if account == self.active_account {
            &self.chat_snapshots
        } else {
            self.accounts
                .get(&account)
                .map(|session| session.chat_snapshots.as_slice())
                .unwrap_or_default()
        }
    }

    fn avatars_of(
        &self,
        account: AccountId,
    ) -> Option<&std::collections::HashMap<String, std::path::PathBuf>> {
        if account == self.active_account {
            Some(&self.avatars)
        } else {
            self.accounts.get(&account).map(|session| &session.avatars)
        }
    }

    pub(super) fn start_call(&mut self, video: bool) {
        if !self.can_call() {
            return;
        }
        let (Some(chat), Some(backend)) = (self.active_chat.clone(), &self.backend) else {
            return;
        };
        backend.send(crate::backend::Command::StartCall { chat, video });
    }

    pub(super) fn show_calls_tab(&mut self, on: bool) {
        self.calls.tab = on;
        if on {
            self.sidebar_visible = true;
            if let Some(backend) = &self.backend {
                backend.send(crate::backend::Command::LoadCalls);
                backend.send(crate::backend::Command::MarkCallsSeen);
            }
        }
    }

    pub(super) fn calls_loaded(&mut self, entries: Vec<CallEntry>, seen_at: i64) {
        let selected_id = self
            .calls
            .selected
            .and_then(|index| self.calls.entries.get(index))
            .map(|entry| (entry.chat.clone(), entry.id.clone()));
        self.calls.entries = entries;
        self.calls.seen_at = if self.calls.tab {
            crate::util::now()
        } else {
            seen_at
        };
        self.calls.selected = selected_id.and_then(|(chat, id)| {
            self.calls
                .entries
                .iter()
                .position(|entry| entry.chat == chat && entry.id == id)
        });
        self.calls.refill(&self.chat_snapshots, &self.avatars);
        self.calls
            .show_selected(&self.chat_snapshots, &self.avatars, self.calls.in_call());
    }

    pub(super) fn select_call(&mut self, index: usize) {
        if index >= self.calls.entries.len() {
            return;
        }
        self.calls.selected = Some(index);
        self.calls
            .show_selected(&self.chat_snapshots, &self.avatars, self.calls.in_call());
        self.sidebar_visible = false;
    }

    fn selected_call(&self) -> Option<&CallEntry> {
        self.calls
            .selected
            .and_then(|index| self.calls.entries.get(index))
    }

    pub(super) fn call_back(&mut self) {
        let Some(entry) = self.selected_call().cloned() else {
            return;
        };
        if self.calls.in_call() || !self.link.is_connected() {
            return;
        }
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::StartCall {
                chat: entry.chat,
                video: entry.video,
            });
        }
    }

    pub(super) fn selected_call_chat(&self) -> Option<String> {
        self.selected_call().map(|entry| entry.chat.clone())
    }

    pub(super) fn handle_call_event(
        &mut self,
        account: AccountId,
        event: NativeEvent,
        sender: &ComponentSender<Self>,
    ) {
        match event {
            NativeEvent::CallVideo(slots) => {
                if !self.calls.busy_for_other(account) {
                    self.calls.slots = Some((account, slots));
                }
            }
            NativeEvent::CallFrames => self.paint_call_video(),
            NativeEvent::Call(session) => self.call_changed(account, session, sender),
            NativeEvent::CallWaiting(session) => self.waiting_changed(account, session),
            _ => {}
        }
    }

    fn paint_call_video(&mut self) {
        let Some((_, slots)) = &self.calls.slots else {
            return;
        };
        let update = slots.take();
        let Some(live) = &self.calls.live else {
            return;
        };
        if let Some(remote) = update.remote {
            live.window
                .remote
                .set_paintable(remote.and_then(texture).as_ref());
        }
        if let Some(local) = update.local {
            live.window
                .preview
                .set_paintable(local.and_then(texture).as_ref());
        }
    }

    fn call_changed(
        &mut self,
        account: AccountId,
        session: CallSession,
        sender: &ComponentSender<Self>,
    ) {
        if session.phase != CallPhase::Ringing {
            self.notifications.clear_call(account, &session.id);
        }
        let finished = session.phase.is_finished();
        self.apply_call_change(account, session, sender);
        if finished {
            self.promote_waiting(account, sender);
        }
        self.sync_ring();
    }

    /// Rings while the call in the window is incoming or being placed, and
    /// only for an incoming call when the person has not asked for quiet.
    pub(super) fn sync_ring(&mut self) {
        let tone = self.calls.live.as_ref().and_then(|live| {
            ring_for(&live.session, || {
                self.settings.notifications && !banners_hidden()
            })
        });
        self.calls.ringer.set(tone);
    }

    fn apply_call_change(
        &mut self,
        account: AccountId,
        session: CallSession,
        sender: &ComponentSender<Self>,
    ) {
        if session.phase.is_finished()
            && account != self.active_account
            && let Some(backend) = self.call_backend(account)
        {
            backend.send(crate::backend::Command::SetOnline(false));
        }
        if let Some((abandoned, id)) = &self.calls.abandoned
            && *abandoned == account
        {
            if session.phase.is_finished() {
                self.calls.abandoned = None;
                return;
            }
            if session.id == *id {
                return;
            }
            self.calls.abandoned = None;
        }
        let same = self.calls.live.as_ref().is_some_and(|live| {
            live.account == account
                && !(live.session.phase.is_finished() && live.session.id != session.id)
        });
        let name = chat_label(self.chats_of(account), &session.chat);
        let avatar_path = self
            .avatars_of(account)
            .and_then(|avatars| avatars.get(&session.chat))
            .cloned();
        if !same && self.calls.busy_for_other(account) {
            self.other_account_call(account, session);
            return;
        }
        if !same && session.phase.is_finished() {
            return;
        }
        if session.phase == CallPhase::Ringing && !same {
            self.notify_call(
                account,
                &session,
                CallBanner::Ringing,
                &name,
                avatar_path.as_deref(),
            );
        }
        if matches!(
            session.phase,
            CallPhase::Ended(crate::model::CallOutcome::Elsewhere)
        ) {
            self.calls.live = None;
            return;
        }
        if !same {
            let input = sender.clone();
            let window = CallWindow::new(std::rc::Rc::new(move |action| {
                input.input(Input::CallAction(action))
            }));
            self.calls.live = Some(LiveCall {
                account,
                session: session.clone(),
                window,
                avatar: None,
                tick: None,
            });
        }
        let screen = self.full_screen(&session, &name);
        let allow_focus = self.window.is_active()
            || self
                .calls
                .live
                .as_ref()
                .is_some_and(|live| live.window.window.is_active());
        let waiting_avatar = self.calls.waiting.as_ref().and_then(|w| w.avatar.clone());
        let Some(live) = self.calls.live.as_mut() else {
            return;
        };
        live.session = session;
        live.avatar = avatar_image(avatar_path.as_deref());
        live.window.render(
            &name,
            &Look {
                avatar: live.avatar.as_ref(),
                waiting_avatar: waiting_avatar.as_ref(),
                allow_focus,
            },
            &screen,
        );
        if !same {
            live.window.window.present();
        }
        let connected = matches!(live.session.phase, CallPhase::Connected { .. });
        if connected && live.tick.is_none() {
            let input = sender.clone();
            live.tick = Some(gtk::glib::timeout_add_seconds_local(1, move || {
                input.input(Input::CallTick);
                gtk::glib::ControlFlow::Continue
            }));
        } else if !connected {
            live.stop_timer();
        }
        if live.session.phase.is_finished() {
            if self
                .calls
                .slots
                .as_ref()
                .is_some_and(|(owner, _)| *owner == account)
            {
                self.calls.slots = None;
            }
            self.calls
                .show_selected(&self.chat_snapshots, &self.avatars, false);
            if let Some(backend) = self.call_backend(account) {
                backend.send(crate::backend::Command::LoadCalls);
            }
        } else {
            self.calls
                .show_selected(&self.chat_snapshots, &self.avatars, true);
        }
    }

    /// Announces a call that rings. The main window being in front already
    /// shows a lone incoming call, but a waiting one is announced regardless,
    /// as the call window is taken.
    fn notify_call(
        &self,
        account: AccountId,
        session: &CallSession,
        banner: CallBanner,
        name: &str,
        avatar: Option<&std::path::Path>,
    ) {
        let seen_anyway = banner == CallBanner::Ringing
            && account == self.active_account
            && self.window.is_active();
        if !self.settings.notifications || seen_anyway {
            return;
        }
        let body = incoming_label(session.video);
        if let Err(error) =
            self.notifications
                .show_call(account, &session.id, banner, name, &body, avatar)
        {
            log::warn!("could not show a notification: {error}");
        }
    }

    /// A button or the body of a call's notification was used. A notification
    /// left over from a call that is no longer ringing does nothing.
    pub(super) fn call_choice(&mut self, account: AccountId, call_id: &str, choice: CallChoice) {
        if self.is_waiting(account, call_id) {
            match choice {
                CallChoice::Decline => self.decline_waiting(),
                CallChoice::EndAndAnswer => self.end_and_answer(),
                CallChoice::Show => {
                    if let Some(live) = &self.calls.live {
                        live.window.window.present();
                    }
                }
                CallChoice::Answer => {}
            }
            return;
        }
        let Some(live) = self
            .calls
            .live
            .as_ref()
            .filter(|live| live.account == account && live.session.id == call_id)
        else {
            return;
        };
        if choice == CallChoice::Show {
            live.window.window.present();
            return;
        }
        if let Some(command) = choice_command(choice, live.session.phase)
            && let Some(backend) = self.call_backend(account)
        {
            backend.send(command);
        }
    }

    pub(super) fn call_tick(&mut self) {
        let Some((account, session)) = self
            .calls
            .live
            .as_ref()
            .map(|live| (live.account, live.session.clone()))
        else {
            return;
        };
        let name = chat_label(self.chats_of(account), &session.chat);
        let screen = self.full_screen(&session, &name);
        let allow_focus = self.window.is_active()
            || self
                .calls
                .live
                .as_ref()
                .is_some_and(|live| live.window.window.is_active());
        let waiting_avatar = self.calls.waiting.as_ref().and_then(|w| w.avatar.clone());
        if let Some(live) = self.calls.live.as_mut() {
            live.window.render(
                &name,
                &Look {
                    avatar: live.avatar.as_ref(),
                    waiting_avatar: waiting_avatar.as_ref(),
                    allow_focus,
                },
                &screen,
            );
        }
    }

    pub(super) fn call_action(&mut self, action: CallAction, sender: &ComponentSender<Self>) {
        self.run_call_action(action, sender);
        self.sync_ring();
    }

    fn run_call_action(&mut self, action: CallAction, sender: &ComponentSender<Self>) {
        let Some(live) = &self.calls.live else {
            return;
        };
        let (account, session) = (live.account, live.session.clone());
        let command = |command| {
            if let Some(backend) = self.call_backend(account) {
                backend.send(command);
            }
        };
        use crate::backend::Command;
        match action {
            CallAction::Answer => command(Command::AnswerCall),
            CallAction::Decline => command(Command::DeclineCall),
            CallAction::HangUp => command(Command::HangUpCall),
            CallAction::ToggleMute => command(Command::SetCallMuted(!session.muted)),
            CallAction::ToggleCamera => command(Command::SetCallCamera(!session.camera_on)),
            CallAction::AcceptVideo => command(Command::AcceptCallVideo),
            CallAction::Retry => {
                self.close_call_window(account);
                if let Some(backend) = self.call_backend(account) {
                    backend.send(Command::StartCall {
                        chat: session.chat,
                        video: session.video,
                    });
                }
            }
            CallAction::DeclineWaiting => self.decline_waiting(),
            CallAction::EndAndAnswer => self.end_and_answer(),
            CallAction::BackToChat => {
                self.close_call_window(account);
                sender.input(Input::OpenAccountChat(account, session.chat));
            }
            CallAction::Close => {
                if !session.phase.is_finished() {
                    command(Command::HangUpCall);
                    self.calls.abandoned = Some((account, session.id));
                }
                self.close_call_window(account);
            }
        }
    }

    fn close_call_window(&mut self, account: AccountId) {
        if self
            .calls
            .live
            .as_ref()
            .is_some_and(|live| live.account == account)
        {
            self.calls.live = None;
        }
    }

    pub(super) fn reload_calls(&mut self) {
        self.calls.reset();
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::LoadCalls);
        }
    }
}
