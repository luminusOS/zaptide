use gtk::prelude::*;
use relm4::{adw, gtk};
use std::{cell::RefCell, rc::Rc};

use super::{tests::message, widget::build_media_widget_with_action};
use crate::{
    model::{Content, Message, PollState},
    native_media::NativeMediaAction,
};

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut widgets = Vec::new();
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        widgets.extend(descendants(&current));
        widgets.push(current);
    }
    widgets
}

#[test]
#[ignore = "needs a display"]
fn ordinary_poll_enables_explicit_vote_only_after_selection() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let message = message(Content::Poll {
        question: "Which meeting time?".into(),
        options: vec!["Morning".into(), "Afternoon".into()],
        state: PollState {
            quiz: false,
            can_vote: true,
            selectable: 1,
            ..Default::default()
        },
    });

    let rendered = build_media_widget_with_action(&message, |_| {});
    let widgets = descendants(rendered.widget.upcast_ref());
    let choices: Vec<_> = widgets
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::CheckButton>())
        .collect();

    assert_eq!(choices.len(), 2, "ordinary polls need selectable options");
    assert!(choices.iter().all(|choice| choice.is_sensitive()));
    let vote = widgets
        .iter()
        .filter_map(|widget| widget.downcast_ref::<gtk::Button>())
        .find(|button| button.label().as_deref() == Some("Vote"))
        .expect("ordinary polls need an explicit Vote button");
    assert!(!vote.is_sensitive(), "a vote requires a selected option");

    choices[1].set_active(true);

    assert!(vote.is_sensitive(), "selecting an option enables voting");
}

fn fixture(state: PollState) -> Message {
    message(Content::Poll {
        question: "Which meeting time?".into(),
        options: vec!["Morning".into(), "Afternoon".into(), "Evening".into()],
        state,
    })
}

fn ready(selectable: usize) -> PollState {
    PollState {
        selectable,
        can_vote: true,
        history_complete: true,
        ..Default::default()
    }
}

fn controls<T: IsA<gtk::Widget>>(root: &gtk::Widget) -> Vec<T> {
    descendants(root)
        .into_iter()
        .filter_map(|widget| widget.downcast::<T>().ok())
        .collect()
}

fn submit(root: &gtk::Widget) -> gtk::Button {
    controls::<gtk::Button>(root)
        .into_iter()
        .find(|button| {
            matches!(
                button.label().as_deref(),
                Some("Vote" | "Update vote" | "Remove vote")
            )
        })
        .expect("explicit vote action")
}

fn render(state: PollState) -> (gtk::Box, Rc<RefCell<Vec<NativeMediaAction>>>) {
    gtk::init().unwrap();
    adw::init().unwrap();
    let actions = Rc::new(RefCell::new(Vec::new()));
    let captured = actions.clone();
    let rendered = build_media_widget_with_action(&fixture(state), move |action| {
        captured.borrow_mut().push(action);
    });
    (rendered.widget, actions)
}

fn assert_poll_action(action: &NativeMediaAction, expected: &[usize]) {
    assert!(matches!(
        action,
        NativeMediaAction::VotePoll { chat, message, choices }
            if chat == "chat-id" && message == "message-id" && choices == expected
    ));
}

#[test]
#[ignore = "needs a display"]
fn single_choice_poll_dispatches_only_explicit_selected_vote() {
    let (root, actions) = render(ready(1));
    let choices = controls::<gtk::CheckButton>(root.upcast_ref());
    let vote = submit(root.upcast_ref());
    vote.emit_clicked();
    choices[0].set_active(true);
    choices[2].set_active(true);
    assert_eq!(
        choices
            .iter()
            .map(|choice| choice.is_active())
            .collect::<Vec<_>>(),
        [false, false, true]
    );
    assert!(actions.borrow().is_empty());
    vote.emit_clicked();
    assert_eq!(actions.borrow().len(), 1);
    assert_poll_action(&actions.borrow()[0], &[2]);
}

#[test]
#[ignore = "needs a display"]
fn multiple_choice_poll_limits_selection_without_optimistic_results() {
    let (root, actions) = render(PollState {
        counts: vec![1, 2, 0],
        voters: 4,
        ..ready(2)
    });
    let choices = controls::<gtk::CheckButton>(root.upcast_ref());
    let bars = controls::<gtk::ProgressBar>(root.upcast_ref());
    let before: Vec<_> = bars.iter().map(|bar| bar.fraction()).collect();
    assert_eq!(before, [0.25, 0.5, 0.0]);
    choices[2].set_active(true);
    choices[0].set_active(true);
    choices[1].set_active(true);
    assert!(!choices[1].is_active(), "third selection exceeds the limit");
    assert!(actions.borrow().is_empty());
    submit(root.upcast_ref()).emit_clicked();
    assert_poll_action(&actions.borrow()[0], &[0, 2]);
    submit(root.upcast_ref()).emit_clicked();
    assert_eq!(
        actions.borrow().len(),
        2,
        "unconfirmed votes remain retryable"
    );
    assert_poll_action(&actions.borrow()[1], &[0, 2]);
    assert_eq!(
        bars.iter().map(|bar| bar.fraction()).collect::<Vec<_>>(),
        before
    );
    let labels: Vec<_> = controls::<gtk::Label>(root.upcast_ref())
        .iter()
        .map(|label| label.text().to_string())
        .collect();
    for expected in ["1 vote", "2 votes", "0 votes", "4 votes"] {
        assert!(labels.iter().any(|label| label == expected));
    }
}

#[test]
#[ignore = "needs a display"]
fn unavailable_poll_rejects_programmatic_submission() {
    for state in [
        PollState {
            can_vote: false,
            ..ready(1)
        },
        ready(0),
    ] {
        let (root, actions) = render(state);
        let choices = controls::<gtk::CheckButton>(root.upcast_ref());
        assert!(choices.iter().all(|choice| !choice.is_sensitive()));
        choices[0].set_active(true);
        submit(root.upcast_ref()).emit_clicked();
        assert!(actions.borrow().is_empty());
        assert!(
            controls::<gtk::Label>(root.upcast_ref())
                .iter()
                .any(|label| label.text().contains("not ready"))
        );
    }
}

#[test]
#[ignore = "needs a display"]
fn saved_poll_vote_can_be_updated_and_removed() {
    let (root, actions) = render(PollState {
        selected: vec![0],
        counts: vec![2, 1, 0],
        voters: 3,
        ..ready(1)
    });
    let choices = controls::<gtk::CheckButton>(root.upcast_ref());
    let vote = submit(root.upcast_ref());
    assert!(choices[0].is_active());
    assert!(!vote.is_sensitive());
    choices[1].set_active(true);
    assert_eq!(vote.label().as_deref(), Some("Update vote"));
    vote.emit_clicked();
    assert_poll_action(&actions.borrow()[0], &[1]);
    let remove = controls::<gtk::Button>(root.upcast_ref())
        .into_iter()
        .find(|button| button.label().as_deref() == Some("Remove vote"))
        .expect("saved single-choice votes need an explicit removal control");
    remove.emit_clicked();
    assert_poll_action(&actions.borrow()[1], &[]);
}

#[test]
#[ignore = "needs a display"]
fn ordinary_poll_preserves_unsent_choices_across_result_updates() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let draft = Rc::new(RefCell::new(None));
    let first = super::widget::build_media_widget_with_poll_choices(
        &fixture(ready(2)),
        |_| {},
        draft.clone(),
    );
    controls::<gtk::CheckButton>(first.widget.upcast_ref())[2].set_active(true);
    let updated = super::widget::build_media_widget_with_poll_choices(
        &fixture(PollState {
            counts: vec![4, 2, 1],
            voters: 6,
            ..ready(2)
        }),
        |_| {},
        draft,
    );
    assert!(controls::<gtk::CheckButton>(updated.widget.upcast_ref())[2].is_active());
    assert!(submit(updated.widget.upcast_ref()).is_sensitive());
    assert_eq!(
        controls::<gtk::ProgressBar>(updated.widget.upcast_ref())[0].fraction(),
        4.0 / 6.0
    );
}

#[test]
#[ignore = "needs a display"]
fn quiz_hides_results_until_answered_and_preserves_typed_action() {
    let (root, actions) = render(PollState {
        quiz: true,
        correct: Some(2),
        counts: vec![7, 4, 9],
        voters: 20,
        ..ready(1)
    });
    assert!(controls::<gtk::ProgressBar>(root.upcast_ref()).is_empty());
    assert!(
        controls::<gtk::Label>(root.upcast_ref())
            .iter()
            .all(|label| { !label.text().contains('✓') && !label.text().contains("votes") })
    );
    for choice in controls::<gtk::CheckButton>(root.upcast_ref()) {
        assert!(
            !gtk::test_accessible_has_property(&choice, gtk::AccessibleProperty::Description),
            "unanswered quiz counts must also be hidden from accessibility"
        );
    }
    controls::<gtk::CheckButton>(root.upcast_ref())[1].set_active(true);
    assert!(actions.borrow().is_empty());
    submit(root.upcast_ref()).emit_clicked();
    assert!(matches!(
        &actions.borrow()[0],
        NativeMediaAction::VoteQuiz { chat, message, choice: 1 }
            if chat == "chat-id" && message == "message-id"
    ));
    let (result, _) = render(PollState {
        quiz: true,
        correct: Some(2),
        selected: vec![1],
        counts: vec![7, 4, 9],
        voters: 20,
        ..ready(1)
    });
    assert!(controls::<gtk::CheckButton>(result.upcast_ref()).is_empty());
    assert!(controls::<gtk::Button>(result.upcast_ref()).is_empty());
    assert_eq!(controls::<gtk::ProgressBar>(result.upcast_ref()).len(), 3);
    assert!(
        controls::<gtk::Label>(result.upcast_ref())
            .iter()
            .any(|label| label.text() == "Incorrect answer")
    );
}

#[test]
#[ignore = "needs a display"]
fn render_poll_cards_at_narrow_width() {
    gtk::init().unwrap();
    adw::init().unwrap();
    let theme = std::env::var("ZAPTIDE_POLL_THEME").unwrap_or_else(|_| "light".into());
    let dark = theme == "dark";
    let manager = adw::StyleManager::default();
    manager.set_color_scheme(if dark {
        adw::ColorScheme::ForceDark
    } else {
        adw::ColorScheme::ForceLight
    });
    if theme == "high-contrast" {
        assert!(manager.is_high_contrast(), "use ADW_DEBUG_HIGH_CONTRAST=1");
    }
    let palette = if dark {
        crate::theme::Palette::dark()
    } else {
        crate::theme::Palette::light()
    };
    let css = gtk::CssProvider::new();
    css.load_from_string(&format!(
        "{}\n{}",
        crate::native_theme::css_for_palette(&palette),
        include_str!("../../data/style.css")
    ));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let directory = std::path::PathBuf::from(
        std::env::var("ZAPTIDE_POLL_SHOTS")
            .expect("set ZAPTIDE_POLL_SHOTS for synthetic artifacts"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    for (direction, selected) in [("incoming", vec![]), ("outgoing", vec![1])] {
        let mut message = fixture(PollState {
            selected,
            counts: vec![1, 3, 0],
            voters: 4,
            ..ready(1)
        });
        if let Content::Poll {
            question, options, ..
        } = &mut message.content
        {
            *question =
                "Which time works best for our fictional neighborhood planning workshop next week?"
                    .into();
            options[0] =
                "Morning session with a longer description that must wrap inside a narrow bubble"
                    .into();
            options[1] = "Afternoon session after the community lunch".into();
        }
        let rendered = build_media_widget_with_action(&message, |_| {});
        rendered.widget.add_css_class("zaptide-bubble");
        rendered.widget.add_css_class(direction);
        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.set_margin_top(12);
        frame.set_margin_bottom(12);
        frame.set_margin_start(12);
        frame.set_margin_end(12);
        frame.append(&rendered.widget);
        let window = adw::Window::builder()
            .default_width(350)
            .default_height(560)
            .content(&frame)
            .build();
        window.present();
        let context = gtk::glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_millis(350);
        while std::time::Instant::now() < until {
            context.iteration(false);
        }
        assert_eq!(
            window.width(),
            350,
            "long text must not force window expansion"
        );
        for choice in controls::<gtk::CheckButton>(rendered.widget.upcast_ref()) {
            assert!(choice.width() <= rendered.widget.width());
            assert!(choice.is_focusable());
        }
        let paintable = gtk::WidgetPaintable::new(Some(&window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            &snapshot,
            f64::from(window.width()),
            f64::from(window.height()),
        );
        let texture = window
            .native()
            .unwrap()
            .renderer()
            .unwrap()
            .render_texture(snapshot.to_node().expect("rendered poll"), None);
        std::fs::write(
            directory.join(format!("poll-{theme}-{direction}.png")),
            texture.save_to_png_bytes(),
        )
        .unwrap();
        window.close();
    }
}
