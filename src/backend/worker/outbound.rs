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
    data: PreparedData,
    mime: String,
    file_name: Option<String>,
}

enum PreparedData {
    Bytes(Vec<u8>),
    Snapshot(tempfile::NamedTempFile),
}

struct FileUploadSource {
    file: tempfile::NamedTempFile,
    len: u64,
}

fn upload_staging_file(directory: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(".upload-")
        .suffix(".tmp")
        .rand_bytes(32)
        .tempfile_in(directory)
}

impl whatsapp_rust::wacore::upload::UploadSource for FileUploadSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn reader_from(&self, offset: u64) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        use std::io::Seek;
        let mut file = std::fs::File::open(self.file.path())?;
        file.seek(std::io::SeekFrom::Start(offset))?;
        Ok(Box::new(file))
    }
}

async fn encrypt_snapshot(
    snapshot: &tempfile::NamedTempFile,
    media_type: MediaType,
) -> Result<
    (
        FileUploadSource,
        whatsapp_rust::wacore::upload::EncryptedMediaInfo,
    ),
    String,
> {
    let input = std::fs::File::open(snapshot.path()).map_err(|error| error.to_string())?;
    let directory = snapshot
        .path()
        .parent()
        .ok_or("Missing staging directory")?;
    let output = upload_staging_file(directory).map_err(|error| error.to_string())?;
    tokio::task::spawn_blocking(move || {
        let mut output = output;
        let info = whatsapp_rust::wacore::upload::encrypt_media_streaming(
            input,
            output.as_file_mut(),
            media_type,
        )
        .map_err(|error| error.to_string())?;
        let len = output
            .as_file()
            .metadata()
            .map_err(|error| error.to_string())?
            .len();
        Ok((FileUploadSource { file: output, len }, info))
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn snapshot_session_file(
    source: &Path,
    directory: &Path,
    generation: u64,
    current_generation: &AtomicU64,
    cache_lock: &tokio::sync::Mutex<()>,
) -> Result<tempfile::NamedTempFile, String> {
    let staging = {
        let _guard = cache_lock.lock().await;
        if current_generation.load(Ordering::Acquire) != generation {
            return Err(ACCOUNT_CHANGED.to_owned());
        }
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(|error| error.to_string())?;
        if current_generation.load(Ordering::Acquire) != generation {
            return Err(ACCOUNT_CHANGED.to_owned());
        }
        upload_staging_file(directory).map_err(|error| error.to_string())?
    };
    let source = source.to_owned();
    let staging = tokio::task::spawn_blocking(move || {
        let mut input = std::fs::File::open(source)?;
        let mut staging = staging;
        std::io::copy(&mut input, staging.as_file_mut())?;
        Ok::<_, std::io::Error>(staging)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;
    let _guard = cache_lock.lock().await;
    if current_generation.load(Ordering::Acquire) != generation {
        return Err(ACCOUNT_CHANGED.to_owned());
    }
    Ok(staging)
}

async fn promote_session_snapshot(
    staging: tempfile::NamedTempFile,
    destination: &Path,
    generation: u64,
    current_generation: &AtomicU64,
    cache_lock: &tokio::sync::Mutex<()>,
) -> Result<PathBuf, String> {
    let _guard = cache_lock.lock().await;
    if current_generation.load(Ordering::Acquire) != generation {
        return Err(ACCOUNT_CHANGED.to_owned());
    }
    promote_download(staging.path(), destination, None).map_err(|error| error.to_string())
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
        data: PreparedData::Bytes(bytes),
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
            data: PreparedData::Bytes(jpeg),
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
            data: PreparedData::Bytes(bytes),
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
            data: PreparedData::Bytes(bytes),
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
        data: PreparedData::Bytes(bytes),
        mime: mime_owned,
        file_name: Some(name),
    })
}

async fn prepare_streamed_file(
    client: &Client,
    snapshot: tempfile::NamedTempFile,
    mime: &str,
    file_name: Option<&str>,
    document: bool,
) -> Result<Prepared, String> {
    let kind = mime.split('/').next().unwrap_or_default();
    let media_type = if document {
        MediaType::Document
    } else {
        match kind {
            "video" => MediaType::Video,
            "audio" => MediaType::Audio,
            _ => MediaType::Document,
        }
    };
    let (source, info) = encrypt_snapshot(&snapshot, media_type).await?;
    let size = info.file_length;
    let upload = client
        .upload_stream(source, info, media_type)
        .await
        .map_err(|error| error.to_string())?;
    let mime = mime.to_owned();
    let (message, content, file_name) = match media_type {
        MediaType::Video => (
            video_message(
                upload,
                VideoOptions {
                    mimetype: Some(mime.clone()),
                    gif_playback: Some(false),
                    ..Default::default()
                },
            ),
            Content::Video {
                caption: None,
                media: media(Some(&mime), Some(size), None, None),
                seconds: None,
                gif: false,
            },
            file_name.map(str::to_owned),
        ),
        MediaType::Audio => (
            audio_message(
                upload,
                AudioOptions {
                    mimetype: Some(mime.clone()),
                    ptt: Some(false),
                    ..Default::default()
                },
            ),
            Content::Audio {
                media: media(Some(&mime), Some(size), None, None),
                seconds: None,
                voice_note: false,
                waveform: Vec::new(),
            },
            file_name.map(str::to_owned),
        ),
        _ => {
            let name = file_name.unwrap_or("file").to_owned();
            (
                document_message(
                    upload,
                    DocumentOptions {
                        mimetype: Some(mime.clone()),
                        file_name: Some(name.clone()),
                        title: Some(name.clone()),
                        ..Default::default()
                    },
                ),
                Content::Document {
                    media: media(Some(&mime), Some(size), None, None),
                    file_name: name.clone(),
                    caption: None,
                    pages: None,
                },
                Some(name),
            )
        }
    };
    Ok(Prepared {
        message,
        content,
        thumbnail: None,
        data: PreparedData::Snapshot(snapshot),
        mime,
        file_name,
    })
}

struct PreparedPhoto {
    snapshot: tempfile::NamedTempFile,
    width: u32,
    height: u32,
    thumbnail: Option<Vec<u8>>,
}

async fn transform_photo_snapshot(
    snapshot: tempfile::NamedTempFile,
    jpeg: bool,
) -> Result<PreparedPhoto, String> {
    tokio::task::spawn_blocking(move || {
        let input = std::fs::File::open(snapshot.path()).map_err(|error| error.to_string())?;
        let decoded = image::ImageReader::new(std::io::BufReader::new(input))
            .with_guessed_format()
            .map_err(|error| error.to_string())?
            .decode()
            .map_err(|error| error.to_string())?;
        let (width, height) = (decoded.width(), decoded.height());
        let thumbnail = thumbnail_jpeg(&decoded);
        let snapshot = if jpeg {
            snapshot
        } else {
            let directory = snapshot
                .path()
                .parent()
                .ok_or("Missing staging directory")?;
            let mut output = upload_staging_file(directory).map_err(|error| error.to_string())?;
            image::codecs::jpeg::JpegEncoder::new_with_quality(output.as_file_mut(), 88)
                .encode_image(&decoded.to_rgb8())
                .map_err(|error| error.to_string())?;
            output
        };
        Ok(PreparedPhoto {
            snapshot,
            width,
            height,
            thumbnail,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn prepare_streamed_photo(
    client: &Client,
    snapshot: tempfile::NamedTempFile,
    jpeg: bool,
) -> Result<Prepared, String> {
    let photo = transform_photo_snapshot(snapshot, jpeg).await?;
    let (source, info) = encrypt_snapshot(&photo.snapshot, MediaType::Image).await?;
    let size = info.file_length;
    let upload = client
        .upload_stream(source, info, MediaType::Image)
        .await
        .map_err(|error| error.to_string())?;
    let mut message = image_message(
        upload,
        ImageOptions {
            caption: None,
            mimetype: Some("image/jpeg".to_owned()),
            jpeg_thumbnail: photo.thumbnail.clone(),
            context_info: None,
        },
    );
    if let Some(image) = message.image_message.as_option_mut() {
        image.width = Some(photo.width);
        image.height = Some(photo.height);
    }
    Ok(Prepared {
        message,
        content: Content::Image {
            caption: None,
            media: media(
                Some(&"image/jpeg".to_owned()),
                Some(size),
                Some(photo.width),
                Some(photo.height),
            ),
        },
        thumbnail: photo.thumbnail,
        data: PreparedData::Snapshot(photo.snapshot),
        mime: "image/jpeg".to_owned(),
        file_name: None,
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
        data: PreparedData::Bytes(bytes),
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
    let path = match prepared.data {
        PreparedData::Bytes(bytes) => {
            write_session_cache_file(
                dir,
                &path,
                &bytes,
                session_generation,
                session_generation_shared,
                None,
                session_cache_lock,
            )
            .await?
        }
        PreparedData::Snapshot(staging) => {
            promote_session_snapshot(
                staging,
                &path,
                session_generation,
                session_generation_shared,
                session_cache_lock,
            )
            .await?
        }
    };
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

/// Keep the cache name well under the 255-byte file name limit once the
/// parts, the extension and a collision number are joined.
const MAX_NAME_CHARS: usize = 120;
const MAX_ID_CHARS: usize = 64;

pub(super) fn media_path(
    dir: &Path,
    chat: &str,
    id: &str,
    mime: &str,
    file_name: Option<&str>,
) -> PathBuf {
    let extension = extension_for(mime, file_name);
    let capped = |text: &str, limit: usize| {
        let mut text = sanitize(text);
        text.truncate(limit);
        text
    };
    let id = capped(id, MAX_ID_CHARS);
    let stem = match file_name.and_then(|name| Path::new(name).file_stem()?.to_str()) {
        Some(name) => format!("{id}-{}-{}", chat_tag(chat), capped(name, MAX_NAME_CHARS)),
        None => format!("{}-{id}", capped(chat, MAX_ID_CHARS)),
    };
    dir.join(format!("{stem}.{extension}"))
}

/// Message ids are only unique per sender, so named documents carry a short
/// digest of their chat to keep two chats from sharing a cache file.
fn chat_tag(chat: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(chat.as_bytes())[..4])
}

const PROMOTION_ATTEMPTS: usize = 100;

/// Moves a finished download to `destination`. `recorded` is the path the
/// archive already holds for this message: when it is `destination` or one
/// of its numbered siblings, the file is this message's own and is replaced
/// (as when two downloads of it overlap). Otherwise no existing file is
/// replaced and a numbered name next to `destination` is used. Returns the
/// path the download now lives at.
pub(super) fn promote_download(
    staged: &Path,
    destination: &Path,
    recorded: Option<&Path>,
) -> std::io::Result<PathBuf> {
    if let Some(own) = recorded.filter(|path| is_destination_or_sibling(path, destination)) {
        std::fs::rename(staged, own)?;
        return Ok(own.to_owned());
    }
    for attempt in 0..PROMOTION_ATTEMPTS {
        let candidate = match attempt {
            0 => destination.to_owned(),
            n => numbered_path(destination, n),
        };
        match rename_no_replace(staged, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

fn numbered_path(path: &Path, n: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(extension) => format!("{stem}-{n}.{}", extension.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    path.with_file_name(name)
}

fn is_destination_or_sibling(path: &Path, destination: &Path) -> bool {
    if path == destination {
        return true;
    }
    if path.parent() != destination.parent() || path.extension() != destination.extension() {
        return false;
    }
    let (Some(stem), Some(own)) = (
        destination.file_stem().and_then(|stem| stem.to_str()),
        path.file_stem().and_then(|stem| stem.to_str()),
    ) else {
        return false;
    };
    own.strip_prefix(stem)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|byte| byte.is_ascii_digit()))
}

fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;
    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        // Some filesystems (NFS, older FUSE) reject the flag; a hard link
        // gives the same no-replace guarantee there.
        Err(Errno::INVAL | Errno::NOSYS) => match std::fs::hard_link(from, to) {
            Ok(()) => {
                if std::fs::remove_file(from).is_err() {
                    log::debug!("could not remove a staged download after linking it");
                }
                Ok(())
            }
            Err(error)
                if matches!(
                    Errno::from_io_error(&error),
                    Some(Errno::PERM | Errno::NOTSUP | Errno::NOSYS)
                ) =>
            {
                rename_if_absent(from, to)
            }
            Err(error) => Err(error),
        },
        Err(errno) => Err(errno.into()),
    }
}

/// Last resort where the filesystem has neither the rename flag nor hard
/// links (FAT, some FUSE mounts): not atomic, but it still never replaces
/// a file that is already there when the check runs.
fn rename_if_absent(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(to) {
        Ok(_) => Err(std::io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::rename(from, to),
        Err(error) => Err(error),
    }
}

pub(super) fn attachment_mime(path: &Path) -> String {
    let is_rpm = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rpm"));
    if is_rpm {
        // mime_guess lists RealAudio's plugin type first for .rpm, which would
        // route a package through the audio message path.
        return "application/x-rpm".to_owned();
    }
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

/// Keep staging names short and independent for overlapping downloads, even
/// when both attempts fetch the same message payload. Stay in the destination
/// directory so promotion can use an atomic rename.
pub(super) fn download_staging_path(dir: &Path) -> Result<PathBuf, getrandom::Error> {
    download_staging_path_with(dir, |bytes| getrandom::fill(bytes))
}

fn download_staging_path_with<E>(
    dir: &Path,
    fill: impl FnOnce(&mut [u8; 16]) -> Result<(), E>,
) -> Result<PathBuf, E> {
    let mut bytes = [0; 16];
    fill(&mut bytes)?;
    Ok(dir.join(format!(".download-{}.tmp", hex::encode(bytes))))
}

/// Staging files untouched for this long belong to a download that was
/// interrupted, never to one still streaming in another instance.
const STALE_STAGING_AGE: Duration = Duration::from_secs(60 * 60);

/// Whether `name` is exactly what [`download_staging_path`] produces.
fn is_staging_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix(".download-"))
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(|token| {
            token.len() == 32
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn is_upload_staging_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix(".upload-"))
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(|token| token.len() == 32 && token.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Removes staging files that interrupted transfers left directly in `dir`:
/// regular files only (a symlink is never followed or removed), named
/// exactly like a staging file, last modified at least `max_age` before
/// `now`. Returns how many were removed.
pub(super) fn remove_stale_staging_files(
    dir: &Path,
    max_age: Duration,
    now: std::time::SystemTime,
) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            is_staging_name(&entry.file_name()) || is_upload_staging_name(&entry.file_name())
        })
        // DirEntry::file_type and DirEntry::metadata describe the entry
        // itself and do not traverse symlinks.
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| {
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age >= max_age))
        })
        .filter(|entry| std::fs::remove_file(entry.path()).is_ok())
        .count()
}

/// Why a streamed download produced no staging file.
#[derive(Debug)]
pub(super) enum StagedDownload<E> {
    /// The download itself failed, verification included.
    Fetch(E),
    /// The cache could not take the file, or the account changed.
    Local(String),
}

const ACCOUNT_CHANGED: &str = "The linked account changed during the download";

/// Streams a download into a new staging file at `path`. `fetch` receives
/// the open file and returns it once the media is complete and verified; on
/// any failure the staging file is removed. The cache lock is held only
/// while the file is created and while the result is checked, not while the
/// body streams, so a logout does not wait for a large download; a cleanup
/// that runs meanwhile bumps the generation and the file is discarded.
pub(super) async fn stream_session_cache_file<E, F, Fut>(
    directory: &Path,
    path: &Path,
    generation: u64,
    current_generation: &AtomicU64,
    cache_lock: &tokio::sync::Mutex<()>,
    fetch: F,
) -> Result<PathBuf, StagedDownload<E>>
where
    F: FnOnce(std::fs::File) -> Fut,
    Fut: std::future::Future<Output = Result<std::fs::File, E>>,
{
    let active = || current_generation.load(Ordering::Acquire) == generation;
    let local = |error: std::io::Error| StagedDownload::Local(error.to_string());
    let file = {
        let _guard = cache_lock.lock().await;
        if !active() {
            return Err(StagedDownload::Local(ACCOUNT_CHANGED.to_owned()));
        }
        tokio::fs::create_dir_all(directory).await.map_err(local)?;
        if !active() {
            return Err(StagedDownload::Local(ACCOUNT_CHANGED.to_owned()));
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(local)?;
        (RemoveOnDrop(Some(path)), file)
    };
    let (staging, file) = file;
    let fetched = fetch(file).await;
    let _guard = cache_lock.lock().await;
    match fetched {
        Ok(file) => {
            drop(file);
            if active() {
                staging.disarm();
                Ok(path.to_owned())
            } else {
                drop(staging);
                Err(StagedDownload::Local(ACCOUNT_CHANGED.to_owned()))
            }
        }
        Err(error) => {
            drop(staging);
            Err(StagedDownload::Fetch(error))
        }
    }
}

/// Removes a staging file unless the download it holds was kept, so a task
/// dropped mid-download (a runtime shutting down) leaves no partial file.
struct RemoveOnDrop<'a>(Option<&'a Path>);

impl RemoveOnDrop<'_> {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Worker {
    /// Clears staging files left by transfers that a crash or a quit cut
    /// short, off the async threads.
    pub(super) fn remove_stale_staging(&self) {
        let dir = self.dirs.media_cache_dir();
        tokio::task::spawn_blocking(move || {
            let removed =
                remove_stale_staging_files(&dir, STALE_STAGING_AGE, std::time::SystemTime::now());
            if removed > 0 {
                log::info!("removed {removed} interrupted transfer staging files");
            }
        });
    }

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
                    let mime = attachment_mime(&path);
                    let file_name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned());
                    let document = documents.contains(&path);
                    let picture = matches!(
                        mime.as_str(),
                        "image/jpeg" | "image/png" | "image/webp" | "image/bmp" | "image/tiff"
                    );
                    let snapshot = snapshot_session_file(
                        &path,
                        &dir,
                        session_generation,
                        &session_generation_shared,
                        &session_cache_lock,
                    )
                    .await?;
                    let prepared = if picture && !document {
                        prepare_streamed_photo(&client, snapshot, mime == "image/jpeg").await?
                    } else {
                        prepare_streamed_file(
                            &client,
                            snapshot,
                            &mime,
                            file_name.as_deref(),
                            document,
                        )
                        .await?
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
                        let queued = Completion {
                            generation: session_generation,
                            done: Done::OutboundBatch {
                                chat: chat.clone(),

                                row: Box::new(row),
                                raw,
                                sent: sent_tx,
                            },
                        }
                        .send(&commands)
                        .is_ok();
                        let success = queued && sent_rx.recv().await.unwrap_or(false);
                        consume_attachment_reply(
                            success,
                            &mut caption,
                            &mut context,
                            &mut quoted,
                            &mut mentions,
                        );
                        let _ = Completion {
                            generation: session_generation,
                            done: Done::AttachmentCompleted {
                                chat: chat.clone(),

                                batch,
                                index,
                                total,
                                path,
                                success,
                            },
                        }
                        .send(&commands);
                    }
                    Err(_) => {
                        let _ = Completion {
                            generation: session_generation,
                            done: Done::AttachmentCompleted {
                                chat: chat.clone(),

                                batch,
                                index,
                                total,
                                path,
                                success: false,
                            },
                        }
                        .send(&commands);
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
                    let _ = Completion {
                        generation: session_generation,
                        done: Done::Outbound {
                            chat,

                            row: Box::new(row),
                            raw,
                        },
                    }
                    .send(&commands);
                }
                Err(error) => {
                    let _ = Completion {
                        generation: session_generation,
                        done: Done::Sent {
                            chat,
                            id: String::new(),

                            error: Some(format!("Could not send the picture: {error}")),
                        },
                    }
                    .send(&commands);
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
                    let _ = Completion {
                        generation: session_generation,
                        done: Done::Outbound {
                            chat,

                            row: Box::new(row),
                            raw,
                        },
                    }
                    .send(&commands);
                }
                Err(error) => {
                    let _ = Completion {
                        generation: session_generation,
                        done: Done::Sent {
                            chat,
                            id: String::new(),

                            error: Some(format!("Could not send the voice message: {error}")),
                        },
                    }
                    .send(&commands);
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
                    let _ = Completion {
                        generation: session_generation,
                        done: Done::Outbound {
                            chat,

                            row: Box::new(row),
                            raw,
                        },
                    }
                    .send(&commands);
                }
                Err(error) => {
                    log::warn!("could not send the sticker: {error}");
                    let _ = Completion {
                        generation: session_generation,
                        done: Done::Sent {
                            chat,
                            id,

                            error: Some(sanitized_send_error().to_owned()),
                        },
                    }
                    .send(&commands);
                }
            }
        });
    }
}

impl Worker {
    /// Save the same audience the protocol library uses to encrypt the send.
    pub(super) fn save_group_recipients(
        &self,
        chat: &str,
        id: &str,
        recipients: &[String],
    ) -> bool {
        let recipients: Vec<_> = recipients
            .iter()
            .map(|id| self.canonical_str(id))
            .filter(|id| !self.is_me(id))
            .collect();
        match self
            .archive
            .snapshot_group_recipients(chat, id, &recipients)
        {
            Ok(()) => true,
            Err(error) => {
                log::warn!("could not save the group message audience: {error}");
                false
            }
        }
    }

    pub(super) fn send_text(
        &mut self,
        chat: ChatId,
        text: String,
        quoting: Option<String>,
        mentions: Vec<String>,
        link_preview: Option<crate::model::LinkPreview>,
    ) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            self.emit(Event::Sent {
                chat,
                success: false,
            });
            return;
        };
        let (context, quoted_row) = self.quote_context(&chat, quoting.as_deref());
        let mut message = outgoing_text(text.clone(), context, &mentions);
        let expiration = self.apply_ephemeral(&chat, &mut message);
        let mut link_preview =
            link_preview.filter(|preview| link_preview::matches_text(&text, preview));
        let thumbnail = link_preview
            .as_mut()
            .and_then(|preview| preview.thumbnail.take());
        if let Some(preview) = &link_preview {
            message = with_link_preview(
                message,
                &text,
                preview.clone(),
                thumbnail.clone(),
                expiration,
            );
        }
        let mentions = self.mentions_of(&mentions);
        let id = client.generate_message_id();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let row = Message {
            id: id.clone(),
            chat: chat.clone(),
            sender: self.me(),
            sender_name: None,
            from_me: true,
            timestamp: crate::util::now(),
            content: link_preview.as_ref().map_or_else(
                || Content::text(text.clone()),
                |preview| Content::Text {
                    text: text.clone(),
                    preview: Some(preview.clone()),
                },
            ),
            status: Delivery::Pending,
            delivered_at: None,
            read_at: None,
            quoted: quoted_row,
            reactions: Vec::new(),
            edited: false,
            mentions,
            forwarded: false,
            thumbnail,
        };
        self.store_message(row, Some(message.encode_to_vec()), None);
        let session = OutgoingSession {
            client,
            commands: self.commands.clone(),
            generation: session_generation,
            generation_shared: session_generation_shared,
        };
        tokio::spawn(
            async move { send_outgoing(session, chat, jid, id, message, expiration).await },
        );
    }

    pub(super) fn set_link_previews(&self, enabled: bool) {
        let Some(client) = self.client.clone() else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        tokio::spawn(async move {
            if let Err(error) = client
                .app_state_settings()
                .set_link_previews_disabled(!enabled)
                .await
            {
                log::warn!("link-preview setting was not synced: {error}");
            }
        });
    }

    /// Answers a buttons or list message with one of its choices. The wire
    /// message is a `ButtonsResponseMessage` or `ListResponseMessage`; the
    /// local row and its archived body are a plain reply quoting the
    /// original, which is what the user sees.
    /// Builds protocol context and archive metadata for an available quoted message.
    pub(super) fn quote_context(
        &self,
        chat: &ChatId,
        quoting: Option<&str>,
    ) -> (Option<wa::ContextInfo>, Option<Quoted>) {
        let Some((context, row)) = quoting.and_then(|id| {
            let raw = self.archive.raw(chat, id).ok().flatten()?;
            let quoted = wa::Message::decode_from_slice(&raw).ok()?;
            let row = self.archive.message(chat, id).ok().flatten()?;
            let jid = Self::jid_of(chat)?;
            let sender = Self::jid_of(&row.sender).unwrap_or_else(|| jid.clone());
            let context = whatsapp_rust::wacore::proto_helpers::build_quote_context_with_info(
                row.id.clone(),
                &sender,
                &jid,
                &jid,
                &quoted,
            );
            Some((context, row))
        }) else {
            return (None, None);
        };
        let quoted = Quoted {
            mentions: row.mentions.clone(),
            id: row.id,
            sender_name: if row.from_me {
                Some("You".to_owned())
            } else {
                row.sender_name
                    .clone()
                    .or_else(|| self.name_for(&row.sender))
            },
            sender: row.sender,
            summary: row.content.summary(),
        };
        (Some(context), Some(quoted))
    }

    pub(super) fn forward_message(
        &mut self,
        from_chat: ChatId,
        message_id: String,
        to_chat: ChatId,
    ) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&to_chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            return;
        };
        let Ok(Some(source)) = self.archive.message(&from_chat, &message_id) else {
            self.emit(Event::Error(
                "This message is not stored on this computer".to_owned(),
            ));
            return;
        };
        if matches!(
            source.content,
            Content::Revoked
                | Content::Unsupported { .. }
                | Content::Call { .. }
                | Content::Poll { .. }
                | Content::Buttons { .. }
                | Content::List { .. }
                | Content::Template { .. }
        ) {
            self.emit(Event::Error("This message cannot be forwarded".to_owned()));
            return;
        }
        let Ok(Some(raw)) = self.archive.raw(&from_chat, &message_id) else {
            self.emit(Event::Error(
                "The original message data is not available to forward".to_owned(),
            ));
            return;
        };
        let Ok(original) = wa::Message::decode_from_slice(&raw) else {
            self.emit(Event::Error(
                "The original message data could not be read".to_owned(),
            ));
            return;
        };
        // whatsapp-rust owns the forwarding rules: unwrap transient wrappers,
        // strip quote chains and secrets, and retain reusable media metadata.
        let (message, expiration) =
            outgoing_forward(&original, self.ephemeral_expiration(&to_chat));
        let id = client.generate_message_id();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let mentions = self.mentions_of(&mentioned_of(&message));
        let thumbnail = thumbnail_of(&message).or_else(|| {
            source
                .thumbnail
                .clone()
                .filter(|bytes| crate::native_media::is_inline_thumbnail(bytes))
        });
        let row = forwarded_row(
            source,
            to_chat.clone(),
            self.me(),
            id.clone(),
            crate::util::now(),
            mentions,
            thumbnail,
        );
        self.store_message(row, Some(message.encode_to_vec()), None);
        self.forward_tails.retain(|_, send| !send.is_finished());
        let previous = self.forward_tails.remove(&to_chat);
        let destination = to_chat.clone();
        let send = send_outgoing(
            OutgoingSession {
                client,
                commands: self.commands.clone(),
                generation: session_generation,
                generation_shared: session_generation_shared,
            },
            to_chat,
            jid,
            id,
            message,
            expiration,
        );
        let tail = tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            send.await
        });
        self.forward_tails.insert(destination, tail);
    }
}

impl Worker {
    pub(super) fn edit_text(
        &mut self,
        chat: ChatId,
        id: String,
        text: String,
        mentions: Vec<String>,
    ) {
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error("Not connected to WhatsApp".to_owned()));
            self.emit(Event::Edited {
                chat,
                id,
                success: false,
            });
            return;
        };
        let content = Content::text(text.clone());
        let mention_rows = self.mentions_of(&mentions);
        let mut message = outgoing_text(text, None, &mentions);
        self.apply_ephemeral(&chat, &mut message);
        let commands = self.commands.clone();
        let events = self.events.clone();
        let waker = self.waker.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        tokio::spawn(async move {
            let result = client.edit_message(jid, id.clone(), message).await;
            let success = result.is_ok();
            let _ = Completion {
                generation: session_generation,
                done: Done::Edited {
                    chat: chat.clone(),
                    id,

                    success,
                    content,
                    mentions: mention_rows,
                },
            }
            .send(&commands);
            if result.is_err()
                && session_generation_shared.load(Ordering::Acquire) == session_generation
            {
                let _ = events.send(Event::Error("Could not edit the message".to_owned()));
                waker.wake();
            }
        });
    }
}

impl Worker {
    /// Archives and sends an uploaded attachment message.
    pub(super) fn outbound(
        &mut self,
        chat: ChatId,
        session_generation: u64,
        row: Message,
        raw: Vec<u8>,
    ) {
        if self.archive_cleanup_failed {
            return;
        }
        // A sticker's pending row is already shown; fail it rather than
        // leave it pending forever.
        let fail = |worker: &mut Self, error: &str| {
            if let Ok(true) =
                worker
                    .archive
                    .set_status(&chat, &row.id, Delivery::Failed, crate::util::now())
            {
                worker.emit_message(&chat, &row.id);
            }
            worker.emit(Event::Error(error.to_owned()));
        };
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            fail(self, "Not connected to WhatsApp");
            return;
        };
        let Ok(mut message) = wa::Message::decode_from_slice(&raw) else {
            fail(self, "Could not encode the attachment");
            return;
        };
        let expiration = self.apply_ephemeral(&chat, &mut message);
        let raw = message.encode_to_vec();
        let id = row.id.clone();
        self.store_message(row, Some(raw), None);
        let session_generation_shared = self.session_generation_shared.clone();
        tokio::spawn(send_outgoing(
            OutgoingSession {
                client,
                commands: self.commands.clone(),
                generation: session_generation,
                generation_shared: session_generation_shared,
            },
            chat,
            jid,
            id,
            message,
            expiration,
        ));
    }

    pub(super) fn outbound_batch(
        &mut self,
        chat: ChatId,
        session_generation: u64,
        row: Message,
        raw: Vec<u8>,
        sent: tokio::sync::mpsc::UnboundedSender<bool>,
    ) {
        if self.archive_cleanup_failed {
            let _ = sent.send(false);
            return;
        }
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            let _ = sent.send(false);
            return;
        };
        let Ok(mut message) = wa::Message::decode_from_slice(&raw) else {
            let _ = sent.send(false);
            return;
        };
        let expiration = self.apply_ephemeral(&chat, &mut message);
        let raw = message.encode_to_vec();
        let id = row.id.clone();
        self.store_message(row, Some(raw), None);
        let commands = self.commands.clone();
        let session_generation_shared = self.session_generation_shared.clone();
        tokio::spawn(async move {
            let success = send_outgoing(
                OutgoingSession {
                    client,
                    commands,
                    generation: session_generation,
                    generation_shared: session_generation_shared,
                },
                chat,
                jid,
                id,
                message,
                expiration,
            )
            .await;
            let _ = sent.send(success);
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn chat_tag_golden_lowercase_digest_prefix() {
        assert_eq!(super::chat_tag("abc"), "ba7816bf");
    }
    #[test]
    fn staging_entropy_golden_and_failure() {
        let dir = std::path::Path::new("/synthetic");
        let path = super::download_staging_path_with(dir, |bytes| {
            *bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 255];
            Ok::<(), &str>(())
        })
        .unwrap();
        assert_eq!(
            path,
            dir.join(".download-000102030405060708090a0b0c0d0eff.tmp")
        );
        assert_eq!(
            super::download_staging_path_with(dir, |_| Err::<(), _>("entropy unavailable")),
            Err("entropy unavailable")
        );
    }
    use super::*;

    #[tokio::test]
    async fn snapshot_is_stable_when_source_changes_and_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::write(&source, b"original").unwrap();
        let generation = AtomicU64::new(1);
        let lock = tokio::sync::Mutex::new(());
        let staging = snapshot_session_file(&source, dir.path(), 1, &generation, &lock)
            .await
            .unwrap();
        let path = staging.path().to_owned();
        std::fs::write(&source, b"changed").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        drop(staging);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn cancelled_preparation_removes_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::write(&source, b"synthetic").unwrap();
        let directory = dir.path().to_owned();
        let (send, receive) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let generation = AtomicU64::new(1);
            let lock = tokio::sync::Mutex::new(());
            let staging = snapshot_session_file(&source, &directory, 1, &generation, &lock)
                .await
                .unwrap();
            send.send(staging.path().to_owned()).unwrap();
            std::future::pending::<()>().await;
            drop(staging);
        });
        let path = receive.await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn invalidated_snapshot_cannot_be_promoted() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        std::fs::write(&source, b"synthetic").unwrap();
        let generation = AtomicU64::new(1);
        let lock = tokio::sync::Mutex::new(());
        let staging = snapshot_session_file(&source, dir.path(), 1, &generation, &lock)
            .await
            .unwrap();
        let path = staging.path().to_owned();
        generation.store(2, Ordering::Release);
        assert!(
            promote_session_snapshot(staging, &destination, 1, &generation, &lock)
                .await
                .is_err()
        );
        assert!(!path.exists());
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn snapshot_promotion_preserves_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("attachment.pdf");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&destination, b"existing").unwrap();
        let generation = AtomicU64::new(1);
        let lock = tokio::sync::Mutex::new(());
        let staging = snapshot_session_file(&source, dir.path(), 1, &generation, &lock)
            .await
            .unwrap();
        let staging_path = staging.path().to_owned();
        let promoted = promote_session_snapshot(staging, &destination, 1, &generation, &lock)
            .await
            .unwrap();
        assert_ne!(promoted, destination);
        assert_eq!(std::fs::read(destination).unwrap(), b"existing");
        assert_eq!(std::fs::read(promoted).unwrap(), b"new");
        assert!(!staging_path.exists());
    }

    #[tokio::test]
    async fn encrypted_file_source_supports_independent_retry_offsets() {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        use whatsapp_rust::wacore::upload::UploadSource;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let plaintext = vec![42; 32769];
        std::fs::write(&source, &plaintext).unwrap();
        let generation = AtomicU64::new(1);
        let lock = tokio::sync::Mutex::new(());
        let snapshot = snapshot_session_file(&source, dir.path(), 1, &generation, &lock)
            .await
            .unwrap();
        let (encrypted, info) = encrypt_snapshot(&snapshot, MediaType::Document)
            .await
            .unwrap();
        let encrypted_path = encrypted.file.path().to_owned();
        let mut all = Vec::new();
        encrypted
            .reader_from(0)
            .unwrap()
            .read_to_end(&mut all)
            .unwrap();
        let mut partial = Vec::new();
        encrypted
            .reader_from(17)
            .unwrap()
            .read_to_end(&mut partial)
            .unwrap();
        assert_eq!(partial, all[17..]);
        assert_eq!(encrypted.len(), all.len() as u64);
        assert_eq!(info.file_length, plaintext.len() as u64);
        assert_eq!(
            info.file_sha256,
            <[u8; 32]>::from(Sha256::digest(&plaintext))
        );
        assert_eq!(info.file_enc_sha256, <[u8; 32]>::from(Sha256::digest(&all)));
        drop(encrypted);
        assert!(!encrypted_path.exists());
    }

    #[tokio::test]
    async fn retrying_uploader_owns_source_until_all_attempts_finish() {
        use std::io::Read;
        use whatsapp_rust::wacore::upload::UploadSource;

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source");
        std::fs::write(&input, vec![7; 8192]).unwrap();
        let generation = AtomicU64::new(1);
        let lock = tokio::sync::Mutex::new(());
        let snapshot = snapshot_session_file(&input, dir.path(), 1, &generation, &lock)
            .await
            .unwrap();
        let (source, _) = encrypt_snapshot(&snapshot, MediaType::Document)
            .await
            .unwrap();
        let path = source.file.path().to_owned();
        let expected = std::fs::read(&path).unwrap();
        let uploader = async move {
            let source = std::sync::Arc::new(source);
            for offset in [0, 31] {
                let source = source.clone();
                let attempt = tokio::task::spawn_blocking(move || {
                    let mut bytes = Vec::new();
                    source.reader_from(offset)?.read_to_end(&mut bytes)?;
                    Ok::<_, std::io::Error>(bytes)
                })
                .await
                .unwrap()
                .unwrap();
                assert_eq!(attempt, expected[offset as usize..]);
                let result = if offset == 0 {
                    Err("synthetic failure")
                } else {
                    Ok(())
                };
                if result.is_ok() {
                    return;
                }
            }
            panic!("retry did not succeed");
        };
        uploader.await;
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn photo_snapshot_preserves_jpeg_and_transcodes_png() {
        let dir = tempfile::tempdir().unwrap();
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            7,
            5,
            image::Rgb([12, 34, 56]),
        ));
        let jpeg = encode_jpeg(&image, 88).unwrap();
        let jpeg_path = dir.path().join("image.jpg");
        let png_path = dir.path().join("image.png");
        std::fs::write(&jpeg_path, &jpeg).unwrap();
        image.save(&png_path).unwrap();
        let generation = AtomicU64::new(1);
        let lock = tokio::sync::Mutex::new(());
        for (path, is_jpeg) in [(&jpeg_path, true), (&png_path, false)] {
            let snapshot = snapshot_session_file(path, dir.path(), 1, &generation, &lock)
                .await
                .unwrap();
            let original_staging = snapshot.path().to_owned();
            let photo = transform_photo_snapshot(snapshot, is_jpeg).await.unwrap();
            assert_eq!((photo.width, photo.height), (7, 5));
            assert!(photo.thumbnail.is_some());
            let transformed = std::fs::read(photo.snapshot.path()).unwrap();
            if is_jpeg {
                assert_eq!(transformed, jpeg);
            } else {
                assert_eq!(transformed, encode_jpeg(&image, 88).unwrap());
                assert!(!original_staging.exists());
            }
            assert_eq!(image::load_from_memory(&transformed).unwrap().width(), 7);
        }
    }

    #[tokio::test]
    #[ignore = "synthetic 200 MB memory measurement"]
    async fn synthetic_large_document_streams_to_disk() {
        use whatsapp_rust::wacore::upload::UploadSource;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let size = 200_000_000;
        std::fs::File::create(&source)
            .unwrap()
            .set_len(size)
            .unwrap();
        let generation = AtomicU64::new(1);
        let lock = tokio::sync::Mutex::new(());
        let snapshot = snapshot_session_file(&source, dir.path(), 1, &generation, &lock)
            .await
            .unwrap();
        let (encrypted, info) = encrypt_snapshot(&snapshot, MediaType::Document)
            .await
            .unwrap();
        assert_eq!(info.file_length, size);
        assert_eq!(encrypted.len(), size + 26);
    }

    #[test]
    fn the_last_resort_rename_never_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join(".download.tmp");
        let taken = dir.path().join("taken.pdf");
        let free = dir.path().join("free.pdf");
        std::fs::write(&staged, b"new").unwrap();
        std::fs::write(&taken, b"old").unwrap();

        let refused = rename_if_absent(&staged, &taken).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&taken).unwrap(), b"old");

        rename_if_absent(&staged, &free).unwrap();
        assert_eq!(std::fs::read(&free).unwrap(), b"new");
        assert!(!staged.exists());
    }

    fn aged(path: &Path, age: Duration) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - age)
            .unwrap();
    }

    #[test]
    fn staging_names_match_only_the_generated_pattern() {
        let dir = Path::new("/cache");
        for _ in 0..32 {
            let staged = download_staging_path(dir).expect("synthetic staging entropy");
            assert!(is_staging_name(staged.file_name().unwrap()), "{staged:?}");
        }
        let temporary = tempfile::tempdir().unwrap();
        let upload = upload_staging_file(temporary.path()).unwrap();
        assert!(is_upload_staging_name(upload.path().file_name().unwrap()));
        let token = "0123456789abcdef0123456789abcdef";
        for name in [
            format!("download-{token}.tmp"),
            format!(".download-{token}.tmp.pdf"),
            format!(".download-{token}"),
            format!(".download-{}.tmp", &token[1..]),
            format!(".download-{token}0.tmp"),
            format!(".download-{}.tmp", token.to_ascii_uppercase()),
            ".download-0123456789abcdef0123456789abcdeg.tmp".to_owned(),
            ".download-.tmp".to_owned(),
            "ABC-0b617fa3-invoice.pdf".to_owned(),
        ] {
            assert!(!is_staging_name(std::ffi::OsStr::new(&name)), "{name}");
        }
    }

    #[test]
    fn stale_staging_files_are_removed_and_everything_else_kept() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("media");
        std::fs::create_dir(&dir).unwrap();
        let hour = Duration::from_secs(60 * 60);

        let stale = download_staging_path(&dir).expect("synthetic staging entropy");
        std::fs::write(&stale, b"interrupted").unwrap();
        aged(&stale, 2 * hour);

        let stale_upload = upload_staging_file(&dir).unwrap();
        aged(stale_upload.path(), 2 * hour);

        let recent = download_staging_path(&dir).expect("synthetic staging entropy");
        std::fs::write(&recent, b"still streaming").unwrap();
        aged(&recent, Duration::from_secs(10 * 60));

        let attachment = dir.join("ABC-0b617fa3-invoice.pdf");
        std::fs::write(&attachment, b"kept").unwrap();
        aged(&attachment, 48 * hour);
        let lookalike = dir.join(".download-notatoken.tmp");
        std::fs::write(&lookalike, b"kept").unwrap();
        aged(&lookalike, 48 * hour);

        let outside = root.path().join("outside.pdf");
        std::fs::write(&outside, b"outside").unwrap();
        aged(&outside, 48 * hour);
        let link = download_staging_path(&dir).expect("synthetic staging entropy");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let old = rustix::fs::Timespec {
            tv_sec: (std::time::SystemTime::now() - 48 * hour)
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            tv_nsec: 0,
        };
        rustix::fs::utimensat(
            rustix::fs::CWD,
            &link,
            &rustix::fs::Timestamps {
                last_access: old,
                last_modification: old,
            },
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();

        let nested_dir = download_staging_path(&dir).expect("synthetic staging entropy");
        std::fs::create_dir(&nested_dir).unwrap();
        let nested = download_staging_path(&nested_dir).expect("synthetic staging entropy");
        std::fs::write(&nested, b"nested").unwrap();
        aged(&nested, 48 * hour);
        let subdir = dir.join("sub");
        std::fs::create_dir(&subdir).unwrap();
        let in_subdir = download_staging_path(&subdir).expect("synthetic staging entropy");
        std::fs::write(&in_subdir, b"nested").unwrap();
        aged(&in_subdir, 48 * hour);

        let removed = remove_stale_staging_files(&dir, hour, std::time::SystemTime::now());

        assert_eq!(removed, 2);
        assert!(!stale.exists());
        assert!(!stale_upload.path().exists());
        for kept in [
            &recent,
            &attachment,
            &lookalike,
            &outside,
            &nested,
            &in_subdir,
        ] {
            assert!(kept.exists(), "{kept:?}");
        }
        let link_metadata = link.symlink_metadata().unwrap();
        assert!(link_metadata.file_type().is_symlink());
        assert!(link_metadata.modified().unwrap() < std::time::SystemTime::now() - hour);
        assert!(nested_dir.is_dir());
    }

    #[test]
    fn a_missing_media_directory_removes_nothing() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            remove_stale_staging_files(
                &root.path().join("absent"),
                Duration::from_secs(1),
                std::time::SystemTime::now()
            ),
            0
        );
    }
}
