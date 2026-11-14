//! Decode WhatsApp history chunks and project conversations for the archive.

use super::*;

/// Decoded history chunk waiting to be canonicalized and archived.
pub(super) struct ParsedHistory {
    pub(super) chats: Vec<ParsedChat>,
    pub(super) push_names: Vec<(String, String)>,
    pub(super) lids: Vec<(String, String)>,
    /// Recent phone stickers included with history sync.
    pub(super) stickers: Vec<wa::StickerMetadata>,
}

pub(super) struct ParsedChat {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) unread: Option<u32>,
    pub(super) archived: bool,
    pub(super) pinned_at: Option<i64>,
    /// Outer None means the history chunk omitted mute metadata.
    pub(super) muted_until: Option<Option<i64>>,
    /// `None` means the history chunk omitted lock metadata.
    pub(super) locked: Option<bool>,
    pub(super) ephemeral_expiration: Option<u32>,
    pub(super) ephemeral_setting_timestamp: Option<i64>,
    pub(super) last_activity: i64,
    pub(super) pn_jid: Option<String>,
    pub(super) lid_jid: Option<String>,
    /// Whether the phone reports more available history.
    pub(super) more_on_phone: Option<bool>,
    pub(super) messages: Vec<ParsedMessage>,
    pub(super) revoked: Vec<HistoryRevoke>,
    pub(super) poll_updates: Vec<HistoryPollUpdate>,
    pub(super) reactions: Vec<HistoryReaction>,
}

pub(super) struct HistoryPollUpdate {
    id: String,
    sender: Option<String>,
    from_me: bool,
    timestamp: i64,
    update: wa::message::PollUpdateMessage,
}

pub(super) struct HistoryRevoke {
    pub(super) id: String,
    pub(super) target: String,
    pub(super) sender: Option<String>,
    pub(super) from_me: bool,
}

/// Standalone reaction from history, applied after the parent row is stored.
pub(super) struct HistoryReaction {
    pub(super) id: String,
    pub(super) target: String,
    pub(super) sender: Option<String>,
    pub(super) from_me: bool,
    pub(super) body: HistoryReactionBody,
}

pub(super) enum HistoryReactionBody {
    Plain(String),
    Encrypted { payload: Vec<u8>, iv: Vec<u8> },
}

pub(super) struct ParsedMessage {
    pub(super) id: String,
    pub(super) sender: Option<String>,
    pub(super) from_me: bool,
    pub(super) push_name: Option<String>,
    pub(super) timestamp: i64,
    pub(super) content: Content,
    pub(super) status: Delivery,
    pub(super) quoted: Option<Quoted>,
    pub(super) reactions: Vec<(Option<String>, bool, String)>,
    pub(super) mentions: Vec<String>,
    pub(super) forwarded: bool,
    pub(super) thumbnail: Option<Vec<u8>>,
    pub(super) raw: Vec<u8>,
    pub(super) poll_secret: Option<Vec<u8>>,
    pub(super) poll_votes: Vec<wa::PollUpdate>,
}

/// Decodes a history chunk off the worker thread.
pub(super) fn parse_history(compressed: &[u8]) -> Result<ParsedHistory, String> {
    let mut stream = HistorySyncStream::new(compressed, MAX_DECOMPRESSED);
    let mut chats = Vec::new();
    loop {
        let conversation = match stream.next_conversation() {
            Ok(Some(conversation)) => conversation,
            Ok(None) => break,
            Err(error) => return Err(error.to_string()),
        };
        chats.push(parse_conversation(conversation));
    }
    let remainder = stream.remainder().map_err(|error| error.to_string())?;
    let push_names = remainder
        .pushnames
        .iter()
        .filter_map(|entry| Some((entry.id.clone()?, entry.pushname.clone()?)))
        .collect();
    let lids = remainder
        .phone_number_to_lid_mappings
        .iter()
        .filter_map(|entry| Some((entry.lid_jid.clone()?, entry.pn_jid.clone()?)))
        .collect();
    Ok(ParsedHistory {
        chats,
        push_names,
        lids,
        stickers: remainder.recent_stickers,
    })
}

pub(super) fn parse_conversation(conversation: wa::Conversation) -> ParsedChat {
    let mut messages = Vec::new();
    let mut revoked = Vec::new();
    let mut poll_updates = Vec::new();
    let mut reactions = Vec::new();
    let mut newest = 0;
    for entry in &conversation.messages {
        let Some(info) = entry.message.as_option() else {
            continue;
        };
        let Some(key) = info.key.as_option() else {
            continue;
        };
        let Some(id) = key.id.clone().filter(|id| !id.is_empty()) else {
            continue;
        };
        let Some(message) = info.message.as_option() else {
            continue;
        };
        let from_me = key.from_me.unwrap_or(false);
        let timestamp = info.message_timestamp.unwrap_or(0) as i64;
        newest = newest.max(timestamp);
        let base = base_message(message);
        let sender = info
            .participant
            .clone()
            .or_else(|| key.participant.clone())
            .filter(|sender| !sender.is_empty())
            .or_else(|| key.remote_jid.clone());
        if let Some(protocol) = base.protocol_message.as_option() {
            if protocol.r#type == Some(wa::message::protocol_message::Type::REVOKE)
                && let Some(target) = protocol.key.as_option().and_then(|key| key.id.clone())
            {
                revoked.push(HistoryRevoke {
                    id,
                    target,
                    sender,
                    from_me,
                });
            }
            continue;
        }
        if let Some(reaction) = base.reaction_message.as_option() {
            if let Some(target) = reaction
                .key
                .as_option()
                .and_then(|key| key.id.clone())
                .filter(|id| !id.is_empty())
            {
                reactions.push(HistoryReaction {
                    id,
                    target,
                    sender,
                    from_me,
                    body: HistoryReactionBody::Plain(
                        reaction_emoji(reaction.text.as_deref(), reaction.grouping_key.as_deref())
                            .unwrap_or_default(),
                    ),
                });
            }
            continue;
        }
        if base.enc_reaction_message.is_set() {
            if let Some(enc) = base.enc_reaction_message.as_option()
                && let Some(target) = enc
                    .target_message_key
                    .as_option()
                    .and_then(|key| key.id.clone())
                    .filter(|id| !id.is_empty())
                && let (Some(payload), Some(iv)) = (enc.enc_payload.clone(), enc.enc_iv.clone())
            {
                reactions.push(HistoryReaction {
                    id,
                    target,
                    sender,
                    from_me,
                    body: HistoryReactionBody::Encrypted { payload, iv },
                });
            }
            continue;
        }
        if let Some(update) = base.poll_update_message.as_option() {
            poll_updates.push(HistoryPollUpdate {
                id,
                sender,
                from_me,
                timestamp,
                update: update.clone(),
            });
            continue;
        }
        let Some(content) = classify(base) else {
            continue;
        };
        use wa::web_message_info::Status;
        let mut status = if from_me {
            match info.status {
                Some(Status::READ) => Delivery::Read,
                Some(Status::PLAYED) => Delivery::Played,
                Some(Status::DELIVERY_ACK) => Delivery::Delivered,
                Some(Status::SERVER_ACK) => Delivery::Sent,
                Some(Status::PENDING) => Delivery::Pending,
                Some(Status::ERROR) => Delivery::Failed,
                _ => Delivery::Sent,
            }
        } else {
            Delivery::None
        };
        // A group's individual receipts may be only a partial list. Only the
        // phone's aggregate status proves delivery/read for historical groups.
        if from_me
            && ChatKind::from_id(&conversation.id) != ChatKind::Group
            && status < Delivery::Read
        {
            if info
                .user_receipt
                .iter()
                .any(|receipt| receipt.read_timestamp.is_some())
            {
                status = Delivery::Read;
            } else if status < Delivery::Delivered
                && info
                    .user_receipt
                    .iter()
                    .any(|receipt| receipt.receipt_timestamp.is_some())
            {
                status = Delivery::Delivered;
            }
        }
        let quoted = context_of(base).and_then(|context| {
            let id = context.stanza_id.clone().filter(|id| !id.is_empty())?;
            Some(Quoted {
                mentions: Vec::new(),
                id,
                sender: context.participant.clone().unwrap_or_default(),
                sender_name: None,
                summary: context
                    .quoted_message
                    .as_option()
                    .and_then(classify)
                    .map_or_else(Content::generic_summary, |content| content.summary()),
            })
        });
        let reactions = info
            .reactions
            .iter()
            .filter_map(|reaction| {
                let text =
                    reaction_emoji(reaction.text.as_deref(), reaction.grouping_key.as_deref())?;
                let key = reaction.key.as_option();
                let from_me = key.and_then(|key| key.from_me).unwrap_or(false);
                let who = key.and_then(|key| key.participant.clone());
                Some((who, from_me, text))
            })
            .collect();
        messages.push(ParsedMessage {
            id,
            sender,
            from_me,
            push_name: non_empty(&info.push_name),
            timestamp,
            content,
            status,
            quoted,
            reactions,
            mentions: mentioned_of(base),
            forwarded: forwarded_of(base),
            thumbnail: thumbnail_of(base),
            raw: message.encode_to_vec(),
            poll_secret: info.message_secret.clone(),
            poll_votes: info.poll_updates.clone(),
        });
    }
    let last_activity = conversation
        .conversation_timestamp
        .or(conversation.last_msg_timestamp)
        .map(|timestamp| timestamp as i64)
        .unwrap_or(0)
        .max(newest);
    use wa::conversation::EndOfHistoryTransferType as End;
    let more_on_phone = conversation
        .end_of_history_transfer_type
        .map(|end| match end {
            End::COMPLETE_BUT_MORE_MESSAGES_REMAIN_ON_PRIMARY
            | End::COMPLETE_ON_DEMAND_SYNC_BUT_MORE_MSG_REMAIN_ON_PRIMARY => true,
            End::COMPLETE_AND_NO_MORE_MESSAGE_REMAIN_ON_PRIMARY
            | End::COMPLETE_ON_DEMAND_SYNC_WITH_MORE_MSG_ON_PRIMARY_BUT_NO_ACCESS => false,
        });
    ParsedChat {
        id: conversation.id.clone(),
        name: non_empty(&conversation.display_name).or_else(|| non_empty(&conversation.name)),
        unread: conversation.unread_count,
        archived: conversation.archived.unwrap_or(false),
        pinned_at: conversation.pinned.map(|when| i64::from(when) * 1000),
        muted_until: conversation.mute_end_time.map(|end| {
            // Zero explicitly clears a history mute; a wrapped -1 means
            // indefinite. Absence of the field must preserve existing state.
            (end != 0).then(|| seconds(end as i64))
        }),
        ephemeral_expiration: conversation.ephemeral_expiration,
        ephemeral_setting_timestamp: conversation.ephemeral_setting_timestamp,
        locked: conversation.locked,
        last_activity,
        pn_jid: conversation.pn_jid.clone(),
        lid_jid: conversation.lid_jid.clone(),
        more_on_phone,
        messages,
        revoked,
        poll_updates,
        reactions,
    }
}

#[derive(Default)]
struct HistoryEffects {
    stickers_dirty: bool,
    groups: Vec<String>,
    finished_polls: Vec<(String, String)>,
    revoked: Vec<(String, String)>,
    poll_messages: Vec<(String, String)>,
}

type FiledHistory = (Vec<(ChatId, usize, Option<bool>)>, HistoryEffects);

#[cfg(test)]
thread_local! {
    pub(super) static PANIC_AFTER_HISTORY_IDENTITIES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Worker {
    fn preflight_history_senders(&self, parsed: &mut ParsedHistory) -> crate::archive::Result<()> {
        fn retain_checked<T>(
            items: &mut Vec<T>,
            mut allowed: impl FnMut(&T) -> crate::archive::Result<bool>,
        ) -> crate::archive::Result<()> {
            let mut kept = Vec::new();
            for item in std::mem::take(items) {
                if allowed(&item)? {
                    kept.push(item);
                }
            }
            *items = kept;
            Ok(())
        }

        let mut seen = HashMap::<(String, String), (String, bool)>::new();
        let mut rejected = false;
        for chat in &mut parsed.chats {
            let chat_id = self.canonical_str(&chat.id);
            if !chat_id.ends_with("@g.us") {
                continue;
            }
            let mut allowed =
                |id: &str, sender: Option<&str>, from_me: bool| -> crate::archive::Result<bool> {
                    let sender = if from_me {
                        self.me()
                    } else {
                        sender
                            .map(|sender| self.canonical_str(sender))
                            .unwrap_or_else(|| chat_id.clone())
                    };
                    let key = (chat_id.clone(), id.to_owned());
                    let collision = self
                        .archive
                        .sender_collision_for(&chat_id, id, &sender, from_me)?
                        || seen.get(&key).is_some_and(|(previous, own)| {
                            *own != from_me || (!from_me && *previous != sender)
                        });
                    if collision {
                        rejected = true;
                        return Ok(false);
                    }
                    seen.entry(key).or_insert((sender, from_me));
                    Ok(true)
                };
            retain_checked(&mut chat.messages, |row| {
                allowed(&row.id, row.sender.as_deref(), row.from_me)
            })?;
            retain_checked(&mut chat.reactions, |row| {
                allowed(&row.id, row.sender.as_deref(), row.from_me)
            })?;
            retain_checked(&mut chat.poll_updates, |row| {
                allowed(&row.id, row.sender.as_deref(), row.from_me)
            })?;
            retain_checked(&mut chat.revoked, |row| {
                allowed(&row.id, row.sender.as_deref(), row.from_me)
            })?;
            let mut missing_identity = false;
            for message in &mut chat.messages {
                for update in &mut message.poll_votes {
                    let permitted = match update.poll_update_message_key.as_option() {
                        Some(key) => {
                            let from_me = key.from_me.unwrap_or(false);
                            match (
                                key.id.as_deref().filter(|id| !id.is_empty()),
                                key.participant
                                    .as_deref()
                                    .filter(|sender| !sender.is_empty()),
                            ) {
                                (Some(id), sender) if from_me || sender.is_some() => {
                                    allowed(id, sender, from_me)?
                                }
                                _ => {
                                    missing_identity = true;
                                    false
                                }
                            }
                        }
                        None => {
                            missing_identity = true;
                            false
                        }
                    };
                    if !permitted {
                        // Keep the unusable entry so a partial snapshot cannot finish recovery.
                        update.poll_update_message_key = MessageField::none();
                    }
                }
            }
            rejected |= missing_identity;
        }
        if rejected {
            parsed.lids.clear();
            for chat in &mut parsed.chats {
                chat.lid_jid = None;
                chat.pn_jid = None;
            }
        }
        Ok(())
    }

    /// Archives a history chunk. `metadata` controls chat-state updates.
    /// Returns each chat's message count and whether the phone has more.
    pub(super) fn apply_history(
        &mut self,
        mut parsed: ParsedHistory,
        metadata: bool,
    ) -> (Vec<(ChatId, usize, Option<bool>)>, bool) {
        if self.preflight_history_senders(&mut parsed).is_err() {
            log::warn!("could not check a history message sender; history remains retryable");
            self.emit_chats();
            self.emit(Event::RefreshTranscript);
            return (Vec::new(), false);
        }
        let mut batch = self.begin_batch();
        if !batch.owned {
            return (Vec::new(), false);
        }
        batch.protect_state(true);
        let (filed, effects) = match batch.file_history(parsed, metadata) {
            Ok(result) => result,
            Err(_) => {
                log::warn!("could not check a history message sender; history remains retryable");
                drop(batch);
                self.emit_chats();
                self.emit(Event::RefreshTranscript);
                return (Vec::new(), false);
            }
        };
        let committed = batch.finish();
        if committed {
            self.stickers_dirty |= effects.stickers_dirty;
            for (chat, id) in effects.finished_polls {
                self.poll_history.finish(&chat, &id);
            }
            for (chat, id) in effects.revoked {
                self.pending_revokes.remove(&(chat, id));
            }
            for id in effects.groups {
                self.request_group_info(&id, false);
            }
            for (chat, id) in effects.poll_messages {
                self.emit_message(&chat, &id);
            }
            self.pump_poll_votes();
            self.pump_poll_history();
            (filed, true)
        } else {
            (Vec::new(), false)
        }
    }

    fn file_history(
        &mut self,
        parsed: ParsedHistory,
        metadata: bool,
    ) -> crate::archive::Result<FiledHistory> {
        let mut effects = HistoryEffects::default();
        for (lid, pn) in &parsed.lids {
            if let (Some(lid), Some(pn)) = (Self::jid_of(lid), Self::jid_of(pn)) {
                self.learn_pair(&lid, &pn);
            }
        }
        if !parsed.stickers.is_empty() {
            effects.stickers_dirty = true;
            log::info!(
                "the phone listed {} recently used stickers",
                parsed.stickers.len()
            );
        }
        for sticker in &parsed.stickers {
            let Some(hash) = sticker_hash(
                sticker.file_sha256.as_deref(),
                sticker.file_enc_sha256.as_deref(),
            ) else {
                continue;
            };
            if let Err(_error) = self.archive.upsert_phone_sticker(
                &hash,
                &sticker.encode_to_vec(),
                seconds(sticker.last_sticker_sent_ts.unwrap_or(0)),
                sticker.weight.unwrap_or(0.0),
            ) {
                log::warn!("could not store a sticker");
            }
        }
        for chat in &parsed.chats {
            if let (Some(lid), Some(pn)) = (&chat.lid_jid, &chat.pn_jid)
                && let (Some(lid), Some(pn)) = (Self::jid_of(lid), Self::jid_of(pn))
            {
                self.learn_pair(&lid, &pn);
            }
        }
        let mut filed = Vec::new();
        // Sent together at the end: an event per name made the window
        // rebuild the open conversation for each one.
        let mut learned = Vec::new();
        for (id, name) in &parsed.push_names {
            let id = self.canonical_str(id);
            if let Some(contact) = self.learn_push_name(&id, name) {
                learned.push(contact);
                self.refresh_chat_name(&id);
            }
        }
        #[cfg(test)]
        PANIC_AFTER_HISTORY_IDENTITIES.with(|panic| {
            assert!(!panic.replace(false), "synthetic history panic");
        });
        for chat in parsed.chats {
            let id = self.canonical_str(&chat.id);
            if id.ends_with("@broadcast") {
                continue;
            }
            let existing = self.archive.chat(&id).ok().flatten();
            if metadata || existing.is_none() {
                let name = match chat.name.filter(|name| !name.is_empty()) {
                    name if id.ends_with("@newsletter") => self
                        .newsletter_names
                        .get(&id)
                        .cloned()
                        .or_else(|| {
                            existing
                                .as_ref()
                                .filter(|row| row.name != fallback_name(&id))
                                .map(|row| row.name.clone())
                        })
                        .or(name)
                        .unwrap_or_else(|| fallback_name(&id)),
                    Some(name) if ChatKind::from_id(&id) == ChatKind::Group => name,
                    Some(name) => {
                        // Prefer the phone's address-book name for direct chats.
                        let contact =
                            self.identity
                                .contacts
                                .entry(id.clone())
                                .or_insert_with(|| Contact {
                                    id: id.clone(),
                                    full_name: None,
                                    push_name: None,
                                });
                        if contact.full_name.is_none()
                            && !name
                                .chars()
                                .all(|c| c.is_ascii_digit() || c == '+' || c == ' ')
                        {
                            contact.full_name = Some(name.clone());
                            let contact = contact.clone();
                            let _ = self.archive.upsert_contact(&contact);
                            learned.push(contact);
                        }
                        self.chat_name(&id, None)
                    }
                    None => self.chat_name(&id, None),
                };
                let mut row = Chat::new(id.clone(), name);
                row.last_activity = chat.last_activity;
                row.unread = existing.as_ref().map_or(0, |existing| existing.unread);
                row.archived = chat.archived;
                row.pinned_at = chat
                    .pinned_at
                    .unwrap_or_else(|| existing.as_ref().map_or(0, |row| row.pinned_at));
                row.pinned = chat.pinned_at.map_or_else(
                    || existing.as_ref().is_some_and(|row| row.pinned),
                    |when| when > 0,
                );
                row.muted_until = chat
                    .muted_until
                    .unwrap_or_else(|| existing.as_ref().and_then(|row| row.muted_until));
                row.locked = chat
                    .locked
                    .unwrap_or_else(|| existing.as_ref().is_some_and(|row| row.locked));
                if self.archive.upsert_chat(&row).is_err() {
                    log::warn!("could not store a chat");
                    continue;
                }
                // History has no lock timestamp, so it must not supersede
                // an app-state update already received from the phone.
                if let Some(locked) = chat.locked {
                    let _ = self.archive.set_locked_snapshot(&id, locked);
                }
            }
            if let Some(expiration) = chat.ephemeral_expiration {
                let _ = self.archive.set_ephemeral(
                    &id,
                    expiration,
                    chat.ephemeral_setting_timestamp.unwrap_or_default(),
                );
            }
            if ChatKind::from_id(&id) == ChatKind::Group {
                effects.groups.push(id.clone());
            }
            let count = chat.messages.len();
            let mut secrets = HashMap::new();
            for message in chat.messages {
                let poll_creator = if message.from_me {
                    self.me()
                } else {
                    message.sender.clone().unwrap_or_else(|| chat.id.clone())
                };
                let sender = if message.from_me {
                    self.me()
                } else {
                    message
                        .sender
                        .as_deref()
                        .map(|sender| self.canonical_str(sender))
                        .unwrap_or_else(|| id.clone())
                };
                if let Some(push_name) = message.push_name.as_deref()
                    && !message.from_me
                    && let Some(contact) = self.learn_push_name(&sender, push_name)
                {
                    learned.push(contact);
                    self.refresh_chat_name(&sender);
                }
                let reactions = message
                    .reactions
                    .into_iter()
                    .map(|(who, from_me, emoji)| Reaction {
                        sender: if from_me {
                            self.me()
                        } else {
                            who.as_deref()
                                .map(|who| self.canonical_str(who))
                                .unwrap_or_else(|| id.clone())
                        },
                        from_me,
                        emoji,
                    })
                    .collect();
                let quoted = message.quoted.map(|quoted| {
                    let sender = self.canonical_str(&quoted.sender);
                    Quoted {
                        sender_name: self.name_for(&sender),
                        sender,
                        ..quoted
                    }
                });
                let mentions = self.mentions_of(&message.mentions);
                let row = Message {
                    id: message.id,
                    chat: id.clone(),
                    sender,
                    sender_name: if message.from_me {
                        None
                    } else {
                        message.push_name
                    },
                    from_me: message.from_me,
                    timestamp: message.timestamp,
                    content: message.content,
                    status: message.status,
                    delivered_at: None,
                    read_at: None,
                    quoted,
                    reactions,
                    edited: false,
                    mentions,
                    forwarded: message.forwarded,
                    thumbnail: message.thumbnail,
                };
                if self.archive.sender_collision(&row)? {
                    continue;
                }
                if let Some(secret) = message
                    .poll_secret
                    .as_deref()
                    .filter(|secret| secret.len() == 32)
                {
                    secrets.insert(row.id.clone(), secret.to_vec());
                }
                let mut poll_history_received = false;
                let raw =
                    ensure_message_secret(message.raw, secrets.get(&row.id).map(Vec::as_slice));
                if matches!(row.content, Content::Poll { .. }) {
                    if let Ok(raw) = wa::Message::decode_from_slice(&raw) {
                        self.remember_poll(
                            &row,
                            &raw,
                            &poll_creator,
                            message.poll_secret.as_deref(),
                        );
                    }
                    poll_history_received = self.history_poll_votes(&row, &message.poll_votes);
                }
                if let Err(error) = self.archive.insert_message(&row, Some(&raw)) {
                    log::warn!("could not store a history message: {error}");
                } else if row.from_me
                    && matches!(
                        row.status,
                        Delivery::Sent | Delivery::Delivered | Delivery::Read | Delivery::Played
                    )
                    && let Some(quoted) = row.quoted.as_ref()
                {
                    self.confirm_answer(&id, &quoted.id, &raw);
                }
                if matches!(row.content, Content::Buttons { .. } | Content::List { .. }) {
                    self.confirm_answers_for(&id, &row.id);
                }
                if matches!(row.content, Content::Poll { .. }) {
                    if poll_history_received {
                        let _ = self.archive.mark_poll_history(&id, &row.id);
                        effects.finished_polls.push((id.clone(), row.id.clone()));
                    }
                    effects.poll_messages.push((id.clone(), row.id.clone()));
                }
            }
            for reaction in chat.reactions {
                let sender = if reaction.from_me {
                    self.me()
                } else {
                    reaction
                        .sender
                        .as_deref()
                        .map(|sender| self.canonical_str(sender))
                        .unwrap_or_else(|| id.clone())
                };
                if self.archive.sender_collision_for(
                    &id,
                    &reaction.id,
                    &sender,
                    reaction.from_me,
                )? {
                    continue;
                }
                self.apply_history_reaction(&id, reaction, &secrets);
            }
            for update in chat.poll_updates {
                let sender = if update.from_me {
                    self.me()
                } else {
                    update.sender.unwrap_or_else(|| chat.id.clone())
                };
                if self.archive.sender_collision_for(
                    &id,
                    &update.id,
                    &self.canonical_str(&sender),
                    update.from_me,
                )? {
                    continue;
                }
                self.file_poll_vote(
                    &id,
                    &update.id,
                    &sender,
                    update.from_me,
                    update.timestamp,
                    &update.update,
                );
            }
            for revoked in chat.revoked {
                let sender = if revoked.from_me {
                    self.me()
                } else {
                    revoked
                        .sender
                        .as_deref()
                        .map(|sender| self.canonical_str(sender))
                        .unwrap_or_else(|| id.clone())
                };
                if self
                    .archive
                    .sender_collision_for(&id, &revoked.id, &sender, revoked.from_me)?
                {
                    continue;
                }
                effects.revoked.push((id.clone(), revoked.target.clone()));
                if let Ok(true) =
                    self.archive
                        .set_content(&id, &revoked.target, &Content::Revoked, false)
                {
                    self.emit_message(&id, &revoked.target);
                    self.emit_chat(&id);
                }
            }
            if (metadata || existing.is_none())
                && let Some(snapshot_unread) = chat.unread
            {
                if snapshot_unread == 0 {
                    let _ = self.archive.mark_read_through(&id, chat.last_activity);
                } else {
                    let unread = self
                        .archive
                        .history_unread(&id, snapshot_unread)
                        .unwrap_or(0);
                    let unread = existing
                        .as_ref()
                        .map_or(unread, |existing| existing.unread.max(unread));
                    let _ = self.archive.set_unread(&id, unread);
                }
            }
            filed.push((id, count, chat.more_on_phone));
        }
        if !learned.is_empty() {
            self.emit(Event::Contacts(learned));
        }
        for (id, _, _) in &filed {
            self.emit_chat(id);
            if id.ends_with("@newsletter") {
                self.request_newsletter_info(id);
            }
        }
        Ok((filed, effects))
    }
}

impl Worker {
    pub(super) async fn on_history_sync(&mut self, lazy: &wa_events::LazyHistorySync) {
        let on_demand =
            lazy.sync_type() == ON_DEMAND || lazy.peer_data_request_session_id().is_some();
        if !on_demand {
            self.sync_deadline = Some(Instant::now() + SYNC_QUIET);
            self.set_syncing(true);
        }
        let compressed = lazy.compressed_bytes().clone();
        let parsed = tokio::task::spawn_blocking(move || parse_history(&compressed)).await;
        let failed = !matches!(&parsed, Ok(Ok(_)));
        match parsed {
            Ok(Ok(parsed)) => {
                if on_demand {
                    log::info!(
                        "poll recovery: on-demand history received; chats={}, messages={}, standalone_votes={}",
                        parsed.chats.len(),
                        parsed
                            .chats
                            .iter()
                            .map(|chat| chat.messages.len())
                            .sum::<usize>(),
                        parsed
                            .chats
                            .iter()
                            .map(|chat| chat.poll_updates.len())
                            .sum::<usize>()
                    );
                }
                let (filed, committed) = self.apply_history(parsed, !on_demand);
                if on_demand && committed {
                    self.answer_older(
                        filed,
                        lazy.peer_data_request_session_id().map(str::to_owned),
                    );
                }
            }
            Ok(Err(error)) => {
                log::warn!("a history chunk could not be read");
                self.emit(Event::Error(format!(
                    "Could not read part of the chat history: {error}"
                )));
            }
            Err(_error) => log::warn!("history parsing worker failed"),
        }
        if !on_demand && lazy.progress().is_some_and(|progress| progress >= 100) {
            self.sync_deadline = Some(Instant::now() + Duration::from_secs(3));
        }
        if emit_after_history_chunk(on_demand, failed, lazy.progress()) {
            self.emit_chats();
        }
    }
}
