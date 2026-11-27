use super::*;
use relm4::ComponentController;

fn settle() {
    let context = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
    while std::time::Instant::now() < deadline {
        context.iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut child = root.first_child();
    while let Some(widget) = child {
        found.extend(descendants(&widget));
        child = widget.next_sibling();
        found.push(widget);
    }
    found
}

fn controls(root: &gtk::Widget) -> (Vec<gtk::CheckButton>, gtk::Button) {
    let poll = descendants(root)
        .into_iter()
        .find(|widget| widget.has_css_class("zaptide-poll"))
        .expect("inline poll");
    let widgets = descendants(&poll);
    let choices = widgets
        .iter()
        .filter_map(|widget| widget.clone().downcast::<gtk::CheckButton>().ok())
        .collect();
    let vote = widgets
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
        .find(|button| button.has_css_class("suggested-action"))
        .expect("explicit vote button");
    (choices, vote)
}

fn votes(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<crate::backend::Command>,
) -> Vec<(String, String, Vec<usize>)> {
    let mut votes = Vec::new();
    while let Ok(command) = receiver.try_recv() {
        if let crate::backend::Command::VotePoll {
            chat,
            message,
            choices,
        } = command
        {
            votes.push((chat, message, choices));
        }
    }
    votes
}

#[test]
#[ignore = "requires an isolated graphical process"]
fn inline_poll_dispatches_explicit_votes_and_preserves_retry_and_identity() {
    use crate::native_media::NativeMediaAction;

    let data = tempfile::tempdir().unwrap();
    let _relm: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.PollFixture");
    relm4::main_application()
        .register(None::<&gtk::gio::Cancellable>)
        .unwrap();
    let (mut backend, events) = Backend::detached();
    let mut commands = backend.capture_commands();
    TEST_BACKEND.with_borrow_mut(|slot| assert!(slot.replace(backend).is_none()));
    let app = NativeApplication::builder().launch(Init {
        dirs: AppDirs::under(data.path()),
    });
    let notifier = app.model().notifier.clone();
    let send = |event| {
        events.send(event).unwrap();
        crate::backend::Wake::wake(&notifier);
        settle();
    };
    let window = app.widget().clone();
    window.present();
    let account = app.model().active_account;
    let chat = "poll-fixture@g.us";
    send(Event::Link(LinkStatus::Connected));
    send(Event::Chats(vec![crate::model::Chat::new(
        chat.into(),
        "Synthetic poll".into(),
    )]));
    app.emit(Input::SelectChat(account, chat.into()));
    settle();
    let mut message =
        tests::grouping_message("poll-fixture", "Synthetic sender", false, 1_700_000_000);
    message.chat = chat.into();
    message.content = crate::model::Content::Poll {
        question: "Synthetic meeting?".into(),
        options: vec!["Morning".into(), "Afternoon".into(), "Evening".into()],
        state: crate::model::PollState {
            can_vote: true,
            selectable: 2,
            history_complete: true,
            counts: vec![0, 1, 0],
            voters: 1,
            ..Default::default()
        },
    };
    let request = app.model().transcript_requests.initial;
    send(Event::Messages {
        chat: chat.into(),
        messages: vec![message.clone()],
        keep_states: Vec::new(),
        older: false,
        complete: true,
        request,
    });
    let root = window.upcast_ref::<gtk::Widget>();
    let (choices, vote) = controls(root);
    choices[0].set_active(true);
    assert!(votes(&mut commands).is_empty());
    vote.emit_clicked();
    settle();
    assert_eq!(
        votes(&mut commands),
        [(chat.into(), message.id.clone(), vec![0])]
    );
    send(Event::PollVoted {
        chat: chat.into(),
        message: message.id.clone(),
        error: Some("Synthetic send failure".into()),
    });
    assert_eq!(app.model().status, gettext("Could not send your vote."));
    assert!(choices[0].is_active());
    assert!(vote.is_sensitive());
    vote.emit_clicked();
    settle();
    assert_eq!(
        votes(&mut commands),
        [(chat.into(), message.id.clone(), vec![0])]
    );

    if let crate::model::Content::Poll { state, .. } = &mut message.content {
        state.counts = vec![0, 2, 0];
        state.voters = 2;
    }
    send(Event::MessageUpdated(Box::new(message.clone())));
    let (choices, old_vote) = controls(root);
    assert!(
        choices[0].is_active(),
        "other votes preserve the unsent choice"
    );
    let action = |choices| NativeMediaAction::VotePoll {
        chat: chat.into(),
        message: message.id.clone(),
        choices,
    };
    app.emit(Input::MediaAction(action(vec![9])));
    app.emit(Input::MediaAction(action(vec![0, 1, 2])));
    let wrong_account = crate::account::AccountId(2);
    app.emit(Input::AccountMediaAction(wrong_account, action(vec![0])));
    settle();
    assert!(
        votes(&mut commands).is_empty(),
        "invalid or other-account actions are rejected"
    );
    choices[0].set_active(false);
    choices[1].set_active(true);

    if let crate::model::Content::Poll { state, .. } = &mut message.content {
        state.selected = vec![0];
        state.counts = vec![1, 2, 0];
        state.voters = 3;
    }
    send(Event::MessageUpdated(Box::new(message.clone())));
    let (choices, saved_vote) = controls(root);
    assert!(!choices[0].is_active());
    assert!(
        choices[1].is_active(),
        "earlier confirmation preserves a newer draft"
    );
    assert!(saved_vote.is_sensitive());
    saved_vote.emit_clicked();
    settle();
    assert_eq!(
        votes(&mut commands),
        [(chat.into(), message.id.clone(), vec![1])]
    );
    if let crate::model::Content::Poll { state, .. } = &mut message.content {
        state.selected = vec![1];
        state.counts = vec![0, 3, 0];
    }
    send(Event::MessageUpdated(Box::new(message.clone())));
    let (choices, saved_vote) = controls(root);
    assert!(choices[1].is_active());
    assert!(
        !saved_vote.is_sensitive(),
        "confirmation clears the unsent draft"
    );
    old_vote.emit_clicked();
    settle();
    assert!(
        votes(&mut commands).is_empty(),
        "replaced controls cannot send stale votes"
    );
    if let crate::model::Content::Poll { state, .. } = &mut message.content {
        state.can_vote = false;
    }
    send(Event::MessageUpdated(Box::new(message.clone())));
    app.emit(Input::MediaAction(action(vec![1])));
    settle();
    assert!(
        votes(&mut commands).is_empty(),
        "unavailable polls reject submission"
    );
    if let crate::model::Content::Poll { state, .. } = &mut message.content {
        state.can_vote = true;
        state.quiz = true;
        state.selected.clear();
    }
    send(Event::MessageUpdated(Box::new(message.clone())));
    app.emit(Input::MediaAction(action(vec![1])));
    settle();
    assert!(
        votes(&mut commands).is_empty(),
        "ordinary actions cannot answer quizzes"
    );
    let (choices, quiz_vote) = controls(root);
    choices[1].set_active(true);
    quiz_vote.emit_clicked();
    settle();
    assert_eq!(
        votes(&mut commands),
        [(chat.into(), message.id.clone(), vec![1])]
    );
    if let crate::model::Content::Poll { state, .. } = &mut message.content {
        state.quiz = false;
        state.selectable = 0;
    }
    send(Event::MessageUpdated(Box::new(message.clone())));
    app.emit(Input::MediaAction(action(Vec::new())));
    settle();
    assert!(
        votes(&mut commands).is_empty(),
        "zero selectable options reject even withdrawal"
    );
    let other_chat = "other-poll-fixture@g.us";
    send(Event::Chats(vec![
        crate::model::Chat::new(chat.into(), "Synthetic poll".into()),
        crate::model::Chat::new(other_chat.into(), "Other synthetic poll".into()),
    ]));
    app.emit(Input::SelectChat(account, other_chat.into()));
    settle();
    app.emit(Input::MediaAction(action(vec![1])));
    quiz_vote.emit_clicked();
    settle();
    assert!(
        votes(&mut commands).is_empty(),
        "old-chat actions and widgets cannot submit"
    );
    window.close();
}
