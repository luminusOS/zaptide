use std::time::{Duration, Instant};

use super::super::*;

fn settle() {
    let context = gtk::glib::MainContext::default();
    let until = Instant::now() + Duration::from_millis(150);
    while Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn visible_text_width(view: &gtk::TextView) -> i32 {
    let buffer = view.buffer();
    let mut line = buffer.start_iter();
    let mut right = 0;
    loop {
        let mut end = line;
        view.forward_display_line_end(&mut end);
        let rect = view.iter_location(&end);
        right = right.max(rect.x() + rect.width());
        if !view.forward_display_line(&mut line) {
            break;
        }
    }
    right
}

fn capture(window: &gtk::Window, name: &str) {
    let Some(directory) = std::env::var_os("RENDER_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    window
        .renderer()
        .unwrap()
        .render_texture(snapshot.to_node().unwrap(), None)
        .save_to_png(directory.join(format!("{name}.png")))
        .unwrap();
}

#[test]
#[ignore = "needs a display"]
fn wrapped_message_bubbles_fit_their_visible_lines() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(
        "{}\n{}",
        include_str!("../../../../data/theme-defaults.css"),
        include_str!("../../../../data/style.css")
    ));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let item: gtk::ListItem = gtk::glib::Object::new();
    let (root, mut widgets) = <MessageRow as RelmListItem>::setup(&item);
    for widget in [
        widgets.separator.upcast_ref::<gtk::Widget>(),
        widgets.unread_divider.upcast_ref(),
        widgets.header.upcast_ref(),
        widgets.quote.upcast_ref(),
        widgets.media.upcast_ref(),
        widgets.audio.upcast_ref(),
        widgets.reactions.upcast_ref(),
        widgets.status_icon.upcast_ref(),
    ] {
        widget.set_visible(false);
    }
    widgets.footer.set("", "12:34", false);
    let window = gtk::Window::builder()
        .default_width(700)
        .default_height(240)
        .child(&root)
        .build();
    window.present();
    let plain = format!("A synthetic wrapping example {}", "m".repeat(30));
    let samples = [
        plain.clone(),
        format!("*{plain}*"),
        format!("{plain}\nAnother synthetic paragraph"),
        format!("Uma revisão sintética {}", "é".repeat(45)),
        format!("- {plain}"),
        format!("> {plain}"),
        format!("*First paragraph*\n\n- *{plain}*\n- Another item"),
        format!("> {plain}\n> Another paragraph"),
    ];
    for (width, font) in [
        (700, 13),
        (340, 13),
        (700, 13),
        (700, 19),
        (340, 19),
        (700, 19),
    ] {
        provider.load_from_string(&format!(
            "{}\n{}\ntextview.zaptide-message-text {{ font-size: {font}px; }}",
            include_str!("../../../../data/theme-defaults.css"),
            include_str!("../../../../data/style.css")
        ));
        window.set_default_size(width, 240);
        for outgoing in [false, true] {
            widgets.leading_space.set_visible(outgoing);
            widgets.trailing_space.set_visible(!outgoing);
            widgets.avatar.set_visible(!outgoing);
            widgets.status.set_visible(outgoing);
            widgets.bubble.set_halign(if outgoing {
                gtk::Align::End
            } else {
                gtk::Align::Start
            });
            widgets
                .bubble
                .add_css_class(if outgoing { "outgoing" } else { "incoming" });
            widgets
                .bubble
                .remove_css_class(if outgoing { "incoming" } else { "outgoing" });
            for (index, text) in samples.iter().enumerate() {
                widgets.set_body("wrapped", text, &[]);
                settle();
                let view = widgets.body.view();
                let right = visible_text_width(&view);
                assert!(right <= view.width(), "text exceeds the bubble width");
                // Automatic hyphens occupy space without a TextBuffer character.
                let hyphen = view.create_pango_layout(Some("‐")).pixel_size().0;
                assert!(
                    view.width() - right <= hyphen + 2,
                    "unused text width: {}, view: {}, line: {right}, window: {width}, font: {font}, outgoing: {outgoing}, sample: {index}",
                    view.width() - right,
                    view.width(),
                );
                let bounds = widgets.bubble.compute_bounds(&root).unwrap();
                assert!(bounds.width() <= 480.0, "bubble: {bounds:?}");
                assert!(bounds.x() >= 0.0 && bounds.x() + bounds.width() <= root.width() as f32);
                if outgoing {
                    let row = widgets
                        .leading_space
                        .parent()
                        .unwrap()
                        .downcast::<gtk::Box>()
                        .unwrap();
                    let row_bounds = row.compute_bounds(&root).unwrap();
                    assert!(
                        (row_bounds.x() + row_bounds.width() - bounds.x() - bounds.width()).abs()
                            <= row.spacing() as f32,
                        "row: {row_bounds:?}, bubble: {bounds:?}"
                    );
                } else {
                    let avatar = widgets.avatar.compute_bounds(&root).unwrap();
                    assert!((bounds.x() - avatar.x() - avatar.width() - 8.0).abs() <= 1.0);
                }
                let end = view.iter_location(&view.buffer().end_iter());
                assert!(view.height() >= end.y() + end.height(), "text is clipped");
                if index == 0 {
                    capture(&window, &format!("bubble-{width}-{font}-{outgoing}"));
                }
            }
        }
    }
    widgets.header.set_visible(true);
    widgets
        .name
        .set_label("Synthetic sender with a longer name");
    widgets.set_body("metadata", "OK", &[]);
    settle();
    assert!(widgets.body.width() >= widgets.header.measure(gtk::Orientation::Horizontal, -1).1);
    let attachment = gtk::Box::new(gtk::Orientation::Vertical, 0);
    attachment.set_size_request(350, 100);
    widgets.media.append(&attachment);
    widgets.media.set_visible(true);
    settle();
    assert!(widgets.media.width() >= 350);
    widgets.media.set_visible(false);
    widgets.header.set_visible(false);
    settle();
    assert!(
        widgets.bubble.width() < 150,
        "rebound bubble did not shrink"
    );
    window.close();
}
