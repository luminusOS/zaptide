//! Outbound attachment preparation, upload, and cache persistence.

use super::*;

pub(super) const THUMBNAIL_SIDE: u32 = 96;

pub(super) struct PastedImageRequest {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rgba: Vec<u8>,
    pub(super) caption: Option<String>,
    pub(super) quoting: Option<String>,
    pub(super) mentions: Vec<String>,
}

struct Prepared {
    message: wa::Message,
    content: Content,
    thumbnail: Option<Vec<u8>>,
    bytes: Vec<u8>,
    mime: String,
    file_name: Option<String>,
}

struct FileOutboundContext<'a> {
    client: &'a Client,
    chat: &'a str,
    me: &'a str,
    dir: &'a Path,
    session_generation: u64,
    session_generation_shared: &'a AtomicU64,
    session_cache_lock: &'a tokio::sync::Mutex<()>,
}

struct FileOutboundRequest {
    /// Id of a pending row already shown for this send, if any.
    id: Option<String>,
    prepared: Prepared,
    caption: Option<String>,
    mentions: Vec<String>,
    context: Option<wa::ContextInfo>,
    quoted: Option<Quoted>,
}

pub(super) fn encode_jpeg(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality);
    encoder
        .encode_image(&image.to_rgb8())
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

/// Builds the pre-download attachment thumbnail.
pub(super) fn thumbnail_jpeg(image: &image::DynamicImage) -> Option<Vec<u8>> {
    let small = image.thumbnail(THUMBNAIL_SIDE, THUMBNAIL_SIDE);
    encode_jpeg(&small, 60).ok()
}

/// Uploads a recording and builds a push-to-talk message with waveform.
async fn prepare_voice(
    client: &Client,
    bytes: Vec<u8>,
    seconds: u32,
    waveform: Vec<u8>,
    context: Option<Box<wa::ContextInfo>>,
) -> Result<Prepared, String> {
    let mime = "audio/ogg; codecs=opus".to_owned();
    let size = bytes.len() as u64;
    let upload = client
        .upload(bytes.clone(), MediaType::Audio, UploadOptions::default())
        .await
        .map_err(|error| error.to_string())?;
    let message = audio_message(
        upload,
        AudioOptions {
            mimetype: Some(mime.clone()),
            duration_seconds: Some(seconds),
            ptt: Some(true),
            waveform: Some(waveform.clone()),
            context_info: context,
        },
    );
    Ok(Prepared {
        message,
        content: Content::Audio {
            media: media(Some(&mime), Some(size), None, None),
            seconds: Some(seconds),
            voice_note: true,
            waveform,
        },
        thumbnail: None,
        bytes,
        mime,
        file_name: None,
    })
}

/// Uploads a file and builds its message. Images are encoded as JPEG.
async fn prepare_media(
    client: &Client,
    bytes: Vec<u8>,
    mime: &str,
    file_name: Option<&str>,
    gif: bool,
) -> Result<Prepared, String> {
    let kind = mime.split('/').next().unwrap_or_default();
    let is_picture = matches!(
        mime,
        "image/jpeg" | "image/png" | "image/webp" | "image/bmp" | "image/tiff"
    );
    if is_picture {
        let decoded = tokio::task::spawn_blocking({
            let bytes = bytes.clone();
            move || image::load_from_memory(&bytes).map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())??;
        let (width, height) = (decoded.width(), decoded.height());
        let jpeg = if mime == "image/jpeg" {
            bytes
        } else {
            encode_jpeg(&decoded, 88)?
        };
        let thumbnail = thumbnail_jpeg(&decoded);
        let upload = client
            .upload(jpeg.clone(), MediaType::Image, UploadOptions::default())
            .await
            .map_err(|error| error.to_string())?;
        let mut message = image_message(
            upload,
            ImageOptions {
                caption: None,
                mimetype: Some("image/jpeg".to_owned()),
                jpeg_thumbnail: thumbnail.clone(),
                context_info: None,
            },
        );
        if let Some(image) = message.image_message.as_option_mut() {
            image.width = Some(width);
            image.height = Some(height);
        }
        return Ok(Prepared {
            message,
            content: Content::Image {
                caption: None,
                media: media(
                    Some(&"image/jpeg".to_owned()),
                    Some(jpeg.len() as u64),
                    Some(width),
                    Some(height),
                ),
            },
            thumbnail,
            bytes: jpeg,
            mime: "image/jpeg".to_owned(),
            file_name: None,
        });
    }
    let size = bytes.len() as u64;
    let mime_owned = mime.to_owned();
    if kind == "video" {
        let upload = client
            .upload(bytes.clone(), MediaType::Video, UploadOptions::default())
            .await
            .map_err(|error| error.to_string())?;
        let message = video_message(
            upload,
            VideoOptions {
                mimetype: Some(mime_owned.clone()),
                gif_playback: Some(gif),
                ..Default::default()
            },
        );
        return Ok(Prepared {
            message,
            content: Content::Video {
                caption: None,
                media: media(Some(&mime_owned), Some(size), None, None),
                seconds: None,
                gif,
            },
            thumbnail: None,
            bytes,
            mime: mime_owned,
            file_name: file_name.map(str::to_owned),
        });
    }
    if kind == "audio" {
        let upload = client
            .upload(bytes.clone(), MediaType::Audio, UploadOptions::default())
            .await
            .map_err(|error| error.to_string())?;
        let message = audio_message(
            upload,
            AudioOptions {
                mimetype: Some(mime_owned.clone()),
                ptt: Some(false),
                ..Default::default()
            },
        );
        return Ok(Prepared {
            message,
            content: Content::Audio {
                media: media(Some(&mime_owned), Some(size), None, None),
                seconds: None,
                voice_note: false,
                waveform: Vec::new(),
            },
            thumbnail: None,
            bytes,
            mime: mime_owned,
            file_name: file_name.map(str::to_owned),
        });
    }
    prepare_document(client, bytes, mime, file_name).await
}

/// Uploads any file as a document, keeping its name and original bytes.
async fn prepare_document(
    client: &Client,
    bytes: Vec<u8>,
    mime: &str,
    file_name: Option<&str>,
) -> Result<Prepared, String> {
    let size = bytes.len() as u64;
    let mime_owned = mime.to_owned();
    let upload = client
        .upload(bytes.clone(), MediaType::Document, UploadOptions::default())
        .await
        .map_err(|error| error.to_string())?;
    let name = file_name.unwrap_or("file").to_owned();
    let message = document_message(
        upload,
        DocumentOptions {
            mimetype: Some(mime_owned.clone()),
            file_name: Some(name.clone()),
            title: Some(name.clone()),
            ..Default::default()
        },
    );
    Ok(Prepared {
        message,
        content: Content::Document {
            media: media(Some(&mime_owned), Some(size), None, None),
            file_name: name.clone(),
            caption: None,
            pages: None,
        },
        thumbnail: None,
        bytes,
        mime: mime_owned,
        file_name: Some(name),
    })
}

/// Uploads a WebP sticker and builds its message without a library builder.
async fn prepare_sticker(client: &Client, bytes: Vec<u8>) -> Result<Prepared, String> {
    let (animated, width, height) = tokio::task::spawn_blocking({
        let bytes = bytes.clone();
        move || {
            let decoder = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(&bytes))
                .map_err(|error| error.to_string())?;
            let animated = decoder.has_animation();
            let (width, height) = image::ImageDecoder::dimensions(&decoder);
            Ok::<_, String>((animated, width, height))
        }
    })
    .await
    .map_err(|error| error.to_string())??;
    let upload = client
        .upload(bytes.clone(), MediaType::Sticker, UploadOptions::default())
        .await
        .map_err(|error| error.to_string())?;
    let message = wa::Message {
        sticker_message: MessageField::some(wa::message::StickerMessage {
            url: Some(upload.url),
            direct_path: Some(upload.direct_path),
            media_key: Some(upload.media_key.to_vec()),
            file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
            file_sha256: Some(upload.file_sha256.to_vec()),
            file_length: Some(upload.file_length),
            mimetype: Some("image/webp".to_owned()),
            media_key_timestamp: Some(upload.media_key_timestamp),
            is_animated: Some(animated),
            width: Some(width),
            height: Some(height),
            ..Default::default()
        }),
        ..Default::default()
    };
    Ok(Prepared {
        message,
        content: Content::Sticker {
            media: media(
                Some(&"image/webp".to_owned()),
                Some(bytes.len() as u64),
                Some(width),
                Some(height),
            ),
            animated,
        },
        thumbnail: None,
        bytes,
        mime: "image/webp".to_owned(),
        file_name: None,
    })
}

/// Copies a sent attachment to media storage and builds its archive row.
async fn file_outbound(
    context: FileOutboundContext<'_>,
    request: FileOutboundRequest,
) -> Result<(Message, Vec<u8>), String> {
    let FileOutboundContext {
        client,
        chat,
        me,
        dir,
        session_generation,
        session_generation_shared,
        session_cache_lock,
    } = context;
    let FileOutboundRequest {
        id,
        mut prepared,
        caption,
        mentions,
        mut context,
        quoted,
    } = request;
    if let Some(caption) = caption.filter(|caption| !caption.trim().is_empty()) {
        match &mut prepared.content {
            Content::Image { caption: slot, .. }
            | Content::Video { caption: slot, .. }
            | Content::Document { caption: slot, .. } => *slot = Some(caption.clone()),
            _ => {}
        }
        if let Some(image) = prepared.message.image_message.as_option_mut() {
            image.caption = Some(caption.clone());
        }
        if let Some(video) = prepared.message.video_message.as_option_mut() {
            video.caption = Some(caption.clone());
        }
        if let Some(document) = prepared.message.document_message.as_option_mut() {
            document.caption = Some(caption);
        }
    }
    if !mentions.is_empty() {
        context.get_or_insert_default().mentioned_jid = mentions.clone();
    }
    if let Some(context) = context {
        prepared.message.set_context_info(context);
    }
    let id = id.unwrap_or_else(|| client.generate_message_id());
    let path = media_path(
        dir,
        chat,
        &id,
        &prepared.mime,
        prepared.file_name.as_deref(),
    );
    if session_generation_shared.load(Ordering::Acquire) != session_generation {
        return Err("The linked account changed while preparing the attachment".to_owned());
    }
    write_session_cache_file(
        dir,
        &path,
        &prepared.bytes,
        session_generation,
        session_generation_shared,
        None,
        session_cache_lock,
    )
    .await?;
    let mut content = prepared.content;
    if let Some(media) = content.media_mut() {
        media.path = Some(path);
    }
    let row = Message {
        id,
        chat: chat.to_owned(),
        sender: me.to_owned(),
        sender_name: None,
        from_me: true,
        timestamp: crate::util::now(),
        content,
        status: Delivery::Pending,
        delivered_at: None,
        read_at: None,
        quoted,
        reactions: Vec::new(),
        edited: false,
        mentions: mentions
            .into_iter()
            .filter_map(|id| {
                let user = id.split('@').next()?.to_owned();
                (!user.is_empty()).then_some(MentionRef {
                    user,
                    id,
                    name: None,
                })
            })
            .collect(),
        forwarded: false,
        thumbnail: prepared.thumbnail,
    };
    Ok((row, prepared.message.encode_to_vec()))
}

pub(super) fn consume_attachment_reply(
    success: bool,
    caption: &mut Option<String>,
    context: &mut Option<wa::ContextInfo>,
    quoted: &mut Option<Quoted>,
    mentions: &mut Vec<String>,
) {
    if success {
        *caption = None;
        *context = None;
        *quoted = None;
        mentions.clear();
    }
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

pub(super) fn media_path(
    dir: &Path,
    chat: &str,
    id: &str,
    mime: &str,
    file_name: Option<&str>,
) -> PathBuf {
    let extension = extension_for(mime, file_name);
    let stem = match file_name.and_then(|name| Path::new(name).file_stem()?.to_str()) {
        Some(name) => format!("{}-{}", sanitize(id), sanitize(name)),
        None => format!("{}-{}", sanitize(chat), sanitize(id)),
    };
    dir.join(format!("{stem}.{extension}"))
}

/// Keep staging names short and independent for overlapping downloads, even
/// when both attempts fetch the same message payload. Stay in the destination
/// directory so promotion can use an atomic rename.
pub(super) fn download_staging_path(dir: &Path) -> PathBuf {
    let token = rand::random::<u128>();
    dir.join(format!(".download-{token:032x}.tmp"))
}

impl Worker {
    pub(super) fn send_files(
        &mut self,
        chat: ChatId,
        paths: Vec<PathBuf>,
        documents: HashSet<PathBuf>,
        caption: Option<String>,
        quoting: Option<String>,
        mentions: Vec<String>,
    ) {
        if paths.is_empty() {
            return;
        }
        let batch = self.next_attachment_batch;
        self.next_attachment_batch = self.next_attachment_batch.wrapping_add(1);
        let total = paths.len();
        let Some(client) = self.client.clone() else {
            for (index, path) in paths.into_iter().enumerate() {
                self.emit(Event::AttachmentCompleted {
                    chat: chat.clone(),
                    batch,
                    index,
                    total,
                    path,
                    success: false,
                });
            }
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let session_cache_lock = self.session_cache_lock.clone();
        let dir = self.dirs.media_cache_dir();
        let me = self.me();
        let (context, quoted) = self.quote_context(&chat, quoting.as_deref());
        tokio::spawn(async move {
            let mut caption = caption;
            let mut context = context;
            let mut quoted = quoted;
            let mut mentions = mentions;
            for (index, path) in paths.into_iter().enumerate() {
                let outcome = async {
                    let bytes = tokio::fs::read(&path)
                        .await
                        .map_err(|error| format!("{}: {error}", path.display()))?;
                    let mime = mime_guess2::from_path(&path)
                        .first_or_octet_stream()
                        .to_string();
                    let file_name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned());
                    let prepared = if documents.contains(&path) {
                        prepare_document(&client, bytes, &mime, file_name.as_deref()).await?
                    } else {
                        prepare_media(&client, bytes, &mime, file_name.as_deref(), false).await?
                    };
                    file_outbound(
                        FileOutboundContext {
                            client: &client,
                            chat: &chat,
                            me: &me,
                            dir: &dir,
                            session_generation,
                            session_generation_shared: &session_generation_shared,
                            session_cache_lock: &session_cache_lock,
                        },
                        FileOutboundRequest {
                            id: None,
                            prepared,
                            caption: caption.clone(),
                            mentions: mentions.clone(),
                            context: context.clone(),
                            quoted: quoted.clone(),
                        },
                    )
                    .await
                }
                .await;
                match outcome {
                    Ok((row, raw)) => {
                        let (sent_tx, mut sent_rx) = tokio::sync::mpsc::unbounded_channel();
                        let queued = commands
                            .send(Command::OutboundBatch {
                                chat: chat.clone(),
                                session_generation,
                                row: Box::new(row),
                                raw,
                                sent: sent_tx,
                            })
                            .is_ok();
                        let success = queued && sent_rx.recv().await.unwrap_or(false);
                        consume_attachment_reply(
                            success,
                            &mut caption,
                            &mut context,
                            &mut quoted,
                            &mut mentions,
                        );
                        let _ = commands.send(Command::AttachmentCompleted {
                            chat: chat.clone(),
                            session_generation,
                            batch,
                            index,
                            total,
                            path,
                            success,
                        });
                    }
                    Err(_) => {
                        let _ = commands.send(Command::AttachmentCompleted {
                            chat: chat.clone(),
                            session_generation,
                            batch,
                            index,
                            total,
                            path,
                            success: false,
                        });
                    }
                }
            }
        });
    }

    pub(super) fn send_pasted_image(&mut self, chat: ChatId, request: PastedImageRequest) {
        let Some(client) = self.client.clone() else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let session_cache_lock = self.session_cache_lock.clone();
        let dir = self.dirs.media_cache_dir();
        let me = self.me();
        let (context, quoted) = self.quote_context(&chat, request.quoting.as_deref());
        tokio::spawn(async move {
            let outcome = async {
                let encoded = tokio::task::spawn_blocking(move || {
                    let image =
                        image::RgbaImage::from_raw(request.width, request.height, request.rgba)
                            .ok_or_else(|| "Clipboard image data is invalid".to_owned())?;
                    encode_jpeg(&image::DynamicImage::ImageRgba8(image), 88)
                })
                .await
                .map_err(|error| error.to_string())??;
                let prepared = prepare_media(&client, encoded, "image/jpeg", None, false).await?;
                file_outbound(
                    FileOutboundContext {
                        client: &client,
                        chat: &chat,
                        me: &me,
                        dir: &dir,
                        session_generation,
                        session_generation_shared: &session_generation_shared,
                        session_cache_lock: &session_cache_lock,
                    },
                    FileOutboundRequest {
                        id: None,
                        prepared,
                        caption: request.caption,
                        mentions: request.mentions,
                        context,
                        quoted,
                    },
                )
                .await
            }
            .await;
            match outcome {
                Ok((row, raw)) => {
                    let _ = commands.send(Command::Outbound {
                        chat,
                        session_generation,
                        row: Box::new(row),
                        raw,
                    });
                }
                Err(error) => {
                    let _ = commands.send(Command::Sent {
                        chat,
                        id: String::new(),
                        session_generation,
                        error: Some(format!("Could not send the picture: {error}")),
                    });
                }
            }
        });
    }

    /// Encodes and sends an OGG/Opus voice message with optional quote.
    pub(super) fn send_voice(&mut self, chat: ChatId, samples: Vec<f32>, quoting: Option<String>) {
        let Some(client) = self.client.clone() else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        let (context, shown) = self.quote_context(&chat, quoting.as_deref());
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let session_cache_lock = self.session_cache_lock.clone();
        let dir = self.dirs.media_cache_dir();
        let me = self.me();
        tokio::spawn(async move {
            let outcome = async {
                let (bytes, seconds, waveform) = tokio::task::spawn_blocking(move || {
                    let mut samples = samples;
                    crate::voice::normalize(&mut samples);
                    let seconds = (samples.len() as f64 / f64::from(crate::voice::RATE))
                        .round()
                        .max(1.0) as u32;
                    let waveform = crate::voice::waveform(&samples);
                    crate::voice::encode(&samples).map(|bytes| (bytes, seconds, waveform))
                })
                .await
                .map_err(|error| error.to_string())??;
                let prepared =
                    prepare_voice(&client, bytes, seconds, waveform, context.map(Box::new)).await?;
                file_outbound(
                    FileOutboundContext {
                        client: &client,
                        chat: &chat,
                        me: &me,
                        dir: &dir,
                        session_generation,
                        session_generation_shared: &session_generation_shared,
                        session_cache_lock: &session_cache_lock,
                    },
                    FileOutboundRequest {
                        id: None,
                        prepared,
                        caption: None,
                        mentions: Vec::new(),
                        context: None,
                        quoted: shown,
                    },
                )
                .await
            }
            .await;
            match outcome {
                Ok((row, raw)) => {
                    let _ = commands.send(Command::Outbound {
                        chat,
                        session_generation,
                        row: Box::new(row),
                        raw,
                    });
                }
                Err(error) => {
                    let _ = commands.send(Command::Sent {
                        chat,
                        id: String::new(),
                        session_generation,
                        error: Some(format!("Could not send the voice message: {error}")),
                    });
                }
            }
        });
    }

    pub(super) fn send_sticker(&mut self, chat: ChatId, path: PathBuf) {
        let (Some(client), Some(_)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let session_cache_lock = self.session_cache_lock.clone();
        let dir = self.dirs.media_cache_dir();
        let me = self.me();
        // Show the sticker at once, as text sends do; the upload fills in
        // the same row, or marks it failed.
        let id = client.generate_message_id();
        let mut preview = media(Some(&"image/webp".to_owned()), None, None, None);
        preview.path = Some(path.clone());
        self.store_message(
            Message {
                id: id.clone(),
                chat: chat.clone(),
                sender: me.clone(),
                sender_name: None,
                from_me: true,
                timestamp: crate::util::now(),
                content: Content::Sticker {
                    media: preview,
                    animated: false,
                },
                status: Delivery::Pending,
                delivered_at: None,
                read_at: None,
                quoted: None,
                reactions: Vec::new(),
                edited: false,
                mentions: Vec::new(),
                forwarded: false,
                thumbnail: None,
            },
            None,
            None,
        );
        tokio::spawn(async move {
            let outcome = async {
                let bytes = tokio::fs::read(&path)
                    .await
                    .map_err(|error| error.to_string())?;
                let prepared = prepare_sticker(&client, bytes).await?;
                file_outbound(
                    FileOutboundContext {
                        client: &client,
                        chat: &chat,
                        me: &me,
                        dir: &dir,
                        session_generation,
                        session_generation_shared: &session_generation_shared,
                        session_cache_lock: &session_cache_lock,
                    },
                    FileOutboundRequest {
                        id: Some(id.clone()),
                        prepared,
                        caption: None,
                        mentions: Vec::new(),
                        context: None,
                        quoted: None,
                    },
                )
                .await
            }
            .await;
            match outcome {
                Ok((row, raw)) => {
                    let _ = commands.send(Command::Outbound {
                        chat,
                        session_generation,
                        row: Box::new(row),
                        raw,
                    });
                }
                Err(error) => {
                    log::warn!("could not send the sticker: {error}");
                    let _ = commands.send(Command::Sent {
                        chat,
                        id,
                        session_generation,
                        error: Some(sanitized_send_error().to_owned()),
                    });
                }
            }
        });
    }
}
