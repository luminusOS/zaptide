use super::*;

async fn download_with_reupload<T, F, Fut, R, Retry, U>(
    downloadable: T,
    retry_allowed: bool,
    fetch: F,
    reupload: R,
    mut refreshed: U,
) -> Result<PathBuf, DownloadFailure>
where
    F: Fn(T) -> Fut,
    Fut: Future<Output = Result<PathBuf, StagedDownload<anyhow::Error>>>,
    R: FnOnce() -> Retry,
    Retry: Future<Output = Option<String>>,
    U: FnMut(String) -> Option<T>,
{
    let error = match fetch(downloadable).await {
        Ok(path) => return Ok(path),
        Err(StagedDownload::Local(error)) => return Err(DownloadFailure::Other(error)),
        Err(StagedDownload::Fetch(error)) => error,
    };
    if !retry_allowed || !failures::needs_reupload(&error) {
        return Err(failures::download_failure(&error));
    }
    let Some(direct_path) = reupload().await else {
        return Err(DownloadFailure::Expired);
    };
    let Some(again) = refreshed(direct_path) else {
        return Err(failures::download_failure(&error));
    };
    fetch(again).await.map_err(|error| match error {
        StagedDownload::Fetch(error) => failures::download_failure(&error),
        StagedDownload::Local(error) => DownloadFailure::Other(error),
    })
}

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
                let session_generation_shared = session_generation_shared.clone();
                let session_cache_lock = session_cache_lock.clone();
                async move {
                    let path = download_staging_path(&dir)
                        .map_err(|error| StagedDownload::Local(error.to_string()))?;
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
            let result = download_with_reupload(
                downloadable,
                jid.is_some() && !media_key.is_empty(),
                fetch,
                || async {
                    let jid = jid.as_ref()?;
                    let request = MediaReuploadRequest {
                        msg_id: &id,
                        chat_jid: jid,
                        media_key: &media_key,
                        is_from_me,
                        participant: participant.as_ref(),
                    };
                    match client.media_reupload().request(&request).await {
                        Ok(MediaRetryResult::Success { direct_path }) => Some(direct_path),
                        Ok(_) => None,
                        Err(_error) => {
                            log::info!("media re-upload was not granted");
                            None
                        }
                    }
                },
                &mut refreshed,
            )
            .await;
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

#[cfg(test)]
mod retry_tests {
    use super::*;
    use std::cell::Cell;
    use whatsapp_rust::http::HttpStatusError;

    #[tokio::test]
    async fn successful_initial_download_never_requests_reupload() {
        let requests = Cell::new(0);
        let result = download_with_reupload(
            0,
            true,
            |_| async { Ok(PathBuf::from("synthetic")) },
            || {
                requests.set(requests.get() + 1);
                async { Some("fresh".to_owned()) }
            },
            |_| Some(1),
        )
        .await;
        assert_eq!(result, Ok(PathBuf::from("synthetic")));
        assert_eq!(requests.get(), 0);
    }

    #[tokio::test]
    async fn reupload_branch_matrix_preserves_attempt_counts() {
        for (status, allowed, request_ok, refresh_ok, expected_fetches, expected_requests) in [
            (403, true, true, true, 2, 1),
            (404, true, true, true, 2, 1),
            (410, true, true, true, 2, 1),
            (404, true, false, true, 1, 1),
            (410, true, true, false, 1, 1),
            (500, true, true, true, 1, 0),
            (403, false, true, true, 1, 0),
        ] {
            let fetches = Cell::new(0);
            let requests = Cell::new(0);
            let result = download_with_reupload(
                0,
                allowed,
                |_| {
                    let n = fetches.get();
                    fetches.set(n + 1);
                    async move {
                        if n == 0 {
                            Err(StagedDownload::Fetch(anyhow::Error::new(HttpStatusError {
                                status,
                            })))
                        } else {
                            Ok(PathBuf::from("synthetic"))
                        }
                    }
                },
                || {
                    requests.set(requests.get() + 1);
                    async move { request_ok.then(|| "fresh".to_owned()) }
                },
                |_| refresh_ok.then_some(1),
            )
            .await;
            assert_eq!(fetches.get(), expected_fetches, "{status}");
            assert_eq!(requests.get(), expected_requests, "{status}");
            assert_eq!(result.is_ok(), expected_fetches == 2);
            if status == 404 && !request_ok {
                assert_eq!(result, Err(DownloadFailure::Expired));
            }
            if status == 410 && !refresh_ok {
                assert_eq!(
                    result,
                    Err(failures::download_failure(&anyhow::Error::new(
                        HttpStatusError { status }
                    )))
                );
            }
        }
    }

    #[tokio::test]
    async fn reupload_does_not_retry_local_errors_or_retry_twice() {
        for local_first in [true, false] {
            let fetches = Cell::new(0);
            let requests = Cell::new(0);
            let result = download_with_reupload(
                0,
                true,
                |_| {
                    let n = fetches.get();
                    fetches.set(n + 1);
                    async move {
                        if local_first || n > 0 {
                            Err(StagedDownload::Local("synthetic local failure".to_owned()))
                        } else {
                            Err(StagedDownload::Fetch(anyhow::Error::new(HttpStatusError {
                                status: 403,
                            })))
                        }
                    }
                },
                || {
                    requests.set(requests.get() + 1);
                    async { Some("fresh".to_owned()) }
                },
                |_| Some(1),
            )
            .await;
            assert_eq!(
                result,
                Err(DownloadFailure::Other("synthetic local failure".to_owned()))
            );
            assert_eq!(fetches.get(), if local_first { 1 } else { 2 });
            assert_eq!(requests.get(), if local_first { 0 } else { 1 });
        }
    }

    #[tokio::test]
    async fn reupload_retry_failure_is_classified_without_another_request() {
        for status in [404, 410, 500] {
            let fetches = Cell::new(0);
            let requests = Cell::new(0);
            let result = download_with_reupload(
                0,
                true,
                |_| {
                    let n = fetches.get();
                    fetches.set(n + 1);
                    async move {
                        let code = if n == 0 { 403 } else { status };
                        Err(StagedDownload::Fetch(anyhow::Error::new(HttpStatusError {
                            status: code,
                        })))
                    }
                },
                || {
                    requests.set(requests.get() + 1);
                    async { Some("fresh".to_owned()) }
                },
                |_| Some(1),
            )
            .await;
            assert_eq!(
                result,
                Err(failures::download_failure(&anyhow::Error::new(
                    HttpStatusError { status }
                )))
            );
            assert_eq!(fetches.get(), 2);
            assert_eq!(requests.get(), 1);
        }
    }
}
