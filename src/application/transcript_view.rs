use gtk::prelude::*;
use relm4::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TranscriptState {
    pub(super) active: bool,
    pub(super) has_messages: bool,
    pub(super) history_complete: bool,
    pub(super) loading_older: bool,
}

pub(super) struct TranscriptViewInit {
    pub(super) state: TranscriptState,
    pub(super) message_view: gtk::ListView,
}

pub(super) struct TranscriptView {
    state: TranscriptState,
}

#[derive(Debug)]
pub(super) enum TranscriptViewInput {
    Sync(TranscriptState),
}

#[derive(Debug)]
pub(super) enum TranscriptViewOutput {
    LoadOlder,
    SelectMessage(u32),
}

impl TranscriptView {
    pub(super) fn is_synced(&self, state: &TranscriptState) -> bool {
        self.state == *state
    }
}

#[relm4::component(pub(super))]
impl SimpleComponent for TranscriptView {
    type Init = TranscriptViewInit;
    type Input = TranscriptViewInput;
    type Output = TranscriptViewOutput;

    view! {
        gtk::Stack {
            set_vexpand: true,

            add_named[Some("empty")] = &adw::StatusPage {
                set_icon_name: Some("chat-message-new-symbolic"),
                set_title: "No Conversation Selected",
                set_description: Some("Choose a chat from the list to start messaging."),
            },

            add_named[Some("conversation")] = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                append = &gtk::Button {
                    set_label: "Load Older Messages",
                    set_halign: gtk::Align::Center,
                    set_margin_top: 6,
                    add_css_class: "flat",
                    #[watch]
                    set_visible: model.state.has_messages && !model.state.history_complete,
                    #[watch]
                    set_sensitive: !model.state.loading_older,
                    connect_clicked[sender] => move |_| sender.output(TranscriptViewOutput::LoadOlder).unwrap(),
                },

                append = &gtk::ScrolledWindow {
                    set_vexpand: true,
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    #[local_ref]
                    message_view -> gtk::ListView {
                        add_css_class: "zaptide-transcript",
                        set_single_click_activate: true,
                        connect_activate[sender] => move |_, position| sender.output(TranscriptViewOutput::SelectMessage(position)).unwrap(),
                    },
                },
            },

            #[watch]
            set_visible_child_name: if model.state.active { "conversation" } else { "empty" },
        }
    }

    fn init(
        init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = Self { state: init.state };
        let message_view = &init.message_view;
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }

    fn update(&mut self, input: Self::Input, _sender: ComponentSender<Self>) {
        match input {
            TranscriptViewInput::Sync(state) => self.state = state,
        }
    }
}
