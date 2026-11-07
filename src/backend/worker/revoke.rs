use super::*;

impl Worker {
    pub(super) fn confirm_revoke(&mut self, chat: &str, id: &str) {
        // An authoritative phone update invalidates any later rollback.
        self.pending_revokes
            .remove(&(chat.to_owned(), id.to_owned()));
        if let Ok(true) = self.archive.set_content(chat, id, &Content::Revoked, false) {
            self.emit_message(chat, id);
            self.emit_chat(chat);
        }
    }

    pub(super) fn begin_revoke(
        &mut self,
        chat: &str,
        id: &str,
    ) -> crate::archive::Result<Option<u64>> {
        let key = (chat.to_owned(), id.to_owned());
        if self.pending_revokes.contains_key(&key) {
            return Ok(None);
        }
        let previous = self
            .archive
            .message(chat, id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        if matches!(previous.content, Content::Revoked) {
            return Ok(None);
        }
        if !self
            .archive
            .set_content(chat, id, &Content::Revoked, false)?
        {
            return Ok(None);
        }
        self.next_revoke_attempt = self.next_revoke_attempt.wrapping_add(1);
        let attempt_id = self.next_revoke_attempt;
        self.pending_revokes.insert(
            key,
            PendingRevoke {
                attempt_id,
                content: previous.content,
                edited: previous.edited,
            },
        );
        self.emit_message(chat, id);
        self.emit_chat(chat);
        Ok(Some(attempt_id))
    }

    pub(super) fn revoke(&mut self, chat: ChatId, id: String) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        let attempt_id = match self.begin_revoke(&chat, &id) {
            Ok(Some(attempt_id)) => attempt_id,
            Ok(None) => return,
            Err(_) => {
                self.emit(Event::Error(
                    "Could not delete the message for everyone".to_owned(),
                ));
                return;
            }
        };
        let session_generation = self.session_generation;
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let success = client
                .revoke_message(jid, &id, RevokeType::Sender)
                .await
                .is_ok();
            let _ = commands.send(Command::RevokeFinished {
                session_generation,
                attempt_id,
                chat,
                message: id,
                success,
            });
        });
    }
}
