//! The window a call is held in.

use super::screen::{
    ButtonKind, CallAction, CallButton, Pip, Screen, Stage, arrange, avatar_size, waiting_order,
};
use super::*;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

const NARROW_BELOW_PX: f64 = 400.0;
const MIN_WIDTH_PX: i32 = 320;

type OnAction = Rc<dyn Fn(CallAction)>;

/// What the window needs to know besides the call itself.
pub(super) struct Look<'a> {
    pub(super) avatar: Option<&'a gtk::gdk::Texture>,
    pub(super) waiting_avatar: Option<&'a gtk::gdk::Texture>,
    /// Whether a button may take the keyboard focus. Not while the person is
    /// typing in another window, where a stray key would answer or hang up.
    pub(super) allow_focus: bool,
}

struct Control {
    action: CallAction,
    kind: ButtonKind,
    button: gtk::Button,
    icon: Option<gtk::Image>,
    label: Option<gtk::Label>,
    wanted: Rc<Cell<bool>>,
    tone_classes: Vec<&'static str>,
}

impl Control {
    fn new(spec: &CallButton, on_action: &OnAction) -> Self {
        let wanted = Rc::new(Cell::new(spec.checked));
        let control = if spec.kind == ButtonKind::Round {
            let button = gtk::ToggleButton::new().upcast::<gtk::Button>();
            button.add_css_class("circular");
            Self::with(spec, button, None, None, wanted.clone())
        } else {
            let icon = gtk::Image::new();
            let label = gtk::Label::new(None);
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            content.set_halign(gtk::Align::Center);
            content.append(&icon);
            content.append(&label);
            let button = gtk::Button::new();
            button.set_child(Some(&content));
            button.add_css_class("pill");
            if spec.kind == ButtonKind::Choice {
                button.add_css_class("call-choice");
            }
            Self::with(spec, button, Some(icon), Some(label), wanted.clone())
        };
        control.button.add_css_class("call-control");
        let action = spec.action;
        let on_action = on_action.clone();
        control.button.connect_clicked(move |button| {
            on_action(action);
            if let Some(toggle) = button.downcast_ref::<gtk::ToggleButton>() {
                let (toggle, wanted) = (toggle.clone(), wanted.clone());
                gtk::glib::idle_add_local_once(move || toggle.set_active(wanted.get()));
            }
        });
        control
    }

    fn with(
        spec: &CallButton,
        button: gtk::Button,
        icon: Option<gtk::Image>,
        label: Option<gtk::Label>,
        wanted: Rc<Cell<bool>>,
    ) -> Self {
        Self {
            action: spec.action,
            kind: spec.kind,
            button,
            icon,
            label,
            wanted,
            tone_classes: Vec::new(),
        }
    }

    fn update(&mut self, spec: &CallButton, over_video: bool) {
        for class in self.tone_classes.drain(..) {
            self.button.remove_css_class(class);
        }
        for class in spec.tone.classes() {
            self.button.add_css_class(class);
            self.tone_classes.push(class);
        }
        self.button.set_sensitive(spec.sensitive);
        if let (Some(icon), Some(label)) = (&self.icon, &self.label) {
            icon.set_icon_name(Some(spec.icon));
            label.set_label(&spec.label);
            return;
        }
        self.button.set_icon_name(spec.icon);
        self.button.set_tooltip_text(Some(&spec.label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(&spec.label)]);
        if over_video {
            self.button.add_css_class("osd");
        } else {
            self.button.remove_css_class("osd");
        }
        self.wanted.set(spec.checked);
        if let Some(toggle) = self.button.downcast_ref::<gtk::ToggleButton>() {
            toggle.set_active(spec.checked);
        }
    }
}

/// The buttons of one place in the window, kept while only their text, icon
/// and state change so a focused button stays focused.
#[derive(Default)]
struct ButtonSet {
    items: Vec<Control>,
    specs: Vec<CallButton>,
}

impl ButtonSet {
    /// Returns whether the buttons were made anew and need placing.
    fn sync(&mut self, specs: &[CallButton], over_video: bool, on_action: &OnAction) -> bool {
        let same_shape = self.items.len() == specs.len()
            && self
                .items
                .iter()
                .zip(specs)
                .all(|(item, spec)| item.action == spec.action && item.kind == spec.kind);
        if !same_shape {
            self.items = specs
                .iter()
                .map(|spec| Control::new(spec, on_action))
                .collect();
        }
        for (item, spec) in self.items.iter_mut().zip(specs) {
            item.update(spec, over_video);
        }
        self.specs = specs.to_vec();
        !same_shape
    }

    fn focus(&self, action: CallAction) -> bool {
        self.items
            .iter()
            .find(|item| item.action == action)
            .is_some_and(|item| item.button.grab_focus())
    }
}

fn empty(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        if let Some(inner) = child.downcast_ref::<gtk::Box>() {
            while let Some(button) = inner.first_child() {
                inner.remove(&button);
            }
        }
        container.remove(&child);
    }
}

struct WaitingCard {
    root: gtk::Box,
    avatar: adw::Avatar,
    title: gtk::Label,
    hint: gtk::Label,
    buttons: gtk::Box,
}

struct Ui {
    window: adw::Window,
    toolbar: adw::ToolbarView,
    title: adw::WindowTitle,
    remote: gtk::Picture,
    top_scrim: gtk::Box,
    bottom_scrim: gtk::Box,
    top: gtk::Box,
    pip_width: adw::Clamp,
    pip_height: adw::Clamp,
    pip: gtk::Overlay,
    pip_stack: gtk::Stack,
    notice: gtk::Box,
    notice_icon: gtk::Image,
    notice_title: gtk::Label,
    notice_body: gtk::Label,
    waiting: WaitingCard,
    identity: gtk::Box,
    filler: gtk::Box,
    rings: gtk::Box,
    badge: gtk::Image,
    avatar: adw::Avatar,
    name: gtk::Label,
    status: gtk::Label,
    detail: gtk::Label,
    rows: gtk::Box,
    controls: RefCell<ButtonSet>,
    waiting_set: RefCell<ButtonSet>,
    on_action: OnAction,
    narrow: Cell<bool>,
    compact: Cell<bool>,
    focused: Cell<Option<CallAction>>,
}

fn label(css: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_wrap(true);
    label.set_justify(gtk::Justification::Center);
    for class in css {
        label.add_css_class(class);
    }
    label
}

fn card_text(css: &[&str]) -> gtk::Label {
    let label = label(css);
    label.set_justify(gtk::Justification::Left);
    label.set_xalign(0.0);
    label
}

impl Ui {
    fn place_controls(&self) {
        let narrow = self.narrow.get();
        let set = self.controls.borrow();
        empty(&self.rows);
        for row in arrange(&set.specs, narrow) {
            let choices = row
                .buttons
                .iter()
                .all(|&index| set.items[index].kind == ButtonKind::Choice);
            let spacing = match (choices, narrow) {
                (true, false) => 16,
                _ => 12,
            };
            let line = gtk::Box::new(gtk::Orientation::Horizontal, spacing);
            if row.stretch {
                line.set_halign(gtk::Align::Fill);
                line.set_margin_start(24);
                line.set_margin_end(24);
            } else {
                line.set_halign(gtk::Align::Center);
            }
            for &index in &row.buttons {
                let item = &set.items[index];
                let button = &item.button;
                let after_toggles = item.action == CallAction::HangUp && row.buttons.len() > 1;
                button.set_margin_start(match (after_toggles, narrow) {
                    (true, false) => 12,
                    (true, true) => 8,
                    _ => 0,
                });
                button.set_hexpand(row.stretch);
                button.set_halign(if row.stretch {
                    gtk::Align::Fill
                } else {
                    gtk::Align::Center
                });
                line.append(button);
            }
            self.rows.append(&line);
        }
    }

    fn place_waiting(&self) {
        let narrow = self.narrow.get();
        let set = self.waiting_set.borrow();
        empty_flat(&self.waiting.buttons);
        self.waiting.buttons.set_orientation(if narrow {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        });
        self.waiting
            .buttons
            .set_spacing(if narrow { 8 } else { 12 });
        for index in waiting_order(&set.specs, narrow) {
            let button = &set.items[index].button;
            button.set_hexpand(true);
            self.waiting.buttons.append(button);
        }
    }

    fn resize_identity(&self) {
        self.avatar
            .set_size(avatar_size(self.narrow.get(), self.compact.get()));
    }

    fn set_narrow(&self, narrow: bool) {
        self.narrow.set(narrow);
        set_class(&self.window, "call-narrow", narrow);
        self.resize_identity();
        let (width, height) = if narrow { (96, 128) } else { (160, 120) };
        for (clamp, size) in [(&self.pip_width, width), (&self.pip_height, height)] {
            clamp.set_maximum_size(size);
            clamp.set_tightening_threshold(size);
        }
        self.place_controls();
        self.place_waiting();
    }

    fn render(&self, name: &str, look: &Look, screen: &Screen) {
        let video = screen.stage == Stage::Video;
        self.window
            .set_title(Some(&format!("{name} · {}", screen.status)));
        self.title
            .set_title(if video { name } else { &screen.title });
        self.title
            .set_subtitle(if video { &screen.status } else { "" });
        self.toolbar.set_extend_content_to_top_edge(video);
        set_class(&self.window, "call-over-video", video);
        self.remote.set_visible(video);
        self.top_scrim.set_visible(video);
        self.bottom_scrim.set_visible(video);
        self.identity.set_visible(!video);
        self.filler.set_visible(video);
        self.top.set_margin_top(if video { 52 } else { 12 });

        set_class(&self.rings, "on", screen.stage == Stage::Ringing);
        self.badge.set_visible(screen.stage == Stage::Failed);
        self.compact.set(screen.waiting.is_some());
        self.resize_identity();
        self.avatar.set_text(Some(name));
        self.avatar.set_custom_image(look.avatar);
        self.name.set_label(name);
        self.status.set_label(&screen.status);
        set_class(&self.status, "error", screen.stage == Stage::Failed);
        self.detail
            .set_label(screen.detail.as_deref().unwrap_or(""));
        self.detail.set_visible(screen.detail.is_some());

        self.pip.set_visible(screen.pip != Pip::Hidden);
        self.pip_stack.set_visible_child_name(match screen.pip {
            Pip::CameraOff => "off",
            _ => "preview",
        });

        self.notice.set_visible(screen.notice.is_some());
        if let Some(notice) = &screen.notice {
            self.notice_icon.set_icon_name(Some(notice.icon));
            self.notice_title.set_label(&notice.title);
            self.notice_body.set_label(&notice.body);
        }

        self.waiting.root.set_visible(screen.waiting.is_some());
        if let Some(card) = &screen.waiting {
            self.waiting.avatar.set_text(Some(&card.caller));
            self.waiting.avatar.set_custom_image(look.waiting_avatar);
            self.waiting.title.set_label(&card.title);
            self.waiting.hint.set_label(&card.hint);
            let rebuilt = self
                .waiting_set
                .borrow_mut()
                .sync(&card.buttons, false, &self.on_action);
            if rebuilt {
                self.place_waiting();
            }
        }

        let rebuilt = self
            .controls
            .borrow_mut()
            .sync(&screen.buttons, video, &self.on_action);
        if rebuilt {
            self.place_controls();
        }
        self.take_focus(screen, look);
    }

    fn take_focus(&self, screen: &Screen, look: &Look) {
        if self.focused.get() == screen.focus {
            return;
        }
        self.focused.set(screen.focus);
        let Some(action) = screen.focus.filter(|_| look.allow_focus) else {
            return;
        };
        if !self.controls.borrow().focus(action) {
            self.waiting_set.borrow().focus(action);
        }
    }
}

fn empty_flat(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

pub(super) struct CallWindow {
    pub(super) window: adw::Window,
    pub(super) remote: gtk::Picture,
    pub(super) preview: gtk::Picture,
    ui: Rc<Ui>,
}

impl CallWindow {
    pub(super) fn new(on_action: OnAction) -> Self {
        let window = adw::Window::builder()
            .title(gettext("Call"))
            .default_width(420)
            .default_height(640)
            .width_request(MIN_WIDTH_PX)
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
        let header = adw::HeaderBar::new();
        header.add_css_class("flat");
        header.set_title_widget(Some(&title));

        let remote = gtk::Picture::new();
        remote.set_content_fit(gtk::ContentFit::Contain);
        remote.set_hexpand(true);
        remote.set_vexpand(true);
        remote.set_visible(false);
        remote.add_css_class("zaptide-call-remote");
        let scrim = |class: &str, align| {
            let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
            scrim.add_css_class(class);
            scrim.set_valign(align);
            scrim.set_can_target(false);
            scrim.set_visible(false);
            scrim
        };
        let top_scrim = scrim("call-scrim-top", gtk::Align::Start);
        let bottom_scrim = scrim("call-scrim-bottom", gtk::Align::End);

        let avatar = adw::Avatar::new(avatar_size(false, false), None, true);
        let badge = gtk::Image::from_icon_name("dialog-warning-symbolic");
        badge.set_pixel_size(20);
        badge.add_css_class("call-badge");
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::End);
        badge.set_visible(false);
        let inner_ring = gtk::Box::new(gtk::Orientation::Vertical, 0);
        inner_ring.add_css_class("call-ring-inner");
        inner_ring.append(&avatar);
        let rings = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rings.add_css_class("call-ring-outer");
        rings.append(&inner_ring);
        let face = gtk::Overlay::new();
        face.set_halign(gtk::Align::Center);
        face.set_child(Some(&rings));
        face.add_overlay(&badge);

        let name = label(&["call-name"]);
        let status = label(&["call-status"]);
        status.set_accessible_role(gtk::AccessibleRole::Status);
        let detail = label(&["call-detail"]);
        detail.set_visible(false);
        let identity = gtk::Box::new(gtk::Orientation::Vertical, 12);
        identity.set_vexpand(true);
        identity.set_valign(gtk::Align::Center);
        identity.set_margin_start(24);
        identity.set_margin_end(24);
        for child in [
            face.upcast_ref::<gtk::Widget>(),
            name.upcast_ref(),
            status.upcast_ref(),
            detail.upcast_ref(),
        ] {
            identity.append(child);
        }
        let filler = gtk::Box::new(gtk::Orientation::Vertical, 0);
        filler.set_vexpand(true);
        filler.set_visible(false);

        let preview = gtk::Picture::new();
        preview.set_content_fit(gtk::ContentFit::Cover);
        preview.update_property(&[gtk::accessible::Property::Label(&gettext(
            "Your camera preview",
        ))]);
        let off_avatar = adw::Avatar::new(32, None, false);
        let off_icon = gtk::Image::from_icon_name("camera-disabled-symbolic");
        let off_label = label(&[]);
        off_label.set_label(&gettext("Camera off"));
        let off = gtk::Box::new(gtk::Orientation::Vertical, 4);
        off.add_css_class("call-pip-off");
        off.set_halign(gtk::Align::Center);
        off.set_valign(gtk::Align::Start);
        off.set_margin_top(10);
        off.append(&off_avatar);
        off.append(&off_icon);
        off.append(&off_label);
        let pip_stack = gtk::Stack::new();
        pip_stack.set_hhomogeneous(false);
        pip_stack.set_vhomogeneous(false);
        pip_stack.add_named(&preview, Some("preview"));
        pip_stack.add_named(&off, Some("off"));
        let chip = gtk::Label::new(Some(&gettext("You")));
        chip.add_css_class("call-pip-chip");
        chip.set_halign(gtk::Align::Start);
        chip.set_valign(gtk::Align::End);
        chip.set_margin_start(6);
        chip.set_margin_bottom(6);
        let pip = gtk::Overlay::new();
        pip.add_css_class("call-pip");
        pip.set_overflow(gtk::Overflow::Hidden);
        let pip_height = adw::Clamp::builder()
            .orientation(gtk::Orientation::Vertical)
            .child(&pip_stack)
            .build();
        let pip_width = adw::Clamp::builder().child(&pip_height).build();
        pip.set_child(Some(&pip_width));
        pip.add_overlay(&chip);
        pip.set_halign(gtk::Align::End);
        pip.set_valign(gtk::Align::Start);
        pip.set_margin_end(14);
        pip.set_margin_top(8);
        pip.set_can_target(false);
        pip.set_visible(false);

        let notice_icon = gtk::Image::from_icon_name("camera-disabled-symbolic");
        notice_icon.set_pixel_size(24);
        let notice_title = card_text(&["heading"]);
        let notice_body = card_text(&["call-card-hint"]);
        let notice_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        notice_text.append(&notice_title);
        notice_text.append(&notice_body);
        let notice = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        notice.add_css_class("call-card");
        notice.set_visible(false);
        notice.set_accessible_role(gtk::AccessibleRole::Group);
        notice.append(&notice_icon);
        notice.append(&notice_text);

        let waiting_avatar = adw::Avatar::new(44, None, true);
        let waiting_title = card_text(&["heading"]);
        let waiting_hint = card_text(&["call-card-hint"]);
        let waiting_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        waiting_text.set_hexpand(true);
        waiting_text.set_valign(gtk::Align::Center);
        waiting_text.append(&waiting_title);
        waiting_text.append(&waiting_hint);
        let waiting_who = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        waiting_who.append(&waiting_avatar);
        waiting_who.append(&waiting_text);
        let waiting_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let waiting_root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        waiting_root.add_css_class("call-card");
        waiting_root.set_visible(false);
        waiting_root.set_accessible_role(gtk::AccessibleRole::Group);
        waiting_root.append(&waiting_who);
        waiting_root.append(&waiting_buttons);

        let top = gtk::Box::new(gtk::Orientation::Vertical, 12);
        top.set_margin_top(12);
        for card in [&waiting_root, &notice] {
            card.set_margin_start(12);
            card.set_margin_end(12);
        }
        top.append(&pip);
        top.append(&waiting_root);
        top.append(&notice);

        let rows = gtk::Box::new(gtk::Orientation::Vertical, 12);
        rows.add_css_class("call-controls");
        rows.set_halign(gtk::Align::Fill);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        column.append(&top);
        column.append(&identity);
        column.append(&filler);
        column.append(&rows);

        let stage = gtk::Overlay::new();
        stage.set_child(Some(&remote));
        stage.add_overlay(&top_scrim);
        stage.add_overlay(&bottom_scrim);
        stage.add_overlay(&column);
        stage.set_measure_overlay(&column, true);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stage));
        window.set_content(Some(&toolbar));

        let ui = Rc::new(Ui {
            window: window.clone(),
            toolbar,
            title,
            remote: remote.clone(),
            top_scrim,
            bottom_scrim,
            top,
            pip_width,
            pip_height,
            pip,
            pip_stack,
            notice,
            notice_icon,
            notice_title,
            notice_body,
            waiting: WaitingCard {
                root: waiting_root,
                avatar: waiting_avatar,
                title: waiting_title,
                hint: waiting_hint,
                buttons: waiting_buttons,
            },
            identity,
            filler,
            rings,
            badge,
            avatar,
            name,
            status,
            detail,
            rows,
            controls: RefCell::default(),
            waiting_set: RefCell::default(),
            on_action,
            narrow: Cell::new(false),
            compact: Cell::new(false),
            focused: Cell::new(None),
        });

        ui.set_narrow(false);
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            NARROW_BELOW_PX - 1.0,
            adw::LengthUnit::Px,
        ));
        let follow = |narrow: bool, ui: &Rc<Ui>| {
            let ui: Weak<Ui> = Rc::downgrade(ui);
            move |_: &adw::Breakpoint| {
                if let Some(ui) = ui.upgrade() {
                    ui.set_narrow(narrow);
                }
            }
        };
        breakpoint.connect_apply(follow(true, &ui));
        breakpoint.connect_unapply(follow(false, &ui));
        window.add_breakpoint(breakpoint);

        Self {
            window,
            remote,
            preview,
            ui,
        }
    }

    pub(super) fn render(&mut self, name: &str, look: &Look, screen: &Screen) {
        self.ui.render(name, look, screen);
    }

    #[cfg(test)]
    pub(super) fn control(&self, action: CallAction) -> Option<gtk::Button> {
        let ui = self.ui.controls.borrow();
        let waiting = self.ui.waiting_set.borrow();
        ui.items
            .iter()
            .chain(waiting.items.iter())
            .find(|item| item.action == action)
            .map(|item| item.button.clone())
    }

    #[cfg(test)]
    pub(super) fn controls(&self) -> Vec<gtk::Button> {
        let ui = self.ui.controls.borrow();
        let waiting = self.ui.waiting_set.borrow();
        let waiting = if self.ui.waiting.root.is_visible() {
            waiting.items.iter()
        } else {
            [].iter()
        };
        ui.items
            .iter()
            .chain(waiting)
            .map(|item| item.button.clone())
            .collect()
    }
}
