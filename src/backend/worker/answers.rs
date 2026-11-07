use super::*;

impl Worker {
    pub(super) fn answer_choice(&mut self, chat: ChatId, message_id: String, choice: String) {
        // Every early exit rebuilds the row, so a control the interface
        // left enabled matches the stored state again.
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            self.emit_message(&chat, &message_id);
            return;
        };
        let original = self.archive.message(&chat, &message_id).ok().flatten();
        let picked = original.as_ref().and_then(|original| {
            let content = &original.content;
            if original.from_me
                || content.answer().is_some()
                || self
                    .answer_sends
                    .values()
                    .any(|(pending_chat, pending_id)| {
                        pending_chat == &chat && pending_id == &message_id
                    })
            {
                return None;
            }
            let (label, description) = content.choice(&choice)?;
            Some((label.to_owned(), description.map(str::to_owned)))
        });
        let (Some(original), Some((label, description))) = (original, picked) else {
            // A second click on an answered message is ignored; anything else
            // (revoked, edited away, from me) is worth telling the user.
            let already_answered = self
                .archive
                .message(&chat, &message_id)
                .ok()
                .flatten()
                .is_some_and(|row| row.content.answer().is_some());
            let pending = self
                .answer_sends
                .values()
                .any(|(pending_chat, pending_id)| {
                    pending_chat == &chat && pending_id == &message_id
                });
            if !already_answered && !pending {
                self.emit(Event::Error("This reply is no longer available".to_owned()));
            }
            self.emit_message(&chat, &message_id);
            return;
        };
        // Interrupted sends remain visible as failed until retry. Remove only
        // attempts that quote this question and carry this exact choice.
        if let Ok(failed) = self.archive.failed_quoted_messages(&chat, &message_id) {
            for (id, raw) in failed {
                let sent_choice = wa::Message::decode_from_slice(&raw)
                    .ok()
                    .and_then(|raw| response_choice_id(raw.get_base_message()));
                if sent_choice.as_deref() == Some(&choice)
                    && let Ok(true) = self.archive.delete_message(&chat, &id)
                {
                    self.emit(Event::MessageDeleted {
                        chat: chat.clone(),
                        id,
                    });
                }
            }
        }
        let (context, quoted_row) = self.quote_context(&chat, Some(&message_id));
        let (wire_label, wire_description) = self
            .archive
            .raw(&chat, &message_id)
            .ok()
            .flatten()
            .and_then(|raw| wa::Message::decode_from_slice(&raw).ok())
            .and_then(|raw| full_choice(raw.get_base_message(), &choice))
            .unwrap_or((label.clone(), description));
        let mut message = match &original.content {
            Content::List { .. } => list_response(&choice, &wire_label, wire_description, context),
            _ => buttons_response(&choice, &wire_label, context),
        };
        let expiration = self.apply_ephemeral(&chat, &mut message);
        let id = client.generate_message_id();
        let row = Message {
            id: id.clone(),
            chat: chat.clone(),
            sender: self.me(),
            sender_name: None,
            from_me: true,
            timestamp: crate::util::now(),
            content: Content::text(label.clone()),
            status: Delivery::Pending,
            delivered_at: None,
            read_at: None,
            quoted: quoted_row,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        };
        self.store_message(row, Some(message.encode_to_vec()), None);
        self.answer_sends
            .insert(id.clone(), (chat.clone(), message_id.clone()));
        tokio::spawn(send_outgoing(
            OutgoingSession {
                client,
                commands: self.commands.clone(),
                generation: self.session_generation,
                generation_shared: self.session_generation_shared.clone(),
            },
            chat,
            jid,
            id,
            message,
            expiration,
        ));
    }

    /// Clears an answer recorded by an older version after an interrupted send.
    pub(super) fn reopen_answer(&mut self, chat: &str, message_id: &str) {
        let Ok(Some(original)) = self.archive.message(chat, message_id) else {
            return;
        };
        let Some(reopened) = original
            .content
            .answer()
            .and_then(|_| original.content.with_answer(None))
        else {
            return;
        };
        if let Ok(true) = self
            .archive
            .set_content(chat, message_id, &reopened, original.edited)
        {
            self.emit_message(chat, message_id);
        }
    }

    pub(super) fn recover_interrupted_answers(&mut self) {
        let Ok(rows) = self.archive.pending_quoted_messages() else {
            return;
        };
        for (chat, id, quoted, raw) in rows {
            let Some(parent) = self.archive.message(&chat, &quoted).ok().flatten() else {
                continue;
            };
            let response = wa::Message::decode_from_slice(&raw).ok();
            let choice = response
                .as_ref()
                .and_then(|message| response_choice_id(message.get_base_message()));
            if choice.is_none()
                || !matches!(
                    parent.content,
                    Content::Buttons { .. } | Content::List { .. }
                )
            {
                continue;
            }
            self.reopen_answer(&chat, &quoted);
            let _ = self
                .archive
                .set_status(&chat, &id, Delivery::Failed, crate::util::now());
        }
    }

    pub(super) fn confirm_answer(&self, chat: &str, original: &str, raw: &[u8]) {
        let choice = wa::Message::decode_from_slice(raw)
            .ok()
            .and_then(|message| response_choice_id(message.get_base_message()));
        let parent = self.archive.message(chat, original).ok().flatten();
        if let (Some(choice), Some(parent)) = (choice, parent)
            && parent.content.answer().is_none()
            && parent.content.choice(&choice).is_some()
            && let Some(marked) = parent.content.with_answer(Some(choice))
            && let Ok(true) = self
                .archive
                .set_content(chat, original, &marked, parent.edited)
        {
            self.emit_message(chat, original);
        }
    }

    pub(super) fn reconcile_confirmed_answers(&self) {
        if let Ok(rows) = self.archive.confirmed_quoted_messages() {
            for (chat, original, raw) in rows {
                self.confirm_answer(&chat, &original, &raw);
            }
        }
    }

    pub(super) fn confirm_answers_for(&self, chat: &str, original: &str) {
        if let Ok(rows) = self.archive.confirmed_answers_for(chat, original) {
            for raw in rows {
                self.confirm_answer(chat, original, &raw);
            }
        }
    }
}
