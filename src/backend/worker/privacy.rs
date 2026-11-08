use super::*;

impl Worker {
    pub(super) fn begin_privacy_recovery(&mut self) -> bool {
        if self.privacy.privacy_confirmed
            || self.privacy.privacy_recovering
            || Instant::now() < self.privacy.privacy_retry
            || !matches!(self.status, LinkStatus::Connected)
        {
            return false;
        }
        if !self.privacy.privacy_ready && self.privacy.privacy_reveal_at.is_none() {
            self.privacy.privacy_reveal_at = Some(Instant::now() + PRIVACY_GRACE);
        }
        self.privacy.privacy_recovering = true;
        true
    }

    pub(super) fn refresh_legacy_preferences(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.begin_privacy_recovery() {
            return;
        }
        let sender = self.wa_sender.clone();
        let generation = self.privacy.privacy_generation;
        if !self.privacy.privacy_ready {
            self.emit(Event::Syncing(true));
        }
        let snapshot = self.privacy.privacy_snapshot;
        tokio::spawn(async move {
            let (locks, complete) = recover_chat_preferences(snapshot, |collections, mode| {
                client.resync_app_state(collections, mode)
            })
            .await;
            // Use the same queue as the replayed mutations, so lock updates
            // are applied before the completion marker can expose chat rows.
            let _ = sender.send(RuntimeEvent::PreferencesRecovered {
                generation,
                locks,
                complete,
            });
        });
    }

    /// `locks` says the lock collection synced; `complete` that everything
    /// requested did, so the one-time recovery need not run again.
    pub(super) fn preferences_recovered(&mut self, generation: u64, locks: bool, complete: bool) {
        // A completed task from an unlinked device cannot authorize showing
        // chats belonging to the next linked account.
        if generation != self.privacy.privacy_generation {
            return;
        }
        self.privacy.privacy_recovering = false;
        if locks
            && (!complete
                || self
                    .archive
                    .set_meta("chat_privacy_ready_v1", "complete")
                    .is_ok())
        {
            // Without `complete` the marker stays unset, so the next start
            // retries the settings this run could not recover.
            self.privacy.privacy_confirmed = true;
            self.privacy.privacy_snapshot = false;
            self.privacy.privacy_reveal_at = None;
            self.reveal_private_content();
            if !complete {
                self.warn_unconfirmed_preferences();
            }
        } else {
            self.privacy.privacy_attempts = self.privacy.privacy_attempts.saturating_add(1);
            self.privacy.privacy_retry =
                Instant::now() + privacy_backoff(self.privacy.privacy_attempts);
            log::warn!(
                "chat lock state recovery failed (attempt {}); retrying later",
                self.privacy.privacy_attempts
            );
            // Waiting longer does not help a failed sync: show what is known.
            self.reveal_unconfirmed();
        }
    }

    pub(super) fn reveal_unconfirmed_after_grace(&mut self) {
        if !self.privacy.privacy_snapshot && !matches!(self.status, LinkStatus::Connected) {
            // A pending resync survives reconnects. Keep its deadline so a
            // silent attempt can still reveal once the link comes back.
            if !self.privacy.privacy_recovering {
                self.privacy.privacy_reveal_at = None;
            }
            return;
        }
        if self
            .privacy
            .privacy_reveal_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.reveal_unconfirmed();
        }
    }

    /// Shows chats whose lock state could not be confirmed yet. Chats already
    /// known to be locked stay hidden, and later lock updates still apply.
    pub(super) fn reveal_unconfirmed(&mut self) {
        self.privacy.privacy_reveal_at = None;
        if self.privacy.privacy_ready {
            return;
        }
        self.reveal_private_content();
        self.warn_unconfirmed_preferences();
    }

    pub(super) fn warn_unconfirmed_preferences(&mut self) {
        if !self.privacy.privacy_warned {
            self.privacy.privacy_warned = true;
            self.emit(Event::Info(
                "Chat settings could not be read from your phone yet. Some chats may show the settings stored on this computer."
                    .to_owned(),
            ));
        }
    }

    pub(super) fn reveal_private_content(&mut self) {
        if self.privacy.privacy_ready {
            return;
        }
        self.privacy.privacy_ready = true;
        self.load_state();
        self.emit(Event::Syncing(self.syncing));
        // The picker may have been sent an empty Received shelf meanwhile.
        self.emit_stickers();
        // Answer the reads made while content was withheld, now that the
        // chat list they belong to has been sent.
        for page in std::mem::take(&mut self.privacy.withheld_pages) {
            match page {
                WithheldPage::Page(chat, before, request) => self.send_page(&chat, before, request),
                WithheldPage::Until(chat, id, before) => self.load_until(chat, id, before),
            }
        }
    }

    pub(super) fn reset_privacy(&mut self) {
        self.privacy.privacy_ready = false;
        self.privacy.privacy_confirmed = false;
        self.privacy.privacy_snapshot = false;
        self.privacy.privacy_reveal_at = None;
        self.privacy.privacy_attempts = 0;
        self.privacy.privacy_warned = false;
        self.privacy.privacy_recovering = false;
        self.privacy.privacy_retry = Instant::now();
        // Reads for the unlinked account must not be answered for the next.
        self.privacy.withheld_pages.clear();
    }
}
