use super::*;

impl Worker {
    pub(super) async fn handle_wa_event(&mut self, event: Arc<wa_events::Event>) {
        use wa_events::Event as E;
        match &*event {
            E::PairingQrCode(qr) => {
                self.qr = Some(qr.code.clone());
                let status = self.unlinked();
                self.set_status(status);
            }
            E::PairingCode(code) => {
                self.pair_code = Some(code.code.clone());
                let status = self.unlinked();
                self.set_status(status);
            }
            E::PairingCodeError(error) => {
                self.pair_code = None;
                self.pairing_phone = None;
                self.emit(Event::Error(format!(
                    "Could not link by phone number: {}",
                    error.error
                )));
                let status = self.unlinked();
                self.set_status(status);
            }
            E::PairingQrCodesExhausted(exhausted) => {
                self.qr = None;
                let status = self.unlinked();
                self.set_status(status);
                if exhausted.disconnected
                    && let Some(client) = self.client.clone()
                {
                    tokio::spawn(async move { client.reconnect_immediately().await });
                }
            }
            E::PairSuccess(pair) => {
                let pn = pair.id.to_non_ad_string();
                let lid = pair.lid.to_non_ad_string();
                if !self.archive_matches_account(Some(&pn), Some(&lid)) {
                    self.emit(Event::Error(
                        "This archive belongs to another account; clearing it before linking."
                            .to_owned(),
                    ));
                    self.on_logged_out().await;
                    return;
                }
                self.qr = None;
                self.pair_code = None;
                self.pairing_phone = None;
                self.remember_identity(Some(pair.id.clone()), Some(pair.lid.clone()), None);
                self.set_status(LinkStatus::Connecting);
            }
            E::Connected(_) => {
                let (pn, lid, name) = match &self.client {
                    Some(client) => (client.pn(), client.lid(), Some(client.push_name())),
                    None => (None, None, None),
                };
                let pn_label = pn.as_ref().map(Jid::to_non_ad_string);
                let lid_label = lid.as_ref().map(Jid::to_non_ad_string);
                if !self.archive_matches_account(pn_label.as_deref(), lid_label.as_deref()) {
                    self.emit(Event::Error(
                        "This archive belongs to another account; clearing it before linking."
                            .to_owned(),
                    ));
                    self.on_logged_out().await;
                    return;
                }
                self.remember_identity(pn, lid, name);
                self.set_status(LinkStatus::Connected);
                self.refresh_legacy_preferences();
                self.retry_avatars();
                self.pump_read_sync();
                self.poll_history.reconnect(Instant::now());
                let _ = self.archive.retry_poll_votes();
                self.pump_poll_votes();
                if let Some(client) = self.client.clone() {
                    let me = self.me_pn.clone().and_then(|pn| Self::jid_of(&pn));
                    let commands = self.commands.clone();
                    let session_generation = self.session_generation;
                    tokio::spawn(async move {
                        if let Err(error) = client.presence().set_available().await {
                            log::debug!("presence not announced: {error}");
                        }
                        // whatsapp-rust also enforces the account privacy setting.
                        match client.fetch_privacy_settings().await {
                            Ok(settings) => {
                                let disabled = !account_allows_receipts(&settings);
                                let _ = commands.send(Command::ReceiptsPrivacy {
                                    session_generation,
                                    disabled,
                                });
                            }
                            Err(error) => log::debug!("privacy settings not fetched: {error}"),
                        }
                        if let Some(me) = me {
                            match client
                                .contacts()
                                .get_user_info(std::slice::from_ref(&me))
                                .await
                            {
                                Ok(info) => {
                                    let about = info
                                        .get(&me)
                                        .and_then(|info| info.status.clone())
                                        .filter(|about| !about.is_empty());
                                    let _ = commands.send(Command::MeInfo {
                                        session_generation,
                                        about,
                                    });
                                }
                                Err(error) => log::debug!("own info not fetched: {error}"),
                            }
                        }
                    });
                }
            }
            E::Disconnected(disconnected) => {
                if matches!(self.status, LinkStatus::Connected | LinkStatus::Connecting) {
                    self.set_status(LinkStatus::Disconnected {
                        reason: disconnected.reason.to_string(),
                    });
                }
            }
            E::LoggedOut(_) => self.on_logged_out().await,
            E::ConnectFailure(failure) => {
                if !failure.reason.is_logged_out() {
                    let detail = failure
                        .message
                        .as_ref()
                        .map(|message| format!(": {message}"))
                        .unwrap_or_default();
                    self.emit(Event::Error(format!(
                        "WhatsApp connection failed ({:?}){detail}",
                        failure.reason
                    )));
                }
            }
            E::StreamReplaced(_) => {
                self.emit(Event::Error(
                    "Another WhatsApp Web session replaced this one".to_owned(),
                ));
            }
            E::TemporaryBan(ban) => {
                self.set_status(LinkStatus::Failed(format!(
                    "WhatsApp has temporarily blocked this account ({:?})",
                    ban.code
                )));
            }
            E::ClientOutdated(_) => {
                self.set_status(LinkStatus::Failed(
                    "WhatsApp rejected this version of ZapTide. Update the app".to_owned(),
                ));
            }
            E::Messages(batch) => {
                for inbound in batch.messages.iter() {
                    self.ingest(&inbound.message, &inbound.info);
                }
            }
            E::UndecryptableMessage(undecryptable) => {
                self.ingest_undecryptable(&undecryptable.info);
            }
            E::Receipt(receipt) => self.on_receipt(receipt),
            E::ChatPresence(presence) => {
                self.learn_source(&presence.source);
                // Match WhatsApp: only other participants appear as typing,
                // including when our presence arrives from a linked device.
                if self.is_me(&self.canonical(&presence.source.sender)) {
                    return;
                }
                self.emit(Event::Typing {
                    chat: self.canonical(&presence.source.chat),
                    sender: self.canonical(&presence.source.sender),
                    composing: matches!(presence.state, ChatPresence::Composing),
                });
            }
            E::Presence(presence) => {
                self.emit(Event::Presence {
                    id: self.canonical(&presence.from),
                    online: !presence.unavailable,
                    last_seen: presence.last_seen.map(|when| when.timestamp()),
                });
            }
            E::ContactUpdate(update) => self.on_contact_update(update),
            E::GroupUpdate(update) => {
                let chat = self.canonical(&update.group_jid);
                if let whatsapp_rust::wacore::stanza::groups::GroupNotificationAction::Ephemeral {
                    expiration,
                    ..
                } = &*update.action
                {
                    self.ensure_chat(&chat, None);
                    let timestamp = update.timestamp.timestamp();
                    let accepted = self
                        .archive
                        .set_ephemeral(&chat, *expiration, timestamp)
                        .unwrap_or(false);
                    log::debug!(
                        target: "zaptide::disappearing",
                        "group timer update: duration={expiration}s timestamp={timestamp} accepted={accepted}"
                    );
                    if accepted {
                        self.emit_chat(&chat);
                    }
                }
                self.request_group_info(&chat, true);
            }
            E::ArchiveUpdate(update) => {
                let chat = self.canonical(&update.jid);
                let _ = self
                    .archive
                    .set_archived(&chat, update.action.archived.unwrap_or(false));
                self.emit_chat(&chat);
            }
            E::PinUpdate(update) => {
                let chat = self.canonical(&update.jid);
                self.ensure_chat(&chat, None);
                let _ = self.archive.set_pinned_at(
                    &chat,
                    update.action.pinned.unwrap_or(false),
                    update.timestamp.timestamp_millis(),
                );
                self.emit_chat(&chat);
            }
            E::MuteUpdate(update) => {
                let chat = self.canonical(&update.jid);
                self.ensure_chat(&chat, None);
                let until = if update.action.muted.unwrap_or(false) {
                    Some(seconds(update.action.mute_end_timestamp.unwrap_or(0)))
                } else {
                    None
                };
                let _ =
                    self.archive
                        .set_muted_at(&chat, until, update.timestamp.timestamp_millis());
                self.emit_chat(&chat);
            }
            E::LockChatUpdate(update) => {
                let chat = self.canonical(&update.jid);
                self.ensure_chat(&chat, None);
                let locked = update.action.locked.unwrap_or(false);
                let _ =
                    self.archive
                        .set_locked_at(&chat, locked, update.timestamp.timestamp_millis());
                self.emit_chat(&chat);
            }
            E::MarkChatAsReadUpdate(update) => {
                let chat = self.canonical(&update.jid);
                self.ensure_chat(&chat, None);
                if update.action.read.unwrap_or(true) {
                    let through = update
                        .action
                        .message_range
                        .as_option()
                        .and_then(|range| range.last_message_timestamp);
                    if let Some(through) = through {
                        let _ = self.archive.mark_read_through(&chat, seconds(through));
                    } else {
                        let _ = self.archive.mark_read(&chat);
                    }
                } else {
                    let _ = self.archive.finish_read_sync(&chat, i64::MAX);
                    let unread = self
                        .archive
                        .chat(&chat)
                        .ok()
                        .flatten()
                        .map_or(1, |row| row.unread.max(1));
                    let _ = self.archive.set_unread(&chat, unread);
                }
                self.emit_chat(&chat);
            }
            E::HistorySync(lazy) => self.on_history_sync(lazy).await,
            E::DisappearingModeChanged(update) => {
                // This is a contact's default for new conversations, not a
                // timer change in an existing chat. Per-chat changes arrive
                // as EPHEMERAL_SETTING or a typed group Ephemeral action.
                let id = self.canonical(&update.from);
                let timestamp = update.setting_timestamp.timestamp();
                if self.is_me(&id) {
                    let stored = self
                        .archive
                        .meta("default_ephemeral_setting_timestamp")
                        .ok()
                        .flatten()
                        .and_then(|value| value.parse::<i64>().ok())
                        .unwrap_or_default();
                    if timestamp >= stored {
                        let _ = self
                            .archive
                            .set_meta("default_ephemeral_expiration", &update.duration.to_string());
                        let _ = self.archive.set_meta(
                            "default_ephemeral_setting_timestamp",
                            &timestamp.to_string(),
                        );
                    }
                }
            }
            E::PictureUpdate(update) => {
                let id = self.canonical(&update.jid);
                self.invalidate_avatar_generations(&id);
                {
                    let _cache_guard = self.session_cache_lock.lock().await;
                    let _ = std::fs::remove_file(self.avatar_file(&id, false));
                    let _ = std::fs::remove_file(self.avatar_file(&id, true));
                }
                if update.removed {
                    self.emit(Event::Avatar {
                        id: id.clone(),
                        full: false,
                        path: None,
                    });
                    self.emit(Event::Avatar {
                        id,
                        full: true,
                        path: None,
                    });
                } else {
                    self.fetch_avatar(id.clone(), false);
                    self.fetch_avatar(id, true);
                }
            }
            E::SelfPushNameUpdated(update) => {
                self.me_name = Some(update.new_name.clone());
                let _ = self.archive.set_meta("me_name", &update.new_name);
            }
            E::OfflineSyncCompleted(_) => self.emit_chats(),
            _ => {}
        }
    }
}
