//! GNOME notifications with in-memory chat activation routing.
//!
//! A notification shows the chat, a message preview, and the chat picture.
//! Chat identifiers stay in this process behind random tokens and are reached
//! only after the user activates a notification.

use std::collections::{HashMap, VecDeque};

use relm4::gtk;
use relm4::gtk::prelude::*;

#[cfg(target_os = "linux")]
mod portal;

const OPEN_CHAT_ACTION: &str = "open-chat";
const CALL_ANSWER_ACTION: &str = "call-answer";
const CALL_DECLINE_ACTION: &str = "call-decline";
const CALL_END_AND_ANSWER_ACTION: &str = "call-end-and-answer";
const CALL_FOCUS_ACTION: &str = "call-focus";
const MAX_PENDING_ACTIVATIONS: usize = 256;

use crate::account::AccountId;
use crate::i18n::gettext;

/// What the person chose on a call's notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallChoice {
    Answer,
    Decline,
    EndAndAnswer,
    /// The notification itself was clicked.
    Show,
}

/// Which buttons a call's notification carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallBanner {
    /// The only call: answer or decline it.
    Ringing,
    /// A call that came in during another: decline it or end the first and answer.
    Waiting,
}
use crate::notification_sound::SoundRequest;

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

    /// The target of a token that stays valid, as the buttons of a call's
    /// notification share one.
    fn get(&self, token: &str) -> Option<&(AccountId, String)> {
        self.chats.get(token)
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

/// GNOME notification sender that routes explicit notification activation to a chat.
///
/// Construct and use on the GTK main thread. The callback runs on that same thread
/// after a user activates a notification. Tokens are process-local and one-shot.
pub struct NativeNotifications {
    application: gtk::Application,
    tokens: std::rc::Rc<std::cell::RefCell<ActivationTokens>>,
    calls: std::rc::Rc<std::cell::RefCell<ActivationTokens>>,
    /// Present in a Flatpak, where the portal can carry a sound.
    #[cfg(target_os = "linux")]
    portal: Option<portal::Support>,
}

impl NativeNotifications {
    /// Registers the notification activation action on `application`.
    pub fn new(
        application: &gtk::Application,
        open_chat: impl Fn(AccountId, String) + 'static,
        call_choice: impl Fn(AccountId, String, CallChoice) + 'static,
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

        let calls = std::rc::Rc::new(std::cell::RefCell::new(ActivationTokens::default()));
        let call_choice = std::rc::Rc::new(call_choice);
        for (name, choice) in [
            (CALL_ANSWER_ACTION, CallChoice::Answer),
            (CALL_DECLINE_ACTION, CallChoice::Decline),
            (CALL_END_AND_ANSWER_ACTION, CallChoice::EndAndAnswer),
            (CALL_FOCUS_ACTION, CallChoice::Show),
        ] {
            let action = gtk::gio::SimpleAction::new(name, Some(&String::static_variant_type()));
            let (calls, call_choice) = (calls.clone(), call_choice.clone());
            action.connect_activate(move |_, parameter| {
                let Some(token) = parameter.and_then(|value| value.get::<String>()) else {
                    return;
                };
                let target = calls.borrow().get(&token).cloned();
                if let Some((account, call_id)) = target {
                    call_choice(account, call_id, choice);
                }
            });
            application.add_action(&action);
        }

        Self {
            application: application.clone(),
            tokens,
            calls,
            #[cfg(target_os = "linux")]
            portal: portal::sandboxed().then(Default::default),
        }
    }

    /// Shows or replaces the chat's notification; the chat ID stays in the
    /// in-memory token map. `sound` is passed on only where the desktop can
    /// take it with the notification, and ignored elsewhere.
    pub fn show(
        &self,
        account: AccountId,
        chat_id: &str,
        title: &str,
        body: &str,
        icon: Option<&std::path::Path>,
        sound: SoundRequest,
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

        let bytes = icon_bytes(title, icon);
        let notification = notification(title, body, bytes.as_ref());
        let default_action = format!("app.{OPEN_CHAT_ACTION}");
        notification
            .set_default_action_and_target_value(&default_action, Some(&token.to_variant()));

        #[cfg(target_os = "linux")]
        if let Some(support) = &self.portal {
            support.probe();
            if support.supports_sound() {
                let fallback = (
                    self.application.clone(),
                    notification.clone(),
                    token.clone(),
                );
                portal::add_notification(
                    portal::Request {
                        id: token.clone(),
                        title: title.to_owned(),
                        body: body.to_owned(),
                        icon: bytes.as_deref().map(<[u8]>::to_vec),
                        default_action: default_action.clone(),
                        default_action_target: token.clone(),
                        sound,
                    },
                    move |reason| {
                        log::debug!("the notification portal refused a notification: {reason}");
                        let (application, notification, token) = fallback;
                        application.send_notification(Some(&token), &notification);
                    },
                );
                return Ok(());
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = sound;

        self.application
            .send_notification(Some(&token), &notification);
        Ok(())
    }

    /// Shows or replaces a call's notification. Its buttons act on the call
    /// whatever account it belongs to; the call id stays in the token map.
    pub fn show_call(
        &self,
        account: AccountId,
        call_id: &str,
        banner: CallBanner,
        title: &str,
        body: &str,
        icon: Option<&std::path::Path>,
    ) -> Result<(), getrandom::Error> {
        use gtk::gio::prelude::*;

        if !self.application.is_registered() {
            return Ok(());
        }
        let target = (account, call_id.to_owned());
        let existing = self.calls.borrow().token_for(&target);
        let token = match existing {
            Some(token) => token,
            None => activation_token()?,
        };
        self.calls.borrow_mut().insert(token.clone(), target);

        let notification = notification(title, body, icon_bytes(title, icon).as_ref());
        let value = token.to_variant();
        notification
            .set_default_action_and_target_value(&format!("app.{CALL_FOCUS_ACTION}"), Some(&value));
        notification.add_button_with_target_value(
            &gettext("Decline"),
            &format!("app.{CALL_DECLINE_ACTION}"),
            Some(&value),
        );
        let (label, action) = match banner {
            CallBanner::Ringing => (gettext("Answer"), CALL_ANSWER_ACTION),
            CallBanner::Waiting => (gettext("End and answer"), CALL_END_AND_ANSWER_ACTION),
        };
        notification.add_button_with_target_value(&label, &format!("app.{action}"), Some(&value));
        self.application
            .send_notification(Some(&token), &notification);
        Ok(())
    }

    /// Withdraws a call's notification and forgets its token.
    pub fn clear_call(&self, account: AccountId, call_id: &str) {
        let target = (account, call_id.to_owned());
        for token in self.calls.borrow_mut().remove_chat(&target) {
            self.application.withdraw_notification(&token);
        }
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
        let removed = self.tokens.borrow_mut().remove_account(account);
        let removed_calls = self.calls.borrow_mut().remove_account(account);
        for token in removed.into_iter().chain(removed_calls) {
            self.application.withdraw_notification(&token);
        }
    }
}

/// The picture to send with a notification. The notification portal drops file
/// icons, so a Flatpak build sends the bytes. Without a picture, draw the same
/// coloured initials the chat list shows.
fn icon_bytes(title: &str, icon: Option<&std::path::Path>) -> Option<gtk::glib::Bytes> {
    icon.and_then(round_icon)
        .and_then(|path| std::fs::read(path).ok())
        .map(gtk::glib::Bytes::from_owned)
        .or_else(|| initials_icon(title))
}

fn notification(
    title: &str,
    body: &str,
    icon: Option<&gtk::glib::Bytes>,
) -> gtk::gio::Notification {
    let notification = gtk::gio::Notification::new(title);
    notification.set_body(Some(body));
    if let Some(bytes) = icon {
        notification.set_icon(&gtk::gio::BytesIcon::new(bytes));
    }
    notification
}

/// Coloured initials as PNG bytes, for chats without a picture. Drawn straight
/// into a snapshot because an unshown `AdwAvatar` never gets laid out; the
/// renderer comes from any realized window.
fn initials_icon(title: &str) -> Option<gtk::glib::Bytes> {
    const SIZE: f32 = 96.0;
    const COLOURS: [(f32, f32, f32); 7] = [
        (0.21, 0.52, 0.89),
        (0.15, 0.64, 0.41),
        (0.90, 0.65, 0.04),
        (0.99, 0.47, 0.0),
        (0.88, 0.11, 0.14),
        (0.57, 0.25, 0.67),
        (0.60, 0.46, 0.20),
    ];
    let renderer = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<gtk::Window>().ok())
        .find_map(|window| window.renderer())?;
    let initials: String = {
        let mut words = title
            .split_whitespace()
            .filter_map(|word| word.chars().next());
        let first = words.next();
        let last = words.next_back();
        first
            .into_iter()
            .chain(last)
            .flat_map(char::to_uppercase)
            .collect()
    };
    let (red, green, blue) = COLOURS[title.bytes().map(usize::from).sum::<usize>() % COLOURS.len()];
    let rect = gtk::graphene::Rect::new(0.0, 0.0, SIZE, SIZE);
    let snapshot = gtk::Snapshot::new();
    snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(rect, SIZE / 2.0));
    snapshot.append_color(&gtk::gdk::RGBA::new(red, green, blue, 1.0), &rect);
    snapshot.pop();
    let layout = gtk::Label::new(None).create_pango_layout(Some(&initials));
    layout.set_font_description(Some(&gtk::pango::FontDescription::from_string(
        "Sans Bold 34",
    )));
    let (width, height) = layout.pixel_size();
    snapshot.translate(&gtk::graphene::Point::new(
        (SIZE - width as f32) / 2.0,
        (SIZE - height as f32) / 2.0,
    ));
    snapshot.append_layout(&layout, &gtk::gdk::RGBA::WHITE);
    let node = snapshot.to_node()?;
    Some(
        renderer
            .render_texture(&node, Some(&rect))
            .save_to_png_bytes(),
    )
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
    fn a_call_token_serves_every_button_until_the_call_is_cleared() {
        let mut calls = ActivationTokens::default();
        calls.insert("ring".into(), (AccountId(2), "CALL1".into()));
        calls.insert("other".into(), (AccountId(1), "CALL1".into()));
        for _ in 0..3 {
            assert_eq!(calls.get("ring"), Some(&(AccountId(2), "CALL1".into())));
        }
        assert_eq!(
            calls.token_for(&(AccountId(1), "CALL1".into())).as_deref(),
            Some("other")
        );
        assert_eq!(calls.remove_chat(&(AccountId(2), "CALL1".into())), ["ring"]);
        assert_eq!(calls.get("ring"), None);
        assert!(calls.get("other").is_some());
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
