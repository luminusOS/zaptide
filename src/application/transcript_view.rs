use crate::i18n::gettext;
use gtk::prelude::*;
use relm4::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TranscriptState {
    pub(super) request_epoch: u64,
    pub(super) active: bool,
    pub(super) has_messages: bool,
    pub(super) history_complete: bool,
    pub(super) loading_older: bool,
    pub(super) recent_messages_pending: bool,
}

impl TranscriptState {
    fn has_older(self) -> bool {
        self.active && self.has_messages && !self.history_complete
    }

    fn can_load_older(self) -> bool {
        self.has_older() && !self.loading_older
    }
}

pub(super) struct TranscriptViewInit {
    pub(super) state: TranscriptState,
    pub(super) message_view: gtk::ListView,
}

pub(super) struct TranscriptView {
    state: TranscriptState,
    /// Scrolled far enough from the newest message to offer a way back.
    away_from_end: bool,
    history_headers: std::rc::Rc<std::cell::RefCell<Vec<gtk::glib::WeakRef<gtk::ListHeader>>>>,
    header_state: std::rc::Rc<std::cell::Cell<TranscriptState>>,
}

/// How far above the end, in pixels, the way-back button appears.
const AWAY_FROM_END: f64 = 240.0;

#[derive(Debug)]
pub(super) enum TranscriptViewInput {
    Sync(TranscriptState),
    AwayFromEnd(bool),
    LoadOlder(u64),
}

#[derive(Debug)]
pub(super) enum TranscriptViewOutput {
    LoadOlder(u64),
    SelectMessage(u32),
    ScrollToRecentMessages,
    AtEnd,
}

impl TranscriptView {
    pub(super) fn is_synced(&self, state: &TranscriptState) -> bool {
        self.state == *state
    }
}

fn sync_history_header(header: &gtk::ListHeader, state: TranscriptState) {
    let Some(button) = header.child().and_downcast::<gtk::Button>() else {
        return;
    };
    let visible = header.start() == 0 && state.has_older();
    super::set_older_button_available(
        &button,
        visible && !state.loading_older,
        state.loading_older,
    );
    button.set_visible(visible);
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
            history_headers: Default::default(),
            header_state: std::rc::Rc::new(std::cell::Cell::new(init.state)),
        };
        let message_view = &init.message_view;
        let widgets = view_output!();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup({
            let sender = sender.clone();
            let state = model.header_state.clone();
            move |_, object| {
                let header = object
                    .downcast_ref::<gtk::ListHeader>()
                    .expect("list header");
                let button = super::older_history_button();
                button.connect_clicked({
                    let sender = sender.clone();
                    let state = state.clone();
                    move |_| sender.input(TranscriptViewInput::LoadOlder(state.get().request_epoch))
                });
                header.set_child(Some(&button));
            }
        });
        factory.connect_bind({
            let headers = model.history_headers.clone();
            let state = model.header_state.clone();
            move |_, object| {
                let header = object
                    .downcast_ref::<gtk::ListHeader>()
                    .expect("list header");
                headers.borrow_mut().push(header.downgrade());
                sync_history_header(header, state.get());
            }
        });
        factory.connect_unbind({
            let headers = model.history_headers.clone();
            move |_, object| {
                let header = object
                    .downcast_ref::<gtk::ListHeader>()
                    .expect("list header");
                if let Some(child) = header.child() {
                    child.set_visible(false);
                }
                headers
                    .borrow_mut()
                    .retain(|weak| weak.upgrade().is_some_and(|current| current != *header));
            }
        });
        factory.connect_teardown(|_, object| {
            if let Some(header) = object.downcast_ref::<gtk::ListHeader>() {
                header.set_child(None::<&gtk::Widget>);
            }
        });
        message_view.set_header_factory(Some(&factory));
        // GTK's list-item focus traversal skips interactive section headers.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let headers = model.history_headers.clone();
            move |_, key, _, modifiers| {
                let backward = key == gtk::gdk::Key::ISO_Left_Tab
                    || (key == gtk::gdk::Key::Tab
                        && modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK));
                if !backward && key != gtk::gdk::Key::Tab {
                    return gtk::glib::Propagation::Proceed;
                }
                for header in headers
                    .borrow()
                    .iter()
                    .filter_map(gtk::glib::WeakRef::upgrade)
                {
                    let Some(button) = header.child().and_downcast::<gtk::Button>() else {
                        continue;
                    };
                    if !button.is_visible() || !button.is_sensitive() {
                        continue;
                    }
                    let Some(row) = button.parent().and_then(|header| header.next_sibling()) else {
                        continue;
                    };
                    if !backward {
                        if button.has_focus() && row.grab_focus() {
                            super::motion::cancel_auto_scroll();
                            return gtk::glib::Propagation::Stop;
                        }
                        continue;
                    }
                    if button.has_focus() {
                        continue;
                    }
                    let Some(focused) = row.root().and_then(|root| root.focus()) else {
                        continue;
                    };
                    if focused != row && !focused.is_ancestor(&row) {
                        continue;
                    }
                    if !row.has_focus() && row.child_focus(gtk::DirectionType::TabBackward) {
                        return gtk::glib::Propagation::Stop;
                    }
                    if button.grab_focus() {
                        super::motion::cancel_auto_scroll();
                        if let Some(scroller) = button
                            .ancestor(gtk::ScrolledWindow::static_type())
                            .and_downcast::<gtk::ScrolledWindow>()
                        {
                            scroller.vadjustment().set_value(0.0);
                        }
                        return gtk::glib::Propagation::Stop;
                    }
                }
                gtk::glib::Propagation::Proceed
            }
        });
        widgets.message_scroller.add_controller(keys);
        super::motion::install_scroll_cancellation(&widgets.message_scroller, message_view);
        super::motion::install_end_following(&widgets.message_scroller);
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
        match input {
            TranscriptViewInput::Sync(state) => self.state = state,
            TranscriptViewInput::AwayFromEnd(away) => self.away_from_end = away,
            TranscriptViewInput::LoadOlder(epoch) => {
                if self.state.request_epoch == epoch && self.state.can_load_older() {
                    let _ = sender.output(TranscriptViewOutput::LoadOlder(epoch));
                }
            }
        }
        self.header_state.set(self.state);
        self.history_headers.borrow_mut().retain(|weak| {
            let Some(header) = weak.upgrade() else {
                return false;
            };
            sync_history_header(&header, self.state);
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture_history_fixture(window: &gtk::Window, name: &str) {
        let Ok(directory) = std::env::var("ZAPTIDE_HISTORY_CAPTURE_DIR") else {
            return;
        };
        assert_eq!(
            std::env::var("ZAPTIDE_TEST_PRIVATE_XVFB").as_deref(),
            Ok("1")
        );
        let paintable = gtk::WidgetPaintable::new(Some(window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            &snapshot,
            f64::from(window.width()),
            f64::from(window.height()),
        );
        let node = snapshot.to_node().expect("fixture render node");
        let texture = window
            .renderer()
            .expect("fixture renderer")
            .render_texture(&node, None);
        texture
            .save_to_png(std::path::Path::new(&directory).join(format!("{name}.png")))
            .expect("fixture screenshot");
    }

    fn paginated_state() -> TranscriptState {
        TranscriptState {
            request_epoch: 0,
            active: true,
            has_messages: true,
            history_complete: false,
            loading_older: false,
            recent_messages_pending: false,
        }
    }

    #[test]
    fn older_history_requires_an_active_nonempty_conversation() {
        let state = paginated_state();
        assert!(state.has_older());
        assert!(
            !TranscriptState {
                active: false,
                ..state
            }
            .has_older()
        );
        assert!(
            !TranscriptState {
                has_messages: false,
                ..state
            }
            .has_older()
        );
    }

    #[test]
    fn running_request_preserves_the_control_but_blocks_activation() {
        let state = TranscriptState {
            loading_older: true,
            ..paginated_state()
        };
        assert!(state.has_older());
        assert!(!state.can_load_older());
        assert!(paginated_state().can_load_older());
    }

    #[test]
    fn completed_history_has_no_pagination_control() {
        let state = TranscriptState {
            history_complete: true,
            ..paginated_state()
        };
        assert!(!state.has_older());
        assert!(!state.can_load_older());
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
                    request_epoch: 0,
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
        adjustment.set_value(0.0);
        spin();
        window.set_visible(false);
        spin();
        adjustment.configure(0.0, 0.0, 200.0, 1.0, 10.0, 100.0);
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
        assert_eq!(
            adjustment.value(),
            900.0,
            "growth follows a view resting at the end"
        );
        assert!(!button.get_visible());
        adjustment.set_value(0.0);
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

    #[test]
    #[ignore = "requires private Xvfb, Python, X11 and XTest"]
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
                root.append(&gtk::Button::with_label("First message control"));
                root.append(&gtk::Button::with_label("Second message control"));
                (root, ())
            }
            fn bind(&mut self, _widgets: &mut (), root: &mut gtk::Box) {
                root.set_height_request(if self.0 { 800 } else { 100 });
                root.set_widget_name(if self.0 {
                    "synthetic-anchor"
                } else {
                    "synthetic-row"
                });
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
        let manager = adw::StyleManager::default();
        manager.set_color_scheme(
            if std::env::var("ZAPTIDE_HISTORY_THEME").as_deref() == Ok("dark") {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::ForceLight
            },
        );
        if std::env::var("ZAPTIDE_HISTORY_THEME").as_deref() == Ok("contrast") {
            assert!(
                manager.is_high_contrast(),
                "fixture must enable actual high contrast"
            );
        }
        let state = TranscriptState {
            request_epoch: 0,
            active: true,
            has_messages: true,
            history_complete: false,
            loading_older: false,
            recent_messages_pending: false,
        };
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = events.clone();
        let controller = TranscriptView::builder()
            .launch(TranscriptViewInit {
                state,
                message_view: list.view.clone(),
            })
            .connect_receiver(move |_, output| captured.borrow_mut().push(output));
        let header = adw::HeaderBar::new();
        let title = gtk::Button::with_label("Chat details");
        header.set_title_widget(Some(&title));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(controller.widget()));
        let window = gtk::Window::builder()
            .default_width(360)
            .default_height(400)
            .child(&toolbar)
            .title("ZapTide History Fixture")
            .build();
        window.present();
        spin();
        fn find_button(widget: &gtk::Widget) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>()
                && button.has_css_class("zaptide-load-older")
            {
                return Some(button.clone());
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(button) = find_button(&current) {
                    return Some(button);
                }
                child = current.next_sibling();
            }
            None
        }
        let button = find_button(list.view.upcast_ref()).expect("inline history button");

        let scroller = list
            .view
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
            .expect("scroller");
        let adjustment = scroller.vadjustment();
        let viewport_height = scroller.height();
        adjustment.set_value(0.0);
        spin();
        assert!(
            button.ancestor(gtk::ListView::static_type()).is_some(),
            "older history belongs above the oldest message inside the list"
        );
        assert!(
            adjustment.upper() > adjustment.page_size() + 800.0,
            "the fixture must overflow"
        );
        assert!(button.is_sensitive(), "scrolled to the start");
        let button_bounds = button.compute_bounds(&window).expect("button bounds");
        let scroller_bounds = scroller.compute_bounds(&window).expect("scroller bounds");
        assert!(
            button_bounds.y() >= scroller_bounds.y(),
            "button={button_bounds:?}, scroller={scroller_bounds:?}, adjustment={}",
            adjustment.value()
        );
        assert!(
            button_bounds.y() + button_bounds.height()
                <= scroller_bounds.y() + scroller_bounds.height()
        );
        capture_history_fixture(&window, "oldest");

        adjustment.set_value(adjustment.upper() - adjustment.page_size());
        spin();
        assert!(!button.is_mapped(), "absent while reading newer messages");
        capture_history_fixture(&window, "recent");
        assert_eq!(scroller.height(), viewport_height, "viewport must not move");
        adjustment.set_value(400.0);
        spin();
        assert!(
            !button.is_mapped(),
            "not offered before the oldest boundary"
        );
        assert_eq!(scroller.height(), viewport_height, "the viewport moved");
        assert!((adjustment.value() - 400.0).abs() <= 1.0);

        adjustment.set_value(500.0);
        spin();
        assert!(!button.is_mapped());
        assert_eq!(scroller.height(), viewport_height, "viewport must not move");
        assert!((adjustment.value() - 500.0).abs() <= 1.0);

        adjustment.set_value(0.0);
        spin();
        assert!(
            button.grab_focus(),
            "older-history action is keyboard focusable"
        );
        super::super::sidebar_runtime_tests::native_x11_input(&window, &scroller, "Tab");
        spin();
        assert!(!button.has_focus(), "Tab leaves the history action");
        super::super::sidebar_runtime_tests::native_x11_input(&window, &scroller, "Shift+Tab");
        spin();
        assert!(button.has_focus(), "Shift+Tab reaches the history action");
        let message_root = button
            .parent()
            .and_then(|header| header.next_sibling())
            .and_then(|row| row.first_child())
            .expect("first message widget");
        let first_control = message_root
            .first_child()
            .and_then(|label| label.next_sibling())
            .and_downcast::<gtk::Button>()
            .expect("first message control");
        let second_control = first_control
            .next_sibling()
            .and_downcast::<gtk::Button>()
            .expect("second message control");
        assert!(second_control.grab_focus());
        super::super::sidebar_runtime_tests::native_x11_input(&window, &scroller, "Shift+Tab");
        spin();
        assert!(
            first_control.has_focus(),
            "Shift+Tab must preserve traversal inside a message"
        );
        assert!(
            !button.has_focus(),
            "message controls must not jump to history"
        );
        assert!(button.grab_focus());
        super::super::sidebar_runtime_tests::native_x11_input(&window, &scroller, "space");
        spin();
        assert!(
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, TranscriptViewOutput::LoadOlder(0)))
        );
        events.borrow_mut().clear();
        controller.emit(TranscriptViewInput::Sync(TranscriptState {
            request_epoch: 1,
            ..state
        }));
        controller.emit(TranscriptViewInput::LoadOlder(0));
        spin();
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, TranscriptViewOutput::LoadOlder(_))),
            "a queued click must not load a replacement conversation"
        );
        controller.emit(TranscriptViewInput::Sync(state));
        spin();
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
        assert!(
            !button.is_sensitive(),
            "a running request disables the action"
        );
        assert!(
            scroller.has_focus(),
            "loading keeps focus within the transcript"
        );
        let stack = button
            .child()
            .and_then(|child| child.first_child())
            .and_downcast::<gtk::Stack>()
            .expect("button stack");
        assert_eq!(stack.visible_child_name().as_deref(), Some("loading"));
        assert_eq!(
            button.tooltip_text().as_deref(),
            Some(gettext("Loading older messages…").as_str())
        );
        capture_history_fixture(&window, "loading");
        events.borrow_mut().clear();
        controller.emit(TranscriptViewInput::LoadOlder(0));
        spin();
        assert!(
            !events
                .borrow()
                .iter()
                .any(|event| matches!(event, TranscriptViewOutput::LoadOlder(_))),
            "loading must reject duplicate requests"
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
        adjustment.set_value(400.0);
        spin();
        assert!(
            title.has_focus(),
            "disabling does not steal unrelated focus"
        );

        adjustment.set_value(0.0);
        spin();
        let anchor = {
            let mut child = list.view.first_child();
            let mut anchor = None;
            while let Some(item) = child {
                if let Some(root) = item.first_child()
                    && root.widget_name() == "synthetic-anchor"
                {
                    anchor = Some(root);
                    break;
                }
                child = item.next_sibling();
            }
            anchor.expect("visible oldest synthetic message")
        };
        let original_offset = anchor.compute_bounds(&scroller).expect("anchor bounds").y();
        for _ in 0..20 {
            list.insert(0, Row(false));
        }
        spin();
        assert!(
            (anchor.compute_bounds(&scroller).expect("anchor bounds").y() - original_offset).abs()
                <= 1.0,
            "prepending must preserve the visible message offset"
        );
        let ids = vec!["synthetic-anchor".to_string()];
        let (id, offset) = super::super::motion::reading_anchor(&list.view, &ids)
            .expect("reader anchor before rebuilding");
        list.clear();
        list.extend_from_iter((0..80).map(|index| Row(index == 20)));
        super::super::motion::restore_reading_anchor(&list.view, 20, id, offset);
        spin();
        let restored = super::super::motion::reading_anchor(&list.view, &ids)
            .expect("reader anchor after rebuilding");
        assert!(
            (restored.1 - f64::from(original_offset)).abs() <= 1.0,
            "rebuilding must preserve the reader offset: {restored:?}"
        );
        super::super::motion::restore_reading_anchor(&list.view, 20, restored.0, restored.1);
        list.clear();
        list.extend_from_iter((0..60).map(|_| Row(false)));
        spin();
        adjustment.set_value(500.0);
        spin();
        assert!(
            (adjustment.value() - 500.0).abs() <= 1.0,
            "obsolete restoration must not scroll a replacement conversation"
        );
        controller.emit(TranscriptViewInput::Sync(TranscriptState {
            history_complete: true,
            ..state
        }));
        spin();
        assert!(
            find_button(list.view.upcast_ref()).is_none_or(|button| !button.is_visible()),
            "nothing older to offer"
        );

        controller.emit(TranscriptViewInput::Sync(state));
        spin();
        adjustment.set_value(0.0);
        spin();
        let button = find_button(list.view.upcast_ref()).expect("replacement history button");
        assert!(button.grab_focus());
        controller.emit(TranscriptViewInput::Sync(TranscriptState {
            active: false,
            ..state
        }));
        spin();
        assert!(!button.has_focus(), "closing the chat clears button focus");
        assert!(!button.is_visible(), "closing the chat hides the action");

        list.clear();
        list.append(Row(false));
        window.set_default_size(280, 400);
        controller.emit(TranscriptViewInput::Sync(state));
        spin();
        adjustment.set_value(0.0);
        spin();
        let button = find_button(list.view.upcast_ref()).expect("short-history action");
        assert!(
            button.is_mapped(),
            "short histories expose their oldest boundary"
        );
        let bounds = button
            .compute_bounds(&scroller)
            .expect("short-history bounds");
        assert!(bounds.x() >= 0.0 && bounds.x() + bounds.width() <= scroller.width() as f32);
        capture_history_fixture(&window, "narrow");
        list.clear();
        controller.emit(TranscriptViewInput::Sync(TranscriptState {
            has_messages: false,
            ..state
        }));
        spin();
        assert!(
            find_button(list.view.upcast_ref()).is_none_or(|button| !button.is_visible()),
            "empty histories have no pagination action"
        );
    }
}
