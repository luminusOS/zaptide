use crate::i18n::gettext;
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
    /// Close enough to the oldest loaded message to offer older history.
    near_start: bool,
}

/// How far above the end, in pixels, the way-back button appears.
const AWAY_FROM_END: f64 = 240.0;

/// How far below the oldest loaded row, in pixels, the older-history button
/// appears: a few rows of lead, so it is ready before the reader hits the edge
/// and absent from the bulk of the page.
const NEAR_START: f64 = 400.0;

fn is_near_start(distance_from_top: f64) -> bool {
    distance_from_top <= NEAR_START
}

/// Older history is offered only when some may exist and the reader is at the
/// start of what is loaded. A running request keeps the control up so its
/// progress does not vanish mid-load.
fn should_offer_older(has_more: bool, loading: bool, near_start: bool) -> bool {
    has_more && (loading || near_start)
}

#[derive(Debug)]
pub(super) enum TranscriptViewInput {
    Sync(TranscriptState),
    AwayFromEnd(bool),
    NearStart(bool),
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

    fn has_older(&self) -> bool {
        self.state.has_messages && !self.state.history_complete
    }

    fn offers_older(&self) -> bool {
        should_offer_older(self.has_older(), self.state.loading_older, self.near_start)
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
                set_title: &gettext("No Conversation Selected"),
                set_description: Some("Choose a chat from the list to start messaging."),
            },

            add_named[Some("conversation")] = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                append = &gtk::Button {
                    set_halign: gtk::Align::Center,
                    set_margin_top: 4,
                    set_margin_bottom: 4,
                    add_css_class: "flat",
                    add_css_class: "zaptide-load-older",
                    set_tooltip_text: Some(&gettext("Show earlier messages from the archive or your phone")),
                    #[watch]
                    set_visible: model.offers_older(),
                    #[watch]
                    set_focusable: model.offers_older(),
                    // Stay sensitive while loading: insensitive styling would dim the
                    // spinner twice. `Input::LoadOlder` already ignores repeat clicks.
                    #[watch]
                    set_can_target: model.offers_older() && !model.state.loading_older,
                    #[watch]
                    update_state: &[
                        gtk::accessible::State::Busy(model.state.loading_older),
                        gtk::accessible::State::Hidden(!model.offers_older()),
                    ],
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
                                set_label: &gettext("Load older messages"),
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
                                set_label: &gettext("Loading older messages…"),
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
                        set_tooltip_text: Some(&gettext("Go to latest message")),
                        update_property: &[gtk::accessible::Property::Label(&gettext("Go to latest message"))],
                        add_css_class: "circular",
                        add_css_class: "suggested-action",
                        add_css_class: "zaptide-jump-button",
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
                        set_tooltip_text: Some(&gettext("Go to new messages")),
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
                                set_label: &gettext("New messages"),
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
            near_start: false,
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
        // Bounds change as rows load without moving the value, which can leave
        // a short transcript at its start with nothing scrolled.
        let near = std::rc::Rc::new(std::cell::Cell::new(false));
        let report = {
            let sender = sender.clone();
            move |adjustment: &gtk::Adjustment| {
                let now_near = is_near_start(adjustment.value());
                if near.replace(now_near) != now_near {
                    sender.input(TranscriptViewInput::NearStart(now_near));
                }
            }
        };
        let report = std::rc::Rc::new(report);
        adjustment.connect_value_changed({
            let report = report.clone();
            move |adjustment| report(adjustment)
        });
        adjustment.connect_changed(move |adjustment| report(adjustment));
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
            TranscriptViewInput::NearStart(near) => self.near_start = near,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_history_needs_both_more_and_nearness() {
        assert!(should_offer_older(true, false, true));
        assert!(!should_offer_older(true, false, false));
        assert!(!should_offer_older(false, false, true));
        assert!(!should_offer_older(false, false, false));
    }

    #[test]
    fn running_request_keeps_the_control_up_only_while_more_may_exist() {
        assert!(should_offer_older(true, true, false));
        assert!(!should_offer_older(false, true, false));
    }

    #[test]
    fn near_start_includes_the_threshold_and_excludes_past_it() {
        assert!(is_near_start(0.0));
        assert!(is_near_start(NEAR_START));
        assert!(!is_near_start(NEAR_START + 0.5));
    }

    fn find_older_button(widget: &gtk::Widget) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>()
            && button.has_css_class("zaptide-load-older")
        {
            return Some(button.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = find_older_button(&current) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }

    /// Run under a display, preferably a virtual one:
    /// `GSETTINGS_BACKEND=memory GDK_BACKEND=x11 xvfb-run -a cargo test --lib older_button -- --ignored`.
    #[test]
    #[ignore = "needs a display"]
    fn older_button_follows_the_scroll_position() {
        use relm4::typed_view::list::{RelmListItem, TypedListView};

        struct Row;
        impl RelmListItem for Row {
            type Root = gtk::Box;
            type Widgets = ();
            fn setup(_item: &gtk::ListItem) -> (gtk::Box, ()) {
                let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
                root.set_height_request(100);
                (root, ())
            }
            fn bind(&mut self, _widgets: &mut (), _root: &mut gtk::Box) {}
        }

        gtk::init().expect("display");
        adw::init().expect("libadwaita");
        let context = gtk::glib::MainContext::default();
        let spin = || {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(400);
            while std::time::Instant::now() < end {
                context.iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };

        let mut list = TypedListView::<Row, gtk::NoSelection>::new();
        list.extend_from_iter((0..60).map(|_| Row));
        let state = TranscriptState {
            active: true,
            has_messages: true,
            history_complete: false,
            loading_older: false,
            recent_messages_pending: false,
        };
        let controller = TranscriptView::builder()
            .launch(TranscriptViewInit {
                state,
                message_view: list.view.clone(),
            })
            .connect_receiver(|_, _| {});
        let window = gtk::Window::builder()
            .default_width(360)
            .default_height(400)
            .child(controller.widget())
            .build();
        window.present();
        spin();

        let button = find_older_button(controller.widget().upcast_ref()).expect("button");
        let scroller = list
            .view
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
            .expect("scroller");
        let adjustment = scroller.vadjustment();
        let viewport_height = scroller.height();
        assert!(
            adjustment.upper() > adjustment.page_size() + NEAR_START,
            "the fixture must overflow past the threshold"
        );
        let offered = |button: &gtk::Button, loading: bool| {
            let offered = button.is_visible();
            assert_eq!(button.is_focusable(), offered);
            assert_eq!(button.can_target(), offered && !loading);
            offered
        };

        assert!(offered(&button, false), "scrolled to the start");

        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        spin();
        assert!(!offered(&button, false), "scrolled to the end");
        assert!(
            scroller.height() > viewport_height,
            "the transcript fills the released space"
        );
        let conversation = scroller.parent().unwrap().parent().unwrap();
        assert_eq!(
            scroller.height(),
            conversation.height(),
            "the hidden older-history control must not reserve transcript space"
        );

        adjustment.set_value(NEAR_START - 1.0);
        spin();
        assert!(offered(&button, false), "back within the threshold");
        assert_eq!(scroller.height(), viewport_height, "the viewport moved");

        adjustment.set_value(NEAR_START + 100.0);
        spin();
        assert!(!offered(&button, false), "just past the threshold");
        assert!(
            scroller.height() > viewport_height,
            "the transcript fills the released space"
        );

        controller.emit(TranscriptViewInput::Sync(TranscriptState {
            loading_older: true,
            ..state
        }));
        spin();
        assert!(offered(&button, true), "a running request keeps it up");

        adjustment.set_value(0.0);
        controller.emit(TranscriptViewInput::Sync(TranscriptState {
            history_complete: true,
            ..state
        }));
        spin();
        assert!(!button.is_visible(), "nothing older to offer");
    }
}
