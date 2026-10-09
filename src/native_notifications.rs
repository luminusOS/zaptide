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

use crate::account::AccountId;

#[derive(Default)]
pub struct ActivationTokens {
    chats: HashMap<String, (AccountId, String)>,
    order: VecDeque<String>,
}

impl ActivationTokens {
    fn insert(
        &mut self,
        token: String,
        chat_id: (AccountId, String),
    ) -> Option<(AccountId, String)> {
        if let Some(previous_chat) = self.chats.insert(token.clone(), chat_id) {
            self.order.retain(|existing| existing != &token);
            self.order.push_back(token);
            return Some(previous_chat);
        }

        self.order.push_back(token);
        None
    }

    fn activate(&mut self, token: &str) -> Option<(AccountId, String)> {
        self.order.retain(|existing| existing != token);
        self.chats.remove(token)
    }

    fn remove_chat(&mut self, chat_id: &(AccountId, String)) -> Vec<String> {
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
    fn token_for(&self, chat_id: &(AccountId, String)) -> Option<String> {
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

    fn remove_account(&mut self, account: AccountId) -> Vec<String> {
        let tokens = self
            .order
            .iter()
            .filter(|token| self.chats.get(*token).is_some_and(|(id, _)| *id == account))
            .cloned()
            .collect::<Vec<_>>();
        for token in &tokens {
            self.chats.remove(token);
        }
        self.order.retain(|token| self.chats.contains_key(token));
        tokens
    }
}

/// Identifies the receiving account without changing single-account notifications.
pub(crate) fn account_body(
    registry: &crate::account::Registry,
    account: AccountId,
    body: &str,
) -> String {
    if registry.accounts.len() > 1
        && let Some(entry) = registry.entry(account)
    {
        format!("{body}\nTo {}", entry.label())
    } else {
        body.to_owned()
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
    pub fn new(
        application: &gtk::Application,
        open_chat: impl Fn(AccountId, String) + 'static,
    ) -> Self {
        use gtk::gio::prelude::*;

        let tokens = std::rc::Rc::new(std::cell::RefCell::new(ActivationTokens::default()));
        let action_tokens = tokens.clone();
        let action =
            gtk::gio::SimpleAction::new(OPEN_CHAT_ACTION, Some(&String::static_variant_type()));
        action.connect_activate(move |_, parameter| {
            let Some(token) = parameter.and_then(|value| value.get::<String>()) else {
                return;
            };
            if let Some((account, chat_id)) = action_tokens.borrow_mut().activate(&token) {
                open_chat(account, chat_id);
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
        account: AccountId,
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

        let target = (account, chat_id.to_owned());
        let existing = self.tokens.borrow().token_for(&target);
        let token = match existing {
            Some(token) => token,
            None => activation_token()?,
        };
        let evicted = {
            let mut tokens = self.tokens.borrow_mut();
            tokens.insert(token.clone(), target);
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
        // The notification portal drops file icons, so a Flatpak build sends the bytes.
        if let Some(bytes) = icon
            .and_then(round_icon)
            .and_then(|path| std::fs::read(path).ok())
        {
            notification.set_icon(&gtk::gio::BytesIcon::new(&gtk::glib::Bytes::from_owned(
                bytes,
            )));
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
    pub fn clear_chat(&self, account: AccountId, chat_id: &str) {
        for token in self
            .tokens
            .borrow_mut()
            .remove_chat(&(account, chat_id.to_owned()))
        {
            self.application.withdraw_notification(&token);
        }
    }

    /// Invalidates every notification belonging to a removed or signed-out account.
    pub fn clear_account(&self, account: AccountId) {
        for token in self.tokens.borrow_mut().remove_account(account) {
            self.application.withdraw_notification(&token);
        }
    }
}

/// A circular copy of a chat picture, as the desktop shows icons unmasked.
/// Cached beside the source and rebuilt when the source is newer.
fn round_icon(source: &std::path::Path) -> Option<std::path::PathBuf> {
    const SIZE: u32 = 96;
    let target = source.with_extension("round.png");
    let modified =
        |path: &std::path::Path| std::fs::metadata(path).and_then(|meta| meta.modified());
    if let (Ok(round), Ok(original)) = (modified(&target), modified(source))
        && round >= original
    {
        return Some(target);
    }
    let mut image = image::open(source)
        .ok()?
        .resize_to_fill(SIZE, SIZE, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let radius = SIZE as f32 / 2.0;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let distance =
            ((x as f32 + 0.5 - radius).powi(2) + (y as f32 + 0.5 - radius).powi(2)).sqrt();
        // One pixel of falloff keeps the edge smooth.
        let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
        pixel[3] = (f32::from(pixel[3]) * coverage) as u8;
    }
    image.save(&target).ok()?;
    Some(target)
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
    use crate::account::AccountId;

    fn chat(id: &str) -> (AccountId, String) {
        (AccountId(1), id.to_owned())
    }

    #[test]
    fn same_chat_in_two_accounts_keeps_two_notifications() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("one".into(), (AccountId(1), "chat".into()));
        tokens.insert("two".into(), (AccountId(2), "chat".into()));
        assert_eq!(
            tokens.token_for(&(AccountId(1), "chat".into())).as_deref(),
            Some("one")
        );
        assert_eq!(
            tokens.token_for(&(AccountId(2), "chat".into())).as_deref(),
            Some("two")
        );
        assert_eq!(
            tokens.remove_chat(&(AccountId(1), "chat".into())),
            vec!["one".to_owned()]
        );
        assert_eq!(tokens.activate("two"), Some((AccountId(2), "chat".into())));
    }

    #[test]
    fn notification_body_names_the_receiving_account_only_with_several_accounts() {
        let mut registry = crate::account::Registry::default();
        let work = registry.add();
        registry.entry_mut(work).unwrap().name = Some("Work".into());
        assert_eq!(
            super::account_body(&registry, work, "New message"),
            "New message"
        );
        let home = registry.add();
        registry.entry_mut(home).unwrap().name = Some("Home".into());
        assert_eq!(
            super::account_body(&registry, work, "New message"),
            "New message\nTo Work"
        );
        assert_eq!(
            super::account_body(&registry, home, "Hello"),
            "Hello\nTo Home"
        );
        assert_eq!(
            super::account_body(&registry, AccountId(99), "Hello"),
            "Hello"
        );
    }

    #[test]
    fn removing_an_account_invalidates_even_chats_missing_from_its_snapshot() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("one".into(), (AccountId(1), "missing".into()));
        tokens.insert("two".into(), (AccountId(2), "missing".into()));
        tokens.insert("three".into(), (AccountId(1), "other".into()));
        assert_eq!(tokens.remove_account(AccountId(1)), ["one", "three"]);
        assert_eq!(tokens.activate("one"), None);
        assert_eq!(tokens.activate("three"), None);
        assert_eq!(
            tokens.activate("two"),
            Some((AccountId(2), "missing".into()))
        );
    }

    #[test]
    fn chat_pictures_become_circles_with_transparent_corners() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("avatar.png");
        image::RgbaImage::from_pixel(40, 40, image::Rgba([200, 10, 10, 255]))
            .save(&source)
            .unwrap();
        let round = super::round_icon(&source).expect("round icon");
        let image = image::open(&round).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(0, 0)[3], 0, "corner is transparent");
        assert_eq!(image.get_pixel(48, 48)[3], 255, "centre is opaque");
        assert_eq!(
            super::round_icon(&source),
            Some(round),
            "cached copy reused"
        );
    }

    #[test]
    fn a_chat_reuses_its_pending_token_so_notifications_replace() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("first".into(), chat("chat-a"));
        tokens.insert("other".into(), chat("chat-b"));
        assert_eq!(tokens.token_for(&chat("chat-a")).as_deref(), Some("first"));
        tokens.activate("first");
        assert_eq!(tokens.token_for(&chat("chat-a")), None);
    }

    #[test]
    fn live_token_routes_to_intended_chat() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("test-token-alpha".into(), chat("chat-intended-42"));

        assert_eq!(
            tokens.activate("test-token-alpha"),
            Some(chat("chat-intended-42"))
        );
    }

    #[test]
    fn expired_token_presents_chat_list() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("expired-token".into(), chat("chat-old"));

        assert!(tokens.activate("expired-token").is_some());
        assert_eq!(tokens.activate("expired-token"), None);
    }

    #[test]
    fn token_never_exposes_message_content() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("token-one".into(), chat("chat-abc"));
        tokens.insert("token-two".into(), chat("chat-abc"));
        assert_eq!(tokens.activate("token-one"), tokens.activate("token-two"));

        let mut fresh = ActivationTokens::default();
        fresh.insert("alpha".into(), chat("same-chat"));
        fresh.insert("beta".into(), chat("same-chat"));
        assert_eq!(fresh.activate("alpha"), Some(chat("same-chat")));
        assert_eq!(fresh.activate("beta"), Some(chat("same-chat")));
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
        tokens.insert("idempotent-token".into(), chat("chat-singleton"));

        assert_eq!(
            tokens.activate("idempotent-token"),
            Some(chat("chat-singleton"))
        );
        assert_eq!(tokens.activate("idempotent-token"), None);
        assert_eq!(tokens.activate("idempotent-token"), None);
    }

    #[test]
    fn notification_after_quit_is_rejected() {
        let mut old_session_tokens = ActivationTokens::default();
        old_session_tokens.insert("stale-token-from-prev-session".into(), chat("old-chat"));

        let mut new_session_tokens = ActivationTokens::default();
        assert_eq!(
            new_session_tokens.activate("stale-token-from-prev-session"),
            None
        );
        assert_eq!(
            old_session_tokens.activate("stale-token-from-prev-session"),
            Some(chat("old-chat"))
        );
    }

    #[test]
    fn activation_is_one_shot_and_routes_only_to_mapped_chat() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("opaque-token".into(), chat("private-chat-id"));

        assert_eq!(tokens.activate("unknown-token"), None);
        assert_eq!(
            tokens.activate("opaque-token"),
            Some(chat("private-chat-id"))
        );
        assert_eq!(tokens.activate("opaque-token"), None);
    }

    #[test]
    fn clearing_chat_invalidates_all_its_tokens_only() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("first".into(), chat("chat-a"));
        tokens.insert("second".into(), chat("chat-a"));
        tokens.insert("third".into(), chat("chat-b"));

        assert_eq!(tokens.remove_chat(&chat("chat-a")), ["first", "second"]);
        assert_eq!(tokens.activate("first"), None);
        assert_eq!(tokens.activate("third"), Some(chat("chat-b")));
    }

    #[test]
    fn capacity_eviction_invalidates_oldest_token() {
        let mut tokens = ActivationTokens::default();
        tokens.insert("oldest".into(), chat("chat-a"));
        tokens.insert("newest".into(), chat("chat-b"));

        assert_eq!(tokens.take_oldest().as_deref(), Some("oldest"));
        assert_eq!(tokens.activate("oldest"), None);
        assert_eq!(tokens.activate("newest"), Some(chat("chat-b")));
    }
}
