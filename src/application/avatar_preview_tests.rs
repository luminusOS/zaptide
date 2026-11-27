use super::*;
use crate::backend::Command;
use relm4::ComponentController;
use std::time::{Duration, Instant};

fn wait_until(mut ready: impl FnMut() -> bool, failure: &str) {
    let context = gtk::glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() {
        assert!(Instant::now() < deadline, "{failure}");
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn find<T: IsA<gtk::Widget> + gtk::glib::object::IsClass>(
    widget: &gtk::Widget,
    matches: &impl Fn(&T) -> bool,
) -> Option<T> {
    if let Ok(candidate) = widget.clone().downcast::<T>()
        && matches(&candidate)
    {
        return Some(candidate);
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(found) = find(&widget, matches) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn status(popover: &gtk::Popover, text: &str) -> bool {
    let label_text = gettext(text);
    find::<gtk::Label>(popover.upcast_ref(), &|label| {
        label.is_visible() && label.label() == label_text
    })
    .is_some()
}

fn picture_width(popover: &gtk::Popover) -> Option<i32> {
    find::<gtk::Picture>(popover.upcast_ref(), &|_| true)
        .and_then(|picture| picture.paintable())
        .map(|paintable| paintable.intrinsic_width())
}

fn assert_preview_fits(popover: &gtk::Popover, dialog: &adw::Dialog) {
    let area = dialog
        .child()
        .and_downcast::<adw::ToolbarView>()
        .expect("Contact Info content area");
    let (_, natural_width, _, _) = popover.measure(gtk::Orientation::Horizontal, -1);
    let (_, natural_height, _, _) = popover.measure(gtk::Orientation::Vertical, natural_width);
    assert!(
        natural_width <= area.width() && natural_height <= area.height(),
        "popover requests {natural_width}x{natural_height} inside {}x{} Contact Info",
        area.width(),
        area.height(),
    );
    let frames = std::rc::Rc::new(std::cell::Cell::new(0));
    let draw_count = frames.clone();
    popover.add_tick_callback(move |_, _| {
        let count = draw_count.get() + 1;
        draw_count.set(count);
        if count < 2 {
            gtk::glib::ControlFlow::Continue
        } else {
            gtk::glib::ControlFlow::Break
        }
    });
    wait_until(|| frames.get() == 2, "preview layout did not settle");
    assert!(
        popover.width() <= area.width() && popover.height() <= area.height(),
        "popover allocated {}x{} inside {}x{} Contact Info",
        popover.width(),
        popover.height(),
        area.width(),
        area.height(),
    );
    let picture = find::<gtk::Picture>(popover.upcast_ref(), &|_| true).expect("preview image");
    assert_eq!(picture.content_fit(), gtk::ContentFit::Contain);
    assert!(picture.width() > 0 && picture.height() > 0);
    let image = picture.paintable().expect("loaded image");
    let paintable = gtk::WidgetPaintable::new(Some(&picture));
    picture.queue_draw();
    let mut painted = None;
    wait_until(
        || {
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                snapshot.upcast_ref::<gtk::gdk::Snapshot>(),
                f64::from(picture.width()),
                f64::from(picture.height()),
            );
            painted = snapshot.to_node();
            painted.is_some()
        },
        "preview image did not paint",
    );
    let bounds = painted.expect("painted preview").bounds();
    let image_width = f64::from(image.intrinsic_width());
    let image_height = f64::from(image.intrinsic_height());
    let scale =
        (f64::from(picture.width()) / image_width).min(f64::from(picture.height()) / image_height);
    assert!(
        (f64::from(bounds.width()) - image_width * scale).abs() <= 1.0
            && (f64::from(bounds.height()) - image_height * scale).abs() <= 1.0,
        "drawn image is {}x{} for source {image_width}x{image_height}",
        bounds.width(),
        bounds.height(),
    );
    eprintln!(
        "preview natural {natural_width}x{natural_height}, allocated {}x{}, image drawn {:.1}x{:.1}, Contact Info {}x{}",
        popover.width(),
        popover.height(),
        bounds.width(),
        bounds.height(),
        area.width(),
        area.height(),
    );
}

#[test]
#[ignore = "requires an isolated graphical process"]
fn contact_avatar_preview_stays_with_its_dialog_and_account() {
    let data = tempfile::tempdir().expect("synthetic fixtures");
    let thumbnail = data.path().join("thumbnail.png");
    let full = data.path().join("full.png");
    let portrait = data.path().join("portrait.png");
    let corrupt = data.path().join("corrupt.png");
    for (path, (width, height)) in [
        (&thumbnail, (32, 32)),
        (&full, (2048, 1400)),
        (&portrait, (1400, 2048)),
    ] {
        image::RgbaImage::from_pixel(width, height, image::Rgba([110, 130, 150, 255]))
            .save(path)
            .expect("synthetic picture");
    }
    std::fs::write(&corrupt, b"not an image").expect("corrupt synthetic picture");

    let _relm: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.AvatarPreviewFixture");
    relm4::main_application()
        .register(None::<&gtk::gio::Cancellable>)
        .expect("register application");
    gtk::Settings::default()
        .expect("GTK settings")
        .set_gtk_enable_animations(false);
    let (mut backend, events) = Backend::detached();
    let mut commands = backend.capture_commands();
    TEST_BACKEND.with_borrow_mut(|slot| assert!(slot.replace(backend).is_none()));
    let app = NativeApplication::builder().launch(Init {
        dirs: AppDirs::under(data.path()),
    });
    let window = app.widget();
    window.set_default_size(420, 480);
    window.present();
    let account = app.model().active_account;
    let notifier = app.model().notifier.clone();
    let send = |event| {
        events.send(event).expect("synthetic event");
        crate::backend::Wake::wake(&notifier);
    };
    let chat = "15550000101@s.whatsapp.net";
    send(Event::Link(LinkStatus::Connected));
    send(Event::Chats(vec![crate::model::Chat::new(
        chat.into(),
        "Synthetic contact".into(),
    )]));
    send(Event::Avatar {
        id: chat.into(),
        full: false,
        path: Some(thumbnail),
    });
    wait_until(
        || app.model().avatars.contains_key(chat),
        "thumbnail event was not delivered",
    );
    app.emit(Input::SelectChat(account, chat.to_owned()));
    wait_until(
        || app.model().open_chat.active_chat.as_deref() == Some(chat),
        "synthetic contact did not open",
    );
    let open_info = || {
        let previous = window.visible_dialog();
        app.emit(Input::ShowChatInfo);
        wait_until(
            || {
                window
                    .visible_dialog()
                    .is_some_and(|dialog| Some(dialog) != previous)
            },
            "new Contact Info did not open",
        );
        window.visible_dialog().expect("Contact Info")
    };
    let mut open_preview = |dialog: &adw::Dialog| {
        let button = find::<gtk::MenuButton>(dialog.upcast_ref(), &|button| {
            button.child().and_downcast::<adw::Avatar>().is_some()
        })
        .expect("picture menu button");
        wait_until(|| button.is_mapped(), "picture button did not map");
        assert!(
            button.grab_focus(),
            "picture preview accepts keyboard focus"
        );
        assert!(button.activate(), "the native activation opens the preview");
        let popover = button.popover().expect("dialog-owned popover");
        assert!(popover.is_autohide(), "outside-click dismissal is enabled");
        wait_until(|| popover.is_mapped(), "preview popover did not map");
        assert_eq!(
            popover.ancestor(adw::Dialog::static_type()),
            Some(dialog.clone().upcast()),
            "the popover is owned by the displayed Contact Info"
        );
        let mut request = None;
        wait_until(
            || {
                while let Ok(command) = commands.try_recv() {
                    if let Command::FetchAvatarPreview { id, request: token } = command {
                        assert_eq!(id, chat);
                        assert!(request.replace(token).is_none(), "one request per open");
                    }
                }
                request.is_some()
            },
            "opening the preview did not request its image",
        );
        assert!(status(&popover, "Loading profile picture…"));
        (popover, request.expect("request token"))
    };
    let respond = |request, path| send(Event::AvatarPreview { request, path });

    let dialog = open_info();
    let (popover, request) = open_preview(&dialog);
    assert_eq!(picture_width(&popover), Some(32));
    respond(request, Some(full.clone()));
    wait_until(
        || picture_width(&popover) == Some(2048),
        "large picture did not replace the thumbnail",
    );
    assert_preview_fits(&popover, &dialog);
    assert!(!status(&popover, "Loading profile picture…"));
    popover.popdown();
    wait_until(|| !popover.is_mapped(), "preview did not dismiss");
    assert_eq!(window.visible_dialog(), Some(dialog.clone()));

    let (popover, request) = open_preview(&dialog);
    respond(request, Some(portrait));
    wait_until(
        || picture_width(&popover) == Some(1400),
        "portrait picture did not replace the thumbnail",
    );
    assert_preview_fits(&popover, &dialog);
    let close_message = "Close";
    let close_label = gettext(close_message);
    let close = find::<gtk::Button>(popover.upcast_ref(), &|button| {
        button.label().as_deref() == Some(close_label.as_str())
    })
    .expect("explicit Close button");
    assert!(close.grab_focus(), "Close accepts keyboard focus");
    assert!(
        close.activate(),
        "Close supports native keyboard activation"
    );
    wait_until(|| !popover.is_mapped(), "Close did not dismiss the preview");
    assert_eq!(window.visible_dialog(), Some(dialog.clone()));
    let avatar = popover
        .ancestor(gtk::MenuButton::static_type())
        .expect("avatar preview trigger");
    let focus = gtk::prelude::GtkWindowExt::focus(window).expect("restored keyboard focus");
    assert!(focus == avatar || focus.is_ancestor(&avatar));

    let (popover, stale) = open_preview(&dialog);
    let replacement = open_info();
    let (new_popover, request) = open_preview(&replacement);
    respond(stale, Some(full.clone()));
    respond(request, Some(corrupt));
    wait_until(
        || status(&new_popover, "Showing smaller picture"),
        "corrupt preview did not fall back to the thumbnail",
    );
    assert_eq!(picture_width(&new_popover), Some(32));
    assert!(!popover.is_mapped());

    new_popover.popdown();
    wait_until(
        || !new_popover.is_mapped(),
        "replacement preview did not dismiss",
    );
    let (popover, request) = open_preview(&replacement);
    replacement.force_close();
    wait_until(
        || window.visible_dialog().is_none() && !popover.is_mapped(),
        "Contact Info and its preview did not close",
    );
    respond(request, Some(full.clone()));
    send(Event::Avatar {
        id: chat.into(),
        full: false,
        path: None,
    });
    wait_until(
        || !app.model().avatars.contains_key(chat),
        "thumbnail removal was not delivered",
    );
    let dialog = open_info();
    assert!(!popover.is_mapped(), "late reply reopened closed preview");
    let (popover, request) = open_preview(&dialog);
    respond(request, None);
    wait_until(
        || status(&popover, "No profile picture"),
        "missing picture did not explain why the preview is empty",
    );
    assert!(picture_width(&popover).is_none());
    popover.popdown();
    wait_until(|| !popover.is_mapped(), "empty preview did not dismiss");

    let (popover, request) = open_preview(&dialog);
    let second_account = {
        let mut state = app.state().get_mut();
        let model = &mut state.model;
        let id = model.registry.add();
        let mut session = accounts::AccountSession::detached_for_tests(id);
        session.dirs = AppDirs::under(&data.path().join("second"));
        session
            .dirs
            .ensure()
            .expect("second account fixture directory");
        model.accounts.insert(id, session);
        id
    };
    app.emit(Input::SwitchAccount(second_account));
    wait_until(
        || {
            app.model().active_account == second_account
                && window.visible_dialog().is_none()
                && !popover.is_mapped()
        },
        "switching accounts did not close Contact Info",
    );
    respond(request, Some(full));
    app.emit(Input::BackendReady(account));
    app.emit(Input::SwitchAccount(account));
    wait_until(
        || app.model().active_account == account,
        "account did not switch back",
    );
    assert!(!popover.is_mapped());
    assert!(window.visible_dialog().is_none());
    window.destroy();
}
