//! Toolkit-neutral presentation data for image, video, audio, and document
//! attachments.
//!
//! Renderers consume [`AttachmentPresentation`] and translate
//! [`AttachmentIntent`] into the existing application actions.

use std::path::PathBuf;
use std::{fmt, fmt::Formatter};

use crate::model::{Action, Content, Media, MediaState, Message};

/// Attachment category presented to a native renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    Document,
}

/// Main affordance for an attachment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentControl {
    Download,
    Downloading,
    Retry,
    Open,
}

/// Event emitted by an attachment control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentIntent {
    Activate,
}

/// Data needed to render an attachment without depending on a UI toolkit.
///
/// This projection intentionally contains no filesystem path and never copies
/// backend failure text, which may contain a local path.
#[derive(Clone, Eq, PartialEq)]
pub struct AttachmentPresentation {
    pub kind: AttachmentKind,
    pub control: AttachmentControl,
    pub title: String,
    /// Safe, user-facing metadata such as duration, dimensions, or size.
    pub detail: String,
    chat: String,
    message: String,
    path: Option<PathBuf>,
}

impl fmt::Debug for AttachmentPresentation {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentPresentation")
            .field("kind", &self.kind)
            .field("control", &self.control)
            .field("title", &self.title)
            .field("detail", &self.detail)
            .finish_non_exhaustive()
    }
}

impl AttachmentPresentation {
    /// Translates activation into the existing open or download action.
    pub fn action(&self, intent: AttachmentIntent) -> Option<Action> {
        match (intent, self.control, &self.path) {
            (AttachmentIntent::Activate, AttachmentControl::Open, Some(path)) => {
                Some(Action::OpenFile(path.clone()))
            }
            (
                AttachmentIntent::Activate,
                AttachmentControl::Download | AttachmentControl::Retry,
                None,
            ) => Some(Action::Download {
                chat: self.chat.clone(),
                message: self.message.clone(),
            }),
            _ => None,
        }
    }
}

/// Projects supported attachment content into renderer-facing presentation.
/// Unsupported message content returns `None`.
pub fn project(message: &Message) -> Option<AttachmentPresentation> {
    let (kind, media, title, detail) = match &message.content {
        Content::Image { media, .. } => (
            AttachmentKind::Image,
            media,
            "Photo".to_owned(),
            dimensions(media).unwrap_or_else(|| crate::util::bytes(media.size)),
        ),
        Content::Video {
            media,
            seconds,
            gif,
            ..
        } => {
            let mut details = Vec::new();
            if let Some(seconds) = seconds {
                details.push(crate::util::duration(*seconds));
            }
            details.push(crate::util::bytes(media.size));
            (
                AttachmentKind::Video,
                media,
                if *gif { "GIF" } else { "Video" }.to_owned(),
                details.join(" · "),
            )
        }
        Content::Audio {
            media,
            seconds,
            voice_note,
            ..
        } => {
            let title = if *voice_note {
                "Voice message"
            } else {
                "Audio"
            };
            let detail = seconds
                .map(crate::util::duration)
                .unwrap_or_else(|| crate::util::bytes(media.size));
            (AttachmentKind::Audio, media, title.to_owned(), detail)
        }
        Content::Document {
            media,
            file_name,
            pages,
            ..
        } => {
            let title = safe_file_name(file_name);
            let mut details = vec![crate::util::bytes(media.size)];
            if let Some(pages) = pages {
                details.push(format!("{pages} pages"));
            }
            (AttachmentKind::Document, media, title, details.join(" · "))
        }
        _ => return None,
    };

    let control = if media.path.is_some() {
        AttachmentControl::Open
    } else {
        match media.state {
            MediaState::Downloading => AttachmentControl::Downloading,
            MediaState::Failed(_) => AttachmentControl::Retry,
            MediaState::Idle => AttachmentControl::Download,
        }
    };

    Some(AttachmentPresentation {
        kind,
        control,
        title,
        detail,
        chat: message.chat.clone(),
        message: message.id.clone(),
        path: media.path.clone(),
    })
}

fn dimensions(media: &Media) -> Option<String> {
    Some(format!("{} × {}", media.width?, media.height?))
}

/// Keeps document labels to a filename component, even for Windows-style paths.
fn safe_file_name(file_name: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn message(content: Content) -> Message {
        Message {
            id: "message-id".into(),
            chat: "chat-id".into(),
            sender: "sender-id".into(),
            sender_name: None,
            from_me: false,
            timestamp: 0,
            content,
            status: Default::default(),
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

    fn media(path: Option<&str>, state: MediaState) -> Media {
        Media {
            mime: "application/octet-stream".into(),
            size: 1_024,
            width: Some(640),
            height: Some(480),
            path: path.map(PathBuf::from),
            state,
        }
    }

    #[test]
    fn projects_image_video_audio_and_document_labels() {
        let image = project(&message(Content::Image {
            media: media(None, MediaState::Idle),
            caption: Some("caption ignored in control label".into()),
        }))
        .unwrap();
        assert_eq!(image.kind, AttachmentKind::Image);
        assert_eq!(image.title, "Photo");
        assert_eq!(image.detail, "640 × 480");

        let video = project(&message(Content::Video {
            media: media(None, MediaState::Idle),
            seconds: Some(65),
            gif: true,
            caption: None,
        }))
        .unwrap();
        assert_eq!(video.kind, AttachmentKind::Video);
        assert_eq!(video.title, "GIF");
        assert_eq!(video.detail, "1:05 · 1.0 KB");

        let audio = project(&message(Content::Audio {
            media: media(None, MediaState::Idle),
            seconds: Some(30),
            voice_note: true,
            waveform: Vec::new(),
        }))
        .unwrap();
        assert_eq!(audio.kind, AttachmentKind::Audio);
        assert_eq!(audio.title, "Voice message");
        assert_eq!(audio.detail, "0:30");

        let document = project(&message(Content::Document {
            media: media(None, MediaState::Idle),
            file_name: r"C:\private\report.pdf".into(),
            caption: None,
            pages: Some(3),
        }))
        .unwrap();
        assert_eq!(document.kind, AttachmentKind::Document);
        assert_eq!(document.title, "report.pdf");
        assert_eq!(document.detail, "1.0 KB · 3 pages");
        assert!(!format!("{document:?}").contains("private"));
    }

    #[test]
    fn download_state_projects_download_loading_retry_and_open_actions() {
        let idle = project(&message(Content::Image {
            media: media(None, MediaState::Idle),
            caption: None,
        }))
        .unwrap();
        assert_eq!(idle.control, AttachmentControl::Download);
        assert_eq!(
            idle.action(AttachmentIntent::Activate),
            Some(Action::Download {
                chat: "chat-id".into(),
                message: "message-id".into(),
            })
        );

        let downloading = project(&message(Content::Video {
            media: media(None, MediaState::Downloading),
            seconds: None,
            gif: false,
            caption: None,
        }))
        .unwrap();
        assert_eq!(downloading.control, AttachmentControl::Downloading);
        assert_eq!(downloading.action(AttachmentIntent::Activate), None);

        let failed = project(&message(Content::Audio {
            media: media(None, MediaState::Failed("/private/path".into())),
            seconds: None,
            voice_note: false,
            waveform: Vec::new(),
        }))
        .unwrap();
        assert_eq!(failed.control, AttachmentControl::Retry);
        assert!(!format!("{failed:?}").contains("/private/path"));
        assert!(matches!(
            failed.action(AttachmentIntent::Activate),
            Some(Action::Download { .. })
        ));

        let ready = project(&message(Content::Document {
            media: media(Some("/private/report.pdf"), MediaState::Idle),
            file_name: "report.pdf".into(),
            caption: None,
            pages: None,
        }))
        .unwrap();
        assert_eq!(ready.control, AttachmentControl::Open);
        assert_eq!(
            ready.action(AttachmentIntent::Activate),
            Some(Action::OpenFile(PathBuf::from("/private/report.pdf")))
        );
        assert!(!format!("{ready:?}").contains("/private/report.pdf"));
    }

    #[test]
    fn ignores_unsupported_content_and_path_like_document_names() {
        assert!(project(&message(Content::text("text"))).is_none());
        for name in ["/", r"C:\", ".."] {
            let attachment = project(&message(Content::Document {
                media: media(None, MediaState::Idle),
                file_name: name.into(),
                caption: None,
                pages: None,
            }))
            .unwrap();
            assert_eq!(attachment.title, "Document");
        }
    }
}
