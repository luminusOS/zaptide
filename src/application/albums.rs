//! Consecutive photos can render as one album while retaining individual rows.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AlbumRole {
    Single,
    /// The first photo; draws the whole album of this many photos.
    Leader(usize),
    Follower,
}

/// Groups runs of photos from one sender, sent close together, into albums.
/// `separated[i]` marks a day or unread divider above message `i`, which
/// ends a run. A caption or quote on a later photo also ends it.
pub(super) fn album_roles(
    messages: &[crate::model::Message],
    separated: &[bool],
) -> Vec<AlbumRole> {
    use crate::model::Content;
    const MAX_PHOTOS: usize = 10;
    const GAP_SECONDS: i64 = 2 * 60;
    let photo = |message: &crate::model::Message| {
        matches!(message.content, Content::Image { .. }) && message.quoted.is_none()
    };
    let captioned = |message: &crate::model::Message| matches!(&message.content, Content::Image { caption: Some(text), .. } if !text.is_empty());
    let joins = |previous: &crate::model::Message, next: &crate::model::Message, index: usize| {
        photo(next)
            && !captioned(next)
            && !separated[index]
            && previous.from_me == next.from_me
            && previous.sender == next.sender
            && next.timestamp >= previous.timestamp
            && next.timestamp - previous.timestamp <= GAP_SECONDS
    };
    let mut roles = vec![AlbumRole::Single; messages.len()];
    let mut start = 0;
    while start < messages.len() {
        let mut end = start + 1;
        if photo(&messages[start]) {
            while end < messages.len()
                && end - start < MAX_PHOTOS
                && joins(&messages[end - 1], &messages[end], end)
            {
                end += 1;
            }
        }
        if end - start >= 2 {
            roles[start] = AlbumRole::Leader(end - start);
            roles[start + 1..end].fill(AlbumRole::Follower);
        }
        start = end;
    }
    roles
}
