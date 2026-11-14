use super::*;

impl Worker {
    pub(super) async fn start_bot(&mut self) {
        let path = self.dirs.session_db();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let store = match device_store::open(&path).await {
            Ok(store) => store,
            Err(error) => {
                self.set_status(LinkStatus::Failed(format!(
                    "Could not open the device store: {error:#}"
                )));
                return;
            }
        };
        let sender = self.wa_sender.clone();
        let bot = Bot::builder()
            .with_backend(store)
            // The window owns presence; see `worker::presence`.
            .with_presence_policy(PresencePolicy::Manual)
            // WhatsApp reads the linked-device name, version, and icon at pairing.
            .with_device_props(
                DevicePropsOverride::new()
                    .with_os("ZapTide")
                    .with_version(app_version())
                    .with_platform_type(wa::device_props::PlatformType::DESKTOP),
            )
            .with_event_handler(UiEvents(sender))
            .build()
            .await;
        match bot {
            Ok(bot) => {
                let handle = bot.spawn();
                self.spawn_presence(handle.client());
                self.client = Some(handle.client());
                self.handle = Some(handle);
                self.set_status(LinkStatus::Connecting);
            }
            Err(error) => self.set_status(LinkStatus::Failed(format!(
                "Could not start WhatsApp: {error}"
            ))),
        }
    }

    pub(super) async fn stop_bot(&mut self) -> bool {
        self.client = None;
        let Some(handle) = self.handle.take() else {
            return true;
        };
        if wait_for_shutdown(Duration::from_secs(5), handle.shutdown()).await {
            true
        } else {
            log::warn!("the WhatsApp connection did not stop in time");
            false
        }
    }
}

impl Worker {
    /// Content is hidden again and lock state must be read from the next
    /// account: used whenever a link ends, however it ends.
    pub(super) async fn on_logged_out(&mut self) {
        self.deferred_incoming.clear();
        self.privacy.privacy_generation = self.privacy.privacy_generation.wrapping_add(1);
        self.pair_request_id = self.pair_request_id.wrapping_add(1);
        self.session_generation = self.session_generation.wrapping_add(1);
        self.session_generation_shared
            .store(self.session_generation, Ordering::Release);
        self.clear_history_sync_state();
        // The archive is cleared on logout, so pending answers have nothing to reopen.
        self.answer_sends.clear();
        self.pending_revokes.clear();
        self.deferred_downloads.clear();
        let markers = archive_cleanup_markers(&self.dirs);
        let archive_marker = self.archive.mark_logout_cleanup_required().is_ok();
        let file_marker = persist_archive_cleanup_marker(&markers);
        if !archive_marker && !file_marker {
            log::error!("could not persist any logout cleanup marker");
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Error(
                "Local conversations could not be cleared. Restart is blocked to protect data."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Local conversation cleanup marker unavailable".to_owned(),
            ));
            return;
        }
        let (wa_sender, wa_events) = mpsc::unbounded_channel();
        self.wa_sender = wa_sender;
        self.wa_events = wa_events;
        if !self.stop_bot().await {
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Chats(Vec::new()));
            self.emit(Event::Contacts(Vec::new()));
            self.emit(Event::Error(
                "Local conversations could not be cleared because the device session did not stop."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Device session shutdown timed out before local cleanup".to_owned(),
            ));
            return;
        }
        let cleanup_result = {
            let _cache_guard = self.session_cache_lock.lock().await;
            clear_logged_out_data(&self.dirs, &self.archive)
        };
        if let Err(error) = cleanup_result {
            log::warn!("could not clear account data: {error}");
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Chats(Vec::new()));
            self.emit(Event::Contacts(Vec::new()));
            self.emit(Event::Error(
                "Local conversations could not be cleared. Restart is blocked to protect data."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Local conversation cleanup failed".to_owned(),
            ));
            return;
        }
        if let Err(error) = remove_archive_cleanup_markers(&markers) {
            log::warn!("could not remove archive cleanup marker: {error}");
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Chats(Vec::new()));
            self.emit(Event::Error(
                "Local conversations were cleared, but cleanup could not be finalized. Restart is blocked."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Local conversation cleanup could not be finalized".to_owned(),
            ));
            return;
        }
        if let Err(error) = self.archive.finish_logout_cleanup() {
            log::error!("could not finish the encrypted archive cleanup marker: {error}");
            self.archive_cleanup_failed = true;
            self.reset_privacy();
            self.emit(Event::Link(LinkStatus::LoggedOut));
            self.emit(Event::Error(
                "Local conversations were cleared, but cleanup could not be finalized. Restart is blocked."
                    .to_owned(),
            ));
            self.set_status(LinkStatus::Failed(
                "Local conversation cleanup could not be finalized".to_owned(),
            ));
            return;
        }
        self.identity.lid_to_pn.clear();
        self.identity.contacts.clear();
        self.group_info.group_info_requested.clear();
        self.group_info.group_info_queue.clear();
        self.group_info.group_info_tries.clear();
        self.group_info.group_info_retry.clear();
        self.newsletter_names.clear();
        self.newsletter_requested.clear();
        self.newsletter_queue.clear();
        self.newsletter_running = false;
        self.newsletter_list_running = false;
        self.presence_subscribed.clear();
        self.read_sync = ReadSync::default();
        self.poll_decrypting = 0;
        self.poll_sending.clear();
        self.abandon_call();
        self.poll_history = Default::default();
        self.pending_avatars.clear();
        self.avatar_generations.clear();
        self.sticker_fetches.clear();
        self.sticker_downloads.clear();
        self.identity.me_pn = None;
        self.identity.me_lid = None;
        self.identity.me_name = None;
        self.emit_profile();
        self.identity.me_about = None;
        self.qr = None;
        self.pair_code = None;
        self.pairing_phone = None;
        self.emit(Event::Chats(Vec::new()));
        self.emit(Event::Contacts(Vec::new()));
        self.reset_privacy();
        self.set_status(LinkStatus::LoggedOut);
        // Recreate the store so the next connection starts linking.
        self.start_bot().await;
    }
}
