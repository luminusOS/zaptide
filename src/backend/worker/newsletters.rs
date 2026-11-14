use super::*;

impl Worker {
    pub(super) fn request_newsletter_info(&mut self, id: &str) {
        if !self.newsletter_names.contains_key(id)
            && self.newsletter_requested.insert(id.to_owned())
        {
            self.newsletter_queue.push_back(id.to_owned());
        }
    }

    pub(super) fn fetch_subscribed_newsletters(&mut self) {
        if self.newsletter_list_running {
            return;
        }
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
        if self.newsletter_running
            || self.newsletter_list_running
            || !matches!(self.status, LinkStatus::Connected)
        {
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
            if names.is_empty() {
                self.newsletter_requested.remove(&id);
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
