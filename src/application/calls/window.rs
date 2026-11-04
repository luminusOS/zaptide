//! The window a call is held in.

use super::screen::{CallAction, CallButton, Screen};
use super::*;

pub(super) struct CallWindow {
    pub(super) window: adw::Window,
    title: adw::WindowTitle,
    pub(super) remote: gtk::Picture,
    pub(super) preview: gtk::Picture,
    identity: gtk::Box,
    avatar: adw::Avatar,
    name: gtk::Label,
    status: gtk::Label,
    detail: gtk::Label,
    pub(super) buttons: adw::WrapBox,
    shown: Vec<CallButton>,
    on_action: std::rc::Rc<dyn Fn(CallAction)>,
}

impl CallWindow {
    pub(super) fn new(on_action: std::rc::Rc<dyn Fn(CallAction)>) -> Self {
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

    pub(super) fn render(
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
