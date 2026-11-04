use super::screen::with_waiting;
use super::tests::{connected, session};
use super::window::Look;
use super::*;
use crate::model::call::{CallDirection, CallOutcome, CallSignal};

fn look(allow_focus: bool) -> Look<'static> {
    Look {
        avatar: None,
        waiting_avatar: None,
        allow_focus,
    }
}

/// A switch keeps its widget, and so its keyboard focus, while the model
/// flips it; the default button is focused only when allowed.
fn controls_follow_the_model() {
    let mut window = CallWindow::new(std::rc::Rc::new(|_| {}));
    let voice = connected(CallDirection::Outgoing, false);
    window.render("Ana", &look(true), &screen(&voice, "Ana", 100));
    let mute = window.control(CallAction::ToggleMute).unwrap();
    mute.grab_focus();
    let mut muted = voice.clone();
    muted.muted = true;
    window.render("Ana", &look(true), &screen(&muted, "Ana", 100));
    let after = window.control(CallAction::ToggleMute).unwrap();
    assert_eq!(mute, after, "the switch is updated, not replaced");
    let toggle = after.downcast_ref::<gtk::ToggleButton>().unwrap();
    assert!(toggle.is_active());
    assert_eq!(after.tooltip_text().as_deref(), Some("Unmute"));
    assert_eq!(
        gtk::prelude::GtkWindowExt::focus(&window.window).as_ref(),
        Some(mute.upcast_ref())
    );
    window.window.destroy();

    for (allowed, expected) in [(true, true), (false, false)] {
        let mut window = CallWindow::new(std::rc::Rc::new(|_| {}));
        window.window.present();
        let ringing = session(CallDirection::Incoming, false);
        window.render("Ana", &look(allowed), &screen(&ringing, "Ana", 100));
        let answer = window.control(CallAction::Answer).unwrap();
        let focused =
            gtk::prelude::GtkWindowExt::focus(&window.window).as_ref() == Some(answer.upcast_ref());
        assert_eq!(focused, expected, "Answer focus when allowed is {allowed}");
        window.window.destroy();
    }
}

/// Renders every call screen and the Calls tab to PNGs under
/// `$RENDER_DIR`, light and dark. Run under a display:
/// `RENDER_DIR=/tmp/r cargo test --lib render_call_screens -- --ignored`.
/// `ZAPTIDE_LOCALEDIR` and `LANGUAGE` render a translation.
#[test]
#[ignore = "needs a display"]
fn render_call_screens() {
    if std::env::var_os("ZAPTIDE_LOCALEDIR").is_some() {
        crate::i18n::init();
    }
    gtk::init().unwrap();
    adw::init().unwrap();
    let dir = std::path::PathBuf::from(std::env::var("RENDER_DIR").expect("RENDER_DIR"));
    let css = gtk::CssProvider::new();
    css.load_from_string(include_str!("../../../data/style.css"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let context = gtk::glib::MainContext::default();
    let settle = || {
        let end = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < end {
            context.iteration(false);
        }
    };
    let shoot = |name: &str, window: &gtk::Window| {
        settle();
        let paintable = gtk::WidgetPaintable::new(Some(window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            &snapshot,
            f64::from(window.width()),
            f64::from(window.height()),
        );
        let node = snapshot.to_node().unwrap();
        let texture = window
            .native()
            .unwrap()
            .renderer()
            .unwrap()
            .render_texture(&node, None);
        std::fs::write(dir.join(format!("{name}.png")), texture.save_to_png_bytes()).unwrap();
    };
    let gradient = |width: u32, height: u32, shift: u8| RgbaFrame {
        width,
        height,
        data: (0..width * height)
            .flat_map(|index| {
                let (x, y) = (index % width, index / width);
                [
                    (x * 255 / width) as u8,
                    (y * 255 / height) as u8,
                    shift,
                    255,
                ]
            })
            .collect(),
    };
    controls_follow_the_model();
    let clicked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut muted = connected(CallDirection::Outgoing, false);
    muted.muted = true;
    let mut video = connected(CallDirection::Outgoing, true);
    video.camera_on = true;
    video.peer_video = true;
    let mut camera_off = video.clone();
    camera_off.camera_on = false;
    let mut no_camera = connected(CallDirection::Incoming, true);
    no_camera.peer_video = true;
    no_camera.camera_unavailable = true;
    let mut no_camera_voice = connected(CallDirection::Incoming, true);
    no_camera_voice.camera_unavailable = true;
    let mut voice_camera = connected(CallDirection::Outgoing, false);
    voice_camera.video = true;
    voice_camera.camera_on = true;
    let mut ended = connected(CallDirection::Outgoing, false);
    ended.apply(CallSignal::LocalHangup, 100 + 138);
    let mut failed = session(CallDirection::Outgoing, false);
    failed.apply(CallSignal::MediaFailed, 101);
    let mut request = connected(CallDirection::Incoming, false);
    request.video_requested = true;
    let screens = [
        ("incoming", session(CallDirection::Incoming, false)),
        ("incoming-video", session(CallDirection::Incoming, true)),
        ("calling", session(CallDirection::Outgoing, false)),
        ("calling-video", session(CallDirection::Outgoing, true)),
        ("voice", connected(CallDirection::Outgoing, false)),
        ("muted", muted),
        ("voice-camera", voice_camera),
        ("video", video),
        ("video-camera-off", camera_off),
        ("video-no-camera", no_camera),
        ("no-camera", no_camera_voice),
        ("request", request),
        ("waiting", connected(CallDirection::Outgoing, false)),
        ("ended", ended),
        ("failed", failed),
    ];
    let name = std::env::var("RENDER_NAME").unwrap_or_else(|_| "Ana Souza".into());
    let other = std::env::var("RENDER_OTHER").unwrap_or_else(|_| "Bruno Lima".into());
    let height = std::env::var("RENDER_HEIGHT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(640);
    for (size_name, width) in [("wide", 420), ("narrow", 360)] {
        for (scheme_name, scheme) in [
            ("light", adw::ColorScheme::ForceLight),
            ("dark", adw::ColorScheme::ForceDark),
        ] {
            adw::StyleManager::default().set_color_scheme(scheme);
            let record = clicked.clone();
            let mut window = CallWindow::new(std::rc::Rc::new(move |action| {
                record.borrow_mut().push(action)
            }));
            window.window.set_default_size(width, height);
            window.window.present();
            for (state, call) in &screens {
                let mut shown = screen(call, &name, 100 + 138);
                if *state == "waiting" {
                    shown = with_waiting(shown, &other, true, &name);
                }
                window.render(
                    &name,
                    &Look {
                        avatar: None,
                        waiting_avatar: None,
                        allow_focus: true,
                    },
                    &shown,
                );
                if call.peer_video {
                    window
                        .remote
                        .set_paintable(texture(gradient(320, 240, 90)).as_ref());
                }
                if call.camera_on {
                    window
                        .preview
                        .set_paintable(texture(gradient(120, 160, 200)).as_ref());
                }
                shoot(
                    &format!("call-{state}-{size_name}-{scheme_name}"),
                    window.window.upcast_ref(),
                );
                let bounds = gtk::graphene::Rect::new(
                    0.0,
                    0.0,
                    window.window.width() as f32,
                    window.window.height() as f32,
                );
                if width < 400
                    && let Some(end) = window.control(CallAction::HangUp)
                {
                    assert_eq!(end.height(), 56, "{state}: End call in a narrow window");
                }
                for control in window.controls() {
                    let (w, h) = (control.width(), control.height());
                    assert!(
                        w >= 44 && h >= 48,
                        "{state} {size_name}: a control is {w}x{h}"
                    );
                    let inside = control
                        .compute_bounds(&window.window)
                        .is_some_and(|b| bounds.contains_rect(&b));
                    assert!(inside, "{state} {size_name}: a control leaves the window");
                }
            }
            if width == 420 && scheme_name == "light" {
                window.control(CallAction::Retry).unwrap().emit_clicked();
                assert_eq!(clicked.borrow().last(), Some(&CallAction::Retry));
            }
            window.window.destroy();
        }
    }

    let ui = CallsUi::new(|_| {}, || {}, || {});
    let chats = [
        crate::model::Chat::new("1@s.whatsapp.net".into(), "Ana Souza".into()),
        crate::model::Chat::new("2@s.whatsapp.net".into(), "Bruno Lima".into()),
    ];
    let entry = |chat: &str, outgoing, outcome, video, seconds, timestamp| CallEntry {
        chat: chat.into(),
        id: format!("{timestamp}"),
        video,
        outgoing,
        outcome,
        seconds,
        timestamp,
    };
    let now = crate::util::now();
    let mut ui = ui;
    ui.entries = vec![
        entry(
            "1@s.whatsapp.net",
            false,
            CallOutcome::Missed,
            false,
            None,
            now - 600,
        ),
        entry(
            "2@s.whatsapp.net",
            true,
            CallOutcome::Completed,
            true,
            Some(138),
            now - 7200,
        ),
        entry(
            "1@s.whatsapp.net",
            false,
            CallOutcome::Completed,
            false,
            Some(65),
            now - 90_000,
        ),
    ];
    ui.selected = Some(1);
    ui.refill(&chats, &Default::default());
    ui.show_selected(&chats, &Default::default(), false);
    for (scheme_name, scheme) in [
        ("light", adw::ColorScheme::ForceLight),
        ("dark", adw::ColorScheme::ForceDark),
    ] {
        adw::StyleManager::default().set_color_scheme(scheme);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&ui.list));
        scroll.set_vexpand(true);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new(&gettext("Recent calls"), "")));
        let side = adw::ToolbarView::new();
        side.add_top_bar(&header);
        side.set_content(Some(&scroll));
        let split = adw::NavigationSplitView::new();
        split.set_sidebar(Some(&adw::NavigationPage::new(&side, "Calls")));
        split.set_content(Some(&adw::NavigationPage::new(&ui.details, "Details")));
        let host = adw::Window::builder()
            .default_width(860)
            .default_height(520)
            .content(&split)
            .build();
        host.present();
        shoot(&format!("calls-tab-{scheme_name}"), host.upcast_ref());
        split.set_sidebar(None::<&adw::NavigationPage>);
        split.set_content(None::<&adw::NavigationPage>);
        scroll.set_child(None::<&gtk::Widget>);
        host.destroy();
    }
}
