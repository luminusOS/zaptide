use super::*;

impl Worker {
    pub(super) fn request_group_info(&mut self, id: &str, force: bool) {
        if force {
            self.group_info.group_info_requested.remove(id);
        } else {
            let known = self.archive.chat(id).ok().flatten().is_some_and(|chat| {
                chat.name != fallback_name(id) && !chat.participants.is_empty()
            });
            if known {
                return;
            }
        }
        if !self.group_info.group_info_requested.insert(id.to_owned()) {
            return;
        }
        if force {
            self.group_info.group_info_queue.push_front(id.to_owned());
        } else {
            self.group_info.group_info_queue.push_back(id.to_owned());
        }
    }

    pub(super) fn group_retry_delay(tries: u32) -> Duration {
        Duration::from_secs(30 * 2u64.pow(tries.saturating_sub(1).min(5)))
            .min(Duration::from_secs(600))
    }

    /// Sends a limited number of group metadata requests per tick.
    pub(super) fn pump_group_info(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = {
            let (due, later): (Vec<_>, Vec<_>) =
                std::mem::take(&mut self.group_info.group_info_retry)
                    .into_iter()
                    .partition(|(at, _)| *at <= now);
            self.group_info.group_info_retry = later;
            due.into_iter().map(|(_, id)| id).collect()
        };
        for id in due {
            if self.group_info.group_info_requested.insert(id.clone()) {
                self.group_info.group_info_queue.push_back(id);
            }
        }
        for _ in 0..2 {
            let Some(id) = self.group_info.group_info_queue.pop_front() else {
                return;
            };
            self.query_group_info(&id);
        }
    }

    /// Schedules metadata retry with backoff, or stops on permanent failure.
    pub(super) fn handle_failed_group(&mut self, chat: String, permanent: bool) {
        self.group_info.group_info_requested.remove(&chat);
        if permanent {
            self.group_info.group_info_tries.remove(&chat);
        } else {
            let tries = self
                .group_info
                .group_info_tries
                .entry(chat.clone())
                .or_insert(0);
            *tries += 1;
            if *tries <= 7 {
                self.group_info
                    .group_info_retry
                    .push((Instant::now() + Self::group_retry_delay(*tries), chat));
            }
        }
    }

    pub(super) fn query_group_info(&mut self, id: &str) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(id)) else {
            // Requeue until the link is available.
            self.group_info.group_info_requested.remove(id);
            let tries = self
                .group_info
                .group_info_tries
                .entry(id.to_owned())
                .or_insert(0);
            *tries += 1;
            self.group_info.group_info_retry.push((
                Instant::now() + Self::group_retry_delay(*tries),
                id.to_owned(),
            ));
            return;
        };
        let commands = self.commands.clone();
        let chat = id.to_owned();
        let me: Vec<String> = [self.identity.me_pn.clone(), self.identity.me_lid.clone()]
            .into_iter()
            .flatten()
            .collect();
        let lids = self.identity.lid_to_pn.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            match client.groups().get_metadata(&jid).await {
                Ok(metadata) => {
                    let canonical = |jid: &Jid| -> String {
                        if jid.is_lid()
                            && let Some(pn) = lids.get(jid.user_base())
                        {
                            return format!("{pn}@s.whatsapp.net");
                        }
                        jid.to_non_ad_string()
                    };
                    let mut participants = Vec::new();
                    let mut admin = false;
                    for participant in &metadata.participants {
                        let id = participant
                            .phone_number
                            .as_ref()
                            .map(canonical)
                            .unwrap_or_else(|| canonical(&participant.jid));
                        let mine = me.contains(&id)
                            || participant
                                .lid
                                .as_ref()
                                .is_some_and(|lid| me.contains(&lid.to_non_ad_string()))
                            || me.contains(&participant.jid.to_non_ad_string());
                        if mine && participant.is_admin() {
                            admin = true;
                        }
                        participants.push(id);
                    }
                    let _ = commands.send(Command::GroupInfo {
                        session_generation,
                        chat,
                        name: (!metadata.subject.is_empty()).then(|| metadata.subject.clone()),
                        participants,
                        read_only: metadata.is_announcement && !admin,
                        // GroupEphemeralSettings carries a trigger mode, not a
                        // timestamp; a zero setting timestamp keeps later
                        // authoritative updates (protocol messages) able to
                        // override the value fetched here.
                        ephemeral_expiration: metadata
                            .ephemeral
                            .as_ref()
                            .and_then(|value| value.expiration),
                        ephemeral_setting_timestamp: None,
                    });
                }
                Err(error) => {
                    let permanent = failures::group_lookup_is_final(&error);
                    log::warn!("could not fetch group metadata");
                    let _ = commands.send(Command::GroupInfoFailed {
                        session_generation,
                        chat,
                        permanent,
                    });
                }
            }
        });
    }
}
