use super::*;
use relm4::ComponentController;

fn settle() {
    let context = gtk::glib::MainContext::default();
    let until = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < until {
        context.iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn find_button(widget: &gtk::Widget, tooltip: &str) -> Option<gtk::Button> {
    if let Ok(button) = widget.clone().downcast::<gtk::Button>()
        && button.tooltip_text().as_deref() == Some(tooltip)
    {
        return Some(button);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(button) = find_button(&current, tooltip) {
            return Some(button);
        }
        child = current.next_sibling();
    }
    None
}

fn stream(widget: &gtk::Widget) -> Option<gtk::MediaStream> {
    if let Ok(picture) = widget.clone().downcast::<gtk::Picture>()
        && let Some(stream) = picture
            .paintable()
            .and_then(|paintable| paintable.dynamic_cast::<gtk::MediaStream>().ok())
    {
        return Some(stream);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(stream) = stream(&current) {
            return Some(stream);
        }
        child = current.next_sibling();
    }
    None
}

fn screenshot(window: &impl IsA<gtk::Widget>, name: &str) {
    let Some(directory) = std::env::var_os("ZAPTIDE_VIEWER_SHOTS") else {
        return;
    };
    let paintable = gtk::WidgetPaintable::new(Some(window));
    window.queue_draw();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let node = loop {
        settle();
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            snapshot.upcast_ref::<gtk::gdk::Snapshot>(),
            f64::from(window.width()),
            f64::from(window.height()),
        );
        if let Some(node) = snapshot.to_node() {
            break node;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture {name} did not render"
        );
    };
    let texture = window
        .native()
        .unwrap()
        .renderer()
        .unwrap()
        .render_texture(&node, None);
    texture
        .save_to_png(std::path::Path::new(&directory).join(format!("{name}.png")))
        .unwrap();
}

#[test]
#[ignore = "needs a display and glycin loaders"]
fn media_viewer_uses_the_main_panel_and_releases_files_on_navigation() {
    use crate::native_media::NativeMediaAction;
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("fixture.png");
    image::RgbaImage::from_pixel(64, 48, image::Rgba([20, 120, 200, 255]))
        .save(&path)
        .unwrap();
    let _application: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.MediaViewerTest");
    let app = NativeApplication::builder().launch(Init {
        dirs: crate::paths::AppDirs::under(data.path()),
    });
    gtk::Settings::default()
        .unwrap()
        .set_gtk_enable_animations(false);
    let window = app.widget().clone();
    window.present();
    settle();
    let mut message = super::tests::grouping_message("photo", "sender", false, 1);
    message.content = crate::model::Content::Image {
        caption: Some("Synthetic photo".into()),
        media: crate::model::Media {
            mime: "image/png".into(),
            size: 100,
            width: Some(64),
            height: Some(48),
            path: Some(path.clone()),
            state: crate::model::MediaState::Idle,
        },
    };
    {
        let mut state = app.state().get_mut();
        state.model.link = LinkStatus::Connecting;
        state.model.open_chat.active_chat = Some("fixture-chat".into());
        state
            .model
            .open_chat
            .message_snapshots
            .insert(message.id.clone(), message);
    }
    let open = || {
        app.emit(Input::MediaAction(NativeMediaAction::ViewPhotos {
            paths: vec![path.clone()],
            start: 0,
        }));
        settle();
        let model = app.model();
        let page = model.media_viewer.as_ref().expect("integrated viewer");
        assert_eq!(page.widget.root(), window.root());
        assert!(
            page.widget.is_mapped(),
            "viewer must be visible, not just parented"
        );
        assert_eq!(
            model
                .media_viewer_host
                .parent()
                .and_downcast::<gtk::Stack>()
                .unwrap()
                .visible_child_name()
                .as_deref(),
            Some("media-viewer")
        );
        assert!(
            window.visible_dialog().is_none(),
            "media must not open a dialog"
        );
        assert!(crate::media_retention::is_held(&path));
    };
    open();
    assert!(!app.widgets().main_split_view.is_collapsed());
    assert!(app.model().sidebar.widget().is_mapped());
    screenshot(&window, "integrated-photo-wide");
    window.set_default_size(420, 640);
    settle();
    assert!(app.widgets().main_split_view.is_collapsed());
    let back = find_button(app.model().media_viewer_host.upcast_ref(), "Back").unwrap();
    assert_eq!(back.icon_name().as_deref(), Some("go-previous-symbolic"));
    assert!(
        app.model()
            .media_viewer
            .as_ref()
            .unwrap()
            .widget
            .is_mapped()
    );
    screenshot(&window, "integrated-photo-narrow");
    let back = find_button(app.model().media_viewer_host.upcast_ref(), "Back").unwrap();
    back.emit_clicked();
    settle();
    assert!(app.model().media_viewer.is_none());
    assert!(!crate::media_retention::is_held(&path));
    assert!(app.model().media_viewer_host.first_child().is_none());
    assert!(
        app.model().composer_view.model().text_view().has_focus(),
        "Back restores composer focus"
    );
    window.set_default_size(1100, 760);
    settle();
    open();
    app.emit(Input::ShowCalls(true));
    settle();
    assert!(app.model().media_viewer.is_none());
    assert!(!crate::media_retention::is_held(&path));
    open();
    let mut call = crate::model::call::CallSession::new(
        "synthetic-call".into(),
        "fixture-chat".into(),
        crate::model::call::CallDirection::Outgoing,
        false,
        1,
    );
    call.phase = crate::model::call::CallPhase::Connecting;
    app.emit(Input::TestCall(call));
    settle();
    assert!(app.model().media_viewer.is_none());
    assert!(app.model().calls.showing_live);
    assert!(!crate::media_retention::is_held(&path));
    open();
    app.emit(Input::EscapePressed);
    settle();
    assert!(app.model().media_viewer.is_none());
    open();
    app.emit(Input::CloseChat);
    settle();
    assert!(app.model().media_viewer.is_none());
    assert!(!crate::media_retention::is_held(&path));

    app.state()
        .get_mut()
        .model
        .open_chat
        .message_snapshots
        .insert("photo".into(), {
            let mut message = super::tests::grouping_message("photo", "sender", false, 1);
            message.content = crate::model::Content::Image {
                caption: None,
                media: crate::model::Media {
                    mime: "image/png".into(),
                    size: 100,
                    width: Some(64),
                    height: Some(48),
                    path: Some(path.clone()),
                    state: crate::model::MediaState::Idle,
                },
            };
            message
        });
    open();
    app.state().get_mut().model.leave_screen();
    settle();
    assert!(app.model().media_viewer.is_none());
    assert!(!crate::media_retention::is_held(&path));

    let video = data.path().join("clip.webm");
    let status = std::process::Command::new("gst-launch-1.0")
        .args("-q videotestsrc num-buffers=90 ! video/x-raw,width=320,height=240,framerate=30/1 ! vp8enc deadline=1 ! webmmux ! filesink".split_whitespace())
        .arg(format!("location={}", video.display())).status().unwrap();
    assert!(status.success());
    let mut video_message = super::tests::grouping_message("video", "sender", false, 1);
    video_message.content = crate::model::Content::Video {
        caption: None,
        media: crate::model::Media {
            mime: "video/webm".into(),
            size: 100,
            width: Some(320),
            height: Some(240),
            path: Some(video.clone()),
            state: crate::model::MediaState::Idle,
        },
        seconds: Some(3),
        gif: false,
    };
    app.state()
        .get_mut()
        .model
        .open_chat
        .message_snapshots
        .insert(video_message.id.clone(), video_message);
    app.emit(Input::MediaAction(NativeMediaAction::ViewVideo(
        video.clone(),
    )));
    settle();
    assert!(app.model().media_viewer.is_some());
    let clip = stream(app.model().media_viewer_host.upcast_ref()).expect("integrated video stream");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !clip.is_prepared() && std::time::Instant::now() < deadline {
        settle();
    }
    assert!(clip.is_prepared());
    assert!(crate::media_retention::is_held(&video));
    screenshot(&window, "integrated-video-wide");
    window.set_default_size(420, 640);
    settle();
    screenshot(&window, "integrated-video-narrow");
    app.emit(Input::CloseMediaViewer);
    settle();
    assert!(!clip.is_prepared());
    assert!(!crate::media_retention::is_held(&video));
    window.close();
}
