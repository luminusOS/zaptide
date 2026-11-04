use super::*;
use crate::model::CallOutcome;
use std::time::{Duration, Instant};

fn call_bubble(outgoing: bool, outcome: CallOutcome) -> NativeMediaWidget {
    build_media_widget_with_action(
        &super::tests::message(Content::Call {
            video: true,
            outgoing,
            outcome,
            seconds: None,
        }),
        |_| {},
    )
}

fn first_image(widget: &gtk::Widget) -> Option<gtk::Image> {
    if let Some(image) = widget.downcast_ref::<gtk::Image>() {
        return Some(image.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(image) = first_image(&current) {
            return Some(image);
        }
        child = current.next_sibling();
    }
    None
}

fn straight_pixels(image: &gtk::Image) -> Vec<[u8; 4]> {
    let paintable = gtk::WidgetPaintable::new(Some(image));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        &snapshot,
        f64::from(image.width()),
        f64::from(image.height()),
    );
    let node = snapshot.to_node().expect("render node");
    let texture = image
        .native()
        .and_then(|native| native.renderer())
        .expect("renderer")
        .render_texture(&node, None);
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, _) = downloader.download_bytes();
    bytes.as_chunks::<4>().0.to_vec()
}

fn settle() {
    let context = glib::MainContext::default();
    let end = Instant::now() + Duration::from_millis(300);
    while Instant::now() < end {
        context.iteration(false);
    }
}

#[test]
#[ignore = "needs a display"]
fn the_missed_call_icon_renders_in_one_solid_error_colour() {
    gtk::init().expect("display");
    adw::init().expect("libadwaita");
    let css = gtk::CssProvider::new();
    css.load_from_string(include_str!("../../data/style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let ended = call_bubble(true, CallOutcome::Completed);
    assert!(
        !first_image(ended.widget.upcast_ref())
            .expect("icon")
            .has_css_class("zaptide-call-missed-icon")
    );
    let bubble = call_bubble(false, CallOutcome::Missed);
    let window = gtk::Window::new();
    window.set_child(Some(&bubble.widget));
    window.present();
    let image = first_image(bubble.widget.upcast_ref()).expect("icon");
    assert!(image.has_css_class("zaptide-call-missed-icon"));
    image.set_pixel_size(96);
    for scheme in [adw::ColorScheme::ForceLight, adw::ColorScheme::ForceDark] {
        adw::StyleManager::default().set_color_scheme(scheme);
        settle();
        let color = image.color();
        assert!(
            color.red() > color.green() && color.red() > color.blue(),
            "{scheme:?}: the icon colour {color:?} is not red"
        );
        let expected = [color.red(), color.green(), color.blue()]
            .map(|c| (c * 255.0).round().clamp(0.0, 255.0));
        let pixels = straight_pixels(&image);
        let drawn: Vec<_> = pixels.iter().filter(|pixel| pixel[3] >= 64).collect();
        assert!(!drawn.is_empty(), "{scheme:?}: nothing was drawn");
        for pixel in &drawn {
            for (channel, want) in pixel.iter().zip(expected) {
                assert!(
                    (f32::from(*channel) - want).abs() <= 4.0,
                    "{scheme:?}: pixel {pixel:?} differs from {expected:?}"
                );
            }
        }
        let solid = pixels.iter().filter(|pixel| pixel[3] >= 250).count();
        let faint = pixels
            .iter()
            .filter(|pixel| (8..250).contains(&pixel[3]))
            .count();
        assert!(
            faint * 6 < solid,
            "{scheme:?}: {faint} translucent pixels against {solid} solid ones"
        );
    }
    window.destroy();
}
