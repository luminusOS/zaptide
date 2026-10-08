use gtk::prelude::*;
use relm4::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TranscriptState {
    pub(super) active: bool,
    pub(super) has_messages: bool,
    pub(super) history_complete: bool,
    pub(super) loading_older: bool,
    pub(super) recent_messages_pending: bool,
}

pub(super) struct TranscriptViewInit {
    pub(super) state: TranscriptState,
    pub(super) message_view: gtk::ListView,
}

pub(super) struct TranscriptView {
    state: TranscriptState,
    /// Scrolled far enough from the newest message to offer a way back.
    away_from_end: bool,
}

/// How far above the end, in pixels, the way-back button appears.
const AWAY_FROM_END: f64 = 240.0;

#[derive(Debug)]
pub(super) enum TranscriptViewInput {
    Sync(TranscriptState),
    AwayFromEnd(bool),
}

#[derive(Debug)]
pub(super) enum TranscriptViewOutput {
    LoadOlder,
    SelectMessage(u32),
    ScrollToRecentMessages,
    AtEnd,
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
                    set_halign: gtk::Align::Center,
                    set_margin_top: 4,
                    set_margin_bottom: 4,
                    add_css_class: "flat",
                    set_tooltip_text: Some("Show earlier messages from the archive or your phone"),
                    #[watch]
                    set_visible: model.state.has_messages && !model.state.history_complete,
                    // Stay sensitive while loading: insensitive styling would dim the
                    // spinner twice. `Input::LoadOlder` already ignores repeat clicks.
                    #[watch]
                    set_can_target: !model.state.loading_older,
                    #[watch]
                    update_state: &[gtk::accessible::State::Busy(model.state.loading_older)],
                    connect_clicked[sender] => move |_| sender.output(TranscriptViewOutput::LoadOlder).unwrap(),
                    #[wrap(Some)]
                    set_child = &gtk::Stack {
                        // Reserve the same space for both states to avoid shifting the transcript.
                        set_hhomogeneous: true,
                        set_vhomogeneous: true,
                        add_named[Some("ready")] = &gtk::Box {
                            set_spacing: 6,
                            set_halign: gtk::Align::Center,
                            append = &gtk::Image {
                                set_icon_name: Some("go-up-symbolic"),
                                set_pixel_size: 16,
                                add_css_class: "dim-label",
                            },
                            append = &gtk::Label {
                                set_label: "Load older messages",
                                add_css_class: "dim-label",
                            },
                        },
                        add_named[Some("loading")] = &gtk::Box {
                            set_spacing: 6,
                            set_halign: gtk::Align::Center,
                            append = &adw::Spinner {
                                set_size_request: (16, 16),
                                set_valign: gtk::Align::Center,
                            },
                            append = &gtk::Label {
                                set_label: "Loading older messages…",
                                add_css_class: "dim-label",
                            },
                        },
                        #[watch]
                        set_visible_child_name: if model.state.loading_older { "loading" } else { "ready" },
                    },
                },

                append = &gtk::Overlay {
                    #[name = "message_scroller"]
                    #[wrap(Some)]
                    set_child = &gtk::ScrolledWindow {
                        set_vexpand: true,
                        set_hscrollbar_policy: gtk::PolicyType::Never,
                        #[local_ref]
                        message_view -> gtk::ListView {
                            add_css_class: "zaptide-transcript",
                            set_single_click_activate: true,
                            connect_activate[sender] => move |_, position| sender.output(TranscriptViewOutput::SelectMessage(position)).unwrap(),
                        },
                    },
                    add_overlay = &gtk::Button {
                        set_halign: gtk::Align::End,
                        set_valign: gtk::Align::End,
                        set_margin_end: 18,
                        set_margin_bottom: 18,
                        set_icon_name: "go-bottom-symbolic",
                        set_tooltip_text: Some("Go to latest message"),
                        update_property: &[gtk::accessible::Property::Label("Go to latest message")],
                        add_css_class: "circular",
                        add_css_class: "osd",
                        #[watch]
                        set_visible: model.away_from_end && !model.state.recent_messages_pending,
                        connect_clicked[sender] => move |_| sender.output(TranscriptViewOutput::ScrollToRecentMessages).unwrap(),
                    },
                    #[name = "recent_messages_button"]
                    add_overlay = &gtk::Button {
                        set_halign: gtk::Align::End,
                        set_valign: gtk::Align::End,
                        set_margin_end: 18,
                        set_margin_bottom: 18,
                        set_tooltip_text: Some("Go to new messages"),
                        add_css_class: "pill",
                        add_css_class: "suggested-action",
                        #[watch]
                        set_visible: model.state.recent_messages_pending,
                        connect_clicked[sender] => move |_| sender.output(TranscriptViewOutput::ScrollToRecentMessages).unwrap(),
                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_spacing: 6,
                            append = &gtk::Image {
                                set_icon_name: Some("go-bottom-symbolic"),
                            },
                            append = &gtk::Label {
                                set_label: "New messages",
                            },
                        },
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
        let model = Self {
            state: init.state,
            away_from_end: false,
        };
        let message_view = &init.message_view;
        let widgets = view_output!();
        let adjustment = widgets.message_scroller.vadjustment();
        let output = sender.clone();
        let away = std::cell::Cell::new(false);
        adjustment.connect_value_changed(move |adjustment| {
            let gap = adjustment.upper() - adjustment.value() - adjustment.page_size();
            if gap <= 48.0 {
                output.output(TranscriptViewOutput::AtEnd).unwrap();
            }
            let now_away = gap > AWAY_FROM_END;
            if away.replace(now_away) != now_away {
                output.input(TranscriptViewInput::AwayFromEnd(now_away));
            }
        });
        widgets
            .recent_messages_button
            .connect_visible_notify(|button| {
                if button.is_visible() {
                    button.announce(
                        "New messages available",
                        gtk::AccessibleAnnouncementPriority::Medium,
                    );
                }
            });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, input: Self::Input, _sender: ComponentSender<Self>) {
        match input {
            TranscriptViewInput::Sync(state) => self.state = state,
            TranscriptViewInput::AwayFromEnd(away) => self.away_from_end = away,
        }
    }
}
