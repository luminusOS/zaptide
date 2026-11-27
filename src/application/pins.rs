//! Pinned messages: the bar above the conversation and the menu actions.

use super::*;
use crate::model::Content;
use std::collections::HashSet;

pub(super) struct OpenChatPins {
    featured: crate::archive::PinnedMessage,
    active_ids: HashSet<String>,
}

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
    pub(super) fn pinned_loaded(
        &mut self,
        chat: &str,
        pin: Option<crate::archive::PinnedMessage>,
        active_ids: HashSet<String>,
    ) {
        if self.open_chat.active_chat.as_deref() != Some(chat) {
            return;
        }
        self.open_chat.pinned = pin.map(|featured| OpenChatPins {
            featured,
            active_ids,
        });
    }

    /// Whether the menu message is an active pin, including non-featured pins.
    pub(super) fn is_pinned(&self, id: &str) -> bool {
        self.open_chat
            .pinned
            .as_ref()
            .is_some_and(|pins| pins.active_ids.contains(id))
    }

    /// The text of the pinned bar: who sent it and what it says.
    pub(super) fn pinned_text(&self) -> String {
        let Some(pin) = &self.open_chat.pinned else {
            return String::new();
        };
        let message = &pin.featured.message;
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
        let Some(pin) = self.open_chat.pinned.as_ref() else {
            return;
        };
        let id = pin.featured.message.id.clone();
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
            let chat = pin.featured.message.chat.clone();
            self.pending_quote_navigation = Some((chat.clone(), id.clone()));
            backend.send(crate::backend::Command::LoadUntil {
                chat,
                id,
                before: oldest,
            });
            self.status = gettext("Loading pinned message");
        }
    }
}

impl NativeApplication {
    pub(super) fn set_disappearing(&self, target: ChatTarget, seconds: u32) {
        let Some(chat) = self.targeted_chat(&target) else {
            return;
        };
        if super::dialogs::timer_picker_position(chat, seconds).is_some()
            && let Some(backend) = &self.backend
        {
            backend.send(crate::backend::Command::SetDisappearing {
                chat: target.chat,
                seconds,
            });
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

    fn has_menu_action(menu: &gtk::gio::MenuModel, action: &str) -> bool {
        (0..menu.n_items()).any(|index| {
            menu.item_attribute_value(index, "action", None)
                .is_some_and(|value| value.str() == Some(action))
                || ["section", "submenu"].into_iter().any(|link| {
                    menu.item_link(index, link)
                        .is_some_and(|nested| has_menu_action(&nested, action))
                })
        })
    }

    #[test]
    #[ignore = "needs a display"]
    fn multiple_pins_offer_unpin_in_the_menu_but_feature_only_the_newest() {
        let data = tempfile::tempdir().unwrap();
        let _application: RelmApp<Input> = RelmApp::new("dev.luminusos.ZapTide.PinMenuTest");
        let app = NativeApplication::builder().launch(Init {
            dirs: crate::paths::AppDirs::under(data.path()),
        });
        let mut state = app.state().get_mut();
        let model = &mut state.model;
        model.open_chat.active_chat = Some("chat".into());
        for id in ["older", "newest", "plain"] {
            let row = crate::application::tests::grouping_message(id, "Alice", false, 1);
            model.open_chat.message_snapshots.insert(id.into(), row);
        }
        let featured = crate::archive::PinnedMessage {
            message: model.open_chat.message_snapshots["newest"].clone(),
            pinned_by: "Alice".into(),
            pinned_at_ms: 2,
            expires_at_ms: None,
        };
        let ids = || HashSet::from(["older".into(), "newest".into()]);
        model.pinned_loaded("chat", Some(featured.clone()), ids());
        assert_eq!(model.pinned_text(), "Alice: newest");

        for (id, pinned) in [("older", true), ("newest", true), ("plain", false)] {
            model.message_target = Some(id.into());
            let menu = model.message_menu_model();
            assert_eq!(
                has_menu_action(menu.upcast_ref(), "message.unpin"),
                pinned,
                "Unpin action for {id}"
            );
            assert_eq!(
                has_menu_action(menu.upcast_ref(), "message.pin"),
                !pinned,
                "Pin action for {id}"
            );
        }

        model.pinned_loaded("chat", None, HashSet::new());
        assert!(model.pinned_text().is_empty());
        assert!(!model.is_pinned("older"));

        model.pinned_loaded("chat", Some(featured.clone()), ids());
        model.chat_ids = vec!["next-chat".into()];
        model.select_chat(0);
        model.pinned_loaded("chat", Some(featured.clone()), ids());
        assert_eq!(model.open_chat.active_chat.as_deref(), Some("next-chat"));
        assert!(model.pinned_text().is_empty());
        assert!(!model.is_pinned("older"));

        model.open_chat.active_chat = Some("chat".into());
        model.pinned_loaded("chat", Some(featured), ids());
        model.leave_screen();
        model.open_chat.active_chat = Some("chat".into());
        assert!(model.pinned_text().is_empty());
        assert!(!model.is_pinned("older"));
    }
}
