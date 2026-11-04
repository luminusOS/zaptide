//! Voice and video calls: the call window, the history list and its details.

use super::*;
use crate::account::AccountId;
use crate::calls::{RgbaFrame, VideoSlots};
use crate::i18n::gettext;
use crate::model::call::{
    CallEntry, CallKind, CallPhase, CallSession, call_clock, call_summary, unseen_missed,
    with_detail,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallAction {
    Answer,
    Decline,
    HangUp,
    ToggleMute,
    ToggleCamera,
    AcceptVideo,
    Retry,
    BackToChat,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Neutral,
    Danger,
    Success,
    Primary,
}

impl Tone {
    fn class(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Danger => "danger",
            Self::Success => "success",
            Self::Primary => "primary",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CallButton {
    pub(super) action: CallAction,
    pub(super) label: String,
    pub(super) icon: &'static str,
    pub(super) tone: Tone,
    pub(super) sensitive: bool,
}

fn button(action: CallAction, label: String, icon: &'static str, tone: Tone) -> CallButton {
    CallButton {
        action,
        label,
        icon,
        tone,
        sensitive: true,
    }
}

fn unless_busy(mut button: CallButton, session: &CallSession) -> CallButton {
    button.sensitive = !session.camera_busy;
    button
}

/// What the call window says and offers for one moment of a call.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Screen {
    pub(super) status: String,
    pub(super) detail: Option<String>,
    pub(super) buttons: Vec<CallButton>,
}

fn timed(label: String, seconds: Option<u32>) -> String {
    match seconds {
        Some(seconds) => with_detail(&label, &call_clock(seconds)),
        None => label,
    }
}

pub(super) fn screen(session: &CallSession, name: &str, now: i64) -> Screen {
    let hang_up = |label: String| {
        button(
            CallAction::HangUp,
            label,
            "call-stop-symbolic",
            Tone::Danger,
        )
    };
    let back = || {
        button(
            CallAction::BackToChat,
            gettext("Back to chat"),
            "go-previous-symbolic",
            Tone::Neutral,
        )
    };
    let mut detail = None;
    let (status, buttons) = match session.phase {
        CallPhase::Ringing => (
            if session.video {
                gettext("Incoming video call")
            } else {
                gettext("Incoming voice call")
            },
            vec![
                button(
                    CallAction::Decline,
                    gettext("Decline"),
                    "call-stop-symbolic",
                    Tone::Danger,
                ),
                button(
                    CallAction::Answer,
                    gettext("Answer"),
                    "call-start-symbolic",
                    Tone::Success,
                ),
            ],
        ),
        CallPhase::Calling => (gettext("Calling…"), vec![hang_up(gettext("Cancel call"))]),
        CallPhase::Connecting => (gettext("Connecting…"), vec![hang_up(gettext("End call"))]),
        CallPhase::Connected { .. } => {
            let seconds = session.seconds(now);
            let status = if session.muted {
                timed(gettext("Microphone muted"), seconds)
            } else {
                timed(gettext("Connected"), seconds)
            };
            let mut buttons = Vec::new();
            if session.video_requested {
                detail = Some(gettext("%1$s wants to switch to video").replace("%1$s", name));
                buttons.push(unless_busy(
                    button(
                        CallAction::AcceptVideo,
                        gettext("Accept video"),
                        "camera-video-symbolic",
                        Tone::Success,
                    ),
                    session,
                ));
            }
            buttons.push(if session.muted {
                button(
                    CallAction::ToggleMute,
                    gettext("Unmute"),
                    "microphone-sensitivity-muted-symbolic",
                    Tone::Neutral,
                )
            } else {
                button(
                    CallAction::ToggleMute,
                    gettext("Mute"),
                    "microphone-sensitivity-high-symbolic",
                    Tone::Neutral,
                )
            });
            if !session.video_requested {
                let camera = if session.camera_on {
                    button(
                        CallAction::ToggleCamera,
                        gettext("Turn camera off"),
                        "camera-disabled-symbolic",
                        Tone::Neutral,
                    )
                } else if session.video || session.peer_video {
                    button(
                        CallAction::ToggleCamera,
                        gettext("Turn camera on"),
                        "camera-video-symbolic",
                        Tone::Neutral,
                    )
                } else {
                    button(
                        CallAction::ToggleCamera,
                        gettext("Start video"),
                        "camera-video-symbolic",
                        Tone::Neutral,
                    )
                };
                buttons.push(unless_busy(camera, session));
            }
            buttons.push(hang_up(gettext("End call")));
            (status, buttons)
        }
        CallPhase::Ended(outcome) => (
            call_summary(
                session.video,
                session.is_outgoing(),
                outcome,
                session.talk_seconds,
            ),
            vec![back()],
        ),
        CallPhase::Failed => (
            gettext("Couldn't connect the call"),
            vec![
                button(
                    CallAction::Retry,
                    gettext("Try again"),
                    "view-refresh-symbolic",
                    Tone::Primary,
                ),
                back(),
            ],
        ),
    };
    Screen {
        status,
        detail,
        buttons,
    }
}

pub(super) fn kind_label(kind: CallKind) -> String {
    match kind {
        CallKind::Missed => gettext("Missed"),
        CallKind::Outgoing => gettext("Outgoing"),
        CallKind::Incoming => gettext("Incoming"),
    }
}

fn when(timestamp: i64) -> String {
    with_detail(
        &crate::util::day_label(timestamp),
        &crate::util::clock(timestamp),
    )
}

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

struct CallWindow {
    window: adw::Window,
    title: adw::WindowTitle,
    remote: gtk::Picture,
    preview: gtk::Picture,
    identity: gtk::Box,
    avatar: adw::Avatar,
    name: gtk::Label,
    status: gtk::Label,
    detail: gtk::Label,
    buttons: adw::WrapBox,
    shown: Vec<CallButton>,
    on_action: std::rc::Rc<dyn Fn(CallAction)>,
}

impl CallWindow {
    fn new(on_action: std::rc::Rc<dyn Fn(CallAction)>) -> Self {
        let window = adw::Window::builder()
            .title(gettext("Call"))
            .default_width(420)
            .default_height(640)
            .build();
        window.add_css_class("zaptide-call-window");
        let application: gtk::Application = relm4::main_application().upcast();
        window.set_application(Some(&application));
        let close = on_action.clone();
        window.connect_close_request(move |_| {
            close(CallAction::Close);
            gtk::glib::Propagation::Stop
        });

        let title = adw::WindowTitle::new("", "");
        title.set_visible(false);
        let header = adw::HeaderBar::new();
        header.add_css_class("flat");
        header.set_title_widget(Some(&title));

        let remote = gtk::Picture::new();
        remote.set_content_fit(gtk::ContentFit::Contain);
        remote.set_vexpand(true);
        remote.set_visible(false);
        remote.add_css_class("zaptide-call-remote");

        let avatar = adw::Avatar::new(128, None, true);
        let name = gtk::Label::new(None);
        name.add_css_class("title-1");
        name.set_wrap(true);
        name.set_justify(gtk::Justification::Center);
        let status = gtk::Label::new(None);
        status.add_css_class("title-4");
        status.set_accessible_role(gtk::AccessibleRole::Status);
        let detail = gtk::Label::new(None);
        detail.add_css_class("dim-label");
        detail.set_visible(false);
        let identity = gtk::Box::new(gtk::Orientation::Vertical, 12);
        identity.set_vexpand(true);
        identity.set_valign(gtk::Align::Center);
        identity.set_margin_start(24);
        identity.set_margin_end(24);
        for child in [
            avatar.upcast_ref::<gtk::Widget>(),
            name.upcast_ref(),
            status.upcast_ref(),
            detail.upcast_ref(),
        ] {
            identity.append(child);
        }

        let buttons = adw::WrapBox::new();
        buttons.set_child_spacing(12);
        buttons.set_line_spacing(12);
        buttons.set_align(0.5);
        buttons.set_margin_start(24);
        buttons.set_margin_end(24);
        buttons.set_margin_top(12);
        buttons.set_margin_bottom(32);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&remote);
        body.append(&identity);
        body.append(&buttons);

        let preview = gtk::Picture::new();
        preview.set_content_fit(gtk::ContentFit::Cover);
        preview.set_size_request(120, 160);
        preview.set_halign(gtk::Align::End);
        preview.set_valign(gtk::Align::Start);
        preview.set_margin_end(16);
        preview.set_margin_top(12);
        preview.set_can_target(false);
        preview.set_visible(false);
        preview.add_css_class("zaptide-call-preview");
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&body));
        overlay.add_overlay(&preview);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&overlay));
        window.set_content(Some(&toolbar));

        Self {
            window,
            title,
            remote,
            preview,
            identity,
            avatar,
            name,
            status,
            detail,
            buttons,
            shown: Vec::new(),
            on_action,
        }
    }

    fn render(
        &mut self,
        session: &CallSession,
        name: &str,
        avatar: Option<&gtk::gdk::Texture>,
        screen: &Screen,
    ) {
        let connected = matches!(session.phase, CallPhase::Connected { .. });
        let remote_video = connected && session.peer_video;
        self.window
            .set_title(Some(&format!("{name} · {}", screen.status)));
        self.title.set_title(name);
        self.title.set_subtitle(&screen.status);
        self.title.set_visible(remote_video);
        self.identity.set_visible(!remote_video);
        self.remote.set_visible(remote_video);
        self.preview
            .set_visible(session.camera_on && !session.phase.is_finished());
        self.avatar.set_text(Some(name));
        self.avatar.set_custom_image(avatar);
        self.name.set_label(name);
        self.status.set_label(&screen.status);
        self.detail
            .set_label(screen.detail.as_deref().unwrap_or(""));
        self.detail.set_visible(screen.detail.is_some());
        if self.shown != screen.buttons {
            while let Some(child) = self.buttons.first_child() {
                self.buttons.remove(&child);
            }
            for spec in &screen.buttons {
                let content = adw::ButtonContent::new();
                content.set_icon_name(spec.icon);
                content.set_label(&spec.label);
                let widget = gtk::Button::new();
                widget.set_child(Some(&content));
                widget.add_css_class("zaptide-call-button");
                widget.add_css_class(spec.tone.class());
                widget.set_sensitive(spec.sensitive);
                let action = spec.action;
                let on_action = self.on_action.clone();
                widget.connect_clicked(move |_| on_action(action));
                self.buttons.append(&widget);
            }
            self.shown = screen.buttons.clone();
        }
    }
}

struct LiveCall {
    account: AccountId,
    session: CallSession,
    window: CallWindow,
    avatar: Option<gtk::gdk::Texture>,
    tick: Option<gtk::glib::SourceId>,
}

impl LiveCall {
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
        open_chat.connect_clicked(move |_| on_open_chat());
        call_back.connect_clicked(move |_| on_call_back());
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        actions.set_halign(gtk::Align::Center);
        actions.set_margin_top(12);
        actions.append(&open_chat);
        actions.append(&call_back);
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
            abandoned: None,
        }
    }

    pub(super) fn unseen(&self) -> usize {
        unseen_missed(&self.entries, self.seen_at)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn in_call(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| !live.session.phase.is_finished())
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
        for entry in &self.entries {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&chat_label(
                chats,
                &entry.chat,
            )));
            row.set_subtitle(&with_detail(
                &kind_label(entry.kind()),
                &when(entry.timestamp),
            ));
            let avatar = adw::Avatar::new(40, Some(&chat_label(chats, &entry.chat)), true);
            avatar.set_custom_image(
                avatar_image(avatars.get(&entry.chat).map(|p| p.as_path())).as_ref(),
            );
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
            .set_custom_image(avatar_image(avatars.get(&entry.chat).map(|p| p.as_path())).as_ref());
        self.detail.name.set_label(&name);
        self.detail.summary.set_label(&call_summary(
            entry.video,
            entry.outgoing,
            entry.outcome,
            entry.seconds,
        ));
        self.detail.moment.set_label(&when(entry.timestamp));
        self.detail.call_back.set_sensitive(!busy);
        self.detail.stack.set_visible_child_name("detail");
    }
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
                let keeps_other_call = self.calls.live.as_ref().is_some_and(|live| {
                    live.account != account && !live.session.phase.is_finished()
                });
                if !keeps_other_call {
                    self.calls.slots = Some((account, slots));
                }
            }
            NativeEvent::CallFrames => self.paint_call_video(),
            NativeEvent::Call(session) => self.call_changed(account, session, sender),
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
        let other_account_busy = self
            .calls
            .live
            .as_ref()
            .is_some_and(|live| live.account != account && !live.session.phase.is_finished());
        let name = chat_label(self.chats_of(account), &session.chat);
        let avatar_path = self
            .avatars_of(account)
            .and_then(|avatars| avatars.get(&session.chat))
            .cloned();
        if !same && other_account_busy {
            if session.phase == CallPhase::Ringing {
                self.notify_ringing(account, &session, &name, avatar_path.as_deref());
            }
            return;
        }
        if !same && session.phase.is_finished() {
            return;
        }
        if session.phase == CallPhase::Ringing && !same {
            self.notify_ringing(account, &session, &name, avatar_path.as_deref());
        } else if session.phase != CallPhase::Ringing {
            self.notifications.clear_chat(account, &session.chat);
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
        let screen = screen(&session, &name, crate::util::now());
        let Some(live) = self.calls.live.as_mut() else {
            return;
        };
        live.session = session;
        live.avatar = avatar_image(avatar_path.as_deref());
        live.window
            .render(&live.session, &name, live.avatar.as_ref(), &screen);
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

    fn notify_ringing(
        &self,
        account: AccountId,
        session: &CallSession,
        name: &str,
        avatar: Option<&std::path::Path>,
    ) {
        if !self.settings.notifications
            || (account == self.active_account && self.window.is_active())
        {
            return;
        }
        let body = if session.video {
            gettext("Incoming video call")
        } else {
            gettext("Incoming voice call")
        };
        if let Err(error) = self
            .notifications
            .show(account, &session.chat, name, &body, avatar)
        {
            log::warn!("could not show a notification: {error}");
        }
    }

    pub(super) fn call_tick(&mut self) {
        let Some(live) = self.calls.live.as_mut() else {
            return;
        };
        let name = chat_label(
            if live.account == self.active_account {
                &self.chat_snapshots
            } else {
                self.accounts
                    .get(&live.account)
                    .map(|session| session.chat_snapshots.as_slice())
                    .unwrap_or_default()
            },
            &live.session.chat,
        );
        let screen = screen(&live.session, &name, crate::util::now());
        live.window
            .render(&live.session, &name, live.avatar.as_ref(), &screen);
    }

    pub(super) fn call_action(&mut self, action: CallAction, sender: &ComponentSender<Self>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::call::{CallDirection, CallOutcome, CallSignal};

    fn session(direction: CallDirection, video: bool) -> CallSession {
        CallSession::new("call".into(), "chat".into(), direction, video, 100)
    }

    fn actions(screen: &Screen) -> Vec<CallAction> {
        screen.buttons.iter().map(|button| button.action).collect()
    }

    fn connected(direction: CallDirection, video: bool) -> CallSession {
        let mut call = session(direction, video);
        call.apply(
            match direction {
                CallDirection::Incoming => CallSignal::LocalAnswered,
                CallDirection::Outgoing => CallSignal::PeerAnswered,
            },
            100,
        );
        call.apply(CallSignal::MediaUp, 100);
        call
    }

    #[test]
    fn a_ringing_call_offers_decline_and_answer() {
        let ringing = screen(&session(CallDirection::Incoming, false), "Ana", 100);
        assert_eq!(ringing.status, "Incoming voice call");
        assert_eq!(actions(&ringing), [CallAction::Decline, CallAction::Answer]);
        assert_eq!(ringing.buttons[0].tone, Tone::Danger);
        assert_eq!(ringing.buttons[1].tone, Tone::Success);
        let video = screen(&session(CallDirection::Incoming, true), "Ana", 100);
        assert_eq!(video.status, "Incoming video call");
    }

    #[test]
    fn an_outgoing_call_can_only_be_cancelled_until_it_connects() {
        let calling = screen(&session(CallDirection::Outgoing, true), "Ana", 100);
        assert_eq!(calling.status, "Calling…");
        assert_eq!(calling.buttons.len(), 1);
        assert_eq!(calling.buttons[0].label, "Cancel call");
        assert_eq!(calling.buttons[0].action, CallAction::HangUp);
    }

    #[test]
    fn a_voice_call_shows_the_timer_and_offers_mute_video_and_end() {
        let call = connected(CallDirection::Outgoing, false);
        let live = screen(&call, "Ana", 100 + 138);
        assert_eq!(live.status, "Connected · 02:18");
        assert_eq!(
            actions(&live),
            [
                CallAction::ToggleMute,
                CallAction::ToggleCamera,
                CallAction::HangUp
            ]
        );
        assert_eq!(live.buttons[0].label, "Mute");
        assert_eq!(live.buttons[1].label, "Start video");
    }

    #[test]
    fn a_muted_call_says_so_and_offers_unmute() {
        let mut call = connected(CallDirection::Outgoing, false);
        call.muted = true;
        let live = screen(&call, "Ana", 100 + 138);
        assert_eq!(live.status, "Microphone muted · 02:18");
        assert_eq!(live.buttons[0].label, "Unmute");
    }

    #[test]
    fn a_video_call_offers_the_camera_switch_by_its_state() {
        let mut call = connected(CallDirection::Outgoing, true);
        call.camera_on = true;
        assert_eq!(
            screen(&call, "Ana", 100).buttons[1].label,
            "Turn camera off"
        );
        call.camera_on = false;
        assert_eq!(screen(&call, "Ana", 100).buttons[1].label, "Turn camera on");
    }

    #[test]
    fn the_camera_button_waits_while_the_camera_is_changing() {
        let mut call = connected(CallDirection::Outgoing, true);
        call.camera_busy = true;
        let live = screen(&call, "Ana", 100);
        let camera = live
            .buttons
            .iter()
            .find(|button| button.action == CallAction::ToggleCamera)
            .expect("the camera button stays");
        assert!(!camera.sensitive);
        assert!(
            live.buttons
                .iter()
                .filter(|button| button.action != CallAction::ToggleCamera)
                .all(|button| button.sensitive)
        );
        call.camera_busy = false;
        assert!(
            screen(&call, "Ana", 100)
                .buttons
                .iter()
                .all(|button| button.sensitive)
        );
    }

    #[test]
    fn a_video_request_is_offered_for_acceptance_by_name() {
        let mut call = connected(CallDirection::Incoming, false);
        call.video_requested = true;
        let live = screen(&call, "Ana", 100);
        assert_eq!(live.detail.as_deref(), Some("Ana wants to switch to video"));
        assert_eq!(live.buttons[0].action, CallAction::AcceptVideo);
        assert!(!actions(&live).contains(&CallAction::ToggleCamera));
    }

    #[test]
    fn a_finished_call_returns_to_the_chat_and_a_failed_one_can_retry() {
        let mut call = connected(CallDirection::Outgoing, false);
        call.apply(CallSignal::LocalHangup, 100 + 138);
        let ended = screen(&call, "Ana", 500);
        assert_eq!(ended.status, "Voice call ended · 02:18");
        assert_eq!(actions(&ended), [CallAction::BackToChat]);

        let mut failed = session(CallDirection::Outgoing, false);
        failed.apply(CallSignal::MediaFailed, 101);
        let failure = screen(&failed, "Ana", 101);
        assert_eq!(failure.status, "Couldn't connect the call");
        assert_eq!(
            actions(&failure),
            [CallAction::Retry, CallAction::BackToChat]
        );
        assert_eq!(failure.buttons[0].tone, Tone::Primary);

        let mut missed = session(CallDirection::Incoming, false);
        missed.apply(CallSignal::PeerTerminated, 101);
        assert_eq!(missed.phase, CallPhase::Ended(CallOutcome::Missed));
        assert_eq!(screen(&missed, "Ana", 101).status, "Missed voice call");
    }

    #[test]
    fn history_rows_use_plain_words_for_the_call_kind() {
        assert_eq!(kind_label(CallKind::Missed), "Missed");
        assert_eq!(kind_label(CallKind::Outgoing), "Outgoing");
        assert_eq!(kind_label(CallKind::Incoming), "Incoming");
    }

    #[test]
    fn a_chat_without_a_name_is_listed_by_its_number() {
        let chats = [crate::model::Chat::new(
            "1@s.whatsapp.net".into(),
            "Ana".into(),
        )];
        assert_eq!(chat_label(&chats, "1@s.whatsapp.net"), "Ana");
        assert!(!chat_label(&chats, "15551234567@s.whatsapp.net").is_empty());
    }

    #[test]
    fn frames_with_the_wrong_size_never_become_pictures() {
        let frame = RgbaFrame {
            width: 2,
            height: 2,
            data: vec![0; 3],
        };
        assert!(texture(frame).is_none());
    }

    /// Renders every call screen and the Calls tab to PNGs under
    /// `$RENDER_DIR`, light and dark. Run under a display:
    /// `RENDER_DIR=/tmp/r cargo test --lib render_call_screens -- --ignored`.
    #[test]
    #[ignore = "needs a display"]
    fn render_call_screens() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let dir = std::path::PathBuf::from(std::env::var("RENDER_DIR").expect("RENDER_DIR"));
        let css = gtk::CssProvider::new();
        css.load_from_string(include_str!("../../data/style.css"));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let context = gtk::glib::MainContext::default();
        let settle = || {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(400);
            while std::time::Instant::now() < end {
                context.iteration(false);
            }
        };
        let shoot = |name: &str, window: &gtk::Window| {
            settle();
            let paintable = gtk::WidgetPaintable::new(Some(window));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                &snapshot,
                f64::from(window.width()),
                f64::from(window.height()),
            );
            let node = snapshot.to_node().unwrap();
            let texture = window
                .native()
                .unwrap()
                .renderer()
                .unwrap()
                .render_texture(&node, None);
            std::fs::write(dir.join(format!("{name}.png")), texture.save_to_png_bytes()).unwrap();
        };
        let gradient = |width: u32, height: u32, shift: u8| RgbaFrame {
            width,
            height,
            data: (0..width * height)
                .flat_map(|index| {
                    let (x, y) = (index % width, index / width);
                    [
                        (x * 255 / width) as u8,
                        (y * 255 / height) as u8,
                        shift,
                        255,
                    ]
                })
                .collect(),
        };
        let clicked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let record = clicked.clone();
        let mut window = CallWindow::new(std::rc::Rc::new(move |action| {
            record.borrow_mut().push(action)
        }));
        let mut muted = connected(CallDirection::Outgoing, false);
        muted.muted = true;
        let mut video = connected(CallDirection::Outgoing, true);
        video.camera_on = true;
        video.peer_video = true;
        let mut ended = connected(CallDirection::Outgoing, false);
        ended.apply(CallSignal::LocalHangup, 100 + 138);
        let mut failed = session(CallDirection::Outgoing, false);
        failed.apply(CallSignal::MediaFailed, 101);
        let mut request = connected(CallDirection::Incoming, false);
        request.video_requested = true;
        let screens = [
            ("incoming", session(CallDirection::Incoming, false)),
            ("incoming-video", session(CallDirection::Incoming, true)),
            ("calling", session(CallDirection::Outgoing, false)),
            ("voice", connected(CallDirection::Outgoing, false)),
            ("muted", muted),
            ("video", video),
            ("request", request),
            ("ended", ended),
            ("failed", failed),
        ];
        for (scheme_name, scheme) in [
            ("light", adw::ColorScheme::ForceLight),
            ("dark", adw::ColorScheme::ForceDark),
        ] {
            adw::StyleManager::default().set_color_scheme(scheme);
            for (name, call) in &screens {
                let shown = screen(call, "Ana Souza", 100 + 138);
                window.render(call, "Ana Souza", None, &shown);
                if call.peer_video {
                    window
                        .remote
                        .set_paintable(texture(gradient(320, 240, 90)).as_ref());
                    window
                        .preview
                        .set_paintable(texture(gradient(120, 160, 200)).as_ref());
                }
                window.window.present();
                shoot(
                    &format!("call-{name}-{scheme_name}"),
                    window.window.upcast_ref(),
                );
            }
        }
        let first = window
            .buttons
            .first_child()
            .and_downcast::<gtk::Button>()
            .unwrap();
        first.emit_clicked();
        assert_eq!(clicked.borrow().len(), 1);
        window.window.destroy();

        let ui = CallsUi::new(|_| {}, || {}, || {});
        let chats = [
            crate::model::Chat::new("1@s.whatsapp.net".into(), "Ana Souza".into()),
            crate::model::Chat::new("2@s.whatsapp.net".into(), "Bruno Lima".into()),
        ];
        let entry = |chat: &str, outgoing, outcome, video, seconds, timestamp| CallEntry {
            chat: chat.into(),
            id: format!("{timestamp}"),
            video,
            outgoing,
            outcome,
            seconds,
            timestamp,
        };
        let now = crate::util::now();
        let mut ui = ui;
        ui.entries = vec![
            entry(
                "1@s.whatsapp.net",
                false,
                CallOutcome::Missed,
                false,
                None,
                now - 600,
            ),
            entry(
                "2@s.whatsapp.net",
                true,
                CallOutcome::Completed,
                true,
                Some(138),
                now - 7200,
            ),
            entry(
                "1@s.whatsapp.net",
                false,
                CallOutcome::Completed,
                false,
                Some(65),
                now - 90_000,
            ),
        ];
        ui.selected = Some(1);
        ui.refill(&chats, &Default::default());
        ui.show_selected(&chats, &Default::default(), false);
        for (scheme_name, scheme) in [
            ("light", adw::ColorScheme::ForceLight),
            ("dark", adw::ColorScheme::ForceDark),
        ] {
            adw::StyleManager::default().set_color_scheme(scheme);
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_child(Some(&ui.list));
            scroll.set_vexpand(true);
            let header = adw::HeaderBar::new();
            header.set_title_widget(Some(&adw::WindowTitle::new(&gettext("Recent calls"), "")));
            let side = adw::ToolbarView::new();
            side.add_top_bar(&header);
            side.set_content(Some(&scroll));
            let split = adw::NavigationSplitView::new();
            split.set_sidebar(Some(&adw::NavigationPage::new(&side, "Calls")));
            split.set_content(Some(&adw::NavigationPage::new(&ui.details, "Details")));
            let host = adw::Window::builder()
                .default_width(860)
                .default_height(520)
                .content(&split)
                .build();
            host.present();
            shoot(&format!("calls-tab-{scheme_name}"), host.upcast_ref());
            split.set_sidebar(None::<&adw::NavigationPage>);
            split.set_content(None::<&adw::NavigationPage>);
            scroll.set_child(None::<&gtk::Widget>);
            host.destroy();
        }
    }
}
