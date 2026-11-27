use super::*;
use relm4::ComponentController;

fn settle() {
    let context = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        context.iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn message(chat: &str, index: i64) -> crate::model::Message {
    let mut message = tests::grouping_message(
        &format!("history-{index}"),
        "Synthetic sender",
        index % 2 == 0,
        1_700_000_000 + index * 60,
    );
    message.chat = chat.into();
    message.content = crate::model::Content::text(
        "Synthetic pagination fixture\n".repeat((index.rem_euclid(5) + 1) as usize),
    );
    message
}

#[test]
#[ignore = "requires an isolated graphical process"]
fn keep_metadata_stays_with_the_current_transcript_request() {
    let data = tempfile::tempdir().expect("private fixture");
    let _relm: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.KeepFixture");
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
    let account = app.model().active_account;
    let a = "keep-first@s.whatsapp.net";
    let b = "keep-second@s.whatsapp.net";
    let id = message(a, 1).id;
    let page = |chat: &str, request, kept: bool| {
        send(Event::Messages {
            chat: chat.into(),
            messages: vec![message(chat, 1)],
            keep_states: if kept {
                vec![(id.clone(), i64::MAX, true)]
            } else {
                Vec::new()
            },
            older: false,
            complete: true,
            request,
        });
    };
    send(Event::Link(LinkStatus::Connected));
    send(Event::Chats(vec![
        crate::model::Chat::new(a.into(), "First".into()),
        crate::model::Chat::new(b.into(), "Second".into()),
    ]));
    let select = |chat: &str| {
        app.emit(Input::SelectChat(account, chat.into()));
        settle();
        app.model().transcript_requests.initial
    };
    let first_request = select(a);
    assert!(first_request.is_some());
    page(a, first_request, true);
    assert!(app.model().open_chat.kept.contains(&id));

    let has_keep_metadata = || {
        let model = app.model();
        model.open_chat.kept.contains(&id) || model.open_chat.keepable.contains_key(&id)
    };

    page(b, select(b), false);
    let leaked_to_b = has_keep_metadata();

    let current_request = select(a);
    assert_ne!(first_request, current_request);
    page(a, first_request, true);
    assert_eq!(app.model().transcript_requests.initial, current_request);
    let leaked_before_current = has_keep_metadata();
    page(a, current_request, false);
    page(a, first_request, true);
    let leaked_from_stale = has_keep_metadata();
    page(b, None, true);
    let leaked_from_inactive_live = has_keep_metadata();

    assert_eq!(
        (
            leaked_to_b,
            leaked_before_current,
            leaked_from_stale,
            leaked_from_inactive_live,
        ),
        (false, false, false, false)
    );
}

#[test]
#[ignore = "requires an isolated graphical process"]
fn final_history_page_preserves_reading_position_and_rejects_stale_clicks() {
    let data = tempfile::tempdir().expect("private fixture");
    let _relm: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.HistoryFixture");
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
    let chat = "history-fixture@s.whatsapp.net";
    let other = "other-fixture@s.whatsapp.net";
    send(Event::Link(LinkStatus::Connected));
    send(Event::Chats(vec![
        crate::model::Chat::new(chat.into(), "Synthetic history".into()),
        crate::model::Chat::new(other.into(), "Synthetic other".into()),
    ]));
    app.emit(Input::SelectChat(account, chat.into()));
    settle();
    let request = app.model().transcript_requests.initial;
    send(Event::Messages {
        chat: chat.into(),
        messages: (60..120).map(|index| message(chat, index)).collect(),
        keep_states: Vec::new(),
        older: false,
        complete: false,
        request,
    });
    let view = app.model().messages.view.clone();
    let scroller = view
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
        .expect("message scroller");
    scroller.vadjustment().set_value(0.0);
    settle();
    let height = scroller.height();
    let (id, offset) = motion::reading_anchor(&view, &app.model().open_chat.message_ids)
        .expect("visible message before pagination");
    let epoch = app.model().transcript_requests.next;
    app.emit(Input::LoadOlder(epoch));
    settle();
    assert!(app.model().loading_older);
    let request = app.model().transcript_requests.older;
    send(Event::Messages {
        chat: chat.into(),
        messages: (0..60).map(|index| message(chat, index)).collect(),
        keep_states: Vec::new(),
        older: true,
        complete: true,
        request,
    });
    let restored = motion::reading_anchor(&view, std::slice::from_ref(&id))
        .expect("same message visible after final page");
    assert_eq!(restored.0, id);
    assert!(
        (restored.1 - offset).abs() <= 1.0,
        "before={offset}, after={restored:?}"
    );
    assert_eq!(scroller.height(), height);
    assert!(app.model().history_complete);
    assert!(!app.model().loading_older);
    assert_eq!(app.model().open_chat.message_ids.len(), 120);
    settle();
    let settled =
        motion::reading_anchor(&view, std::slice::from_ref(&id)).expect("settled message");
    assert!((settled.1 - offset).abs() <= 1.0);

    let stale_epoch = app.model().transcript_requests.next;
    app.emit(Input::SelectChat(account, other.into()));
    app.emit(Input::LoadOlder(stale_epoch));
    settle();
    assert_eq!(app.model().open_chat.active_chat.as_deref(), Some(other));
    assert!(app.model().transcript_requests.older.is_none());
    assert!(!app.model().loading_older);
    window.close();
}
