use super::{Content, Message, gettext};

/// Who sent a photo or video, when, its caption, and a one-line summary of
/// the file, for the viewer.
#[derive(Clone)]
pub(in crate::native_media_widgets) struct ViewerDetails {
    pub(super) title: String,
    pub(super) subtitle: String,
    pub(super) caption: Option<String>,
    pub(super) thumbnail: Option<Vec<u8>>,
    pub(super) metadata: String,
    pub(super) dimensions: Option<(u32, u32)>,
}

impl ViewerDetails {
    pub(in crate::native_media_widgets) fn of(message: &Message) -> Self {
        let title = if message.from_me {
            gettext("You")
        } else {
            message
                .sender_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| {
                    crate::model::phone_of(&message.sender)
                        .map(crate::util::phone)
                        .unwrap_or_default()
                })
        };
        let (caption, metadata, dimensions) = match &message.content {
            Content::Image { caption, media } => (
                caption.clone(),
                metadata_line(&media.mime, media.width, media.height, None, media.size),
                media.width.zip(media.height),
            ),
            Content::Video {
                caption,
                media,
                seconds,
                ..
            } => (
                caption.clone(),
                metadata_line(&media.mime, media.width, media.height, *seconds, media.size),
                None,
            ),
            _ => (None, String::new(), None),
        };
        Self {
            title,
            subtitle: crate::util::clock(message.timestamp),
            caption: caption.filter(|caption| !caption.is_empty()),
            thumbnail: message.thumbnail.clone(),
            metadata,
            dimensions,
        }
    }
}

fn format_name(mime: &str) -> Option<String> {
    let subtype = mime.split_once('/')?.1;
    let subtype = subtype.split([';', '+']).next()?.trim();
    let subtype = subtype.strip_prefix("x-").unwrap_or(subtype);
    let name = match subtype {
        "" => return None,
        "quicktime" => "MOV",
        "matroska" => "MKV",
        other => return Some(other.to_ascii_uppercase()),
    };
    Some(name.to_owned())
}

fn dimensions_text(width: Option<u32>, height: Option<u32>) -> Option<String> {
    match (width, height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => {
            Some(format!("{width} × {height}"))
        }
        _ => None,
    }
}

fn file_size_text(size: u64) -> Option<String> {
    (size > 0).then(|| crate::util::bytes(size))
}

fn metadata_line(
    mime: &str,
    width: Option<u32>,
    height: Option<u32>,
    seconds: Option<u32>,
    size: u64,
) -> String {
    [
        format_name(mime),
        dimensions_text(width, height),
        seconds.map(crate::util::duration),
        file_size_text(size),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_the_prototype() {
        assert_eq!(
            metadata_line("image/png", Some(1600), Some(1000), None, 184 * 1024),
            "PNG · 1600 × 1000 · 188.4\u{a0}kB"
        );
        assert_eq!(
            metadata_line("video/mp4", Some(1280), Some(720), Some(3), 1_258_291),
            "MP4 · 1280 × 720 · 0:03 · 1.3\u{a0}MB"
        );
    }

    #[test]
    fn metadata_skips_what_is_unknown() {
        assert_eq!(metadata_line("image/jpeg", None, Some(10), None, 0), "JPEG");
        assert_eq!(metadata_line("", Some(0), Some(0), None, 12), "12 bytes");
        assert_eq!(metadata_line("garbage", None, None, None, 0), "");
    }

    #[test]
    fn format_names_follow_the_extension_people_know() {
        for (mime, name) in [
            ("image/jpeg", "JPEG"),
            ("image/svg+xml", "SVG"),
            ("video/quicktime", "MOV"),
            ("video/x-matroska", "MKV"),
            ("video/webm; codecs=vp8", "WEBM"),
        ] {
            assert_eq!(format_name(mime).as_deref(), Some(name), "{mime}");
        }
        assert_eq!(format_name("image/"), None);
    }

    #[test]
    fn file_sizes_round_to_the_nearest_unit() {
        assert_eq!(file_size_text(0), None);
        assert_eq!(file_size_text(1023).as_deref(), Some("1.0\u{a0}kB"));
        assert_eq!(file_size_text(1024).as_deref(), Some("1.0\u{a0}kB"));
        assert_eq!(
            file_size_text(1024 * 1024 - 1).as_deref(),
            Some("1.0\u{a0}MB")
        );
        assert_eq!(
            file_size_text(5 * 1024 * 1024).as_deref(),
            Some("5.2\u{a0}MB")
        );
        assert_eq!(file_size_text(3 << 30).as_deref(), Some("3.2\u{a0}GB"));
    }
}
