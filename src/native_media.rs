//! Privacy-safe, toolkit-independent presentation for native message rows.

use std::{
    fmt,
    io::Cursor,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::model::{Content, MediaState, Message};

/// Maximum encoded bytes accepted by the thumbnail decoder.
pub const MAX_THUMBNAIL_INPUT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum edge length accepted from untrusted image metadata.
pub const MAX_THUMBNAIL_DIMENSION: u32 = 8_192;
/// Maximum decoded pixels accepted before allocating an output image.
pub const MAX_THUMBNAIL_PIXELS: u64 = 32 * 1024 * 1024;
/// Maximum edge length of generated thumbnail pixels.
pub const MAX_THUMBNAIL_EDGE: u32 = 512;

/// Still-image bytes or explicit placeholder for content this helper will not decode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThumbnailResult {
    Image(DecodedThumbnail),
    Placeholder(ThumbnailPlaceholder),
}

/// Bounded RGBA thumbnail, safe to hand to a native image widget.
#[derive(Clone, Eq, PartialEq)]
pub struct DecodedThumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl fmt::Debug for DecodedThumbnail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedThumbnail")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_bytes", &self.rgba.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThumbnailPlaceholder {
    DecodeFailed,
    TooLarge,
}

/// Decode a still image under encoded-size, dimension, allocation, and output bounds.
/// Animated images and video intentionally return placeholders rather than decoding frames.
pub fn decode_thumbnail(bytes: &[u8]) -> ThumbnailResult {
    if bytes.len() > MAX_THUMBNAIL_INPUT_BYTES {
        return ThumbnailResult::Placeholder(ThumbnailPlaceholder::TooLarge);
    }

    let limits = image_limits();
    let dimension_reader = match image::ImageReader::new(Cursor::new(bytes)).with_guessed_format() {
        Ok(reader) => reader,
        Err(_) => return ThumbnailResult::Placeholder(ThumbnailPlaceholder::DecodeFailed),
    };
    let Ok((width, height)) = dimension_reader.into_dimensions() else {
        return ThumbnailResult::Placeholder(ThumbnailPlaceholder::DecodeFailed);
    };
    let pixels = u64::from(width).checked_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_THUMBNAIL_DIMENSION
        || height > MAX_THUMBNAIL_DIMENSION
        || pixels.is_none_or(|pixels| pixels > MAX_THUMBNAIL_PIXELS)
    {
        return ThumbnailResult::Placeholder(ThumbnailPlaceholder::TooLarge);
    }

    let mut reader = match image::ImageReader::new(Cursor::new(bytes)).with_guessed_format() {
        Ok(reader) => reader,
        Err(_) => return ThumbnailResult::Placeholder(ThumbnailPlaceholder::DecodeFailed),
    };
    reader.limits(limits);
    let Ok(decoded) = reader.decode() else {
        return ThumbnailResult::Placeholder(ThumbnailPlaceholder::DecodeFailed);
    };
    let thumbnail = decoded
        .thumbnail(MAX_THUMBNAIL_EDGE, MAX_THUMBNAIL_EDGE)
        .to_rgba8();
    ThumbnailResult::Image(DecodedThumbnail {
        width: thumbnail.width(),
        height: thumbnail.height(),
        rgba: thumbnail.into_raw(),
    })
}

/// Explicit renderer-safe content data. Debug output redacts user-authored content.
#[derive(Clone, Eq, PartialEq)]
pub enum NativeMediaContent {
    Document {
        file_name: String,
        detail: String,
    },
    Contact {
        display_name: String,
    },
    Location {
        label: String,
    },
    Poll {
        question: String,
        options: Vec<PollOptionPresentation>,
    },
    /// Quick-reply labels under the message text.
    Buttons {
        text: String,
        footer: Option<String>,
        labels: Vec<String>,
        /// Index of the button already answered.
        answered: Option<usize>,
    },
    /// List heading with the button label and its sections.
    List {
        title: String,
        description: Option<String>,
        button: String,
        footer: Option<String>,
        sections: Vec<ListSectionPresentation>,
        /// Title of the row already chosen.
        answered: Option<String>,
    },
    /// Business template: body text and (label, address) buttons.
    Template {
        text: String,
        footer: Option<String>,
        links: Vec<(String, String)>,
    },
    VideoPlaceholder,
    UnsupportedPlaceholder,
    Other,
}

impl fmt::Debug for NativeMediaContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Document { .. } => "Document",
            Self::Contact { .. } => "Contact",
            Self::Location { .. } => "Location",
            Self::Poll { .. } => "Poll",
            Self::Buttons { .. } => "Buttons",
            Self::List { .. } => "List",
            Self::Template { .. } => "Template",
            Self::VideoPlaceholder => "VideoPlaceholder",
            Self::UnsupportedPlaceholder => "UnsupportedPlaceholder",
            Self::Other => "Other",
        };
        formatter.write_str(name)
    }
}

/// One list section: optional heading and its rows as (title, detail).
#[derive(Clone, Eq, PartialEq)]
pub struct ListSectionPresentation {
    pub title: Option<String>,
    pub rows: Vec<(String, Option<String>)>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct PollOptionPresentation {
    pub text: String,
    pub votes: usize,
    pub selected: bool,
}

impl fmt::Debug for PollOptionPresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PollOptionPresentation")
            .field("votes", &self.votes)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ReplyPresentation {
    pub sender: Option<String>,
    pub summary: String,
}

impl fmt::Debug for ReplyPresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReplyPresentation(..)")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ReactionPresentation {
    pub emoji: String,
    pub count: usize,
    pub selected: bool,
}

impl fmt::Debug for ReactionPresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReactionPresentation")
            .field("count", &self.count)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct NativeMessageContent {
    pub content: NativeMediaContent,
    pub reply: Option<ReplyPresentation>,
    pub forwarded: bool,
    pub reactions: Vec<ReactionPresentation>,
}

impl fmt::Debug for NativeMessageContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeMessageContent")
            .field("content", &self.content)
            .field("has_reply", &self.reply.is_some())
            .field("forwarded", &self.forwarded)
            .field("reaction_count", &self.reactions.len())
            .finish()
    }
}

/// Build display content without exposing protocol identifiers, vCards, coordinates, or paths.
pub fn project_content(message: &Message) -> NativeMessageContent {
    let content = match &message.content {
        Content::Document {
            media,
            file_name,
            pages,
            ..
        } => {
            let mut detail = vec![crate::util::bytes(media.size)];
            if let Some(pages) = pages {
                detail.push(format!("{pages} pages"));
            }
            NativeMediaContent::Document {
                file_name: safe_file_name(file_name),
                detail: detail.join(" · "),
            }
        }
        Content::Contact { display_name, .. } => NativeMediaContent::Contact {
            display_name: display_name.clone(),
        },
        Content::Location { name, address, .. } => NativeMediaContent::Location {
            label: name
                .as_deref()
                .or(address.as_deref())
                .unwrap_or("Location")
                .to_owned(),
        },
        Content::Poll {
            question,
            options,
            state,
        } => NativeMediaContent::Poll {
            question: question.clone(),
            options: options
                .iter()
                .enumerate()
                .map(|(index, text)| PollOptionPresentation {
                    text: text.clone(),
                    votes: state.counts.get(index).copied().unwrap_or(0),
                    selected: state.selected.contains(&index),
                })
                .collect(),
        },
        Content::Buttons {
            text,
            footer,
            buttons,
            answered,
        } => NativeMediaContent::Buttons {
            text: text.clone(),
            footer: footer.clone(),
            labels: buttons.iter().map(|button| button.label.clone()).collect(),
            answered: answered
                .as_ref()
                .and_then(|id| buttons.iter().position(|button| &button.id == id)),
        },
        Content::List {
            title,
            description,
            button,
            footer,
            sections,
            answered,
        } => NativeMediaContent::List {
            title: title.clone(),
            description: description.clone(),
            button: button.clone(),
            footer: footer.clone(),
            sections: sections
                .iter()
                .map(|section| ListSectionPresentation {
                    title: section.title.clone(),
                    rows: section
                        .rows
                        .iter()
                        .map(|row| (row.title.clone(), row.description.clone()))
                        .collect(),
                })
                .collect(),
            answered: answered
                .as_deref()
                .and_then(|id| message.content.choice(id))
                .map(|(title, _)| title.to_owned()),
        },
        Content::Template {
            text,
            footer,
            links,
        } => NativeMediaContent::Template {
            text: text.clone(),
            footer: footer.clone(),
            links: links
                .iter()
                .map(|link| (link.label.clone(), link.url.clone()))
                .collect(),
        },
        Content::Video { .. } => NativeMediaContent::VideoPlaceholder,
        Content::Unsupported { .. } => NativeMediaContent::UnsupportedPlaceholder,
        _ => NativeMediaContent::Other,
    };

    let reply = message.quoted.as_ref().map(|quoted| ReplyPresentation {
        sender: quoted.sender_name.clone(),
        summary: quoted.summary.clone(),
    });
    let mut reactions: Vec<ReactionPresentation> = Vec::new();
    for reaction in &message.reactions {
        if let Some(existing) = reactions
            .iter_mut()
            .find(|entry| entry.emoji == reaction.emoji)
        {
            existing.count += 1;
            existing.selected |= reaction.from_me;
        } else {
            reactions.push(ReactionPresentation {
                emoji: reaction.emoji.clone(),
                count: 1,
                selected: reaction.from_me,
            });
        }
    }
    NativeMessageContent {
        content,
        reply,
        forwarded: message.forwarded,
        reactions,
    }
}

/// Keeps document labels to a filename component, even for Windows-style paths.
pub(crate) fn safe_file_name(file_name: &str) -> String {
    let name = file_name.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = name
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    if name.trim().is_empty() || name == "." || name == ".." {
        "Document".to_owned()
    } else {
        name
    }
}

/// Typed activation result for a downloadable or already-local attachment.
#[derive(Clone, Eq, PartialEq)]
pub enum NativeMediaAction {
    Open(PathBuf),
    Download {
        chat: String,
        message: String,
    },
    /// Reply to an interactive message with one of its quick-reply buttons.
    AnswerButton {
        chat: String,
        message: String,
        button: String,
    },
    /// Reply to a list message with one of its rows.
    AnswerListRow {
        chat: String,
        message: String,
        row: String,
    },
}

impl fmt::Debug for NativeMediaAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(_) => formatter.write_str("Open(..)"),
            Self::Download { .. } => formatter.write_str("Download { .. }"),
            Self::AnswerButton { .. } => formatter.write_str("AnswerButton { .. }"),
            Self::AnswerListRow { .. } => formatter.write_str("AnswerListRow { .. }"),
        }
    }
}

/// Resolve attachment activation without putting paths or identifiers in presentation data.
pub fn attachment_action(message: &Message) -> Option<NativeMediaAction> {
    let media = message.content.media()?;
    // A file removed from the cache downloads again.
    if let Some(path) = media.path.as_ref().filter(|path| path.is_file()) {
        return Some(NativeMediaAction::Open(path.clone()));
    }
    match &media.state {
        MediaState::Downloading => None,
        MediaState::Idle | MediaState::Failed(_) => Some(NativeMediaAction::Download {
            chat: message.chat.clone(),
            message: message.id.clone(),
        }),
    }
}

/// Shared generation source for cancelling stale asynchronous decodes.
#[derive(Clone, Default)]
pub struct DecodeToken(Arc<AtomicU64>, Vec<DecodeToken>);

impl DecodeToken {
    /// Makes `cancel` on this token cancel `child` too. A token serves one
    /// decode at a time, so a widget with several images needs one per image.
    pub fn adopt(&mut self, child: DecodeToken) {
        self.1.push(child);
    }

    /// Start new decode generation; all earlier tickets become stale.
    pub fn issue(&self) -> DecodeTicket {
        let generation = self.0.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        DecodeTicket {
            source: self.0.clone(),
            generation,
        }
    }

    /// Invalidates outstanding decode tickets when virtualized row is recycled.
    pub fn cancel(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
        self.1.iter().for_each(DecodeToken::cancel);
    }
}

#[derive(Clone)]
pub struct DecodeTicket {
    source: Arc<AtomicU64>,
    generation: u64,
}

impl DecodeTicket {
    /// True while no newer decode has superseded this ticket.
    pub fn is_current(&self) -> bool {
        self.source.load(Ordering::Acquire) == self.generation
    }

    /// Decode only for the current generation, discarding completion if superseded mid-decode.
    pub fn decode_thumbnail(&self, bytes: &[u8]) -> Option<ThumbnailResult> {
        if !self.is_current() {
            return None;
        }
        let result = decode_thumbnail(bytes);
        self.is_current().then_some(result)
    }
}

pub(crate) fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_THUMBNAIL_DIMENSION);
    limits.max_image_height = Some(MAX_THUMBNAIL_DIMENSION);
    limits.max_alloc = Some(MAX_THUMBNAIL_PIXELS.saturating_mul(4));
    limits
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::PathBuf;

    use super::*;
    use crate::model::{Delivery, Media, MediaState, Quoted, Reaction};

    fn media() -> Media {
        Media {
            mime: "application/octet-stream".into(),
            size: 2_048,
            width: Some(640),
            height: Some(480),
            path: Some(PathBuf::from("/private/user/photo.jpg")),
            state: MediaState::Idle,
        }
    }

    fn message(content: Content) -> Message {
        Message {
            id: "private-message-id".into(),
            chat: "private-chat-id".into(),
            sender: "private-sender".into(),
            sender_name: None,
            from_me: false,
            timestamp: 0,
            content,
            status: Delivery::None,
            delivered_at: None,
            read_at: None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            mentions: Vec::new(),
            forwarded: false,
            thumbnail: None,
        }
    }

    #[test]
    fn thumbnail_decode_is_bounded_and_rejects_oversized_inputs() {
        let image = image::RgbaImage::from_pixel(1_024, 128, image::Rgba([1, 2, 3, 255]));
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let ThumbnailResult::Image(thumbnail) = decode_thumbnail(encoded.get_ref()) else {
            panic!("expected decoded thumbnail");
        };
        assert!(thumbnail.width <= MAX_THUMBNAIL_EDGE);
        assert!(thumbnail.height <= MAX_THUMBNAIL_EDGE);
        assert_eq!(
            thumbnail.rgba.len(),
            (thumbnail.width * thumbnail.height * 4) as usize
        );

        let too_wide = image::RgbaImage::from_pixel(
            MAX_THUMBNAIL_DIMENSION + 1,
            1,
            image::Rgba([1, 2, 3, 255]),
        );
        let mut encoded_too_wide = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(too_wide)
            .write_to(&mut encoded_too_wide, image::ImageFormat::Png)
            .unwrap();
        assert_eq!(
            decode_thumbnail(encoded_too_wide.get_ref()),
            ThumbnailResult::Placeholder(ThumbnailPlaceholder::TooLarge)
        );

        let oversized = vec![0; MAX_THUMBNAIL_INPUT_BYTES + 1];
        assert_eq!(
            decode_thumbnail(&oversized),
            ThumbnailResult::Placeholder(ThumbnailPlaceholder::TooLarge)
        );
    }

    #[test]
    fn rich_content_carries_renderable_data_but_debug_redacts_private_payloads() {
        let mut message = message(Content::Poll {
            question: "Private poll question".into(),
            options: vec!["Private option".into(), "Publicly rendered option".into()],
            state: crate::model::PollState {
                counts: vec![4],
                selected: vec![1],
                ..Default::default()
            },
        });
        message.quoted = Some(Quoted {
            id: "secret-id".into(),
            sender: "secret-address".into(),
            sender_name: Some("Private sender".into()),
            summary: "Private reply text".into(),
            mentions: Vec::new(),
        });
        message.forwarded = true;
        message.reactions = vec![
            Reaction {
                sender: "private-reactor-a".into(),
                from_me: false,
                emoji: "🔥".into(),
            },
            Reaction {
                sender: "private-reactor-b".into(),
                from_me: true,
                emoji: "🔥".into(),
            },
        ];

        let projected = project_content(&message);
        let NativeMediaContent::Poll { question, options } = &projected.content else {
            panic!("expected poll presentation");
        };
        assert_eq!(question, "Private poll question");
        assert_eq!(options[0].votes, 4);
        assert_eq!(options[1].votes, 0);
        assert!(options[1].selected);
        assert_eq!(
            projected.reply.as_ref().unwrap().summary,
            "Private reply text"
        );
        assert!(projected.forwarded);
        assert_eq!(projected.reactions[0].count, 2);
        assert!(projected.reactions[0].selected);

        let debug = format!("{projected:?}");
        for secret in [
            "Private poll question",
            "Private option",
            "Private sender",
            "Private reply text",
            "private-reactor-a",
            "secret-address",
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn document_contact_and_location_projection_omits_paths_vcard_and_coordinates() {
        let cases = [
            message(Content::Document {
                media: media(),
                file_name: r"C:\Users\private\report.pdf".into(),
                caption: None,
                pages: Some(2),
            }),
            message(Content::Contact {
                display_name: "Private contact".into(),
                vcard: "BEGIN:VCARD;TEL:+15555550123".into(),
            }),
            message(Content::Location {
                latitude: 12.345,
                longitude: -67.89,
                name: Some("Private place".into()),
                address: Some("Private street".into()),
            }),
        ];
        let projected: Vec<_> = cases.iter().map(project_content).collect();
        assert!(
            matches!(&projected[0].content, NativeMediaContent::Document { file_name, .. } if file_name == "report.pdf")
        );
        assert!(
            matches!(&projected[1].content, NativeMediaContent::Contact { display_name } if display_name == "Private contact")
        );
        assert!(
            matches!(&projected[2].content, NativeMediaContent::Location { label } if label == "Private place")
        );
        let debug = format!("{projected:?}");
        for secret in ["C:\\Users", "+15555550123", "12.345", "-67.89"] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn actions_are_typed_and_decode_tickets_cancel_stale_work() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut pending = message(Content::Image {
            media: media(),
            caption: None,
        });
        if let Content::Image { media, .. } = &mut pending.content {
            media.path = Some(file.path().to_path_buf());
        }
        assert_eq!(
            attachment_action(&pending),
            Some(NativeMediaAction::Open(file.path().to_path_buf()))
        );

        let mut downloadable = message(Content::Image {
            media: media(),
            caption: None,
        });
        if let Content::Image { media, .. } = &mut downloadable.content {
            media.path = None;
        }
        assert_eq!(
            attachment_action(&downloadable),
            Some(NativeMediaAction::Download {
                chat: "private-chat-id".into(),
                message: "private-message-id".into(),
            })
        );
        if let Content::Image { media, .. } = &mut downloadable.content {
            media.state = MediaState::Downloading;
        }
        assert_eq!(attachment_action(&downloadable), None);

        let mut album = DecodeToken::default();
        let tile = DecodeToken::default();
        album.adopt(tile.clone());
        let ticket = tile.issue();
        assert!(ticket.is_current());
        album.cancel();
        assert!(!ticket.is_current());

        let token = DecodeToken::default();
        let first = token.issue();
        assert!(first.is_current());
        let second = token.issue();
        assert!(!first.is_current());
        assert_eq!(first.decode_thumbnail(&[]), None);
        assert!(second.is_current());
        assert_eq!(
            second.decode_thumbnail(&[]),
            Some(ThumbnailResult::Placeholder(
                ThumbnailPlaceholder::DecodeFailed
            ))
        );
    }

    #[test]
    fn action_debug_omits_paths_and_protocol_identifiers() {
        let actions = [
            NativeMediaAction::Open(PathBuf::from("/private/user/photo.jpg")),
            NativeMediaAction::Download {
                chat: "private-chat-id".into(),
                message: "private-message-id".into(),
            },
            NativeMediaAction::AnswerButton {
                chat: "private-chat-id".into(),
                message: "private-message-id".into(),
                button: "private-button-id".into(),
            },
            NativeMediaAction::AnswerListRow {
                chat: "private-chat-id".into(),
                message: "private-message-id".into(),
                row: "private-row-id".into(),
            },
        ];

        let debug = format!("{actions:?}");
        assert!(debug.contains("Open(..)"));
        assert!(debug.contains("Download { .. }"));
        assert!(debug.contains("AnswerButton { .. }"));
        assert!(!debug.contains("private-button-id"));
        assert!(debug.contains("AnswerListRow { .. }"));
        assert!(!debug.contains("private-row-id"));
        for secret in [
            "/private/user/photo.jpg",
            "private-chat-id",
            "private-message-id",
        ] {
            assert!(!debug.contains(secret));
        }
    }
}
