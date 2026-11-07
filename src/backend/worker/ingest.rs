use super::*;

impl Worker {
    /// Returns raw mention tokens and canonical ids.
    pub(super) fn mentions_of(&self, raw: &[String]) -> Vec<MentionRef> {
        raw.iter()
            .filter_map(|jid| {
                let user = jid.split('@').next()?.to_owned();
                if user.is_empty() {
                    return None;
                }
                Some(MentionRef {
                    user,
                    id: self.canonical_str(jid),
                    name: None,
                })
            })
            .collect()
    }

    pub(super) fn ingest(&mut self, message: &Arc<wa::Message>, info: &MessageInfo) {
        if info.source.chat.is_status_broadcast() {
            return;
        }
        let chat = self.canonical(&info.source.chat);
        let from_me = info.source.is_from_me;
        let sender = if from_me {
            self.me()
        } else {
            self.canonical(&info.source.sender)
        };
        let identity = self
            .archive
            .sender_collision_for(&chat, &info.id, &sender, from_me)
            .and_then(|collision| {
                if collision {
                    Ok(None)
                } else if chat.ends_with("@g.us") {
                    self.archive.has_message(&chat, &info.id).map(Some)
                } else {
                    Ok(Some(false))
                }
            });
        let existing = match identity {
            Ok(None) => return,
            Ok(Some(existing)) => existing,
            Err(_) => {
                log::warn!("could not check a group message sender; retrying archive lookup");
                if self.deferred_incoming.len() < 256 {
                    self.deferred_incoming
                        .push_back((message.clone(), info.clone()));
                } else {
                    self.emit(Event::Error("Could not save an incoming message. Request chat history after fixing archive access.".into()));
                }
                return;
            }
        };
        if existing {
            let mut source = info.source.clone();
            source.sender_alt = None;
            self.learn_source(&source);
        } else {
            self.learn_source(&info.source);
        }
        let chat = self.canonical(&info.source.chat);
        let sender = if from_me {
            self.me()
        } else {
            self.canonical(&info.source.sender)
        };
        let push_name = (!info.push_name.is_empty()).then(|| info.push_name.clone());
        let base = base_message(message);
        if let Some(expiration) = base.get_ephemeral_expiration()
            && self
                .archive
                .ephemeral_expiration(&chat)
                .ok()
                .flatten()
                .is_none()
        {
            self.ensure_chat(&chat, push_name.as_deref());
            let _ = self.archive.set_ephemeral(&chat, expiration, 0);
        }

        if let Some(protocol) = base.protocol_message.as_option() {
            use wa::message::protocol_message::Type;
            if protocol.r#type == Some(Type::EPHEMERAL_SETTING) {
                if let Some(expiration) = protocol.ephemeral_expiration {
                    let timestamp = protocol
                        .ephemeral_setting_timestamp
                        .unwrap_or_else(|| info.timestamp.timestamp());
                    let used_fallback = protocol.ephemeral_setting_timestamp.is_none();
                    self.ensure_chat(&chat, push_name.as_deref());
                    let accepted = self
                        .archive
                        .set_ephemeral(&chat, expiration, timestamp)
                        .unwrap_or(false);
                    log::debug!(
                        target: "zaptide::disappearing",
                        "protocol timer update: duration={expiration}s timestamp={timestamp} fallback_timestamp={used_fallback} accepted={accepted}"
                    );
                    if accepted {
                        self.emit_chat(&chat);
                    }
                } else {
                    log::debug!(
                        target: "zaptide::disappearing",
                        "protocol timer update missing expiration"
                    );
                }
                return;
            }
            let Some(target) = protocol.key.as_option().and_then(|key| key.id.clone()) else {
                return;
            };
            match protocol.r#type {
                Some(Type::REVOKE) => {
                    self.confirm_revoke(&chat, &target);
                }
                Some(Type::MESSAGE_EDIT) => {
                    if let Some(edited) = protocol.edited_message.as_option()
                        && let Some(mut content) = classify(edited)
                    {
                        // Preserve downloaded media when updating a caption.
                        if let Ok(Some(existing)) = self.archive.message(&chat, &target)
                            && let (Some(new), Some(old)) =
                                (content.media_mut(), existing.content.media())
                        {
                            new.path = old.path.clone();
                        }
                        if let Ok(true) = self.archive.set_content(&chat, &target, &content, true) {
                            self.emit_message(&chat, &target);
                            self.emit_chat(&chat);
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if let Some(reaction) = base.reaction_message.as_option() {
            self.store_plain_reaction(&chat, &sender, from_me, reaction);
            return;
        }
        if base.enc_reaction_message.is_set() {
            self.store_enc_reaction(&chat, &sender, from_me, base);
            return;
        }
        if let Some(update) = base.poll_update_message.as_option() {
            self.ingest_poll_vote(
                &chat,
                &info.id,
                &info.source.sender.to_non_ad_string(),
                from_me,
                info.timestamp.timestamp(),
                update,
            );
            return;
        }
        let Some(content) = classify(base) else {
            return;
        };
        let quoted = self.quoted_of(base);
        let mentions = self.mentions_of(&mentioned_of(base));
        let row = Message {
            id: info.id.to_string(),
            chat: chat.clone(),
            sender,
            sender_name: if from_me {
                None
            } else {
                push_name.as_ref().map(ToString::to_string)
            },
            from_me,
            timestamp: info.timestamp.timestamp(),
            content,
            status: if from_me {
                Delivery::Sent
            } else {
                Delivery::None
            },
            delivered_at: None,
            read_at: None,
            quoted,
            reactions: Vec::new(),
            edited: false,
            mentions,
            forwarded: forwarded_of(base),
            thumbnail: thumbnail_of(base),
        };
        let is_poll = matches!(row.content, Content::Poll { .. });
        self.remember_poll(&row, message, &info.source.sender.to_non_ad_string(), None);
        self.store_message(row, Some(message.encode_to_vec()), push_name.as_deref());
        if is_poll {
            self.pump_poll_votes();
        }
    }

    pub(super) fn retry_deferred_incoming(&mut self) {
        let pending = std::mem::take(&mut self.deferred_incoming);
        for (message, info) in pending {
            self.ingest(&message, &info);
        }
    }

    pub(super) fn ingest_undecryptable(&mut self, info: &MessageInfo) {
        if info.source.chat.is_status_broadcast() || info.source.is_from_me {
            return;
        }
        let chat = self.canonical(&info.source.chat);
        match self.archive.message(&chat, &info.id) {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(_) => {
                log::warn!("could not check an undecryptable message before learning its sender");
                return;
            }
        }
        self.learn_source(&info.source);
        let chat = self.canonical(&info.source.chat);
        let push_name = (!info.push_name.is_empty()).then(|| info.push_name.clone());
        let row = Message {
            id: info.id.to_string(),
            chat,
            sender: self.canonical(&info.source.sender),
            sender_name: push_name.as_ref().map(ToString::to_string),
            from_me: false,
            timestamp: info.timestamp.timestamp(),
            content: Content::Unsupported {
                what: crate::model::unsupported::UNDECRYPTED.to_owned(),
            },
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        };
        self.store_message(row, None, push_name.as_deref());
    }

    /// Archives a message and emits chat and row updates.
    pub(super) fn store_message(
        &mut self,
        message: Message,
        raw: Option<Vec<u8>>,
        push_name: Option<&str>,
    ) {
        match self.archive.sender_collision(&message) {
            Ok(true) => return,
            Ok(false) => {}
            Err(_) => {
                log::warn!("could not check a group message sender before storing it");
                self.emit(Event::Error(
                    "Could not save a message. Request chat history after fixing archive access."
                        .into(),
                ));
                return;
            }
        }
        let chat = message.chat.clone();
        self.ensure_chat(&chat, if message.from_me { None } else { push_name });
        if let Some(push_name) = push_name
            && !message.from_me
        {
            let sender = message.sender.clone();
            self.remember_push_name(&sender, push_name);
        }
        let is_new = self
            .archive
            .message(&chat, &message.id)
            .ok()
            .flatten()
            .is_none();
        if let Err(error) = self.archive.insert_message(&message, raw.as_deref()) {
            log::warn!("could not store a message: {error}");
            return;
        }
        if message.from_me
            && matches!(
                message.status,
                Delivery::Sent | Delivery::Delivered | Delivery::Read | Delivery::Played
            )
            && let (Some(quoted), Some(raw)) = (message.quoted.as_ref(), raw.as_deref())
        {
            self.confirm_answer(&chat, &quoted.id, raw);
        }
        if matches!(
            message.content,
            Content::Buttons { .. } | Content::List { .. }
        ) {
            self.confirm_answers_for(&chat, &message.id);
        }
        let unread = is_new
            && !message.from_me
            && self
                .archive
                .read_through(&chat)
                .ok()
                .flatten()
                // A new live message may share the read message's second. Its
                // distinct id already passed the duplicate check above.
                .is_none_or(|through| message.timestamp >= through);
        if unread {
            let _ = self.archive.bump_unread(&chat);
        } else if message.from_me
            && matches!(
                message.status,
                Delivery::Sent | Delivery::Delivered | Delivery::Read | Delivery::Played
            )
        {
            // A reply sent from the phone/another companion reads the preceding
            // conversation there. Replayed replies cannot clear newer arrivals.
            let _ = self.archive.mark_read_to(&chat, &message.id);
        }
        let mut stored = self
            .archive
            .message(&chat, &message.id)
            .ok()
            .flatten()
            .unwrap_or(message);
        self.polish(&mut stored);
        // Notify only for live incoming messages, not history replay.
        let incoming = (unread && !self.syncing).then(|| stored.clone());
        self.emit(Event::Messages {
            request: None,
            chat: chat.clone(),
            messages: vec![stored],
            older: false,
            complete: false,
        });
        self.emit_chat(&chat);
        if let Some(message) = incoming {
            self.emit(Event::Incoming {
                chat,
                message: Box::new(message),
            });
        }
    }

    pub(super) fn quoted_of(&self, base: &wa::Message) -> Option<Quoted> {
        let context = context_of(base)?;
        let id = context.stanza_id.clone().filter(|id| !id.is_empty())?;
        let sender = context
            .participant
            .as_deref()
            .map(|participant| self.canonical_str(participant))
            .unwrap_or_default();
        let (summary, listed) = context
            .quoted_message
            .as_option()
            .map(|quoted| {
                (
                    classify(quoted)
                        .map_or_else(Content::generic_summary, |content| content.summary()),
                    self.mentions_of(&mentioned_of(base_message(quoted))),
                )
            })
            .unwrap_or_else(|| (Content::generic_summary(), Vec::new()));
        // Recover quote mentions from `@user` tokens when metadata is missing.
        let summary = self.pn_tokens(&summary);
        let mentions = if listed.is_empty() {
            self.mention_tokens(&summary)
        } else {
            listed
        };
        Some(Quoted {
            sender_name: self.name_for(&sender),
            id,
            sender,
            summary,
            mentions,
        })
    }

    /// Replaces known privacy ids in `@user` tokens with phone-number ids.
    pub(super) fn pn_tokens(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find('@') {
            out.push_str(&rest[..at]);
            out.push('@');
            let after = &rest[at + 1..];
            let digits = after
                .char_indices()
                .find(|(_, c)| !c.is_ascii_digit())
                .map_or(after.len(), |(index, _)| index);
            match self.identity.lid_to_pn.get(&after[..digits]) {
                Some(pn) if digits > 0 => {
                    out.push_str(pn);
                    rest = &after[digits..];
                }
                _ => rest = after,
            }
        }
        out.push_str(rest);
        out
    }

    /// Infers canonical mention ids from `@user` tokens.
    pub(super) fn mention_tokens(&self, text: &str) -> Vec<MentionRef> {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        let mut offset = 0;
        while found.len() < 128 {
            let Some(at) = text[offset..].find('@').map(|at| at + offset) else {
                break;
            };
            let after = &text[at + 1..];
            let digits = after
                .char_indices()
                .find(|(_, c)| !c.is_ascii_digit())
                .map_or(after.len(), |(index, _)| index);
            let user = &after[..digits];
            let boundary_before = at == 0
                || text[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\''));
            let boundary_after = after[digits..]
                .chars()
                .next()
                .is_none_or(|c| !crate::safety::mention_token_char(c));
            if digits >= 5 && boundary_before && boundary_after && seen.insert(user.to_owned()) {
                let id = match self.identity.lid_to_pn.get(user) {
                    Some(pn) => format!("{pn}@s.whatsapp.net"),
                    None => format!("{user}@s.whatsapp.net"),
                };
                let id = self.canonical_str(&id);
                found.push(MentionRef {
                    user: user.to_owned(),
                    id,
                    name: None,
                });
            }
            offset = at + 1;
        }
        found
    }
}
