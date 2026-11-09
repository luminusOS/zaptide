use super::*;

#[test]
#[ignore = "needs private X11 display and native Compose fixture"]
fn native_composer_ime_preedit_and_commit() {
    use std::{
        cell::RefCell,
        rc::Rc,
        time::{Duration, Instant},
    };
    libadwaita::init().unwrap();
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_enable_undo(true);
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
    let window = gtk::Window::builder()
        .title("Markdown IME Fixture")
        .default_width(500)
        .default_height(160)
        .child(controller.widget())
        .build();
    window.present();
    let view = controller.model().text_view();
    let preedit = Rc::new(RefCell::new(Vec::<String>::new()));
    let observed = preedit.clone();
    view.connect_preedit_changed(move |_, value| observed.borrow_mut().push(value.to_owned()));
    buffer.set_text("*raw* ");
    buffer.place_cursor(&buffer.end_iter());
    view.grab_focus();
    let context = gtk::glib::MainContext::default();
    let settle = Instant::now() + Duration::from_millis(400);
    while Instant::now() < settle {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let script = std::env::var("ZMD_IME_SCRIPT").unwrap();
    let mut child = std::process::Command::new("python3")
        .arg(script)
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        while context.pending() {
            context.iteration(false);
        }
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < until, "native Compose injection timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    let until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "*raw* é"
    );
    assert!(
        preedit.borrow().iter().any(|text| !text.is_empty()),
        "no native preedit signal: {:?}",
        *preedit.borrow()
    );
    eprintln!(
        "native preedit transitions: {:?}; commit: é",
        *preedit.borrow()
    );
    buffer.undo();
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "*raw* "
    );
    buffer.redo();
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        "*raw* é"
    );
    window.close();
}

#[test]
#[ignore = "needs a display and synthetic screenshot environment"]
fn synthetic_composer_and_markdown_visual_accessibility_fixture() {
    use std::time::{Duration, Instant};
    libadwaita::init().unwrap();
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_enable_undo(true);
    let drafts = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let sink = drafts.clone();
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
        .connect_receiver(move |_, output| {
            if let ComposerViewOutput::DraftChanged(text) = output {
                sink.borrow_mut().push(text);
            }
        });
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(20)
        .margin_end(20)
        .build();
    let raw = "*bold* _italic_ ~strike~ `é😀` ```mono```\n- bullet example that wraps onto another line with readable continuation\n123456789. numbered example wraps naturally with a long marker\n> quote with https://example.com";
    let bubble = crate::native_media_widgets::text_bubble::TextBubble::new();
    bubble.set_formatted(raw, &[]);
    let rtl = crate::native_media_widgets::text_bubble::TextBubble::new();
    rtl.set_formatted("> مرحبا بالعالم *نص* https://example.com\n- אבגדה Unicode mixed wraps with repeated content repeated content", &[]);
    rtl.view().set_direction(gtk::TextDirection::Rtl);
    root.append(&gtk::Label::new(Some("Synthetic message, left to right")));
    root.append(&bubble);
    root.append(&gtk::Label::new(Some("Synthetic message, mixed direction")));
    root.append(&rtl);
    let card_message = crate::model::Message {
        id: "synthetic".into(),
        chat: "synthetic".into(),
        sender: "synthetic".into(),
        sender_name: None,
        from_me: false,
        timestamp: 0,
        content: crate::model::Content::Buttons {
            text: "*Card* _text_ `code`\n- wrapped item with https://example.com".into(),
            footer: None,
            buttons: Vec::new(),
            answered: None,
        },
        status: crate::model::Delivery::None,
        delivered_at: None,
        read_at: None,
        quoted: None,
        reactions: Vec::new(),
        edited: false,
        mentions: Vec::new(),
        forwarded: false,
        thumbnail: None,
    };
    let card = crate::native_media_widgets::build_media_widget_with_action(&card_message, |_| {});
    root.append(&gtk::Label::new(Some("Synthetic interactive card")));
    root.append(&card.widget);
    root.append(&gtk::Label::new(Some("Composer, raw Markdown")));
    root.append(controller.widget());
    let window = gtk::Window::builder()
        .title("Markdown Synthetic Fixture")
        .default_width(620)
        .default_height(700)
        .child(&root)
        .build();
    window.present();
    let context = gtk::glib::MainContext::default();
    let settle = |duration| {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    buffer.set_text(raw);
    let view = controller.model().text_view();
    view.grab_focus();
    buffer.place_cursor(&buffer.end_iter());
    view.emit_by_name::<()>("insert-at-cursor", &[&" Ω"]);
    assert!(
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .ends_with(" Ω")
    );
    assert!(buffer.can_undo());
    buffer.undo();
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        raw
    );
    buffer.redo();
    assert!(
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .ends_with(" Ω")
    );
    buffer.select_range(&buffer.iter_at_offset(1), &buffer.iter_at_offset(5));
    settle(Duration::from_millis(500));
    assert!(
        drafts
            .borrow()
            .last()
            .is_some_and(|text| text.ends_with(" Ω"))
    );
    let link = buffer.tag_table().lookup(LINK_TAG).unwrap();
    let link_offset = raw[..raw.find("https://example.com").unwrap()]
        .chars()
        .count() as i32;
    assert!(buffer.iter_at_offset(link_offset).has_tag(&link));
    let directory = std::env::var("ZMD_SCREENSHOT_DIR").unwrap();
    let capture = |name: &str| {
        let status = std::process::Command::new("import")
            .args(["-window", "root", &format!("{directory}/{name}")])
            .status()
            .unwrap();
        assert!(status.success());
    };
    capture("synthetic-markdown-normal.png");
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        "textview.zaptide-message-text, textview.synthetic-composer { font-size: 22px; }",
    );
    let display = gtk::gdk::Display::default().unwrap();
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );
    view.add_css_class("synthetic-composer");
    window.set_default_size(480, 850);
    settle(Duration::from_millis(500));
    capture("synthetic-markdown-large.png");
    let mut child = root.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.is::<gtk::Label>() {
            widget.set_visible(false);
        }
    }
    bubble.set_visible(false);
    rtl.set_visible(false);
    card.widget.set_visible(false);
    window.set_default_size(620, 450);
    settle(Duration::from_millis(300));
    capture("synthetic-composer-closeup.png");
    view.scroll_to_iter(&mut buffer.end_iter(), 0.0, false, 0.0, 0.0);
    settle(Duration::from_millis(200));
    capture("synthetic-composer-end.png");
    controller.widget().set_visible(false);
    bubble.set_visible(true);
    rtl.set_visible(true);
    card.widget.set_visible(true);
    gtk::style_context_remove_provider_for_display(&display, &provider);
    window.set_default_size(400, 450);
    settle(Duration::from_millis(300));
    capture("synthetic-bubbles-closeup.png");
    bubble.set_visible(false);
    rtl.set_visible(false);
    card.widget.set_visible(false);
    controller.widget().set_visible(true);
    buffer.select_range(&buffer.iter_at_offset(1), &buffer.iter_at_offset(5));
    settle(Duration::from_millis(300));
    let script = std::env::var("ZMD_ATSPI_SCRIPT").unwrap();
    let log = format!("{directory}/review2-atspi-tree.log");
    let output = std::fs::File::create(log).unwrap();
    let mut child = std::process::Command::new("python3")
        .arg(script)
        .stdout(output.try_clone().unwrap())
        .stderr(output)
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(12);
    let status = loop {
        while context.pending() {
            context.iteration(false);
        }
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < until, "AT-SPI query timed out");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(status.success(), "AT-SPI query failed");
    settle(Duration::from_millis(200));
    assert!(
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .ends_with(" Ω界")
    );
    assert!(
        drafts
            .borrow()
            .last()
            .is_some_and(|text| text.ends_with(" Ω界"))
    );
    window.close();
}

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
