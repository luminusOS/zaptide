use super::*;

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
