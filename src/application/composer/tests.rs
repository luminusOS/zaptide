use super::*;

#[test]
#[ignore = "needs a display"]
fn selecting_emoji_restores_composer_focus() {
    libadwaita::init().unwrap();
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text("Hello ");
    let inserted = buffer.clone();
    let sent = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sent_messages = sent.clone();
    let controller = ComposerView::builder()
        .launch(ComposerViewInit {
            state: ComposerState {
                active: true,
                editable: true,
                link_previews: false,
                ..Default::default()
            },
            buffer: buffer.clone(),
            enter_sends: std::rc::Rc::new(std::cell::Cell::new(true)),
            recording_meter_area: gtk::DrawingArea::new(),
        })
        .connect_receiver(move |_, output| match output {
            ComposerViewOutput::InsertEmoji(emoji) => {
                inserted.insert(&mut inserted.end_iter(), &emoji)
            }
            ComposerViewOutput::SendText { text, .. } => sent_messages.borrow_mut().push(text),
            _ => {}
        });
    let window = gtk::Window::builder()
        .default_width(600)
        .default_height(400)
        .child(controller.widget())
        .build();
    window.present();
    let settle = || {
        let context = gtk::glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    fn find(widget: &gtk::Widget) -> Option<gtk::MenuButton> {
        if let Some(button) = widget.downcast_ref::<gtk::MenuButton>()
            && button.icon_name().as_deref() == Some("face-smile-symbolic")
        {
            return Some(button.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(button) = find(&widget) {
                return Some(button);
            }
            child = widget.next_sibling();
        }
        None
    }
    settle();
    let view = controller.model().text_view();
    view.grab_focus();
    let button = find(controller.widget().upcast_ref()).unwrap();
    button.popup();
    settle();
    let popover = button.popover().unwrap();
    let content = popover.child().unwrap();
    let search = content.first_child().unwrap();
    search.grab_focus();
    let scroller = search
        .next_sibling()
        .unwrap()
        .downcast::<gtk::ScrolledWindow>()
        .unwrap();
    let grid = scroller
        .child()
        .unwrap()
        .downcast::<gtk::GridView>()
        .unwrap();
    grid.set_model(Some(&gtk::NoSelection::new(Some(gtk::StringList::new(&[
        "🙌🏻",
    ])))));
    grid.emit_by_name::<()>("activate", &[&0u32]);
    settle();
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    let focused = view.has_focus();
    let closed = !popover.is_visible();
    view.emit_by_name::<()>("insert-at-cursor", &[&"!"]);
    let keys = view.observe_controllers();
    let keys = (0..keys.n_items())
        .filter_map(|i| keys.item(i).and_downcast::<gtk::EventControllerKey>())
        .find(|keys| keys.propagation_phase() == gtk::PropagationPhase::Bubble)
        .unwrap();
    let consumed = keys.emit_by_name::<bool>(
        "key-pressed",
        &[
            &gtk::gdk::Key::Return,
            &0u32,
            &gtk::gdk::ModifierType::empty(),
        ],
    );
    settle();
    window.close();
    assert_eq!(text, "Hello 🙌🏻");
    assert!(closed);
    assert!(focused, "keyboard focus did not return to the composer");
    assert!(consumed);
    assert_eq!(&*sent.borrow(), &["Hello 🙌🏻!"]);
}

#[test]
#[ignore = "needs a display"]
fn pasted_wrapped_link_keeps_vertical_insets() {
    libadwaita::init().unwrap();
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../../../data/style.css"));
    let display = gtk::gdk::Display::default().unwrap();
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let buffer = gtk::TextBuffer::new(None);
    let controller = ComposerView::builder()
        .launch(ComposerViewInit {
            state: ComposerState {
                active: true,
                editable: true,
                link_previews: false,
                ..Default::default()
            },
            buffer: buffer.clone(),
            enter_sends: Default::default(),
            recording_meter_area: gtk::DrawingArea::new(),
        })
        .connect_receiver(|_, _| {});
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layout.append(&gtk::Box::builder().vexpand(true).build());
    layout.append(controller.widget());
    let window = gtk::Window::builder()
        .default_width(600)
        .default_height(300)
        .child(&layout)
        .build();
    window.present();
    let view = controller.model().text_view();
    view.grab_focus();
    let settle = || {
        let context = gtk::glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    };
    settle();
    for text in [
        "Short message".to_owned(),
        format!("https://example.com/{}", "a".repeat(96)),
        "A wrapped plain text message ".repeat(5),
        format!("https://example.com/{}", "a".repeat(1200)),
    ] {
        buffer.set_text("");
        settle();
        buffer.insert_at_cursor(&text);
        view.scroll_mark_onscreen(&buffer.get_insert());
        settle();
        let scroll = view
            .parent()
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        assert_eq!(scroll.vadjustment().page_size(), f64::from(view.height()));
        let rect = view.iter_location(&buffer.start_iter());
        let (_, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
        let end = view.iter_location(&buffer.end_iter());
        let (_, end_y) =
            view.buffer_to_window_coords(gtk::TextWindowType::Widget, end.x(), end.y());
        if scroll.vadjustment().upper() <= scroll.vadjustment().page_size() {
            assert!(y >= 7, "first line lost its upper inset: {y}");
        }
        assert!(
            view.height() - end_y - end.height() >= 7,
            "last line lost its lower inset"
        );
        assert!(view.height() <= 160);
    }
    window.close();
    gtk::style_context_remove_provider_for_display(&display, &provider);
}
