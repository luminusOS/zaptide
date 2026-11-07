use super::*;

impl Worker {
    /// Complete only requests identified by the phone's response session id.
    pub(super) fn answer_older(
        &mut self,
        filed: Vec<(ChatId, usize, Option<bool>)>,
        protocol_id: Option<String>,
    ) {
        let Some(protocol_id) = protocol_id else {
            // ON_DEMAND type alone cannot identify which local retry produced this chunk.
            return;
        };
        for (chat, count, more_on_phone) in filed {
            let Some(request) = self.older.pending_older.get_mut(&chat) else {
                continue;
            };
            if request.protocol_id.as_deref() == Some(protocol_id.as_str()) {
                self.record_older_response(chat, count, more_on_phone);
            } else if request.protocol_id.is_none() {
                if let Some(early) = request.early_responses.get_mut(&protocol_id) {
                    early.received_count = early.received_count.saturating_add(count);
                    if let Some(more_on_phone) = more_on_phone {
                        early.more_on_phone = Some(more_on_phone);
                    }
                } else if request.early_responses.len() < MAX_EARLY_HISTORY_IDS {
                    let mut early = EarlyOlderResponse {
                        received_count: count,
                        ..Default::default()
                    };
                    early.more_on_phone = more_on_phone;
                    request.early_responses.insert(protocol_id.clone(), early);
                } else {
                    log::warn!(
                        "too many unrelated history IDs arrived before request registration"
                    );
                }
            }
        }
    }

    pub(super) fn record_older_response(
        &mut self,
        chat: ChatId,
        count: usize,
        more_on_phone: Option<bool>,
    ) {
        let Some(request) = self.older.pending_older.get_mut(&chat) else {
            return;
        };
        request.received_count = request.received_count.saturating_add(count);
        // `more_on_phone` comes from EndOfHistoryTransferType and marks the target chat's final chunk.
        if let Some(more_on_phone) = more_on_phone {
            request.more_on_phone = Some(more_on_phone);
        }
        if request.more_on_phone.is_some() {
            let request = self
                .older
                .pending_older
                .remove(&chat)
                .expect("request still present");
            self.finish_older_request(
                chat.clone(),
                request.received_count,
                request.more_on_phone,
                request.before,
                request.request,
            );
            if let Some((queued, before)) = request.queued {
                self.finish_older_request(
                    chat,
                    request.received_count,
                    request.more_on_phone,
                    before,
                    queued,
                );
            }
        }
    }

    pub(super) fn finish_older_request(
        &mut self,
        chat: ChatId,
        count: usize,
        more_on_phone: Option<bool>,
        (before_time, before_id): super::PageKey,
        request: u64,
    ) {
        let mut more = count > 0 && more_on_phone != Some(false);
        match self
            .archive
            .messages(&chat, Some((before_time, &before_id)), 501)
        {
            Ok(mut messages) => {
                if messages.len() > 500 {
                    messages.remove(0);
                    more = true;
                }
                for message in &mut messages {
                    self.polish(message);
                }
                self.emit(Event::Messages {
                    request: Some(request),
                    chat: chat.clone(),
                    messages,
                    older: true,
                    complete: false,
                })
            }
            Err(error) => log::warn!("could not read older messages: {error}"),
        }
        self.emit(Event::OlderFetched {
            chat,
            more,
            request,
        });
    }

    pub(super) fn older_request_started(
        &mut self,
        chat: ChatId,
        request_id: u64,
        protocol_id: String,
    ) {
        let Some(request) = self.older.pending_older.get_mut(&chat) else {
            return;
        };
        if request.request_id != request_id {
            return;
        }
        request.protocol_id = Some(protocol_id.clone());
        let early = request.early_responses.remove(&protocol_id);
        if let Some(early) = early {
            self.record_older_response(chat, early.received_count, early.more_on_phone);
        }
    }

    /// Times out unanswered phone-history requests.
    pub(super) fn expire_older_requests(&mut self) {
        let expired: Vec<ChatId> = self
            .older
            .pending_older
            .iter()
            .filter(|(_, request)| request.asked.elapsed() > PHONE_PATIENCE)
            .map(|(chat, _)| chat.clone())
            .collect();
        for chat in expired {
            let pending = self
                .older
                .pending_older
                .remove(&chat)
                .expect("request still present");

            self.emit(Event::OlderFetched {
                chat: chat.clone(),
                more: true,
                request: pending.request,
            });
            if let Some((request, before)) = pending.queued {
                self.retry_queued_older(chat.clone(), request, before);
            }
            // Report the timeout once per chat; later retries back off silently.
            if self.older.older_warned.insert(chat) {
                self.emit(Event::Error(
                    "Your phone did not send older messages. Check that it is online".to_owned(),
                ));
            }
        }
    }

    pub(super) fn retry_queued_older(
        &mut self,
        chat: ChatId,
        request: u64,
        before: super::PageKey,
    ) {
        let has_archived = self
            .archive
            .messages(&chat, Some((before.0, &before.1)), 1)
            .is_ok_and(|messages| !messages.is_empty());
        if has_archived {
            self.finish_older_request(chat, 1, Some(true), before, request);
        } else {
            self.fetch_older(chat, request, Some(before));
        }
    }

    pub(super) fn fetch_older(
        &mut self,
        chat: ChatId,
        request: u64,
        before: Option<super::PageKey>,
    ) {
        if self.archive_cleanup_failed {
            return;
        }

        if self.older.pending_older.contains_key(&chat) {
            let before = before.unwrap_or_else(|| (i64::MAX, String::new()));
            self.older.pending_older.get_mut(&chat).unwrap().queued = Some((request, before));

            return;
        }
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            // Offline requests retry after reconnection; the banner shows state.
            self.emit(Event::OlderFetched {
                chat,
                more: true,
                request,
            });
            return;
        };
        // Chats without messages request history from the current time.
        let (id, from_me, timestamp) = match self.archive.oldest(&chat) {
            Ok(Some(oldest)) => (oldest.id, oldest.from_me, oldest.timestamp),
            _ => (String::new(), false, crate::util::now()),
        };
        let asked = Instant::now();
        self.older.next_older_request_id = self.older.next_older_request_id.wrapping_add(1);
        let request_id = self.older.next_older_request_id;
        self.older.pending_older.insert(
            chat.clone(),
            PendingOlder {
                request,
                queued: None,
                request_id,
                asked,
                before: (timestamp, id.clone()),
                protocol_id: None,
                received_count: 0,
                more_on_phone: None,
                early_responses: HashMap::new(),
            },
        );
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            // The pinned library documents the returned stanza ID as the
            // correlation value carried by peerDataRequestSessionId.
            match client
                // Despite its `Ms` name, the protocol field takes Unix seconds.
                // https://github.com/tulir/whatsmeow/commit/54650307d891f89ab346a57953d316106caee371
                .fetch_message_history(&jid, &id, from_me, timestamp, PHONE_BATCH)
                .await
            {
                Ok(protocol_id) => {
                    let _ = commands.send(Command::OlderStarted {
                        session_generation,
                        request_id,
                        chat,
                        protocol_id,
                    });
                }
                Err(error) => {
                    log::warn!("could not request older messages");
                    let _ = commands.send(Command::OlderFailed {
                        session_generation,
                        request_id,
                        chat: chat.clone(),
                        error: format!("Could not request older messages from your phone: {error}"),
                    });
                }
            }
        });
    }
}

impl Worker {
    pub(super) fn load_chat(&mut self, chat: ChatId, before: Option<super::PageKey>, request: u64) {
        if before.is_none()
            && let Ok(ids) = self.archive.starred_ids(&chat)
        {
            self.emit(Event::StarredIds {
                chat: chat.clone(),
                ids,
            });
        }
        self.send_page(&chat, before.clone(), request);
        if before.is_none() && ChatKind::from_id(&chat) == ChatKind::Group {
            self.request_group_info(&chat, false);
        }
        if before.is_none()
            && ChatKind::from_id(&chat) == ChatKind::Direct
            && chat != self.me()
            && self.presence_subscribed.insert(chat.clone())
            && let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat))
        {
            tokio::spawn(async move {
                if let Err(error) = client.presence().subscribe(jid).await {
                    log::debug!("presence not subscribed: {error}");
                }
            });
        }
    }

    /// Sends one page of the archive, the newest one or the one before
    /// `before`.
    pub(super) fn send_page(
        &mut self,
        chat: &ChatId,
        before: Option<super::PageKey>,
        request: u64,
    ) {
        if self.withhold(WithheldPage::Page(chat.clone(), before.clone(), request)) {
            return;
        }
        if self.archive_cleanup_failed {
            self.emit(Event::Error(
                "Local conversation cleanup must finish before chats can be opened.".to_owned(),
            ));
            return;
        }
        match self.archive.messages(
            chat,
            before.as_ref().map(|(time, id)| (*time, id.as_str())),
            PAGE + 1,
        ) {
            Ok(mut messages) => {
                let complete = messages.len() <= PAGE;
                if !complete {
                    messages.remove(0);
                }
                for message in &mut messages {
                    self.polish(message);
                }
                self.emit(Event::Messages {
                    request: Some(request),
                    chat: chat.clone(),
                    messages,
                    older: before.is_some(),
                    complete,
                });
            }
            Err(error) => self.emit(Event::Error(format!("Could not read the chat: {error}"))),
        }
    }

    /// Keeps a transcript read for later while private content is withheld.
    /// Its answer would be dropped, and the interface, having asked once, would
    /// wait for it forever: a chat opened then stayed empty until a new message
    /// arrived.
    pub(super) fn withhold(&mut self, page: WithheldPage) -> bool {
        if self.privacy.privacy_ready {
            return false;
        }
        if !self.privacy.withheld_pages.contains(&page) {
            self.privacy.withheld_pages.push(page);
        }
        true
    }

    pub(super) fn load_until(&mut self, chat: ChatId, id: String, before: super::PageKey) {
        if self.withhold(WithheldPage::Until(
            chat.clone(),
            id.clone(),
            before.clone(),
        )) {
            return;
        }
        if self.archive_cleanup_failed {
            return;
        }
        let Ok(Some(target)) = self.archive.message(&chat, &id) else {
            self.emit(Event::Messages {
                request: None,
                chat: chat.clone(),
                messages: Vec::new(),
                older: true,
                complete: false,
            });
            self.emit(Event::Error(
                "This message is not stored on this computer".to_owned(),
            ));
            return;
        };
        match self
            .archive
            .messages_range(&chat, target.timestamp, (before.0, &before.1), 2000)
        {
            Ok(mut messages) => {
                for message in &mut messages {
                    self.polish(message);
                }
                self.emit(Event::Messages {
                    request: None,
                    chat,
                    messages,
                    older: true,
                    complete: false,
                });
            }
            Err(error) => self.emit(Event::Error(format!("Could not read the chat: {error}"))),
        }
    }
}
