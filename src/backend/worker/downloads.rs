use super::*;

impl Worker {
    pub(super) async fn retry_deferred_downloads(&mut self, now: Instant) {
        // Take one batch so a still-failing read cannot retry in a tight loop.
        let pending = std::mem::take(&mut self.deferred_downloads);
        for mut download in pending {
            if now < download.retry_at {
                self.deferred_downloads.push(download);
            } else if let Some(completion) = download.completion.take() {
                self.handle_command(completion).await;
            }
        }
    }

    pub(super) fn download(&mut self, chat: ChatId, id: String) {
        if self.archive_cleanup_failed {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.emit(Event::Media {
                chat,
                message: id,
                result: Err(DownloadFailure::Other(
                    "Not connected to WhatsApp".to_owned(),
                )),
            });
            return;
        };
        let raw = self.archive.raw(&chat, &id).ok().flatten();
        let Some(raw) = raw else {
            self.emit(Event::Media {
                chat,
                message: id,
                result: Err(DownloadFailure::Other(
                    "Attachment download keys are missing".to_owned(),
                )),
            });
            return;
        };
        let raw_fingerprint = message_raw_fingerprint(&raw);
        let Some(message) = wa::Message::decode_from_slice(&raw).ok() else {
            self.emit(Event::Media {
                chat,
                message: id,
                result: Err(DownloadFailure::Other(
                    "Attachment download keys are missing".to_owned(),
                )),
            });
            return;
        };
        let base = base_message(&message).clone();
        let (downloadable, mime, file_name): (Box<dyn Downloadable>, String, Option<String>) =
            if let Some(image) = base.image_message.as_option() {
                (
                    Box::new(image.clone()),
                    image.mimetype.clone().unwrap_or_default(),
                    None,
                )
            } else if let Some(video) = base
                .video_message
                .as_option()
                .or(base.ptv_message.as_option())
            {
                (
                    Box::new(video.clone()),
                    video.mimetype.clone().unwrap_or_default(),
                    None,
                )
            } else if let Some(audio) = base.audio_message.as_option() {
                (
                    Box::new(audio.clone()),
                    audio.mimetype.clone().unwrap_or_default(),
                    None,
                )
            } else if let Some(document) = base.document_message.as_option() {
                (
                    Box::new(document.clone()),
                    document.mimetype.clone().unwrap_or_default(),
                    document.file_name.clone(),
                )
            } else if let Some(sticker) = base.sticker_message.as_option() {
                (
                    Box::new(sticker.clone()),
                    sticker.mimetype.clone().unwrap_or_default(),
                    None,
                )
            } else {
                self.emit(Event::Media {
                    chat,
                    message: id,
                    result: Err(DownloadFailure::Other(
                        "This message has no downloadable file".to_owned(),
                    )),
                });
                return;
            };
        // Keep metadata needed for one media re-upload request and retry.
        let media_key = base
            .image_message
            .as_option()
            .and_then(|media| media.media_key.clone())
            .or_else(|| {
                base.video_message
                    .as_option()
                    .or(base.ptv_message.as_option())
                    .and_then(|media| media.media_key.clone())
            })
            .or_else(|| {
                base.audio_message
                    .as_option()
                    .and_then(|media| media.media_key.clone())
            })
            .or_else(|| {
                base.document_message
                    .as_option()
                    .and_then(|media| media.media_key.clone())
            })
            .or_else(|| {
                base.sticker_message
                    .as_option()
                    .and_then(|media| media.media_key.clone())
            })
            .unwrap_or_default();
        let jid = Self::jid_of(&chat);
        let row = self.archive.message(&chat, &id).ok().flatten();
        let is_from_me = row.as_ref().is_some_and(|row| row.from_me);
        let participant = match (&jid, &row) {
            (Some(jid), Some(row)) if jid.is_group() => Self::jid_of(&row.sender),
            _ => None,
        };
        let mut fresh_base = base;
        let mut refreshed = move |direct: String| -> Option<Box<dyn Downloadable>> {
            if let Some(media) = fresh_base.image_message.as_option_mut() {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            if let Some(media) = fresh_base
                .video_message
                .as_option_mut()
                .or(fresh_base.ptv_message.as_option_mut())
            {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            if let Some(media) = fresh_base.audio_message.as_option_mut() {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            if let Some(media) = fresh_base.document_message.as_option_mut() {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            if let Some(media) = fresh_base.sticker_message.as_option_mut() {
                media.direct_path = Some(direct);
                media.url = None;
                return Some(Box::new(media.clone()));
            }
            None
        };
        let dir = self.dirs.media_cache_dir();
        let destination = media_path(&dir, &chat, &id, &mime, file_name.as_deref());
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let session_cache_lock = self.session_cache_lock.clone();
        tokio::spawn(async move {
            let fetch = |downloadable: Box<dyn Downloadable>| {
                let client = client.clone();
                let dir = dir.clone();
                let path = download_staging_path(&dir);
                let session_generation_shared = session_generation_shared.clone();
                let session_cache_lock = session_cache_lock.clone();
                async move {
                    stream_session_cache_file(
                        &dir,
                        &path,
                        session_generation,
                        &session_generation_shared,
                        &session_cache_lock,
                        |file| async move { client.download_to_writer(&*downloadable, file).await },
                    )
                    .await
                }
            };
            let result = match fetch(downloadable).await {
                Ok(path) => Ok(path),
                Err(StagedDownload::Local(error)) => Err(DownloadFailure::Other(error)),
                Err(StagedDownload::Fetch(error)) => {
                    let expired = failures::needs_reupload(&error);
                    match (&jid, expired && !media_key.is_empty()) {
                        (Some(jid), true) => {
                            // Ask the phone to re-upload expired media, then retry once.
                            let request = MediaReuploadRequest {
                                msg_id: &id,
                                chat_jid: jid,
                                media_key: &media_key,
                                is_from_me,
                                participant: participant.as_ref(),
                            };
                            match client.media_reupload().request(&request).await {
                                Ok(MediaRetryResult::Success { direct_path }) => {
                                    match refreshed(direct_path) {
                                        Some(again) => {
                                            fetch(again).await.map_err(|error| match error {
                                                StagedDownload::Fetch(error) => {
                                                    failures::download_failure(&error)
                                                }
                                                StagedDownload::Local(error) => {
                                                    DownloadFailure::Other(error)
                                                }
                                            })
                                        }
                                        None => Err(failures::download_failure(&error)),
                                    }
                                }
                                Ok(_) => Err(DownloadFailure::Expired),
                                Err(_error) => {
                                    log::info!("media re-upload was not granted");
                                    Err(DownloadFailure::Expired)
                                }
                            }
                        }
                        _ => Err(failures::download_failure(&error)),
                    }
                }
            };
            let _ = Completion {
                generation: session_generation,
                done: Done::Downloaded {
                    chat,
                    id,
                    raw_fingerprint,
                    destination,
                    result,
                },
            }
            .send(&commands);
        });
    }
}
