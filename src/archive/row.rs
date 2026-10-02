use crate::model::{Chat, Content, LastMessage, Message};

use super::{kind_from_name, status_from_rank};

fn content_from_json(json: &str) -> Content {
    serde_json::from_str(json).unwrap_or(Content::Unsupported {
        what: "unreadable".into(),
    })
}

pub(super) fn message_from_row(chat: &str, row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let content: String = row.get(5)?;
    let quoted: Option<String> = row.get(7)?;
    let reactions: String = row.get(8)?;
    let mentions: String = row.get(11)?;
    Ok(Message {
        id: row.get(0)?,
        chat: chat.to_owned(),
        sender: row.get(1)?,
        sender_name: row.get(2)?,
        from_me: row.get(3)?,
        timestamp: row.get(4)?,
        content: content_from_json(&content),
        status: status_from_rank(row.get(6)?),
        delivered_at: row.get(13)?,
        read_at: row.get(14)?,
        quoted: quoted.and_then(|quoted| serde_json::from_str(&quoted).ok()),
        reactions: serde_json::from_str(&reactions).unwrap_or_default(),
        edited: row.get(9)?,
        mentions: serde_json::from_str(&mentions).unwrap_or_default(),
        forwarded: row.get(12)?,
        thumbnail: row.get(10)?,
    })
}

pub(super) fn chat_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Chat> {
    let content: Option<String> = row.get(10)?;
    let last = match content {
        Some(content) => {
            let content = content_from_json(&content);
            Some(LastMessage {
                from_me: row.get(8)?,
                sender: row.get::<_, Option<String>>(12)?.unwrap_or_default(),
                sender_name: row.get(9)?,
                summary: content.summary(),
                status: status_from_rank(row.get(11)?),
            })
        }
        None => None,
    };
    let kind: String = row.get(2)?;
    let participants: String = row.get(13)?;
    Ok(Chat {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: kind_from_name(&kind),
        last_activity: row.get(3)?,
        unread: row.get(4)?,
        archived: row.get(5)?,
        pinned: row.get(6)?,
        pinned_at: row.get(15)?,
        muted_until: row.get(7)?,
        locked: row.get(17)?,
        last,
        participants: serde_json::from_str(&participants).unwrap_or_default(),
        read_only: row.get(14)?,
        ephemeral_expiration: row
            .get::<_, Option<u32>>(16)?
            .filter(|expiration| *expiration != 0),
    })
}
