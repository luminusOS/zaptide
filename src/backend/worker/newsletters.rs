use super::*;

impl Worker {
    fn newsletter_retry_delay(tries: u32) -> Duration {
        Duration::from_secs(30 * 2u64.pow(tries.saturating_sub(1).min(5)))
            .min(Duration::from_secs(600))
    }

    pub(super) fn request_newsletter_info(&mut self, id: &str) {
        if !self.newsletter_names.contains_key(id)
            && self.newsletter_requested.insert(id.to_owned())
        {
            self.newsletter_queue.push_back(id.to_owned());
        }
    }

    pub(super) fn fetch_subscribed_newsletters(&mut self) {
        self.newsletter_list_pending = true;
        self.newsletter_tries.retain(|id, tries| {
            if *tries > 7 {
                self.newsletter_requested.remove(id);
                false
            } else {
                true
            }
        });
        let Some(client) = self.client.clone() else {
            return;
        };
        if let Ok(chats) = self.archive.chats() {
            for chat in chats {
                if chat.id.ends_with("@newsletter") {
                    self.request_newsletter_info(&chat.id);
                }
            }
        }
        if self.newsletter_running || self.newsletter_list_running {
            return;
        }
        self.newsletter_list_pending = false;
        let commands = self.commands.clone();
        let generation = self.session_generation;
        self.newsletter_list_running = true;
        tokio::spawn(async move {
            let names = match tokio::time::timeout(
                Duration::from_secs(30),
                client.newsletter().list_subscribed(),
            )
            .await
            {
                Ok(Ok(entries)) => entries
                    .into_iter()
                    .map(|entry| (entry.jid.to_non_ad_string(), entry.name))
                    .collect(),
                _ => {
                    log::warn!("could not fetch subscribed channel metadata");
                    Vec::new()
                }
            };
            let _ = Completion {
                generation,
                done: Done::NewsletterNames {
                    names,
                    requested: None,
                },
            }
            .send(&commands);
        });
    }

    pub(super) fn pump_newsletter_info(&mut self) {
        let now = Instant::now();
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.newsletter_retry)
            .into_iter()
            .partition(|(at, _)| *at <= now);
        self.newsletter_retry = later;
        for (_, id) in due {
            if !self.newsletter_names.contains_key(&id) {
                self.newsletter_queue.push_back(id);
            }
        }
        if self.newsletter_running
            || self.newsletter_list_running
            || !matches!(self.status, LinkStatus::Connected)
        {
            return;
        }
        if self.newsletter_list_pending {
            self.fetch_subscribed_newsletters();
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let id = loop {
            let Some(id) = self.newsletter_queue.pop_front() else {
                return;
            };
            if !self.newsletter_names.contains_key(&id) {
                break id;
            }
        };
        let Some(jid) = Self::jid_of(&id) else { return };
        self.newsletter_running = true;
        let commands = self.commands.clone();
        let generation = self.session_generation;
        tokio::spawn(async move {
            let names = match tokio::time::timeout(
                Duration::from_secs(30),
                client.newsletter().get_metadata(&jid),
            )
            .await
            {
                Ok(Ok(entry)) => vec![(entry.jid.to_non_ad_string(), entry.name)],
                _ => {
                    log::warn!("could not fetch channel metadata");
                    Vec::new()
                }
            };
            let _ = Completion {
                generation,
                done: Done::NewsletterNames {
                    names,
                    requested: Some(id),
                },
            }
            .send(&commands);
        });
    }

    pub(super) fn apply_newsletter_names(
        &mut self,
        names: Vec<(ChatId, String)>,
        requested: Option<ChatId>,
    ) {
        if let Some(id) = requested {
            self.newsletter_running = false;
            if !names
                .iter()
                .any(|(name_id, name)| name_id == &id && !name.trim().is_empty())
            {
                let tries = self.newsletter_tries.entry(id.clone()).or_insert(0);
                *tries += 1;
                if *tries <= 7 {
                    self.newsletter_retry
                        .push((Instant::now() + Self::newsletter_retry_delay(*tries), id));
                }
            } else {
                self.newsletter_tries.remove(&id);
            }
        } else {
            self.newsletter_list_running = false;
        }
        for (id, name) in names {
            if !id.ends_with("@newsletter") || name.trim().is_empty() {
                continue;
            }
            self.newsletter_names.insert(id.clone(), name.clone());
            if self
                .archive
                .chat(&id)
                .ok()
                .flatten()
                .is_some_and(|chat| chat.name != name)
                && self.archive.rename_chat(&id, &name).is_ok()
            {
                self.emit_chat(&id);
            }
        }
    }
}
