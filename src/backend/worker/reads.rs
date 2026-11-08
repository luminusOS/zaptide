use super::*;

impl Worker {
    pub(super) fn mark_read(&mut self, chat: ChatId, receipts: bool) {
        let Ok(Some(row)) = self.archive.chat(&chat) else {
            return;
        };
        // Collect before advancing the archive's read position.
        let ids = if receipts {
            self.archive
                .unread_incoming(&chat, row.unread)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let _ = self.archive.mark_read(&chat);
        self.emit_chat(&chat);
        if row.unread == 0 {
            return;
        }
        let _ = self.archive.queue_read_sync(&chat);
        self.pump_read_sync();
        self.send_read_receipts(chat, ids);
    }

    pub(super) fn pump_read_sync(&mut self) {
        if !matches!(self.status, LinkStatus::Connected) || !self.read_sync.ready(Instant::now()) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        for (chat, through) in self.archive.pending_reads().unwrap_or_default() {
            let Some(jid) = Self::jid_of(&chat) else {
                continue;
            };
            let Some(attempt_id) = self.read_sync.start(&chat, through, Instant::now()) else {
                break;
            };
            let client = client.clone();
            let commands = self.commands.clone();
            let session_generation = self.session_generation;
            tokio::spawn(async move {
                // This update is private to our devices, even with blue ticks
                // disabled. Keep the original position when retrying offline
                // reads, not the latest message received since the local read.
                let range = whatsapp_rust::message_range(through, None, Vec::new());
                // A timed-out patch may still have reached the phone. Keep the durable
                // position queued and retry the same-or-newer read range at least once.
                let outcome = bounded_read_sync(
                    READ_SYNC_TIMEOUT,
                    client
                        .chat_actions()
                        .mark_chat_as_read(&jid, true, Some(range)),
                )
                .await;
                match outcome {
                    ReadSyncOutcome::Success => {}
                    ReadSyncOutcome::Failed => {
                        log::debug!("chat read state not synced");
                    }
                    ReadSyncOutcome::TimedOut => {
                        log::warn!("chat read-state sync timed out; keeping position queued");
                    }
                }
                let _ = Completion {
                    generation: session_generation,
                    done: Done::ReadSyncFinished {
                        attempt_id,
                        chat,
                        through,
                        success: outcome == ReadSyncOutcome::Success,
                    },
                }
                .send(&commands);
            });
            // Every read-state write uses regular_low. A queue of spawned tasks
            // would each retry the same broken collection before we can back off.
            break;
        }
    }

    pub(super) fn send_read_receipts(&self, chat: ChatId, ids: Vec<(String, String)>) {
        if ids.is_empty() {
            return;
        }
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            return;
        };
        let is_group = jid.is_group();
        let mut by_sender: HashMap<Option<String>, Vec<String>> = HashMap::new();
        for (id, sender) in ids {
            by_sender
                .entry(is_group.then_some(sender))
                .or_default()
                .push(id);
        }
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            if !receipts_allowed(&client, &jid, &commands, session_generation).await {
                return;
            }
            for (sender, ids) in by_sender {
                let sender = sender.and_then(|sender| sender.parse::<Jid>().ok());
                let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
                if let Err(error) = client.mark_as_read(&jid, sender.as_ref(), &ids).await {
                    log::debug!("read receipt not sent: {error}");
                }
            }
        });
    }
}

impl Worker {
    pub(super) fn on_receipt(&mut self, receipt: &wa_events::Receipt) {
        let mut batch = self.begin_batch();
        if !batch.owned {
            return;
        }
        batch.protect_state(false);
        batch.file_receipt(receipt);
        batch.finish();
    }

    pub(super) fn file_receipt(&mut self, receipt: &wa_events::Receipt) {
        self.learn_source(&receipt.source);
        let chat = self.canonical(&receipt.source.chat);
        log::debug!(
            "receipt {:?} from {} (chat {chat}, from me: {}, offline: {}) for {:?}",
            receipt.r#type,
            receipt.source.sender,
            receipt.source.is_from_me,
            receipt.offline,
            receipt.message_ids
        );
        let status = match receipt.r#type {
            ReceiptType::Delivered => Delivery::Delivered,
            // An inactive-device receipt still means delivered.
            ReceiptType::Inactive => Delivery::Delivered,
            ReceiptType::Read => Delivery::Read,
            ReceiptType::Played => Delivery::Played,
            ReceiptType::ReadSelf | ReceiptType::PlayedSelf => {
                // The receipt time is when the phone read, not the position
                // it read through. A delayed receipt must leave newer messages.
                // A peer named by an unmapped privacy id is found by message id.
                let mut read = Vec::new();
                for id in &receipt.message_ids {
                    let target = match self.archive.message(&chat, id).ok().flatten() {
                        Some(message) => (!message.from_me).then(|| chat.clone()),
                        None => self.archive.incoming_chat_of(id).ok().flatten(),
                    };
                    if let Some(target) = target {
                        let _ = self.archive.mark_read_to(&target, id);
                        if !read.contains(&target) {
                            read.push(target);
                        }
                    }
                }
                for target in read {
                    self.emit_chat(&target);
                }
                return;
            }
            // Own-device delivery counts as read only in the self chat.
            ReceiptType::Sender if chat == self.me() => Delivery::Read,
            _ => return,
        };
        let at = receipt.timestamp.timestamp();
        if ChatKind::from_id(&chat) == ChatKind::Group {
            let recipient = self.canonical(&receipt.source.sender);
            if self.is_me(&recipient) {
                return;
            }
            let mut updated = Vec::new();
            for id in &receipt.message_ids {
                if !self
                    .archive
                    .message(&chat, id)
                    .ok()
                    .flatten()
                    .is_some_and(|row| row.from_me)
                {
                    continue;
                }
                match self
                    .archive
                    .group_receipt(&chat, id, &recipient, status, at)
                {
                    Ok(true) => updated.extend(self.updated_message(&chat, id)),
                    Ok(false) => {}
                    Err(error) => log::warn!("could not file a group receipt: {error}"),
                }
            }
            self.emit_messages(updated);
            self.emit_chat(&chat);
            return;
        }
        let mut newest = 0;
        let mut changed = 0;
        let mut updated = Vec::new();
        for id in &receipt.message_ids {
            match self.archive.set_status(&chat, id, status, at) {
                Ok(true) => {
                    changed += 1;
                    updated.extend(self.updated_message(&chat, id));
                }
                Ok(false) => {}
                Err(_error) => log::warn!("could not file a receipt"),
            }
            if let Ok(Some(message)) = self.archive.message(&chat, id) {
                newest = newest.max(message.timestamp);
            }
        }
        log::debug!(
            "receipt moved {changed} of {} messages to {status:?}",
            receipt.message_ids.len(),
        );
        // Read receipts advance all earlier messages.
        if status >= Delivery::Read
            && newest > 0
            && let Ok(ids) = self.archive.advance_statuses(&chat, newest, status, at)
        {
            for id in ids {
                updated.extend(self.updated_message(&chat, &id));
            }
        }
        self.emit_messages(updated);
        self.emit_chat(&chat);
    }
}

impl Worker {
    /// Sends a played receipt for an incoming voice message.
    pub(super) fn mark_played(&mut self, chat: ChatId, message: String, sender: String) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            return;
        };
        let sender = if jid.is_group() {
            sender.parse::<Jid>().ok()
        } else {
            None
        };
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            if !receipts_allowed(&client, &jid, &commands, session_generation).await {
                return;
            }
            if let Err(error) = client
                .mark_as_played(&jid, sender.as_ref(), &[message.as_str()])
                .await
            {
                log::debug!("played receipt not sent: {error}");
            }
        });
    }
}
