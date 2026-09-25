//! Toolkit-neutral adapters for native controls that dispatch model actions.

use crate::model::{Action, ChatId, PollDraft};

/// Builds a reaction action for an explicit chat and message.
pub fn react(
    chat: impl Into<ChatId>,
    message: impl Into<String>,
    emoji: impl Into<String>,
) -> Action {
    Action::React {
        chat: chat.into(),
        message: message.into(),
        emoji: emoji.into(),
    }
}

/// Builds a forward action without deriving its source from current UI state.
pub fn forward(
    from_chat: impl Into<ChatId>,
    message: impl Into<String>,
    to_chat: impl Into<ChatId>,
) -> Action {
    Action::Forward {
        from_chat: from_chat.into(),
        message: message.into(),
        to_chat: to_chat.into(),
    }
}

/// Builds a delete-for-everyone action for an explicit chat and message.
pub fn delete_for_everyone(chat: impl Into<ChatId>, message: impl Into<String>) -> Action {
    Action::DeleteForEveryone {
        chat: chat.into(),
        message: message.into(),
    }
}

/// Builds a local-delete action for an explicit chat and message.
pub fn delete_for_me(chat: impl Into<ChatId>, message: impl Into<String>) -> Action {
    Action::DeleteForMe {
        chat: chat.into(),
        message: message.into(),
    }
}

/// Builds a poll-creation action for an explicit chat.
pub fn create_poll(chat: impl Into<ChatId>, draft: PollDraft) -> Action {
    Action::CreatePoll {
        chat: chat.into(),
        draft,
    }
}

/// Builds a poll-vote action for an explicit chat and poll message.
pub fn vote_poll(
    chat: impl Into<ChatId>,
    message: impl Into<String>,
    choices: Vec<usize>,
) -> Action {
    Action::VotePoll {
        chat: chat.into(),
        message: message.into(),
        choices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaction_preserves_explicit_chat_message_and_emoji() {
        assert_eq!(
            react("group@g.us", "message-7", "🦀"),
            Action::React {
                chat: "group@g.us".into(),
                message: "message-7".into(),
                emoji: "🦀".into(),
            }
        );
    }

    #[test]
    fn forward_preserves_source_message_and_destination() {
        assert_eq!(
            forward("source@g.us", "message-7", "destination@s.whatsapp.net"),
            Action::Forward {
                from_chat: "source@g.us".into(),
                message: "message-7".into(),
                to_chat: "destination@s.whatsapp.net".into(),
            }
        );
    }

    #[test]
    fn delete_adapters_preserve_explicit_chat_and_message() {
        assert_eq!(
            delete_for_everyone("group@g.us", "outgoing-1"),
            Action::DeleteForEveryone {
                chat: "group@g.us".into(),
                message: "outgoing-1".into(),
            }
        );
        assert_eq!(
            delete_for_me("person@s.whatsapp.net", "incoming-2"),
            Action::DeleteForMe {
                chat: "person@s.whatsapp.net".into(),
                message: "incoming-2".into(),
            }
        );
    }

    #[test]
    fn poll_adapters_preserve_chat_message_draft_and_choices() {
        let draft = PollDraft {
            question: "Lunch?".into(),
            options: vec!["Soup".into(), "Salad".into()],
            multiple: false,
        };
        assert_eq!(
            create_poll("group@g.us", draft.clone()),
            Action::CreatePoll {
                chat: "group@g.us".into(),
                draft,
            }
        );
        assert_eq!(
            vote_poll("group@g.us", "poll-3", vec![1, 0]),
            Action::VotePoll {
                chat: "group@g.us".into(),
                message: "poll-3".into(),
                choices: vec![1, 0],
            }
        );
    }
}
