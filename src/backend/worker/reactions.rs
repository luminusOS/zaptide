//! Reaction ingestion and decryption for live messages and history snapshots.

use super::*;

impl Worker {
    pub(super) fn store_plain_reaction(
        &mut self,
        chat: &str,
        sender: &str,
        from_me: bool,
        reaction: &wa::message::ReactionMessage,
    ) {
        let Some(target) = reaction
            .key
            .as_option()
            .and_then(|key| key.id.clone())
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let emoji = reaction_emoji(reaction.text.as_deref(), reaction.grouping_key.as_deref())
            .unwrap_or_default();
        self.store_reaction(chat, &target, sender, from_me, &emoji);
    }

    pub(super) fn store_enc_reaction(
        &mut self,
        chat: &str,
        sender: &str,
        from_me: bool,
        message: &wa::Message,
    ) {
        let Some(env) = extract_secret_encrypted(message) else {
            return;
        };
        if env.kind != SecretEncKind::EncReaction {
            return;
        }
        let Some(target) = env.target_id().filter(|id| !id.is_empty()) else {
            return;
        };
        let Some(emoji) =
            self.decrypt_enc_reaction(chat, target, sender, env.enc_payload, env.enc_iv, None)
        else {
            return;
        };
        self.store_reaction(chat, target, sender, from_me, &emoji);
    }

    fn store_reaction(
        &mut self,
        chat: &str,
        target: &str,
        sender: &str,
        from_me: bool,
        emoji: &str,
    ) {
        if let Ok(Some(updated)) = self
            .archive
            .set_reaction(chat, target, sender, from_me, emoji)
        {
            self.emit(Event::MessageUpdated(Box::new(updated)));
        }
    }

    pub(super) fn apply_history_reaction(
        &mut self,
        chat: &str,
        reaction: HistoryReaction,
        secrets: &HashMap<String, Vec<u8>>,
    ) {
        let sender = if reaction.from_me {
            self.me()
        } else {
            reaction
                .sender
                .as_deref()
                .map(|sender| self.canonical_str(sender))
                .unwrap_or_else(|| chat.to_owned())
        };
        let emoji = match reaction.body {
            HistoryReactionBody::Plain(emoji) => emoji,
            HistoryReactionBody::Encrypted { payload, iv } => {
                let Some(emoji) = self.decrypt_enc_reaction(
                    chat,
                    &reaction.target,
                    &sender,
                    &payload,
                    &iv,
                    Some(secrets),
                ) else {
                    return;
                };
                emoji
            }
        };
        self.store_reaction(chat, &reaction.target, &sender, reaction.from_me, &emoji);
    }

    fn decrypt_enc_reaction(
        &self,
        chat: &str,
        target: &str,
        reactor: &str,
        payload: &[u8],
        iv: &[u8],
        secrets: Option<&HashMap<String, Vec<u8>>>,
    ) -> Option<String> {
        let parent = self.archive.message(chat, target).ok().flatten()?;
        let secret = secrets
            .and_then(|secrets| secrets.get(target).cloned())
            .or_else(|| {
                self.archive
                    .poll_key(chat, target)
                    .ok()
                    .flatten()
                    .map(|(_, secret)| secret)
            })
            .or_else(|| {
                self.archive
                    .raw(chat, target)
                    .ok()
                    .flatten()
                    .as_deref()
                    .and_then(message_secret_from_raw)
            })?;
        let parent_jid = Self::jid_of(&parent.sender)?;
        let reactor_jid = Self::jid_of(reactor)?;
        let fallback_parent = self.alt_jid(&parent_jid);
        let fallback_reactor = self.alt_jid(&reactor_jid);
        let inner = decrypt_secret_encrypted_with_fallback(
            payload,
            iv,
            &secret,
            SecretEncKind::EncReaction,
            target,
            &parent_jid,
            &reactor_jid,
            fallback_parent.as_ref(),
            fallback_reactor.as_ref(),
        )
        .ok()?;
        let reaction = inner.reaction_message.as_option()?;
        Some(
            reaction_emoji(reaction.text.as_deref(), reaction.grouping_key.as_deref())
                .unwrap_or_default(),
        )
    }

    fn alt_jid(&self, jid: &Jid) -> Option<Jid> {
        if jid.is_lid() {
            let pn = self.lid_to_pn.get(jid.user_base())?;
            format!("{pn}@s.whatsapp.net").parse().ok()
        } else {
            self.lid_to_pn.iter().find_map(|(lid, pn)| {
                (*pn == jid.user_base())
                    .then(|| format!("{lid}@lid").parse().ok())
                    .flatten()
            })
        }
    }
}
