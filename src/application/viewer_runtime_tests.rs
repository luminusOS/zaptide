use super::*;
use relm4::ComponentController;
use std::{path::Path, time::Duration};

fn settle() {
    let context = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + Duration::from_millis(300);
    while std::time::Instant::now() < deadline {
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn find_widget<T: IsA<gtk::Widget> + Clone + gtk::glib::object::IsClass>(
    root: &gtk::Widget,
    predicate: &impl Fn(&T) -> bool,
) -> Option<T> {
    if let Ok(widget) = root.clone().downcast::<T>()
        && predicate(&widget)
    {
        return Some(widget);
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_widget(&widget, predicate) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn write_picture(path: &Path, gray: u8) {
    image::RgbaImage::from_pixel(32, 32, image::Rgba([gray, gray, gray, 255]))
        .save(path)
        .expect("write synthetic picture");
}

fn assert_picture(avatar: &adw::Avatar, gray: u8) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while avatar.custom_image().is_none() && std::time::Instant::now() < deadline {
        settle();
    }
    let image = avatar
        .custom_image()
        .expect("media viewer must display the sender's cached profile picture");
    let texture = image
        .downcast::<gtk::gdk::Texture>()
        .expect("sender profile texture");
    let mut pixels = vec![0; (texture.width() * texture.height() * 4) as usize];
    texture.download(&mut pixels, texture.width() as usize * 4);
    assert_eq!(
        &pixels[..4],
        &[gray, gray, gray, 255],
        "viewer must show this sender's picture, not the group or previous sender"
    );
}

fn capture_window(window: &adw::ApplicationWindow) {
    let Some(directory) = std::env::var_os("ZAPTIDE_VIEWER_SHOTS") else {
        return;
    };
    assert!(window.is_mapped(), "capture the displayed synthetic window");
    std::fs::create_dir_all(&directory).expect("create screenshot directory");
    let paintable = gtk::WidgetPaintable::new(Some(window));
    window.queue_draw();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
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
            "synthetic viewer did not render"
        );
    };
    let texture = window
        .native()
        .expect("mapped native window")
        .renderer()
        .expect("window renderer")
        .render_texture(&node, None);
    texture
        .save_to_png(Path::new(&directory).join("viewer-sender-avatar.png"))
        .expect("save synthetic viewer screenshot");
}

#[test]
#[ignore = "requires an isolated graphical process and glycin loaders"]
fn viewer_album_displays_sender_profiles_and_clears_missing_picture() {
    use crate::native_media::NativeMediaAction;

    let data = tempfile::tempdir().expect("private synthetic fixtures");
    let _relm: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.ViewerAvatarFixture");
    relm4::main_application()
        .register(None::<&gtk::gio::Cancellable>)
        .expect("register fixture");
    let (backend, events) = Backend::detached();
    TEST_BACKEND.with_borrow_mut(|slot| assert!(slot.replace(backend).is_none()));
    let app = NativeApplication::builder().launch(Init {
        dirs: AppDirs::under(data.path()),
    });
    let notifier = app.model().notifier.clone();
    let send = |event| {
        events.send(event).expect("fixture event");
        crate::backend::Wake::wake(&notifier);
        settle();
    };
    let window = app.widget().clone();
    window.present();
    let account = app.model().active_account;
    let chat = "viewer-fixture@g.us";
    let incoming = "15550000101@s.whatsapp.net";
    let own = "15550000102@s.whatsapp.net";
    let missing = "15550000103@s.whatsapp.net";
    let outgoing_alias = "viewer-own-alias@lid";
    let corrupt = "15550000104@s.whatsapp.net";
    send(Event::Link(LinkStatus::Connected));
    send(Event::Profile {
        phone: Some("15550000102".into()),
        name: Some("Synthetic account".into()),
    });
    send(Event::Chats(vec![crate::model::Chat::new(
        chat.into(),
        "Synthetic group".into(),
    )]));
    for (id, gray) in [
        (chat, 30),
        (incoming, 100),
        (own, 200),
        (outgoing_alias, 70),
    ] {
        let path = data.path().join(format!("avatar-{gray}.png"));
        write_picture(&path, gray);
        send(Event::Avatar {
            id: id.into(),
            full: false,
            path: Some(path),
        });
    }
    let corrupt_path = data.path().join("corrupt-avatar.png");
    std::fs::write(&corrupt_path, b"synthetic invalid image").expect("write corrupt fixture");
    send(Event::Avatar {
        id: corrupt.into(),
        full: false,
        path: Some(corrupt_path),
    });
    app.emit(Input::SelectChat(account, chat.into()));
    settle();
    let paths: Vec<_> = (0..4)
        .map(|index| {
            let path = data.path().join(format!("photo-{index}.png"));
            write_picture(&path, 240);
            path
        })
        .collect();
    let messages = [
        (incoming, "Synthetic sender", false),
        (outgoing_alias, "Synthetic account", true),
        (corrupt, "Sender with corrupt picture", false),
        (missing, "Sender without picture", false),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (sender, name, from_me))| {
        let mut message = tests::grouping_message(
            &format!("viewer-photo-{index}"),
            name,
            from_me,
            1_700_000_000 + index as i64,
        );
        message.chat = chat.into();
        message.sender = sender.into();
        message.content = crate::model::Content::Image {
            caption: None,
            media: crate::model::Media {
                mime: "image/png".into(),
                size: std::fs::metadata(&paths[index]).unwrap().len(),
                width: Some(32),
                height: Some(32),
                path: Some(paths[index].clone()),
                state: crate::model::MediaState::Idle,
            },
        };
        message
    })
    .collect();
    let request = app.model().transcript_requests.initial;
    send(Event::Messages {
        chat: chat.into(),
        messages,
        older: false,
        complete: true,
        request,
    });
    app.emit(Input::MediaAction(NativeMediaAction::ViewPhotos {
        paths,
        start: 0,
    }));
    settle();
    let host = app.model().media_viewer_host.clone();
    let avatar =
        find_widget::<adw::Avatar>(host.upcast_ref(), &|_| true).expect("viewer header avatar");
    assert!(
        avatar.is_mapped(),
        "sender avatar is displayed in the viewer"
    );
    assert_eq!(avatar.text().as_deref(), Some("Synthetic sender"));
    assert_picture(&avatar, 100);
    capture_window(&window);

    let next = find_widget::<gtk::Button>(host.upcast_ref(), &|button| {
        button.icon_name().as_deref() == Some("go-next-symbolic")
    })
    .expect("next photo button");
    next.emit_clicked();
    settle();
    assert_eq!(avatar.text().as_deref(), Some("You"));
    assert_picture(&avatar, 200);

    next.emit_clicked();
    settle();
    assert_eq!(
        avatar.text().as_deref(),
        Some("Sender with corrupt picture")
    );
    assert!(
        avatar.custom_image().is_none(),
        "an unreadable profile picture must fall back to the sender's initials"
    );

    next.emit_clicked();
    settle();
    assert_eq!(avatar.text().as_deref(), Some("Sender without picture"));
    assert!(
        avatar.custom_image().is_none(),
        "a sender without a profile picture must not inherit the previous sender's picture"
    );
    window.close();
}
