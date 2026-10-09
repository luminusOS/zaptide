use crate::backend::LinkStatus;
use gtk::prelude::*;
use relm4::prelude::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct LinkPageState {
    pub(super) link: LinkStatus,
    pub(super) title: String,
    pub(super) status: String,
    pub(super) qr_texture: Option<gtk::gdk::Texture>,
    pub(super) phone_linking: bool,
    pub(super) busy: bool,
    /// Another account exists to go back to.
    pub(super) can_cancel: bool,
}

pub(super) struct LinkPageInit {
    /// Reaches the other accounts when this one cannot be used.
    pub(super) account_button: gtk::MenuButton,
    pub(super) state: LinkPageState,
    pub(super) menu: gtk::gio::Menu,
}

pub(super) struct LinkPage {
    state: LinkPageState,
    /// Read by the Escape shortcut, which lives outside the model.
    can_cancel: std::rc::Rc<std::cell::Cell<bool>>,
}

#[derive(Debug)]
pub(super) enum LinkPageInput {
    Sync(LinkPageState),
}

#[derive(Debug)]
pub(super) enum LinkPageOutput {
    PairWithPhone(String),
    TogglePhoneLinking,
    CopyPairCode,
    Reconnect,
    Cancel,
}

impl LinkPageState {
    fn pair_code(&self) -> Option<&str> {
        match &self.link {
            LinkStatus::Unlinked {
                pair_code: Some(code),
                ..
            } => Some(code),
            _ => None,
        }
    }

    fn pairing_requested(&self) -> bool {
        matches!(
            self.link,
            LinkStatus::Unlinked {
                pairing_phone: Some(_),
                ..
            }
        )
    }
}

impl LinkPage {
    pub(super) fn is_synced(&self, state: &LinkPageState) -> bool {
        self.state == *state
    }
}

#[relm4::component(pub(super))]
impl SimpleComponent for LinkPage {
    type Init = LinkPageInit;
    type Input = LinkPageInput;
    type Output = LinkPageOutput;

    view! {
        adw::ToolbarView {
            add_top_bar = &adw::HeaderBar {
                set_show_title: false,
                pack_start: &init.account_button,
                pack_start = &gtk::Button {
                    set_label: "Cancel",
                    #[watch]
                    set_visible: model.state.can_cancel,
                    connect_clicked[sender] => move |_| sender.output(LinkPageOutput::Cancel).unwrap(),
                },
                pack_end = &gtk::MenuButton {
                    set_icon_name: "open-menu-symbolic",
                    set_tooltip_text: Some("Main menu"),
                    set_menu_model: Some(&init.menu),
                },
            },

            #[wrap(Some)]
            set_content = &gtk::ScrolledWindow {
                set_hscrollbar_policy: gtk::PolicyType::Never,
                #[wrap(Some)]
                set_child = &adw::Clamp {
                    set_maximum_size: 420,
                    set_valign: gtk::Align::Center,
                    #[wrap(Some)]
                    set_child = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 18,
                        set_margin_top: 24,
                        set_margin_bottom: 24,
                        set_margin_start: 24,
                        set_margin_end: 24,

                        append = &gtk::Label {
                            add_css_class: "title-1",
                            set_wrap: true,
                            set_justify: gtk::Justification::Center,
                            #[watch]
                            set_label: if model.state.phone_linking && model.state.pair_code().is_none() { "Link with phone number" } else { model.state.title.as_str() },
                        },
                        #[name = "status_label"]
                        append = &gtk::Label {
                            add_css_class: "dim-label",
                            set_wrap: true,
                            set_justify: gtk::Justification::Center,
                            #[watch]
                            set_label: if model.state.phone_linking && model.state.pair_code().is_none() && !model.state.pairing_requested() { "Choose your country and enter your phone number. WhatsApp will send a code to type on your phone." } else { model.state.status.as_str() },
                        },
                        append = &gtk::Picture {
                            add_css_class: "zaptide-qr",
                            set_halign: gtk::Align::Center,
                            set_size_request: (264, 264),
                            set_can_shrink: false,
                            set_alternative_text: Some("WhatsApp device-linking QR code"),
                            #[watch]
                            set_visible: model.state.qr_texture.is_some() && !model.state.phone_linking,
                            #[watch]
                            set_paintable: model.state.qr_texture.as_ref(),
                        },
                        append = &gtk::Box {
                            set_halign: gtk::Align::Center,
                            set_spacing: 12,
                            add_css_class: "card",
                            add_css_class: "zaptide-pair-code",
                            #[watch]
                            set_visible: model.state.pair_code().is_some(),
                            append = &gtk::Label {
                                add_css_class: "zaptide-pair-code-label",
                                add_css_class: "monospace",
                                set_selectable: true,
                                #[watch]
                                set_label: model.state.pair_code().unwrap_or_default(),
                            },
                            append = &gtk::Button {
                                set_icon_name: "edit-copy-symbolic",
                                set_tooltip_text: Some("Copy Code"),
                                set_valign: gtk::Align::Center,
                                add_css_class: "flat",
                                add_css_class: "circular",
                                connect_clicked[sender] => move |_| sender.output(LinkPageOutput::CopyPairCode).unwrap(),
                            },
                        },
                        append = &adw::Spinner {
                            set_halign: gtk::Align::Center,
                            set_size_request: (32, 32),
                            #[watch]
                            set_visible: model.state.busy,
                        },
                        append = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 12,
                            #[watch]
                            set_visible: model.state.phone_linking && model.state.pair_code().is_none() && !model.state.pairing_requested(),
                            append = &gtk::Box {
                                set_spacing: 8,
                                #[name = "country_picker"]
                                append = &super::country_picker() -> gtk::DropDown {},
                                #[name = "phone_entry"]
                                append = &gtk::Entry {
                                    set_hexpand: true,
                                    set_placeholder_text: Some("Phone number"),
                                    set_input_purpose: gtk::InputPurpose::Phone,
                                    connect_activate[sender, country_picker] => move |entry| {
                                        let phone = super::international_phone(&country_picker, entry);
                                        sender.output(LinkPageOutput::PairWithPhone(phone)).unwrap();
                                    },
                                },
                            },
                            append = &gtk::Button {
                                set_label: "Get Code",
                                set_halign: gtk::Align::Center,
                                add_css_class: "pill",
                                add_css_class: "suggested-action",
                                connect_clicked[sender, phone_entry, country_picker] => move |_| {
                                    let phone = super::international_phone(&country_picker, &phone_entry);
                                    sender.output(LinkPageOutput::PairWithPhone(phone)).unwrap();
                                },
                            },
                        },
                        append = &gtk::Button {
                            set_halign: gtk::Align::Center,
                            add_css_class: "pill",
                            #[watch]
                            set_visible: matches!(model.state.link, LinkStatus::Unlinked { .. }),
                            #[watch]
                            set_label: if model.state.phone_linking || model.state.pairing_requested() || model.state.pair_code().is_some() { "Use QR Code Instead" } else { "Link With Phone Number" },
                            connect_clicked[sender] => move |_| sender.output(LinkPageOutput::TogglePhoneLinking).unwrap(),
                        },
                        append = &gtk::Button {
                            set_label: "Try Again",
                            set_halign: gtk::Align::Center,
                            add_css_class: "pill",
                            add_css_class: "suggested-action",
                            #[watch]
                            set_visible: matches!(model.state.link, LinkStatus::Failed(_) | LinkStatus::LoggedOut | LinkStatus::Disconnected { .. }),
                            connect_clicked[sender] => move |_| sender.output(LinkPageOutput::Reconnect).unwrap(),
                        },
                    },
                },
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let can_cancel = std::rc::Rc::new(std::cell::Cell::new(init.state.can_cancel));
        let model = Self {
            state: init.state,
            can_cancel: can_cancel.clone(),
        };
        let widgets = view_output!();
        let escape = gtk::EventControllerKey::new();
        // Bubble: a focused field or open menu uses Escape first.
        escape.set_propagation_phase(gtk::PropagationPhase::Bubble);
        let escape_sender = sender.clone();
        escape.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape && can_cancel.get() {
                let _ = escape_sender.output(LinkPageOutput::Cancel);
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        root.add_controller(escape);
        widgets
            .status_label
            .set_accessible_role(gtk::AccessibleRole::Status);
        widgets
            .status_label
            .connect_notify_local(Some("label"), |widget, _| {
                if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                    let text = label.label();
                    if !text.is_empty() {
                        label.announce(&text, gtk::AccessibleAnnouncementPriority::Medium);
                    }
                }
            });
        ComponentParts { model, widgets }
    }

    fn update(&mut self, input: Self::Input, _sender: ComponentSender<Self>) {
        match input {
            LinkPageInput::Sync(state) => {
                self.can_cancel.set(state.can_cancel);
                self.state = state;
            }
        }
    }
}
