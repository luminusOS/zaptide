use std::{cell::RefCell, rc::Rc};

use super::*;
use crate::i18n::gettext;

pub(crate) type PollChoices = Rc<RefCell<Option<Vec<usize>>>>;

fn vote_count(count: usize) -> String {
    let text = if count == 1 {
        gettext("{count} vote")
    } else {
        gettext("{count} votes")
    };
    text.replace("{count}", &count.to_string())
}

fn vote_label(saved: &[usize], choices: &[usize]) -> String {
    if saved.is_empty() {
        gettext("Vote")
    } else if choices.is_empty() {
        gettext("Remove vote")
    } else {
        gettext("Update vote")
    }
}

pub(super) fn append(
    root: &gtk::Box,
    message: &Message,
    draft: PollChoices,
    on_action: Rc<dyn Fn(NativeMediaAction)>,
) {
    let Content::Poll {
        question,
        options,
        state,
    } = &message.content
    else {
        return;
    };
    root.add_css_class("zaptide-poll");
    let kind_text = if state.quiz {
        gettext("Quiz")
    } else {
        gettext("Poll")
    };
    let kind = add_label(root, &kind_text);
    kind.add_css_class("caption-heading");
    kind.add_css_class("dim-label");
    let question = add_label(root, question);
    question.add_css_class("heading");
    question.set_max_width_chars(34);
    let answered_quiz = state.quiz && !state.selected.is_empty();
    let limit = state.selectable.min(options.len());
    let limit = if state.quiz { limit.min(1) } else { limit };
    let can_vote = state.can_vote && limit > 0 && !answered_quiz;
    let hint = if answered_quiz {
        match state.correct {
            Some(index) if state.selected.contains(&index) => gettext("Correct answer"),
            Some(_) => gettext("Incorrect answer"),
            None => gettext("Answer submitted"),
        }
    } else if !can_vote {
        gettext("This poll is not ready for voting. Reconnect and try again.")
    } else if limit == 1 {
        gettext("Choose one answer")
    } else {
        gettext("Select up to {count} answers").replace("{count}", &limit.to_string())
    };
    let hint = add_label(root, &hint);
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    let saved = state.selected.clone();
    let initial = draft.borrow().clone().unwrap_or_else(|| saved.clone());
    let vote = gtk::Button::with_label(&vote_label(&saved, &initial));
    vote.add_css_class("suggested-action");
    vote.set_sensitive(can_vote && initial != saved);
    let denominator = state
        .counts
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .max(state.voters);
    let mut first: Option<gtk::CheckButton> = None;
    for (index, option) in options.iter().enumerate() {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_hexpand(true);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let text = gtk::Label::builder()
            .label(option)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(28)
            .build();
        if answered_quiz && state.correct == Some(index) {
            text.set_label(&format!("✓ {option}"));
        }
        line.append(&text);
        let count = state.counts.get(index).copied().unwrap_or(0);
        let show_results = !state.quiz || answered_quiz;
        if show_results {
            let count_label = gtk::Label::new(Some(&vote_count(count)));
            count_label.add_css_class("caption");
            line.append(&count_label);
        }
        content.append(&line);
        if show_results {
            let bar = gtk::ProgressBar::new();
            bar.set_fraction(if denominator == 0 {
                0.0
            } else {
                count as f64 / denominator as f64
            });
            content.append(&bar);
        }
        if answered_quiz {
            content.add_css_class("zaptide-poll-option");
            if saved.contains(&index) {
                content.add_css_class("selected");
            }
            root.append(&content);
            continue;
        }
        let button = gtk::CheckButton::builder()
            .child(&content)
            .css_classes(["zaptide-poll-option"])
            .sensitive(can_vote)
            .build();
        button.update_property(&[gtk::accessible::Property::Label(option)]);
        if show_results {
            button.update_property(&[gtk::accessible::Property::Description(&vote_count(count))]);
        }
        if limit == 1 {
            if let Some(first) = &first {
                button.set_group(Some(first));
            } else {
                first = Some(button.clone());
            }
        }
        button.set_active(initial.contains(&index));
        let (draft, saved, vote) = (draft.clone(), saved.clone(), vote.downgrade());
        button.connect_toggled(move |button| {
            let mut selected = draft.borrow().clone().unwrap_or_else(|| saved.clone());
            if button.is_active() {
                if limit == 1 {
                    selected.clear();
                }
                if !selected.contains(&index) {
                    selected.push(index);
                }
                if selected.len() > limit {
                    button.set_active(false);
                    return;
                }
            } else {
                selected.retain(|&choice| choice != index);
            }
            selected.sort_unstable();
            *draft.borrow_mut() = Some(selected.clone());
            if let Some(vote) = vote.upgrade() {
                vote.set_label(&vote_label(&saved, &selected));
                vote.set_sensitive(can_vote && selected != saved);
            }
        });
        root.append(&button);
    }
    let remove = if !state.quiz && !saved.is_empty() && limit == 1 {
        let remove = gtk::Button::with_label(&gettext("Remove vote"));
        remove.set_sensitive(can_vote);
        let (chat, id, on_action) = (message.chat.clone(), message.id.clone(), on_action.clone());
        remove.connect_clicked(move |button| {
            if can_vote && button.is_sensitive() {
                on_action(NativeMediaAction::VotePoll {
                    chat: chat.clone(),
                    message: id.clone(),
                    choices: Vec::new(),
                });
            }
        });
        Some(remove)
    } else {
        None
    };
    if !state.quiz || !answered_quiz {
        let (chat, id, saved, quiz) = (message.chat.clone(), message.id.clone(), saved, state.quiz);
        vote.connect_clicked(move |button| {
            if !can_vote || !button.is_sensitive() {
                return;
            }
            let choices = draft.borrow().clone().unwrap_or_else(|| saved.clone());
            if choices == saved || choices.len() > limit {
                return;
            }
            if quiz {
                if let Some(&choice) = choices.first() {
                    on_action(NativeMediaAction::VoteQuiz {
                        chat: chat.clone(),
                        message: id.clone(),
                        choice,
                    });
                }
            } else {
                on_action(NativeMediaAction::VotePoll {
                    chat: chat.clone(),
                    message: id.clone(),
                    choices,
                });
            }
        });
        root.append(&vote);
    }
    if let Some(remove) = remove {
        root.append(&remove);
    }
    if !state.quiz || answered_quiz {
        let summary = add_label(root, &vote_count(state.voters));
        summary.add_css_class("caption");
        summary.add_css_class("dim-label");
        if answered_quiz
            && let Some(answer) = state.selected.first().and_then(|&index| options.get(index))
        {
            let answer = add_label(
                root,
                &gettext("Your answer: {answer}").replace("{answer}", answer),
            );
            answer.add_css_class("caption");
        }
        if !state.history_complete {
            let incomplete = add_label(root, &gettext("Results may be incomplete"));
            incomplete.add_css_class("caption");
            incomplete.add_css_class("dim-label");
        }
    }
}
