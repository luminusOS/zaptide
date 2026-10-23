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
            });
        }
        // The phone names the sender of a group message from someone else;
        // everywhere else the key has no participant.
        let participant = (ChatKind::from_id(&chat) == ChatKind::Group && !message.from_me)
            .then(|| Self::jid_of(&message.sender))
            .flatten();
        let from_me = message.from_me;
        self.tell_phone(&chat, move |client, jid| async move {
            let actions = client.chat_actions();
            if starred {
                actions
                    .star_message(&jid, participant.as_ref(), &id, from_me)
                    .await
            } else {
                actions
                    .unstar_message(&jid, participant.as_ref(), &id, from_me)
                    .await
            }
            .map_err(|error| error.to_string())
        });
    }
}
