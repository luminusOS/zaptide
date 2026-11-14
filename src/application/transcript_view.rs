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

fn should_offer_older(has_more: bool, loading: bool, near_start: bool) -> bool {
    has_more && !loading && near_start
}

#[derive(Debug)]
pub(super) enum TranscriptViewInput {
    Sync(TranscriptState),
    AwayFromEnd(bool),
    NearStart(bool),
    ReportOlder,
}

#[derive(Debug)]
pub(super) enum TranscriptViewOutput {
    OlderAvailable(bool),
    SelectMessage(u32),
    ScrollToRecentMessages,
    AtEnd,
}

impl TranscriptView {
    pub(super) fn is_synced(&self, state: &TranscriptState) -> bool {
        self.state == *state
    }

    fn has_older(&self) -> bool {
        self.state.active && self.state.has_messages && !self.state.history_complete
    }

    pub(super) fn offers_older(&self) -> bool {
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

            add_named[Some("conversation")] = &gtk::Overlay {
                #[wrap(Some)]
                set_child = &gtk::Overlay {
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
        let away = std::rc::Rc::new(std::cell::Cell::new(false));
        let report_away = {
            let sender = sender.clone();
            move |adjustment: &gtk::Adjustment| {
                let gap = adjustment.upper() - adjustment.value() - adjustment.page_size();
                let now_away = gap > AWAY_FROM_END;
                if away.replace(now_away) != now_away {
                    sender.input(TranscriptViewInput::AwayFromEnd(now_away));
                }
            }
        };
        let report_away = std::rc::Rc::new(report_away);
        let on_value_changed = report_away.clone();
        adjustment.connect_value_changed(move |adjustment| {
            let gap = adjustment.upper() - adjustment.value() - adjustment.page_size();
            if gap <= 48.0 {
                output.output(TranscriptViewOutput::AtEnd).unwrap();
            }
            on_value_changed(adjustment);
        });
        adjustment.connect_changed(move |adjustment| report_away(adjustment));
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

    fn update(&mut self, input: Self::Input, sender: ComponentSender<Self>) {
        let was_available = self.offers_older();
        let report = matches!(input, TranscriptViewInput::ReportOlder);
        match input {
            TranscriptViewInput::Sync(state) => self.state = state,
            TranscriptViewInput::AwayFromEnd(away) => self.away_from_end = away,
            TranscriptViewInput::NearStart(near) => self.near_start = near,
            TranscriptViewInput::ReportOlder => {}
        }
        let available = self.offers_older();
        if report || available != was_available {
            let _ = sender.output(TranscriptViewOutput::OlderAvailable(available));
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
    fn running_request_disables_the_control() {
        assert!(!should_offer_older(true, true, true));
        assert!(!should_offer_older(false, true, false));
    }

    #[test]
    fn near_start_includes_the_threshold_and_excludes_past_it() {
        assert!(is_near_start(0.0));
        assert!(is_near_start(NEAR_START));
        assert!(!is_near_start(NEAR_START + 0.5));
    }

    #[test]
    #[ignore = "needs a display"]
    fn jump_button_tracks_transcript_growth_without_scrolling() {
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
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured_events = events.clone();
        let controller = TranscriptView::builder()
            .launch(TranscriptViewInit {
                state: TranscriptState {
                    active: true,
                    has_messages: true,
                    history_complete: true,
                    loading_older: false,
                    recent_messages_pending: false,
                },
                message_view: list.view.clone(),
            })
            .connect_receiver(move |_, output| captured_events.borrow_mut().push(output));
        let window = gtk::Window::builder()
            .default_width(360)
            .default_height(400)
            .child(controller.widget())
            .build();
        window.present();
        spin();
        list.extend_from_iter((0..60).map(|_| Row));
        spin();

        fn find(widget: &gtk::Widget) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>()
                && button.has_css_class("zaptide-jump-button")
            {
                return Some(button.clone());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(button) = find(&current) {
                    return Some(button);
                }
                child = current.next_sibling();
            }
            None
        }
        let button = find(controller.widget().upcast_ref()).expect("jump button");
        let adjustment = list
            .view
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
            .expect("scroller")
            .vadjustment();
        window.set_visible(false);
        spin();
        adjustment.configure(0.0, 0.0, 100.0, 1.0, 10.0, 100.0);
        spin();
        events.borrow_mut().clear();
        let changed = std::rc::Rc::new(std::cell::Cell::new(0));
        let value_changed = std::rc::Rc::new(std::cell::Cell::new(0));
        adjustment.connect_changed({
            let changed = changed.clone();
            move |_| changed.set(changed.get() + 1)
        });
        adjustment.connect_value_changed({
            let value_changed = value_changed.clone();
            move |_| value_changed.set(value_changed.get() + 1)
        });

        adjustment.set_upper(1000.0);
        spin();
        assert_eq!(adjustment.value(), 0.0);
        assert!(changed.get() > 0, "upper growth emits changed");
        assert_eq!(value_changed.get(), 0, "upper growth does not move value");
        assert!(button.get_visible(), "newer rows are below the viewport");
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, TranscriptViewOutput::AtEnd))
        );

        changed.set(0);
        adjustment.set_upper(100.0);
        spin();
        assert_eq!(adjustment.value(), 0.0);
        assert!(changed.get() > 0, "upper reduction emits changed");
        assert_eq!(
            value_changed.get(),
            0,
            "upper reduction does not move value"
        );
        assert!(!button.get_visible(), "button hides when bounds shrink");
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, TranscriptViewOutput::AtEnd))
        );

        adjustment.set_upper(1000.0);
        spin();
        assert!(button.get_visible());
        events.borrow_mut().clear();
        adjustment.set_value(900.0);
        spin();
        assert!(value_changed.get() > 0, "scrolling emits value-changed");
        assert!(!button.get_visible(), "button hides at the end");
        assert!(
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, TranscriptViewOutput::AtEnd))
        );
    }

    /// Run under a display, preferably a virtual one:
    /// `GSETTINGS_BACKEND=memory GDK_BACKEND=x11 xvfb-run -a cargo test --lib older_button -- --ignored`.
    #[test]
    #[ignore = "needs a display"]
    fn older_button_follows_the_scroll_position() {
        use relm4::typed_view::list::{RelmListItem, TypedListView};

        struct Row(bool);
        impl RelmListItem for Row {
            type Root = gtk::Box;
            type Widgets = ();
            fn setup(_item: &gtk::ListItem) -> (gtk::Box, ()) {
                let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
                let label = gtk::Label::new(Some("Synthetic message"));
                label.set_valign(gtk::Align::Start);
                root.append(&label);
                (root, ())
            }
            fn bind(&mut self, _widgets: &mut (), root: &mut gtk::Box) {
                root.set_height_request(if self.0 { 800 } else { 100 });
            }
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
        list.extend_from_iter((0..60).map(|index| Row(index == 0)));
        let state = TranscriptState {
            active: true,
            has_messages: true,
            history_complete: false,
            loading_older: false,
            recent_messages_pending: false,
        };
        let button = super::super::older_history_button();
        let button_for_output = button.clone();
        let controller = TranscriptView::builder()
            .launch(TranscriptViewInit {
                state,
                message_view: list.view.clone(),
            })
            .connect_receiver(move |_, output| {
                if let TranscriptViewOutput::OlderAvailable(available) = output {
                    super::super::set_older_button_available(
                        &button_for_output,
                        available,
                        true,
                        false,
                    );
                }
            });
        let header = adw::HeaderBar::new();
        let title = gtk::Button::with_label("Chat details");
        header.set_title_widget(Some(&title));
        header.pack_start(&button);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(controller.widget()));
        let window = gtk::Window::builder()
            .default_width(360)
            .default_height(400)
            .child(&toolbar)
            .build();
        window.present();
        spin();

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
        assert!(button.is_sensitive(), "scrolled to the start");
        let button_bounds = button.compute_bounds(&window).expect("button bounds");
        let scroller_bounds = scroller.compute_bounds(&window).expect("scroller bounds");
        assert!(button_bounds.y() + button_bounds.height() <= scroller_bounds.y());

        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        spin();
        assert!(!button.is_sensitive(), "scrolled to the end");
        assert_eq!(scroller.height(), viewport_height, "viewport must not move");
        adjustment.set_value(NEAR_START - 1.0);
        spin();
        assert!(button.is_sensitive(), "back within the threshold");
        assert_eq!(scroller.height(), viewport_height, "the viewport moved");
        assert_eq!(adjustment.value(), NEAR_START - 1.0);

        adjustment.set_value(NEAR_START + 100.0);
        spin();
        assert!(!button.is_sensitive(), "just past the threshold");
        assert_eq!(scroller.height(), viewport_height, "viewport must not move");
        assert_eq!(adjustment.value(), NEAR_START + 100.0);

        adjustment.set_value(NEAR_START - 1.0);
        spin();
        assert!(
            button.grab_focus(),
            "older-history action is keyboard focusable"
        );
        adjustment.set_value(NEAR_START + 1.0);
        spin();
        assert!(!button.has_focus(), "focus leaves the disabled action");
        assert!(title.has_focus(), "focus moves to the chat title");
        assert!(!button.is_sensitive());
        assert_eq!(
            scroller.height(),
            viewport_height,
            "focus must not move the viewport"
        );
        adjustment.set_value(0.0);
        spin();
        assert!(button.grab_focus());
        controller.emit(TranscriptViewInput::Sync(TranscriptState {
            loading_older: true,
            ..state
        }));
        spin();
        super::super::set_older_button_available(&button, false, true, true);
        assert!(
            !button.is_sensitive(),
            "a running request disables the action"
        );
        assert!(title.has_focus(), "loading transfers focus to the title");
        let stack = button
            .child()
            .and_downcast::<gtk::Stack>()
            .expect("button stack");
        assert_eq!(stack.visible_child_name().as_deref(), Some("loading"));
        assert_eq!(
            button.tooltip_text().as_deref(),
            Some(gettext("Loading older messages…").as_str())
        );
        spin();
        assert_eq!(
            scroller.height(),
            viewport_height,
            "loading must not move the viewport"
        );
        assert_eq!(
            button
                .compute_bounds(&window)
                .expect("button bounds")
                .width(),
            button_bounds.width()
        );

        controller.emit(TranscriptViewInput::Sync(state));
        spin();
        assert!(button.is_sensitive());
        assert_eq!(stack.visible_child_name().as_deref(), Some("ready"));
        assert!(title.grab_focus());
        adjustment.set_value(NEAR_START + 1.0);
        spin();
        assert!(
            title.has_focus(),
            "disabling does not steal unrelated focus"
        );

        adjustment.set_value(0.0);
        controller.emit(TranscriptViewInput::Sync(TranscriptState {
            history_complete: true,
            ..state
        }));
        spin();
        assert!(!button.is_sensitive(), "nothing older to offer");

        controller.emit(TranscriptViewInput::Sync(state));
        spin();
        assert!(button.grab_focus());
        super::super::set_older_button_available(&button, false, true, false);
        assert!(title.has_focus(), "switching chats transfers focus");
        controller.emit(TranscriptViewInput::ReportOlder);
        spin();
        assert!(
            button.is_sensitive(),
            "the new chat can report an unchanged availability"
        );

        assert!(button.grab_focus());
        super::super::set_older_button_available(&button, false, false, false);
        controller.emit(TranscriptViewInput::Sync(TranscriptState {
            active: false,
            ..state
        }));
        spin();
        assert!(!button.has_focus(), "closing the chat clears button focus");
        assert!(
            !button.is_sensitive(),
            "closing the chat disables the action"
        );
    }
}
