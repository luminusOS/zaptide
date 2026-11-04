use super::screen::with_waiting;
use super::tests::{connected, session};
use super::*;
use crate::model::call::{CallDirection, CallOutcome, CallSignal};

/// Renders every call screen and the Calls tab to PNGs under
/// `$RENDER_DIR`, light and dark. Run under a display:
/// `RENDER_DIR=/tmp/r cargo test --lib render_call_screens -- --ignored`.
#[test]
#[ignore = "needs a display"]
fn render_call_screens() {
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
    let clicked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = clicked.clone();
    let mut window = CallWindow::new(std::rc::Rc::new(move |action| {
        record.borrow_mut().push(action)
    }));
    let mut muted = connected(CallDirection::Outgoing, false);
    muted.muted = true;
    let mut video = connected(CallDirection::Outgoing, true);
    video.camera_on = true;
    video.peer_video = true;
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
        ("voice", connected(CallDirection::Outgoing, false)),
        ("muted", muted),
        ("video", video),
        ("request", request),
        ("waiting", connected(CallDirection::Outgoing, false)),
        ("ended", ended),
        ("failed", failed),
    ];
    for (scheme_name, scheme) in [
        ("light", adw::ColorScheme::ForceLight),
        ("dark", adw::ColorScheme::ForceDark),
    ] {
        adw::StyleManager::default().set_color_scheme(scheme);
        for (name, call) in &screens {
            let mut shown = screen(call, "Ana Souza", 100 + 138);
            if *name == "waiting" {
                shown = with_waiting(shown, "Bruno Lima", true);
            }
            window.render(call, "Ana Souza", None, &shown);
            if call.peer_video {
                window
                    .remote
                    .set_paintable(texture(gradient(320, 240, 90)).as_ref());
                window
                    .preview
                    .set_paintable(texture(gradient(120, 160, 200)).as_ref());
            }
            window.window.present();
            shoot(
                &format!("call-{name}-{scheme_name}"),
                window.window.upcast_ref(),
            );
        }
    }
    let first = window
        .buttons
        .first_child()
        .and_downcast::<gtk::Button>()
        .unwrap();
    first.emit_clicked();
    assert_eq!(clicked.borrow().len(), 1);
    window.window.destroy();

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
