//! Pinned messages: the bar above the conversation and the menu actions.

use super::*;
use crate::model::Content;

/// What the message menu offers for pinning, in the order WhatsApp lists it.
pub(super) const PIN_CHOICES: [u32; 3] = [86_400, 604_800, 2_592_000];

/// How a pin duration reads in the menu.
pub(super) fn pin_choice_label(seconds: u32) -> String {
    match seconds {
        86_400 => gettext("24 hours"),
        604_800 => gettext("7 days"),
        2_592_000 => gettext("30 days"),
        other => gettext("{seconds} seconds").replace("{seconds}", &other.to_string()),
    }
}

/// Whether a message can be pinned: it needs a body that is still there.
pub(super) fn can_pin(message: &crate::model::Message) -> bool {
    !matches!(
        message.content,
        Content::Revoked
            | Content::System { .. }
            | Content::Call { .. }
            | Content::Unsupported { .. }
    )
}

impl NativeApplication {
    pub(super) fn pinned_loaded(&mut self, chat: &str, pin: Option<crate::archive::PinnedMessage>) {
        if self.open_chat.active_chat.as_deref() != Some(chat) {
            return;
        }
        self.open_chat.pinned = pin;
    }

    /// Whether the message in the menu is the one the bar shows.
    pub(super) fn is_pinned(&self, id: &str) -> bool {
        self.open_chat
            .pinned
            .as_ref()
            .is_some_and(|pin| pin.message.id == id)
    }

    /// The text of the pinned bar: who sent it and what it says.
    pub(super) fn pinned_text(&self) -> String {
        let Some(pin) = &self.open_chat.pinned else {
            return String::new();
        };
        let message = &pin.message;
        let summary = message_text(message).unwrap_or_else(|| message.content.summary());
        let summary = summary.lines().next().unwrap_or_default().to_owned();
        let who = if message.from_me {
            gettext("You")
        } else {
            message
                .sender_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_default()
        };
        if who.is_empty() {
            summary
        } else {
            format!("{who}: {summary}")
        }
    }

    pub(super) fn pin_selected(&mut self, seconds: u32) {
        let Some((chat, id)) = self
            .open_chat
            .active_chat
            .clone()
            .zip(self.selected_message_id())
        else {
            return;
        };
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::PinMessage { chat, id, seconds });
        }
    }

    pub(super) fn unpin_selected(&mut self) {
        let Some((chat, id)) = self
            .open_chat
            .active_chat
            .clone()
            .zip(self.selected_message_id())
        else {
            return;
        };
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::UnpinMessage { chat, id });
        }
    }

    /// Scrolls to the pinned message, loading older history first if needed.
    pub(super) fn jump_to_pinned(&mut self) {
        let Some(pin) = self.open_chat.pinned.clone() else {
            return;
        };
        let id = pin.message.id;
        if self.open_chat.message_ids.contains(&id) {
            self.scroll_message_into_view(&id);
            return;
        }
        // The pinned message is older than what is loaded: page back from the
        // oldest message on screen, as following a quote does.
        let Some(oldest) = self
            .open_chat
            .message_ids
            .first()
            .and_then(|first| self.open_chat.message_snapshots.get(first))
            .map(|message| (message.timestamp, message.id.clone()))
        else {
            return;
        };
        if let Some(backend) = &self.backend {
            self.pending_quote_navigation = Some((pin.message.chat.clone(), id.clone()));
            backend.send(crate::backend::Command::LoadUntil {
                chat: pin.message.chat,
                id,
                before: oldest,
            });
            self.status = gettext("Loading pinned message");
        }
    }
}

impl NativeApplication {
    /// The timer picked in the chat details, for the chat that is open.
    pub(super) fn set_disappearing(&mut self, seconds: u32) {
        let Some(chat) = self.open_chat.active_chat.clone() else {
            return;
        };
        let current = self
            .chat_snapshots
            .iter()
            .find(|snapshot| snapshot.id == chat)
            .map_or(0, |snapshot| snapshot.ephemeral_expiration.unwrap_or(0));
        if current == seconds {
            return;
        }
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::SetDisappearing { chat, seconds });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(content: Content) -> crate::model::Message {
        let mut message = crate::application::tests::grouping_message("m", "Alice", false, 1);
        message.content = content;
        message
    }

    #[test]
    fn only_messages_with_a_body_can_be_pinned() {
        assert!(can_pin(&message(Content::text("synthetic"))));
        assert!(!can_pin(&message(Content::Revoked)));
        assert!(!can_pin(&message(Content::System {
            event: crate::model::SystemEvent::Pinned
        })));
    }

    #[test]
    fn the_picker_follows_who_may_change_the_timer() {
        let direct = crate::model::Chat::new("15551234567@s.whatsapp.net".into(), "Ana".into());
        assert_eq!(
            super::super::dialogs::timer_picker_position(&direct, 0),
            Some(0)
        );
        assert_eq!(
            super::super::dialogs::timer_picker_position(&direct, 604_800),
            Some(2)
        );
        // A value WhatsApp offers no choice for is shown, not edited.
        assert_eq!(
            super::super::dialogs::timer_picker_position(&direct, 3_600),
            None
        );
        let mut group = crate::model::Chat::new("123-456@g.us".into(), "Team".into());
        assert_eq!(
            super::super::dialogs::timer_picker_position(&group, 0),
            None
        );
        group.admin = true;
        assert_eq!(
            super::super::dialogs::timer_picker_position(&group, 0),
            Some(0)
        );
    }

    #[test]
    fn pin_choices_match_what_the_backend_accepts() {
        assert_eq!(PIN_CHOICES, [86_400, 604_800, 2_592_000]);
        for seconds in PIN_CHOICES {
            assert!(!pin_choice_label(seconds).contains("seconds"));
        }
    }
}
