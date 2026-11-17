//! Starring messages here and applying stars from the phone.

use super::*;

/// Most starred messages one chat's listing carries.
pub(super) const STARRED_LIMIT: usize = 500;

impl Worker {
    pub(super) fn set_starred(&mut self, chat: ChatId, id: String, starred: bool) {
        let Ok(Some(message)) = self.archive.message(&chat, &id) else {
            return;
        };
        let now = jiff::Timestamp::now().as_millisecond();
        if self
            .archive
            .set_starred(&chat, &id, starred, now)
            .unwrap_or(false)
        {
            self.emit(Event::StarChanged {
                chat: chat.clone(),
                id: id.clone(),
                starred,
                local: true,
            });
            if matches!(message.content, Content::Sticker { .. }) {
                self.fetch_missing_stickers();
                self.stickers_dirty = true;
            }
        }
        // The phone names the sender of a group message from someone else;
        // everywhere else the key has no participant.
        let participant = (ChatKind::from_id(&chat) == ChatKind::Group && !message.from_me)
            .then(|| Self::jid_of(&message.sender))
            .flatten();
        let from_me = message.from_me;
        self.tell_phone(&chat, move |client, jid| async move {
            if jid.is_newsletter() {
                return star_newsletter_post(&client, &jid, &id, from_me, starred).await;
            }
            let target = whatsapp_rust::MessageId::new(&id)
                .and_then(|message_id| {
                    whatsapp_rust::MessageRef::new(&jid, message_id, participant.as_ref(), from_me)
                })
                .map_err(|error| error.to_string())?;
            let actions = client.chat_actions();
            if starred {
                actions.star_message(&target).await
            } else {
                actions.unstar_message(&target).await
            }
            .map_err(|error| error.to_string())
        });
    }
}

/// A message reference cannot name a newsletter post, so its star goes out
/// with the index the chat API builds for any other message without a
/// participant: chat, id, sender and `0`.
async fn star_newsletter_post(
    client: &Client,
    jid: &Jid,
    id: &str,
    from_me: bool,
    starred: bool,
) -> Result<(), String> {
    let mut action = wa::sync_action_value::StarAction::default();
    action.starred = Some(starred);
    let mut value = wa::SyncActionValue::default();
    value.star_action = MessageField::some(action);
    value.timestamp = Some(jiff::Timestamp::now().as_millisecond());
    let chat = jid.to_string();
    client
        .send_app_state_action(
            &whatsapp_rust::schemas::STAR,
            &[&chat, id, if from_me { "1" } else { "0" }, "0"],
            &value,
        )
        .await
        .map_err(|error| error.to_string())
}
