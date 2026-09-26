//! GNOME notifications with in-memory chat activation routing.
//!
//! A notification shows the chat, a message preview, and the chat picture.
//! Chat identifiers stay in this process behind random tokens and are reached
//! only after the user activates a notification.

use std::collections::{HashMap, VecDeque};

use relm4::gtk;
use relm4::gtk::prelude::*;

const OPEN_CHAT_ACTION: &str = "open-chat";
const MAX_PENDING_ACTIVATIONS: usize = 256;

#[derive(Default)]
pub struct ActivationTokens {
    chats: HashMap<String, String>,
    order: VecDeque<String>,
}

impl ActivationTokens {
    fn insert(&mut self, token: String, chat_id: String) -> Option<String> {
        if let Some(previous_chat) = self.chats.insert(token.clone(), chat_id) {
            self.order.retain(|existing| existing != &token);
            self.order.push_back(token);
            return Some(previous_chat);
        }

        self.order.push_back(token);
        None
    }

    fn activate(&mut self, token: &str) -> Option<String> {
        self.order.retain(|existing| existing != token);
        self.chats.remove(token)
    }

    fn remove_chat(&mut self, chat_id: &str) -> Vec<String> {
        let tokens = self
            .order
            .iter()
            .filter(|token| self.chats.get(*token).is_some_and(|chat| chat == chat_id))
            .cloned()
            .collect::<Vec<_>>();
        for token in &tokens {
            self.chats.remove(token);
        }
        self.order.retain(|token| self.chats.contains_key(token));
        tokens
    }

    /// The pending token of a chat, so a newer message replaces its notification.
    fn token_for(&self, chat_id: &str) -> Option<String> {
        self.order
            .iter()
            .find(|token| self.chats.get(*token).is_some_and(|chat| chat == chat_id))
            .cloned()
    }

    fn take_oldest(&mut self) -> Option<String> {
        let token = self.order.pop_front()?;
        self.chats.remove(&token);
        Some(token)
    }
}

/// GNOME notification sender that routes explicit notification activation to a chat.
///
/// Construct and use on the GTK main thread. The callback runs on that same thread
/// after a user activates a notification. Tokens are process-local and one-shot.
pub struct NativeNotifications {
    application: gtk::Application,
    tokens: std::rc::Rc<std::cell::RefCell<ActivationTokens>>,
}

impl NativeNotifications {
    /// Registers the notification activation action on `application`.
    pub fn new(application: &gtk::Application, open_chat: impl Fn(String) + 'static) -> Self {
        use gtk::gio::prelude::*;

        let tokens = std::rc::Rc::new(std::cell::RefCell::new(ActivationTokens::default()));
        let action_tokens = tokens.clone();
        let action =
            gtk::gio::SimpleAction::new(OPEN_CHAT_ACTION, Some(&String::static_variant_type()));
        action.connect_activate(move |_, parameter| {
            let Some(token) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            if let Some(chat_id) = action_tokens.borrow_mut().activate(&token) {
                open_chat(chat_id);
            }
        });
        application.add_action(&action);

        Self {
            application: application.clone(),
            tokens,
        }
    }

    /// Shows or replaces the chat's notification; the chat ID stays in the
    /// in-memory token map.
    pub fn show(
        &self,
        chat_id: &str,
        title: &str,
        body: &str,
        icon: Option<&std::path::Path>,
    ) -> Result<(), getrandom::Error> {
        use gtk::gio::prelude::*;

        // Unit/Xvfb harnesses may construct a GTK application without a session
        // bus. Real desktop notification activation requires registration.
        if !self.application.is_registered() {
            return Ok(());
        }

        let existing = self.tokens.borrow().token_for(chat_id);
        let token = match existing {
            Some(token) => token,
            None => activation_token()?,
        };
        let evicted = {
            let mut tokens = self.tokens.borrow_mut();
            tokens.insert(token.clone(), chat_id.to_owned());
            if tokens.chats.len() > MAX_PENDING_ACTIVATIONS {
                tokens.take_oldest()
            } else {
                None
            }
        };
        if let Some(evicted) = evicted {
            self.application.withdraw_notification(&evicted);
        }

        let notification = gtk::gio::Notification::new(title);
        notification.set_body(Some(body));
        if let Some(icon) = icon {
            notification.set_icon(&gtk::gio::FileIcon::new(&gtk::gio::File::for_path(icon)));
        }
        notification.set_default_action_and_target_value(
            &format!("app.{OPEN_CHAT_ACTION}"),
            Some(&token.to_variant()),
        );
        self.application
            .send_notification(Some(&token), &notification);
        Ok(())
    }

    /// Withdraws pending notifications and activation tokens for one chat.
    pub fn clear_chat(&self, chat_id: &str) {
        for token in self.tokens.borrow_mut().remove_chat(chat_id) {
            self.application.withdraw_notification(&token);
        }
    }
}

pub fn activation_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        // Writing to a String cannot fail.
        let _ = write!(token, "{byte:02x}");
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::ActivationTokens;

    #[test]
    fn a_chat_reuses_its_pending_token_so_notifications_replace() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("first".into(), "chat-a".into());
        tokens.insert("other".into(), "chat-b".into());
        assert_eq!(tokens.token_for("chat-a").as_deref(), Some("first"));
        tokens.activate("first");
        assert_eq!(tokens.token_for("chat-a"), None);
    }

    #[test]
    fn live_token_routes_to_intended_chat() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("test-token-alpha".into(), "chat-intended-42".into());

        assert_eq!(
            tokens.activate("test-token-alpha"),
            Some("chat-intended-42".into())
        );
    }

    #[test]
    fn expired_token_presents_chat_list() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("expired-token".into(), "chat-old".into());

        assert!(tokens.activate("expired-token").is_some());
        assert_eq!(tokens.activate("expired-token"), None);
    }

    #[test]
    fn token_never_exposes_message_content() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("token-one".into(), "chat-abc".into());
        tokens.insert("token-two".into(), "chat-abc".into());
        assert_eq!(tokens.activate("token-one"), tokens.activate("token-two"));

        let mut fresh = ActivationTokens::default();
        fresh.insert("alpha".into(), "same-chat".into());
        fresh.insert("beta".into(), "same-chat".into());
        assert_eq!(fresh.activate("alpha"), Some("same-chat".into()));
        assert_eq!(fresh.activate("beta"), Some("same-chat".into()));
    }

    #[test]
    fn token_is_opaque_in_memory() {
        let token1 = super::activation_token().unwrap();
        let token2 = super::activation_token().unwrap();

        assert_eq!(token1.len(), 32);
        assert_eq!(token2.len(), 32);
        assert!(token1.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(token2.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!token1.contains('@'));
        assert!(!token1.contains("s.whatsapp.net"));
        assert_ne!(token1, token2);
    }

    #[test]
    fn repeated_activation_is_idempotent() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("idempotent-token".into(), "chat-singleton".into());

        assert_eq!(
            tokens.activate("idempotent-token"),
            Some("chat-singleton".into())
        );
        assert_eq!(tokens.activate("idempotent-token"), None);
        assert_eq!(tokens.activate("idempotent-token"), None);
    }

    #[test]
    fn notification_after_quit_is_rejected() {
        let mut old_session_tokens = ActivationTokens::default();
        old_session_tokens.insert("stale-token-from-prev-session".into(), "old-chat".into());

        let mut new_session_tokens = ActivationTokens::default();
        assert_eq!(
            new_session_tokens.activate("stale-token-from-prev-session"),
            None
        );
        assert_eq!(
            old_session_tokens.activate("stale-token-from-prev-session"),
            Some("old-chat".into())
        );
    }

    #[test]
    fn activation_is_one_shot_and_routes_only_to_mapped_chat() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("opaque-token".into(), "private-chat-id".into());

        assert_eq!(tokens.activate("unknown-token"), None);
        assert_eq!(
            tokens.activate("opaque-token"),
            Some("private-chat-id".into())
        );
        assert_eq!(tokens.activate("opaque-token"), None);
    }

    #[test]
    fn clearing_chat_invalidates_all_its_tokens_only() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("first".into(), "chat-a".into());
        tokens.insert("second".into(), "chat-a".into());
        tokens.insert("third".into(), "chat-b".into());

        assert_eq!(tokens.remove_chat("chat-a"), ["first", "second"]);
        assert_eq!(tokens.activate("first"), None);
        assert_eq!(tokens.activate("third"), Some("chat-b".into()));
    }

    #[test]
    fn capacity_eviction_invalidates_oldest_token() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("oldest".into(), "chat-a".into());
        tokens.insert("newest".into(), "chat-b".into());

        assert_eq!(tokens.take_oldest(), Some("oldest".into()));
        assert_eq!(tokens.activate("oldest"), None);
        assert_eq!(tokens.activate("newest"), Some("chat-b".into()));
    }
}
