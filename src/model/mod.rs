//! UI models for chats, messages, and view actions.
//!
//! The backend translates protocol types into these models, keeping protobufs
//! out of views and giving the archive a stable shape.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Chat JID string: `<phone>@s.whatsapp.net`, `<id>@g.us`, or `<id>@lid`.
pub type ChatId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatKind {
    Direct,
    Group,
    /// Read-only newsletter or broadcast list.
    Broadcast,
}

impl ChatKind {
    pub fn from_id(id: &str) -> Self {
        match id.rsplit('@').next() {
            Some("g.us") => Self::Group,
            Some("newsletter") | Some("broadcast") => Self::Broadcast,
            _ => Self::Direct,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chat {
    pub id: ChatId,
    /// Best known address-book, push, or phone-number name.
    pub name: String,
    pub kind: ChatKind,
    /// Latest-message Unix timestamp used for ordering.
    pub last_activity: i64,
    pub unread: u32,
    pub archived: bool,
    pub pinned: bool,
    /// Pin time in Unix milliseconds; zero for older archives with no ordering.
    pub pinned_at: i64,
    /// Mute end as Unix seconds; `Some(0)` means indefinite.
    pub muted_until: Option<i64>,
    /// Latest message shown in the chat list.
    pub last: Option<LastMessage>,
    /// Canonical group-member ids, empty until loaded.
    pub participants: Vec<String>,
    /// Whether this is an announcement group where we cannot post.
    pub read_only: bool,
    /// Hidden while WhatsApp chat lock is enabled on the phone.
    pub locked: bool,
    /// Disappearing-message duration in seconds, if enabled.
    pub ephemeral_expiration: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LastMessage {
    pub from_me: bool,
    pub sender: String,
    /// Group-message sender.
    pub sender_name: Option<String>,
    pub summary: String,
    pub status: Delivery,
}

impl Chat {
    pub fn new(id: ChatId, name: String) -> Self {
        let kind = ChatKind::from_id(&id);
        Self {
            id,
            name,
            kind,
            last_activity: 0,
            unread: 0,
            archived: false,
            pinned: false,
            pinned_at: 0,
            muted_until: None,
            last: None,
            participants: Vec::new(),
            read_only: false,
            locked: false,
            ephemeral_expiration: None,
        }
    }

    /// Newsletter publishing permissions are not supported by this client.
    pub fn can_send(&self) -> bool {
        !self.locked && !self.read_only && self.kind != ChatKind::Broadcast
    }

    pub fn is_group(&self) -> bool {
        self.kind == ChatKind::Group
    }

    pub fn muted(&self, now: i64) -> bool {
        matches!(self.muted_until, Some(0)) || self.muted_until.is_some_and(|until| until > now)
    }

    /// Direct-chat phone number as digits.
    pub fn phone(&self) -> Option<&str> {
        phone_of(&self.id)
    }
}

/// Extracts digits from a `<phone>@s.whatsapp.net` id.
pub fn phone_of(id: &str) -> Option<&str> {
    let (user, server) = id.split_once('@')?;
    (server == "s.whatsapp.net" && user.chars().all(|c| c.is_ascii_digit())).then_some(user)
}

/// Outgoing-message delivery state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Delivery {
    /// Incoming message without outgoing receipts.
    #[default]
    None,
    /// Sent to the backend but not acknowledged by the server.
    Pending,
    Sent,
    Delivered,
    Read,
    Played,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// WhatsApp message id, unique within a chat.
    pub id: String,
    pub chat: ChatId,
    /// Sender JID, including our own for outgoing messages.
    pub sender: String,
    /// Group sender's push name at receipt time.
    pub sender_name: Option<String>,
    pub from_me: bool,
    /// Unix seconds.
    pub timestamp: i64,
    pub content: Content,
    pub status: Delivery,
    /// First delivered-receipt Unix timestamp for outgoing messages.
    #[serde(default)]
    pub delivered_at: Option<i64>,
    /// First read or played receipt Unix timestamp.
    #[serde(default)]
    pub read_at: Option<i64>,
    pub quoted: Option<Quoted>,
    pub reactions: Vec<Reaction>,
    pub edited: bool,
    /// Mentions in the text or caption.
    #[serde(default)]
    pub mentions: Vec<MentionRef>,
    /// Forwarded from another chat.
    #[serde(default)]
    pub forwarded: bool,
    /// JPEG preview sent with an attachment or link.
    #[serde(default)]
    pub thumbnail: Option<Vec<u8>>,
}

/// Raw WhatsApp mention token and its canonical id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MentionRef {
    pub user: String,
    pub id: String,
}

/// Link metadata attached by WhatsApp.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkPreview {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
}

impl Message {
    /// One-line summary used in chat rows and quotes.
    pub fn summary(&self) -> String {
        self.content.summary()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quoted {
    pub id: String,
    pub sender: String,
    pub sender_name: Option<String>,
    pub summary: String,
    /// Mentions in quoted text.
    #[serde(default)]
    pub mentions: Vec<MentionRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reaction {
    pub sender: String,
    pub from_me: bool,
    pub emoji: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Content {
    Text {
        text: String,
        #[serde(default)]
        preview: Option<LinkPreview>,
    },
    Image {
        caption: Option<String>,
        media: Media,
    },
    Video {
        caption: Option<String>,
        media: Media,
        seconds: Option<u32>,
        gif: bool,
    },
    Audio {
        media: Media,
        seconds: Option<u32>,
        voice_note: bool,
        /// Sender-provided 64-bar voice waveform.
        #[serde(default)]
        waveform: Vec<u8>,
    },
    Document {
        media: Media,
        file_name: String,
        caption: Option<String>,
        pages: Option<u32>,
    },
    Sticker {
        media: Media,
        animated: bool,
    },
    Location {
        latitude: f64,
        longitude: f64,
        name: Option<String>,
        address: Option<String>,
    },
    Contact {
        display_name: String,
        vcard: String,
    },
    Poll {
        question: String,
        options: Vec<String>,
        #[serde(default)]
        state: PollState,
    },
    /// "This message was deleted."
    Revoked,
    /// Unsupported content with a user-facing description.
    Unsupported {
        what: String,
    },
}

/// Poll information safe to send to the interface; encryption keys stay in the worker.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PollState {
    pub selectable: usize,
    pub counts: Vec<usize>,
    pub selected: Vec<usize>,
    pub voters: usize,
    pub can_vote: bool,
    pub history_complete: bool,
    pub refresh_needed: bool,
    pub refreshing: bool,
    pub refresh_failed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PollDraft {
    pub question: String,
    pub options: Vec<String>,
    pub multiple: bool,
}

impl Default for PollDraft {
    fn default() -> Self {
        Self {
            question: String::new(),
            options: vec![String::new(); 2],
            multiple: true,
        }
    }
}

impl PollDraft {
    pub fn validated(&self) -> Result<Self, &'static str> {
        let question = self.question.trim().to_owned();
        let options: Vec<String> = self
            .options
            .iter()
            .map(|option| option.trim().to_owned())
            .collect();
        if question.is_empty() || question.chars().count() > 255 {
            return Err("Enter a question of up to 255 characters.");
        }
        if !(2..=12).contains(&options.len())
            || options
                .iter()
                .any(|option| option.is_empty() || option.chars().count() > 100)
        {
            return Err("Add 2–12 answers, each with 1–100 characters.");
        }
        let mut unique = std::collections::HashSet::new();
        if options.iter().any(|option| !unique.insert(option)) {
            return Err("Each answer must be different.");
        }
        Ok(Self {
            question,
            options,
            multiple: self.multiple,
        })
    }

    pub fn selectable(&self) -> usize {
        if self.multiple { self.options.len() } else { 1 }
    }
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            preview: None,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Text { text, .. } => text.lines().next().unwrap_or_default().to_owned(),
            Self::Image { caption, .. } => with_caption("Photo", caption),
            Self::Video { caption, gif, .. } => {
                with_caption(if *gif { "GIF" } else { "Video" }, caption)
            }
            Self::Audio {
                voice_note,
                seconds,
                ..
            } => {
                let label = if *voice_note {
                    "Voice message"
                } else {
                    "Audio"
                };
                match seconds {
                    Some(seconds) => format!("{label} ({})", crate::util::duration(*seconds)),
                    None => label.to_owned(),
                }
            }
            Self::Document { file_name, .. } => format!("Document: {file_name}"),
            Self::Sticker { .. } => "Sticker".to_owned(),
            Self::Location { name, .. } => match name {
                Some(name) => format!("Location: {name}"),
                None => "Location".to_owned(),
            },
            Self::Contact { display_name, .. } => format!("Contact: {display_name}"),
            Self::Poll { question, .. } => format!("Poll: {question}"),
            Self::Revoked => "This message was deleted".to_owned(),
            Self::Unsupported { what } => format!("Unsupported message ({what})"),
        }
    }

    pub fn media(&self) -> Option<&Media> {
        match self {
            Self::Image { media, .. }
            | Self::Video { media, .. }
            | Self::Audio { media, .. }
            | Self::Document { media, .. }
            | Self::Sticker { media, .. } => Some(media),
            _ => None,
        }
    }

    pub fn media_mut(&mut self) -> Option<&mut Media> {
        match self {
            Self::Image { media, .. }
            | Self::Video { media, .. }
            | Self::Audio { media, .. }
            | Self::Document { media, .. }
            | Self::Sticker { media, .. } => Some(media),
            _ => None,
        }
    }
}

fn with_caption(label: &str, caption: &Option<String>) -> String {
    match caption
        .as_deref()
        .and_then(|caption| caption.lines().next())
    {
        Some(caption) if !caption.is_empty() => format!("{label}: {caption}"),
        _ => label.to_owned(),
    }
}

/// Attachment metadata, download state, and optional local file. Download keys
/// remain in the archive's raw message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub mime: String,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Decrypted downloaded file.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Non-persisted download state.
    #[serde(skip)]
    pub state: MediaState,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum MediaState {
    #[default]
    Idle,
    Downloading,
    Failed(String),
}

/// Contact names from app-state sync and message push names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contact {
    pub id: String,
    pub full_name: Option<String>,
    pub push_name: Option<String>,
}

impl Contact {
    pub fn display_name(&self) -> Option<&str> {
        self.full_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(self.push_name.as_deref().filter(|name| !name.is_empty()))
    }

    /// WhatsApp display name: address-book name or `~`-prefixed push name.
    pub fn label(&self) -> Option<String> {
        if let Some(name) = self.full_name.as_deref().filter(|name| !name.is_empty()) {
            return Some(name.to_owned());
        }
        self.push_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .map(|name| format!("~{name}"))
    }
}

/// Imported sticker pack stored as a named WebP directory.
#[derive(Clone, Debug, PartialEq)]
pub struct StickerPack {
    pub name: String,
    pub dir: PathBuf,
    pub stickers: Vec<PathBuf>,
    /// Whether this pack was created locally (vs downloaded from WhatsApp).
    pub local: bool,
}

/// Actions queued by views and applied after drawing.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    CreatePoll {
        chat: ChatId,
        draft: PollDraft,
    },
    VotePoll {
        chat: ChatId,
        message: String,
        choices: Vec<usize>,
    },
    Download {
        chat: ChatId,
        message: String,
    },
    /// Plays or pauses a downloaded voice or audio message.
    PlayVoice {
        message: String,
        path: PathBuf,
    },
    /// Seeks to a fraction from 0 to 1 and starts playback.
    SeekVoice {
        message: String,
        path: PathBuf,
        fraction: f32,
    },
    /// Cycles voice playback speed between 1x, 1.5x, and 2x.
    CycleVoiceSpeed,
    /// Starts, cancels, or sends a voice recording.
    StartRecording,
    CancelRecording,
    SendRecording,
    OpenFile(PathBuf),
    /// Forwards an archived message to another chat.
    Forward {
        from_chat: ChatId,
        message: String,
        to_chat: ChatId,
    },
    /// Revokes an outgoing message for everyone.
    DeleteForEveryone {
        chat: ChatId,
        message: String,
    },
    /// Deletes a message locally.
    DeleteForMe {
        chat: ChatId,
        message: String,
    },
    React {
        chat: ChatId,
        message: String,
        emoji: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polls_validate_trimmed_questions_and_distinct_bounded_answers() {
        let mut draft = PollDraft {
            question: " Lunch? ".into(),
            options: vec![" Pizza ".into(), "Pasta".into()],
            multiple: false,
        };
        let valid = draft.validated().unwrap();
        assert_eq!(valid.question, "Lunch?");
        assert_eq!(valid.options, ["Pizza", "Pasta"]);
        assert_eq!(valid.selectable(), 1);
        draft.options[1] = "Pizza".into();
        assert!(draft.validated().is_err());
        draft.options[1].clear();
        assert!(draft.validated().is_err());
        draft.options = (0..13).map(|i| format!("Answer {i}")).collect();
        assert!(draft.validated().is_err());
        draft.options.pop();
        draft.multiple = true;
        assert_eq!(draft.validated().unwrap().selectable(), 12);
        draft.question = "🍕".repeat(256);
        assert!(draft.validated().is_err());
    }

    fn media() -> Media {
        Media {
            mime: "image/jpeg".into(),
            size: 1,
            width: None,
            height: None,
            path: None,
            state: MediaState::Idle,
        }
    }

    #[test]
    fn kinds_come_from_the_server_part() {
        assert_eq!(ChatKind::from_id("1@s.whatsapp.net"), ChatKind::Direct);
        assert_eq!(ChatKind::from_id("1@lid"), ChatKind::Direct);
        assert_eq!(ChatKind::from_id("1-2@g.us"), ChatKind::Group);
        assert_eq!(ChatKind::from_id("1@newsletter"), ChatKind::Broadcast);
    }

    #[test]
    fn summaries_read_like_whatsapp() {
        assert_eq!(Content::text("hi\nthere").summary(), "hi");
        assert_eq!(
            Content::Image {
                caption: Some("look".into()),
                media: media()
            }
            .summary(),
            "Photo: look"
        );
        assert_eq!(
            Content::Image {
                caption: None,
                media: media()
            }
            .summary(),
            "Photo"
        );
        assert_eq!(
            Content::Audio {
                media: media(),
                seconds: Some(65),
                voice_note: true,
                waveform: Vec::new()
            }
            .summary(),
            "Voice message (1:05)"
        );
    }

    #[test]
    fn phones_only_come_from_phone_ids() {
        assert_eq!(
            phone_of("393331234567@s.whatsapp.net"),
            Some("393331234567")
        );
        assert_eq!(phone_of("12345@lid"), None);
        assert_eq!(phone_of("1-2@g.us"), None);
    }

    #[test]
    fn labels_mark_names_people_chose_themselves() {
        let saved = Contact {
            id: "1".into(),
            full_name: Some("Ada".into()),
            push_name: Some("ada l".into()),
        };
        assert_eq!(saved.label().as_deref(), Some("Ada"));
        let stranger = Contact {
            id: "2".into(),
            full_name: None,
            push_name: Some("Bob".into()),
        };
        assert_eq!(stranger.label().as_deref(), Some("~Bob"));
        assert_eq!(Contact::default().label(), None);
    }

    #[test]
    fn old_text_content_still_parses() {
        let old: Content = serde_json::from_str(r#"{"kind":"text","text":"hi"}"#).expect("parses");
        assert_eq!(old, Content::text("hi"));
    }

    #[test]
    fn content_survives_json() {
        let content = Content::Document {
            media: media(),
            file_name: "a.pdf".into(),
            caption: None,
            pages: Some(3),
        };
        let json = serde_json::to_string(&content).expect("serializes");
        let back: Content = serde_json::from_str(&json).expect("parses");
        assert_eq!(back, content);
    }
}
