use super::*;

#[test]
fn the_counter_shows_only_for_several_items() {
    assert_eq!(counter_text(0, 1), None);
    assert_eq!(counter_text(0, 0), None);
    assert_eq!(counter_text(0, 2).as_deref(), Some("1 of 2"));
    assert_eq!(counter_text(11, 12).as_deref(), Some("12 of 12"));
}

#[test]
fn zoom_keys_ignore_other_chords() {
    let none = gdk::ModifierType::empty();
    let ctrl = gdk::ModifierType::CONTROL_MASK;
    for (key, state, command) in [
        (gdk::Key::plus, ctrl, ZoomKey::In),
        (gdk::Key::equal, ctrl, ZoomKey::In),
        (gdk::Key::KP_Add, none, ZoomKey::In),
        (
            gdk::Key::plus,
            ctrl | gdk::ModifierType::SHIFT_MASK,
            ZoomKey::In,
        ),
        (gdk::Key::minus, ctrl, ZoomKey::Out),
        (gdk::Key::KP_Subtract, ctrl, ZoomKey::Out),
        (gdk::Key::_0, ctrl, ZoomKey::Fit),
        (gdk::Key::KP_0, none, ZoomKey::Fit),
        (gdk::Key::_1, none, ZoomKey::Actual),
        (gdk::Key::_1, ctrl, ZoomKey::Actual),
    ] {
        assert_eq!(zoom_key(key, state), Some(command), "{key:?}");
    }
    assert_eq!(zoom_key(gdk::Key::_1, gdk::ModifierType::ALT_MASK), None);
    assert_eq!(zoom_key(gdk::Key::Left, ctrl), None);
    assert_eq!(zoom_key(gdk::Key::c, ctrl), None);
}

#[test]
fn arrows_pan_only_when_bare() {
    let none = gdk::ModifierType::empty();
    assert_eq!(pan_key(gdk::Key::Left, none), Some((-1.0, 0.0)));
    assert_eq!(pan_key(gdk::Key::Down, none), Some((0.0, 1.0)));
    assert_eq!(pan_key(gdk::Key::Right, gdk::ModifierType::ALT_MASK), None);
    assert_eq!(pan_key(gdk::Key::Up, gdk::ModifierType::CONTROL_MASK), None);
    assert_eq!(pan_key(gdk::Key::a, none), None);
}

#[test]
fn the_zoom_level_has_an_accessible_name() {
    assert_eq!(zoom_level_text(120), "Zoom level 120 percent");
}

#[test]
fn video_clock_and_volume_icons() {
    assert_eq!(clock_text(-5), "0:00");
    assert_eq!(clock_text(3_400_000), "0:03");
    assert_eq!(clock_text(125_000_000), "2:05");
    assert_eq!(volume_icon(0.0, false), "audio-volume-muted-symbolic");
    assert_eq!(volume_icon(1.0, true), "audio-volume-muted-symbolic");
    assert_eq!(volume_icon(0.2, false), "audio-volume-low-symbolic");
    assert_eq!(volume_icon(0.5, false), "audio-volume-medium-symbolic");
    assert_eq!(volume_icon(1.0, false), "audio-volume-high-symbolic");
}

fn descendants(root: &gtk::Widget, found: &mut Vec<gtk::Widget>) {
    let mut child = root.first_child();
    while let Some(widget) = child {
        found.push(widget.clone());
        descendants(&widget, found);
        child = widget.next_sibling();
    }
}

fn tree(window: &gtk::Window) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    descendants(window.upcast_ref(), &mut found);
    found
}

fn button(window: &gtk::Window, tooltip: &str) -> gtk::Button {
    tree(window)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.tooltip_text().as_deref() == Some(tooltip))
        .unwrap_or_else(|| panic!("no button {tooltip}"))
}

fn has_button(window: &gtk::Window, tooltip: &str) -> bool {
    tree(window).into_iter().any(|widget| {
        widget
            .downcast::<gtk::Button>()
            .is_ok_and(|button| button.tooltip_text().as_deref() == Some(tooltip))
    })
}

fn zoom_percent(window: &gtk::Window) -> String {
    tree(window)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .filter(|button| {
            matches!(
                button.tooltip_text().as_deref(),
                Some("Actual Size" | "Fit to Window")
            )
        })
        .find_map(|button| button.child().and_downcast::<gtk::Label>())
        .expect("zoom level")
        .label()
        .to_string()
}

fn label(window: &gtk::Window, class: &str) -> gtk::Label {
    tree(window)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
        .find(|label| label.has_css_class(class))
        .unwrap_or_else(|| panic!("no label {class}"))
}

fn screenshot(window: &impl IsA<gtk::Widget>, name: &str) {
    let Some(directory) = std::env::var_os("ZAPTIDE_VIEWER_SHOTS") else {
        return;
    };
    super::super::playback_tests::drain();
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        snapshot.upcast_ref::<gdk::Snapshot>(),
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let node = snapshot.to_node().expect("render node");
    let texture = window
        .native()
        .and_then(|native| native.renderer())
        .expect("renderer")
        .render_texture(&node, None);
    let path = std::path::Path::new(&directory).join(format!("{name}.png"));
    texture.save_to_png(path).expect("screenshot");
}

fn fixture_message(
    directory: &std::path::Path,
    name: &str,
    width: u32,
    height: u32,
) -> crate::model::Message {
    let path = directory.join(name);
    image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x * 255 / width) as u8, (y * 255 / height) as u8, 160, 255])
    })
    .save(&path)
    .unwrap();
    let mut thumbnail = Vec::new();
    image::RgbaImage::from_fn(64, 40, |x, y| {
        image::Rgba([(x * 4) as u8, (y * 6) as u8, 160, 255])
    })
    .write_to(
        &mut std::io::Cursor::new(&mut thumbnail),
        image::ImageFormat::Png,
    )
    .unwrap();
    let mut message = crate::native_media_widgets::tests::message(Content::Image {
        caption: Some("Quarterly roadmap".into()),
        media: crate::model::Media {
            mime: "image/png".into(),
            size: 184 * 1024,
            width: Some(width),
            height: Some(height),
            path: Some(path),
            state: crate::model::MediaState::Idle,
        },
    });
    message.sender_name = Some("Marina Costa".into());
    message.thumbnail = Some(thumbnail);
    message
}

fn init_display() -> tokio::runtime::Runtime {
    gtk::init().expect("display");
    adw::init().expect("libadwaita");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../../../data/style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    runtime
}

fn photo_item(message: &Message) -> PhotoItem {
    PhotoItem {
        path: match attachment_action(message) {
            Some(NativeMediaAction::Open(path)) => path,
            _ => panic!("fixture is on disk"),
        },
        details: ViewerDetails::of(message),
    }
}

#[test]
#[ignore = "needs a display and glycin loaders"]
fn long_captions_keep_the_photo_visible_and_open_the_complete_text() {
    use super::super::playback_tests::{drain, wait_until};

    let runtime = init_display();
    let _guard = runtime.enter();
    let directory = tempfile::tempdir().unwrap();
    let mut message = fixture_message(directory.path(), "caption.png", 1200, 1600);
    let text = (1..=40)
        .map(|number| format!("Synthetic paragraph {number}: a long description of the sample photograph with enough text to wrap across several lines."))
        .collect::<Vec<_>>()
        .join("\n\n");
    if let Content::Image { caption, .. } = &mut message.content {
        *caption = Some(text.clone());
    }
    let long = photo_item(&message);
    if let Content::Image { caption, .. } = &mut message.content {
        *caption = Some("Short caption".into());
    }
    let short = photo_item(&message);
    if let Content::Image { caption, .. } = &mut message.content {
        *caption = None;
    }
    let empty = photo_item(&message);
    let window = adw::Window::builder()
        .default_width(960)
        .default_height(640)
        .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
        .build();
    window.present();
    drain();
    let window_ref = window.upcast_ref::<gtk::Window>();
    show_photo(
        &window,
        Rc::new(vec![long, short, empty]),
        0,
        Rc::new(|_| {}),
    );
    wait_until("photo did not load", || {
        button(window_ref, "Copy Image").is_sensitive()
    });
    drain();
    let footer = tree(window_ref)
        .into_iter()
        .find(|widget| widget.has_css_class("zaptide-viewer-caption"))
        .unwrap();
    assert!(
        footer.height() <= 120,
        "caption must not displace the photo: {} px",
        footer.height()
    );
    let preview = label(window_ref, "zaptide-viewer-caption-preview");
    assert!(!preview.label().contains('\n'));
    assert!(preview.layout().is_ellipsized());
    let open = tree(window_ref)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::MenuButton>().ok())
        .find(|button| button.label().as_deref() == Some("View Caption"))
        .expect("caption disclosure");
    let popover = open.popover().unwrap();
    let scroller = popover
        .child()
        .and_downcast::<gtk::ScrolledWindow>()
        .unwrap();
    let full = label(window_ref, "zaptide-viewer-caption-full");
    for width in [960, 420, 350] {
        window.set_default_size(width, 640);
        drain();
        assert!(footer.height() <= 120, "compact footer at {width} px");
        assert!(open.activate(), "caption opens from keyboard activation");
        wait_until("caption did not open", || popover.is_mapped());
        drain();
        assert_eq!(full.label().as_str(), text);
        assert!(full.is_selectable());
        assert!(
            full.selection_bounds().is_none(),
            "opening the caption must not select its text at {width} px"
        );
        assert!(
            scroller.height() <= 240,
            "full caption has a bounded viewport"
        );
        assert!(
            popover.width() <= window.width(),
            "caption fits at {width} px"
        );
        let scroll = scroller.vadjustment();
        assert!(scroll.upper() > scroll.page_size());
        scroll.set_value(scroll.upper() - scroll.page_size());
        drain();
        assert!(scroll.value() > 0.0, "last paragraph can be reached");
        screenshot(&popover, &format!("caption-expanded-{width}"));
        full.select_region(0, 19);
        assert_eq!(full.selection_bounds(), Some((0, 19)));
        full.emit_by_name::<()>("copy-clipboard", &[]);
        let copied = Rc::new(RefCell::new(None));
        full.clipboard()
            .read_text_async(gtk::gio::Cancellable::NONE, {
                let copied = copied.clone();
                move |result| *copied.borrow_mut() = Some(result.unwrap())
            });
        wait_until("selected caption was not copied", || {
            copied.borrow().is_some()
        });
        assert_eq!(
            copied.borrow().as_ref().unwrap().as_deref(),
            Some("Synthetic paragraph")
        );
        open.popdown();
        drain();
        open.popup();
        drain();
        assert!(
            full.selection_bounds().is_none(),
            "reopening clears the previous selection"
        );
        open.popdown();
        drain();
        screenshot(window_ref, &format!("caption-collapsed-{width}"));
    }
    open.popup();
    drain();
    button(window_ref, "Next Photo").emit_clicked();
    drain();
    assert!(
        !popover.is_visible(),
        "album navigation closes the old caption"
    );
    assert_eq!(preview.label(), "Short caption");
    assert_eq!(full.label(), "Short caption");
    open.popup();
    drain();
    assert_eq!(scroller.vadjustment().value(), 0.0);
    open.popdown();
    button(window_ref, "Next Photo").emit_clicked();
    drain();
    assert!(!open.is_visible());
    assert!(!preview.is_visible());
    assert!(label(window_ref, "zaptide-viewer-metadata").is_mapped());
    window.close();
}

#[test]
#[ignore = "requires a graphical display and glycin loaders"]
fn copy_image_places_a_texture_on_the_clipboard_without_opening_viewer() {
    use super::super::playback_tests::wait_until;

    let runtime = init_display();
    let _guard = runtime.enter();
    let directory = tempfile::tempdir().unwrap();
    let message = fixture_message(directory.path(), "clipboard.png", 32, 24);
    let path = photo_item(&message).path;
    let clipboard = gdk::Display::default().unwrap().clipboard();
    super::super::copy_image(path.clone(), &clipboard);
    wait_until("image decode did not finish", || {
        !crate::media_retention::is_held(&path)
    });
    let result = Rc::new(RefCell::new(None));
    let received = result.clone();
    clipboard.read_texture_async(None::<&gtk::gio::Cancellable>, move |texture| {
        *received.borrow_mut() = Some(texture.unwrap());
    });
    wait_until("clipboard read did not finish", || {
        result.borrow().is_some()
    });
    let texture = result
        .borrow_mut()
        .take()
        .unwrap()
        .expect("clipboard texture");
    assert_eq!((texture.width(), texture.height()), (32, 24));
}

#[test]
#[ignore = "requires a graphical display and GStreamer plugins"]
fn closing_video_stops_playback_and_releases_file() {
    use super::super::playback_tests::{drain, wait_until};

    let runtime = init_display();
    let _guard = runtime.enter();
    let directory = tempfile::tempdir().unwrap();
    let video = directory.path().join("clip.webm");
    let status = std::process::Command::new("gst-launch-1.0")
        .args("-q videotestsrc num-buffers=90 ! video/x-raw,width=640,height=360,framerate=30/1 ! vp8enc deadline=1 ! webmmux ! filesink".split_whitespace())
        .arg(format!("location={}", video.display()))
        .status()
        .unwrap();
    assert!(status.success());
    let message = fixture_message(directory.path(), "image.png", 32, 32);
    let details = ViewerDetails::of(&message);
    let window = adw::Window::builder()
        .default_width(800)
        .default_height(600)
        .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
        .build();
    window.present();
    drain();
    let window_ref = window.upcast_ref::<gtk::Window>();
    show_video(&window, &video, &details, Rc::new(|_| {}));
    wait_until("video controls did not appear", || {
        has_button(window_ref, "Pause")
    });
    assert!(crate::media_retention::is_held(&video));
    let clip = playing_clip(window_ref).expect("video stream");
    let released = clip.downgrade();
    button(window_ref, "Back").emit_clicked();
    drain();
    assert!(!clip.is_prepared(), "closing releases playback");
    assert!(!crate::media_retention::is_held(&video));
    drop(clip);
    wait_until("closed viewer kept its video stream", || {
        released.upgrade().is_none()
    });
    window.close();
}

fn open_photo_state() -> Rc<PhotoState> {
    OPEN_PHOTO.with(|open| open.borrow().clone().expect("a photo viewer was opened"))
}

fn controller<T: IsA<gtk::EventController> + IsA<glib::Object>>(
    widget: &impl IsA<gtk::Widget>,
) -> T {
    let controllers = widget.observe_controllers();
    (0..controllers.n_items())
        .filter_map(|index| controllers.item(index))
        .find_map(|controller| controller.downcast::<T>().ok())
        .expect("controller")
}

struct Opening {
    returned: std::time::Duration,
    first_frame: Option<std::time::Duration>,
    settled: std::time::Duration,
    worst_stall: std::time::Duration,
}

/// Runs `open`, then pumps the main loop until `settled` holds plus a
/// moment more, recording how long the call blocked, when the viewer
/// first painted, when it settled, and the longest gap in which the
/// main loop did not turn.
fn measure_opening(
    window: &adw::Window,
    open: impl FnOnce(),
    settled: impl Fn() -> bool,
) -> Opening {
    use std::time::{Duration, Instant};
    let window_ref = window.upcast_ref::<gtk::Window>();
    let started = Instant::now();
    let last = Rc::new(Cell::new(started));
    let worst = Rc::new(Cell::new(Duration::ZERO));
    let first_frame = Rc::new(Cell::new(None));
    let source = glib::timeout_add_local(Duration::from_millis(2), {
        let (last, worst) = (last.clone(), worst.clone());
        move || {
            let now = Instant::now();
            let gap = now - last.replace(now);
            worst.set(worst.get().max(gap));
            glib::ControlFlow::Continue
        }
    });
    let clock = window.frame_clock().expect("frame clock");
    let painted = clock.connect_after_paint({
        let (first_frame, window) = (first_frame.clone(), window_ref.clone());
        move |_| {
            if first_frame.get().is_none() && has_button(&window, "Back") {
                first_frame.set(Some(started.elapsed()));
            }
        }
    });
    open();
    let returned = started.elapsed();
    super::super::playback_tests::wait_until("viewer did not settle", &settled);
    let settled_at = started.elapsed();
    let tail = Instant::now() + Duration::from_millis(300);
    super::super::playback_tests::wait_until("tail", || Instant::now() >= tail);
    source.remove();
    clock.disconnect(painted);
    Opening {
        returned,
        first_frame: first_frame.get(),
        settled: settled_at,
        worst_stall: worst.get(),
    }
}

fn report(label: &str, opening: &Opening) {
    let ms = |duration: std::time::Duration| duration.as_secs_f64() * 1000.0;
    eprintln!(
        "OPEN {label}: call returns {:.1} ms, first viewer frame {} ms, ready {:.1} ms, worst main-loop stall {:.1} ms",
        ms(opening.returned),
        opening
            .first_frame
            .map_or("never".to_owned(), |d| format!("{:.1}", ms(d))),
        ms(opening.settled),
        ms(opening.worst_stall),
    );
}

fn generate(directory: &std::path::Path, name: &str, pipeline: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    let status = std::process::Command::new("gst-launch-1.0")
        .arg("-q")
        .args(pipeline.split_whitespace())
        .arg(format!("location={}", path.display()))
        .status()
        .expect("GStreamer fixture generator");
    assert!(status.success(), "{pipeline}");
    path
}

fn media_message(
    content: impl FnOnce(crate::model::Media) -> Content,
    path: &std::path::Path,
    mime: &str,
    (width, height): (u32, u32),
) -> Message {
    let mut message = fixture_message(path.parent().unwrap(), "thumb.png", 64, 40);
    message.content = content(crate::model::Media {
        mime: mime.into(),
        size: std::fs::metadata(path).unwrap().len(),
        width: Some(width),
        height: Some(height),
        path: Some(path.to_path_buf()),
        state: crate::model::MediaState::Idle,
    });
    message
}

/// Needs a display, glycin loaders, bubblewrap and GStreamer's
/// openh264 plugins; prints the timings it measures:
/// `cargo test --lib opening_media -- --ignored --nocapture`.
#[test]
#[ignore = "needs a display, glycin loaders and GStreamer H.264 plugins"]
fn opening_media_keeps_the_main_loop_turning() {
    use super::super::playback_tests::{drain, wait_until};
    let runtime = init_display();
    let _runtime = runtime.enter();
    let directory = tempfile::tempdir().unwrap();
    let big_jpeg = generate(
        directory.path(),
        "big.jpg",
        "videotestsrc num-buffers=1 pattern=circular ! video/x-raw,width=6000,height=4000 ! jpegenc quality=90 ! filesink",
    );
    let big_png = generate(
        directory.path(),
        "big.png",
        "videotestsrc num-buffers=1 pattern=circular ! video/x-raw,width=6000,height=4000 ! pngenc compression-level=1 ! filesink",
    );
    let phone_jpeg = generate(
        directory.path(),
        "phone.jpg",
        "videotestsrc num-buffers=1 pattern=circular ! video/x-raw,width=1197,height=1229 ! jpegenc quality=90 ! filesink",
    );
    let clip = generate(
        directory.path(),
        "clip.mp4",
        "videotestsrc num-buffers=90 pattern=ball ! video/x-raw,width=1920,height=1080,framerate=30/1 ! videoconvert ! openh264enc ! h264parse ! mp4mux ! filesink",
    );
    let window = adw::Window::builder()
        .default_width(1100)
        .default_height(760)
        .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
        .build();
    window.present();
    drain();
    let window_ref = window.upcast_ref::<gtk::Window>();
    let on_action: Rc<dyn Fn(NativeMediaAction)> = Rc::new(|_| {});

    for (label, path, mime, size) in [
        (
            "phone JPEG 1197x1229",
            &phone_jpeg,
            "image/jpeg",
            (1197, 1229),
        ),
        ("JPEG 6000x4000", &big_jpeg, "image/jpeg", (6000, 4000)),
        ("PNG 6000x4000", &big_png, "image/png", (6000, 4000)),
    ] {
        let message = media_message(
            |media| Content::Image {
                caption: None,
                media,
            },
            path,
            mime,
            size,
        );
        let item = PhotoItem {
            path: path.clone(),
            details: ViewerDetails::of(&message),
        };
        for attempt in ["cold", "warm"] {
            let items = Rc::new(vec![item.clone()]);
            let opening = measure_opening(
                &window,
                || show_photo(&window, items, 0, on_action.clone()),
                || button(window_ref, "Copy Image").is_sensitive(),
            );
            report(&format!("{label} {attempt}"), &opening);
            assert!(
                opening.returned < std::time::Duration::from_millis(100),
                "{label} {attempt} blocked the caller"
            );
            assert!(
                opening
                    .first_frame
                    .is_some_and(|frame| frame < std::time::Duration::from_millis(250)),
                "{label} {attempt} was late to appear"
            );
            button(window_ref, "Back").emit_clicked();
            wait_until("viewer did not close", || !has_button(window_ref, "Back"));
            drain();
        }
    }

    let message = media_message(
        |media| Content::Video {
            caption: None,
            media,
            seconds: Some(3),
            gif: false,
        },
        &clip,
        "video/mp4",
        (1920, 1080),
    );
    let details = ViewerDetails::of(&message);
    for attempt in ["cold", "warm"] {
        let opening = measure_opening(
            &window,
            || show_video(&window, &clip, &details, on_action.clone()),
            || has_button(window_ref, "Pause"),
        );
        report(&format!("video H.264 1920x1080 {attempt}"), &opening);
        assert!(
            opening.returned < std::time::Duration::from_millis(100),
            "video {attempt} blocked the caller"
        );
        button(window_ref, "Back").emit_clicked();
        wait_until("viewer did not close", || !has_button(window_ref, "Back"));
        drain();
    }
    window.destroy();
    drain();
    wait_until("quiet", || true);
}

/// Needs a display and glycin's loaders:
/// `cargo test --lib photos_zoom_freely -- --ignored`. Setting
/// `ZAPTIDE_VIEWER_SHOTS` also saves screenshots of each state.
#[test]
#[ignore = "needs a display and glycin loaders"]
fn photos_zoom_freely_about_the_pointer() {
    use super::super::playback_tests::{drain, wait_until};
    let none = gdk::ModifierType::empty();
    let ctrl = gdk::ModifierType::CONTROL_MASK;
    let runtime = init_display();
    let _runtime = runtime.enter();
    let directory = tempfile::tempdir().unwrap();
    let first = fixture_message(directory.path(), "one.png", 3000, 2000);
    let second = fixture_message(directory.path(), "two.png", 800, 600);
    let window = adw::Window::builder()
        .default_width(1100)
        .default_height(760)
        .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
        .build();
    window.present();
    drain();
    let window_ref = window.upcast_ref::<gtk::Window>();
    let on_action: Rc<dyn Fn(NativeMediaAction)> = Rc::new(|_| {});
    let album = Rc::new(vec![photo_item(&first), photo_item(&second)]);

    show_photo(&window, album, 0, on_action);
    let photo = open_photo_state();
    wait_until("photo did not load", || photo.ready.get());
    drain();
    let scroller = photo.scroller.upgrade().unwrap();
    let view = photo.view_size();
    let fit = (view.0 / 3000.0).min(view.1 / 2000.0);
    assert!((photo.canvas.scale() - fit).abs() < 1e-6);
    assert_eq!(zoom_percent(window_ref), format!("{}%", zoom::percent(fit)));
    assert_eq!(photo.canvas.level(), Level::Fit);
    assert!(
        photo.texture_size().0 < 3000,
        "the first decode fits the view"
    );
    assert!(button(window_ref, "Zoom Out").is_sensitive());
    assert!(button(window_ref, "Zoom In").is_sensitive());
    screenshot(window_ref, "zoom-fit");

    let pointer = (view.0 * 0.55, view.1 * 0.45);
    let on_screen = |photo: &PhotoState, point: (f64, f64)| {
        let scale = photo.canvas.scale();
        let view = photo.view_size();
        let picture = photo.canvas.picture();
        let (across, down) = (
            photo.scroller.upgrade().unwrap().hadjustment().value(),
            photo.scroller.upgrade().unwrap().vadjustment().value(),
        );
        (
            point.0 * scale + zoom::origin(picture.0, view.0, scale) - across,
            point.1 * scale + zoom::origin(picture.1, view.1, scale) - down,
        )
    };
    let under_pointer = |photo: &PhotoState| {
        let scale = photo.canvas.scale();
        let view = photo.view_size();
        let picture = photo.canvas.picture();
        let scroller = photo.scroller.upgrade().unwrap();
        (
            (scroller.hadjustment().value() + pointer.0 - zoom::origin(picture.0, view.0, scale))
                / scale,
            (scroller.vadjustment().value() + pointer.1 - zoom::origin(picture.1, view.1, scale))
                / scale,
        )
    };
    let anchored = |photo: &PhotoState, point: (f64, f64)| {
        let shown = on_screen(photo, point);
        (shown.0 - pointer.0).abs() < 1.5 && (shown.1 - pointer.1).abs() < 1.5
    };

    assert_eq!(
        photo.on_scroll(none, -1.0, Some(pointer)),
        glib::Propagation::Proceed,
        "the wheel alone is left to the scrolled window"
    );
    assert_eq!(photo.canvas.level(), Level::Fit);
    assert_eq!(
        photo.on_scroll(ctrl, -1.0, Some(pointer)),
        glib::Propagation::Stop
    );
    drain();
    assert!((photo.canvas.scale() - fit * 1.1).abs() < 1e-6);
    assert_eq!(
        zoom_percent(window_ref),
        format!("{}%", zoom::percent(fit * 1.1))
    );
    for step in 0..6 {
        let point = under_pointer(&photo);
        photo.on_scroll(ctrl, -1.0, Some(pointer));
        drain();
        assert!(
            anchored(&photo, point),
            "the point under the pointer moved on step {step}"
        );
    }

    for _ in 0..80 {
        photo.on_scroll(ctrl, -1.0, Some(pointer));
    }
    drain();
    assert_eq!(photo.canvas.scale(), 8.0);
    assert_eq!(zoom_percent(window_ref), "800%");
    assert!(!button(window_ref, "Zoom In").is_sensitive());
    assert!(button(window_ref, "Zoom Out").is_sensitive());
    wait_until("the full photo did not load for the zoom", || {
        photo.texture_size().0 == 3000
    });
    screenshot(window_ref, "zoom-800");
    let before = (
        scroller.hadjustment().value(),
        scroller.vadjustment().value(),
    );
    let drag: gtk::GestureDrag = controller(&scroller);
    drag.emit_by_name::<()>("drag-begin", &[&10.0f64, &10.0f64]);
    drag.emit_by_name::<()>("drag-update", &[&-120.0f64, &-60.0f64]);
    drag.emit_by_name::<()>("drag-end", &[&-120.0f64, &-60.0f64]);
    assert!((scroller.hadjustment().value() - before.0 - 120.0).abs() < 1.0);
    assert!((scroller.vadjustment().value() - before.1 - 60.0).abs() < 1.0);

    for _ in 0..400 {
        photo.on_scroll(ctrl, 1.0, Some(pointer));
    }
    drain();
    let smallest = fit / 4.0;
    assert!((photo.canvas.scale() - smallest).abs() < 1e-6);
    assert!(!button(window_ref, "Zoom Out").is_sensitive());
    assert!(button(window_ref, "Zoom In").is_sensitive());
    screenshot(window_ref, "zoom-out-past-fit");

    press(window_ref, gdk::Key::_1, ctrl);
    assert_eq!(zoom_percent(window_ref), "100%");
    press(window_ref, gdk::Key::minus, ctrl);
    assert_eq!(zoom_percent(window_ref), "80%");
    press(
        window_ref,
        gdk::Key::plus,
        ctrl | gdk::ModifierType::SHIFT_MASK,
    );
    press(window_ref, gdk::Key::equal, ctrl);
    assert_eq!(zoom_percent(window_ref), "125%");
    button(window_ref, "Zoom Out").emit_clicked();
    assert_eq!(zoom_percent(window_ref), "100%");
    press(window_ref, gdk::Key::_0, ctrl);
    assert_eq!(photo.canvas.level(), Level::Fit);
    assert!(has_button(window_ref, "Actual Size"));
    button(window_ref, "Actual Size").emit_clicked();
    assert_eq!(zoom_percent(window_ref), "100%");
    button(window_ref, "Fit to Window").emit_clicked();
    assert_eq!(photo.canvas.level(), Level::Fit);

    let click: gtk::GestureClick = controller(&scroller);
    let point = under_pointer(&photo);
    click.emit_by_name::<()>("pressed", &[&2i32, &pointer.0, &pointer.1]);
    drain();
    assert_eq!(photo.canvas.level(), Level::Scale(1.0));
    assert!(
        anchored(&photo, point),
        "double click did not keep the anchor"
    );
    click.emit_by_name::<()>("pressed", &[&2i32, &pointer.0, &pointer.1]);
    assert_eq!(photo.canvas.level(), Level::Fit);

    press(window_ref, gdk::Key::plus, ctrl);
    assert_ne!(photo.canvas.level(), Level::Fit);
    button(window_ref, "Next Photo").emit_clicked();
    assert_eq!(
        photo.canvas.level(),
        Level::Fit,
        "zoom resets for the next photo"
    );
    assert!(!button(window_ref, "Zoom In").is_sensitive());
    wait_until("second photo did not load", || photo.ready.get());
    drain();
    assert!(button(window_ref, "Zoom In").is_sensitive());

    let narrow = adw::Window::builder()
        .default_width(360)
        .default_height(700)
        .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
        .build();
    narrow.present();
    drain();
    let narrow_ref = narrow.upcast_ref::<gtk::Window>();
    show_photo(
        &narrow,
        Rc::new(vec![photo_item(&first), photo_item(&second)]),
        0,
        Rc::new(|_| {}),
    );
    let narrow_photo = open_photo_state();
    wait_until("narrow photo did not load", || narrow_photo.ready.get());
    drain();
    screenshot(narrow_ref, "zoom-narrow");
    let header = tree(narrow_ref)
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::HeaderBar>().ok())
        .expect("header");
    assert!(
        header.measure(gtk::Orientation::Horizontal, -1).0 <= narrow_ref.width(),
        "the header fits a narrow window"
    );
    let level = button(narrow_ref, "Actual Size");
    let close = button(narrow_ref, "Back");
    for widget in [level.upcast_ref::<gtk::Widget>(), close.upcast_ref()] {
        let bounds = widget
            .compute_bounds(narrow_ref)
            .expect("bounds in the window");
        assert!(
            bounds.x() >= 0.0 && bounds.x() + bounds.width() <= narrow_ref.width() as f32,
            "{} is inside the narrow window",
            widget.type_().name()
        );
    }
    button(narrow_ref, "Back").emit_clicked();
    narrow.destroy();
    OPEN_PHOTO.with(|open| open.take());
    window.destroy();
    drain();
}

/// Needs a display and GStreamer's VP8 plugins:
/// `cargo test --lib videos_open_in -- --ignored`.
#[test]
#[ignore = "needs a display and GStreamer VP8 plugins"]
fn videos_open_in_the_background_with_a_poster() {
    use super::super::playback_tests::{drain, wait_until};
    let runtime = init_display();
    let _runtime = runtime.enter();
    let directory = tempfile::tempdir().unwrap();
    let clip = generate(
        directory.path(),
        "clip.webm",
        "videotestsrc num-buffers=90 ! video/x-raw,width=640,height=360,framerate=30/1 ! vp8enc deadline=1 ! webmmux ! filesink",
    );
    let message = media_message(
        |media| Content::Video {
            caption: None,
            media,
            seconds: Some(3),
            gif: false,
        },
        &clip,
        "video/webm",
        (640, 360),
    );
    let details = ViewerDetails::of(&message);
    let window = adw::Window::builder()
        .default_width(1100)
        .default_height(760)
        .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
        .build();
    window.present();
    drain();
    let window_ref = window.upcast_ref::<gtk::Window>();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let on_action: Rc<dyn Fn(NativeMediaAction)> = {
        let opened = opened.clone();
        Rc::new(move |action| {
            if let NativeMediaAction::Open(path) = action {
                opened.borrow_mut().push(path);
            }
        })
    };
    let spinner_shown = |window: &gtk::Window| {
        tree(window)
            .into_iter()
            .any(|widget| widget.is::<adw::Spinner>() && widget.is_mapped())
    };

    show_video(&window, &clip, &details, on_action.clone());
    assert!(has_button(window_ref, "Back"), "the viewer is up at once");
    assert!(
        !has_button(window_ref, "Pause"),
        "playing waits for the pipeline"
    );
    drain();
    wait_until("video controls did not appear", || {
        has_button(window_ref, "Pause")
    });
    wait_until("the poster stayed over the video", || {
        !spinner_shown(window_ref)
    });
    assert!(opened.borrow().is_empty());
    button(window_ref, "Back").emit_clicked();
    wait_until("viewer did not close", || !has_button(window_ref, "Back"));

    let broken = directory.path().join("broken.webm");
    std::fs::write(&broken, b"synthetic invalid video").unwrap();
    show_video(&window, &broken, &details, on_action.clone());
    wait_until("failure page did not show", || {
        failure_button(window_ref).is_some()
    });
    assert!(!spinner_shown(window_ref));
    button(window_ref, "Back").emit_clicked();
    wait_until("viewer did not close", || !has_button(window_ref, "Back"));

    show_video(&window, &clip, &details, on_action);
    button(window_ref, "Back").emit_clicked();
    wait_until("viewer did not close", || !has_button(window_ref, "Back"));
    drain();
    window.destroy();
    drain();
}

#[test]
fn stepping_stays_within_the_items() {
    assert_eq!(step_target(0, 1, 3), Some(1));
    assert_eq!(step_target(2, 1, 3), None);
    assert_eq!(step_target(0, -1, 3), None);
    assert_eq!(step_target(1, -1, 3), Some(0));
    assert_eq!(step_target(1, 0, 3), None);
    assert_eq!(step_target(0, 1, 0), None);
}

#[test]
fn arrow_keys_step_unless_a_modifier_is_held() {
    let none = gdk::ModifierType::empty();
    assert_eq!(key_step(gdk::Key::Left, none, false), Some(-1));
    assert_eq!(key_step(gdk::Key::Right, none, false), Some(1));
    assert_eq!(key_step(gdk::Key::Left, none, true), Some(1));
    assert_eq!(key_step(gdk::Key::Right, none, true), Some(-1));
    assert_eq!(key_step(gdk::Key::Up, none, false), None);
    for held in [
        gdk::ModifierType::ALT_MASK,
        gdk::ModifierType::CONTROL_MASK,
        gdk::ModifierType::SHIFT_MASK,
    ] {
        assert_eq!(key_step(gdk::Key::Right, held, false), None);
    }
    let locks = gdk::ModifierType::LOCK_MASK;
    assert_eq!(key_step(gdk::Key::Right, locks, false), Some(1));
}

#[test]
fn seeks_stay_within_the_video() {
    assert_eq!(seek_target(-4.0, 30_000_000), 0);
    assert_eq!(seek_target(12_000_000.0, 30_000_000), 12_000_000);
    assert_eq!(seek_target(99_000_000.0, 30_000_000), 30_000_000);
    assert_eq!(seek_target(5.0, 0), 0);
}

#[test]
fn accessible_names_say_whose_media_it_is() {
    assert_eq!(photo_label("Marina"), "Photo from Marina");
    assert_eq!(video_label("Marina"), "Video from Marina");
    assert_eq!(of_text("0:12", "0:30"), "0:12 of 0:30");
}

fn dialog_keys(window: &gtk::Window) -> gtk::EventControllerKey {
    let dialog = tree(window)
        .into_iter()
        .find(|widget| widget.widget_name() == "zaptide-media-viewer")
        .expect("viewer page");
    let controllers = dialog.observe_controllers();
    (0..controllers.n_items())
        .filter_map(|index| controllers.item(index))
        .find_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
        .expect("viewer key controller")
}

fn press(window: &gtk::Window, key: gdk::Key, modifiers: gdk::ModifierType) {
    use gtk::glib::translate::IntoGlib;
    dialog_keys(window).emit_by_name::<bool>("key-pressed", &[&key.into_glib(), &0u32, &modifiers]);
}

fn playing_clip(window: &gtk::Window) -> Option<VideoStream> {
    tree(window).into_iter().find_map(|widget| {
        widget
            .downcast::<gtk::Picture>()
            .ok()?
            .paintable()
            .and_downcast::<VideoStream>()
    })
}

fn failure_button(window: &gtk::Window) -> Option<gtk::Button> {
    tree(window)
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| {
            button.label().as_deref() == Some("Open in Another App") && button.is_mapped()
        })
}

/// Needs a display, and runs alone because GTK initialises on one thread:
/// `cargo test --lib viewer_steps -- --ignored`. Setting
/// `ZAPTIDE_VIEWER_SHOTS=/tmp/opencode/shots` also saves screenshots of
/// each state.
#[test]
#[ignore = "needs a display and GStreamer VP8 plugins"]
fn viewer_steps_zooms_and_controls_video() {
    use super::super::playback_tests::{drain, wait_until};
    gtk::init().expect("display");
    adw::init().expect("libadwaita");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let _runtime = runtime.enter();
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("../../../data/style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    let directory = tempfile::tempdir().unwrap();
    let first = fixture_message(directory.path(), "one.png", 1600, 1000);
    let second = fixture_message(directory.path(), "two.png", 800, 600);
    let window = adw::Window::builder()
        .default_width(1100)
        .default_height(760)
        .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
        .build();
    window.present();
    drain();
    let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
    let on_action: Rc<dyn Fn(NativeMediaAction)> = {
        let opened = opened.clone();
        Rc::new(move |action| {
            if let NativeMediaAction::Open(path) = action {
                opened.borrow_mut().push(path);
            }
        })
    };
    let item = |message: &Message| PhotoItem {
        path: match attachment_action(message) {
            Some(NativeMediaAction::Open(path)) => path,
            _ => panic!("fixture is on disk"),
        },
        details: ViewerDetails::of(message),
    };
    let album = Rc::new(vec![item(&first), item(&second)]);
    let window_ref = window.upcast_ref::<gtk::Window>();

    show_photo(&window, album.clone(), 0, on_action.clone());
    assert!(
        album
            .iter()
            .all(|item| crate::media_retention::is_held(&item.path)),
        "photos on screen are kept from the cleanup"
    );
    assert!(
        !button(window_ref, "Copy Image").is_sensitive(),
        "copy waits for the full photo"
    );
    assert!(
        !button(window_ref, "Zoom In").is_sensitive(),
        "zoom waits for the full photo"
    );
    wait_until("photo did not load", || {
        button(window_ref, "Copy Image").is_sensitive()
    });
    drain();
    assert_eq!(label(window_ref, "zaptide-viewer-pill").label(), "1 of 2");
    assert!(label(window_ref, "zaptide-viewer-time").is_visible());
    assert_eq!(
        label(window_ref, "zaptide-viewer-name").label(),
        "Marina Costa"
    );
    assert_eq!(
        label(window_ref, "zaptide-viewer-metadata").label(),
        "PNG · 1600 × 1000 · 188.4\u{a0}kB"
    );
    assert!(button(window_ref, "Zoom Out").is_sensitive());
    assert!(button(window_ref, "Zoom In").is_sensitive());
    assert!(has_button(window_ref, "Actual Size"));
    assert!(!button(window_ref, "Previous Photo").is_sensitive());
    screenshot(window_ref, "13-view-image");
    press(window_ref, gdk::Key::_1, gdk::ModifierType::empty());
    assert_eq!(zoom_percent(window_ref), "100%");
    assert!(has_button(window_ref, "Fit to Window"));
    drain();
    screenshot(window_ref, "14-image-original-size");
    press(window_ref, gdk::Key::Right, gdk::ModifierType::empty());
    assert_eq!(
        label(window_ref, "zaptide-viewer-pill").label(),
        "1 of 2",
        "arrows scroll a photo at 100%"
    );
    press(window_ref, gdk::Key::_0, gdk::ModifierType::CONTROL_MASK);
    assert!(has_button(window_ref, "Actual Size"));
    press(window_ref, gdk::Key::Right, gdk::ModifierType::ALT_MASK);
    assert_eq!(label(window_ref, "zaptide-viewer-pill").label(), "1 of 2");
    press(window_ref, gdk::Key::Right, gdk::ModifierType::empty());
    assert_eq!(label(window_ref, "zaptide-viewer-pill").label(), "2 of 2");
    assert!(
        has_button(window_ref, "Actual Size"),
        "zoom resets for the next photo"
    );
    assert!(!button(window_ref, "Next Photo").is_sensitive());
    assert!(
        !button(window_ref, "Copy Image").is_sensitive(),
        "copy waits for the next photo"
    );
    wait_until("second photo did not load", || {
        button(window_ref, "Copy Image").is_sensitive()
    });
    press(window_ref, gdk::Key::Left, gdk::ModifierType::empty());
    assert_eq!(label(window_ref, "zaptide-viewer-pill").label(), "1 of 2");
    button(window_ref, "Next Photo").emit_clicked();
    button(window_ref, "Open With Another App").emit_clicked();
    assert_eq!(opened.borrow().len(), 1);
    drain();
    assert!(!has_button(window_ref, "Next Photo"), "viewer closed");
    assert!(
        album
            .iter()
            .all(|item| !crate::media_retention::is_held(&item.path)),
        "a closed viewer releases its photos"
    );

    let video = directory.path().join("clip.webm");
    let status = std::process::Command::new("gst-launch-1.0")
        .args("-q videotestsrc num-buffers=90 ! video/x-raw,width=640,height=360,framerate=30/1 ! vp8enc deadline=1 ! queue ! mux. audiotestsrc wave=silence num-buffers=140 ! audioconvert ! vorbisenc ! queue ! mux. webmmux name=mux ! filesink".split_whitespace())
        .arg(format!("location={}", video.display()))
        .status()
        .expect("GStreamer fixture generator");
    assert!(status.success());
    let mut message = first.clone();
    message.content = Content::Video {
        caption: None,
        media: crate::model::Media {
            mime: "video/mp4".into(),
            size: 1_258_291,
            width: Some(1280),
            height: Some(720),
            path: Some(video.clone()),
            state: crate::model::MediaState::Idle,
        },
        seconds: Some(3),
        gif: false,
    };
    let details = ViewerDetails::of(&message);
    show_video(&window, &video, &details, on_action.clone());
    wait_until("video controls did not appear", || {
        has_button(window_ref, "Pause")
    });
    assert!(!has_button(window_ref, "Copy Image"));
    assert!(!has_button(window_ref, "Zoom In"));
    assert_eq!(
        label(window_ref, "zaptide-viewer-metadata").label(),
        "MP4 · 1280 × 720 · 0:03 · 1.3\u{a0}MB"
    );
    let clip = playing_clip(window_ref).expect("video stream");
    let released = clip.downgrade();
    let seek = tree(window_ref)
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Scale>().ok())
        .filter(|scale| scale.has_css_class("zaptide-video-seek"))
        .expect("seek bar");
    assert!(seek.adjustment().step_increment() > 0.0);
    assert!(seek.adjustment().page_increment() > seek.adjustment().step_increment());
    screenshot(window_ref, "15-play-video");
    button(window_ref, "Pause").emit_clicked();
    wait_until("video did not pause", || has_button(window_ref, "Play"));
    drain();
    screenshot(window_ref, "16-video-paused");
    button(window_ref, "Fullscreen").emit_clicked();
    wait_until("window did not enter fullscreen", || window.is_fullscreen());
    assert!(has_button(window_ref, "Exit Fullscreen"));
    button(window_ref, "Back").emit_clicked();
    wait_until("closing the viewer left the window fullscreen", || {
        !window.is_fullscreen()
    });
    drain();
    assert!(!has_button(window_ref, "Back"), "viewer closed");
    assert!(!clip.is_prepared(), "closing releases the playback");
    drop(clip);
    wait_until("closed viewer kept its video stream", || {
        released.upgrade().is_none()
    });

    let broken = directory.path().join("broken.webm");
    std::fs::write(&broken, b"synthetic invalid video").unwrap();
    show_video(&window, &broken, &details, on_action.clone());
    wait_until("failure page did not show", || {
        failure_button(window_ref).is_some()
    });
    drain();
    screenshot(window_ref, "48-video-unavailable");
    let failed = playing_clip(window_ref).expect("failed stream").downgrade();
    let before = opened.borrow().len();
    failure_button(window_ref).unwrap().emit_clicked();
    assert_eq!(opened.borrow().len(), before + 1);
    wait_until("failed viewer kept its video stream", || {
        failed.upgrade().is_none()
    });

    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
    show_video(&window, &broken, &details, on_action.clone());
    wait_until("light failure page did not show", || {
        failure_button(window_ref).is_some()
    });
    drain();
    screenshot(window_ref, "48-video-unavailable-light");
    button(window_ref, "Back").emit_clicked();
    drain();
    show_video(&window, &video, &details, on_action.clone());
    wait_until("light video controls did not appear", || {
        has_button(window_ref, "Pause")
    });
    let volume = tree(window_ref)
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::MenuButton>().ok())
        .expect("volume button");
    volume.popup();
    drain();
    screenshot(
        &volume.popover().expect("volume popover"),
        "15-volume-popover-light",
    );
    button(window_ref, "Back").emit_clicked();
    drain();
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);

    let narrow = adw::Window::builder()
        .default_width(400)
        .default_height(700)
        .content(&gtk::Box::new(gtk::Orientation::Vertical, 0))
        .build();
    narrow.present();
    drain();
    let narrow_ref = narrow.upcast_ref::<gtk::Window>();
    show_photo(&narrow, album, 0, on_action.clone());
    wait_until("narrow photo did not load", || {
        button(narrow_ref, "Copy Image").is_sensitive()
    });
    drain();
    assert!(!label(narrow_ref, "zaptide-viewer-time").is_visible());
    screenshot(narrow_ref, "narrow-album");
    button(narrow_ref, "Back").emit_clicked();
    drain();
    show_video(&narrow, &video, &details, on_action.clone());
    wait_until("narrow video controls did not appear", || {
        has_button(narrow_ref, "Pause")
    });
    for _ in 0..4 {
        drain();
    }
    screenshot(narrow_ref, "narrow-video");
    button(narrow_ref, "Back").emit_clicked();
    drain();
    narrow.destroy();
    window.destroy();
    drain();
}
