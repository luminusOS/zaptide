//! Disappearing messages. Each message that carries a timer remembers when it
//! runs out, so changing the chat timer later never reaches back to older
//! messages. A message someone chose to keep never runs out.

use super::{Archive, Result};
use rusqlite::params;
use std::path::PathBuf;

pub const INDEX: &str = "
CREATE INDEX IF NOT EXISTS messages_expiring ON messages(expires_at)
    WHERE expires_at IS NOT NULL AND kept = 0;
";

/// A message whose time has run out, with its downloaded file if it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpiredMessage {
    pub chat: String,
    pub id: String,
    pub media_path: Option<PathBuf>,
}

impl Archive {
    /// Sets or clears when a message disappears, in seconds. A kept message is
    /// left alone. Returns whether anything changed.
    pub fn set_expires_at(&self, chat: &str, id: &str, expires_at: Option<i64>) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE messages SET expires_at = ?3
             WHERE chat = ?1 AND id = ?2 AND kept = 0 AND expires_at IS NOT ?3",
            params![chat, id, expires_at],
        )? > 0)
    }

    /// Keeps a message in a disappearing chat, or lets it run out again.
    pub fn set_kept(&self, chat: &str, id: &str, kept: bool) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE messages SET kept = ?3 WHERE chat = ?1 AND id = ?2 AND kept != ?3",
            params![chat, id, kept],
        )? > 0)
    }

    /// Ids of the kept messages of one chat.
    pub fn kept_ids(&self, chat: &str) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM messages WHERE chat = ?1 AND kept = 1")?;
        statement
            .query_map(params![chat], |row| row.get(0))?
            .collect()
    }

    // Exercised directly by the round-trip test below; no caller needs it
    // outside tests yet.
    #[allow(dead_code)]
    pub fn is_kept(&self, chat: &str, id: &str) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT kept FROM messages WHERE chat = ?1 AND id = ?2",
                params![chat, id],
                |row| row.get(0),
            )
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(other),
            })
    }

    /// The oldest messages whose time has run out, up to `limit`.
    pub fn expired_messages(&self, now: i64, limit: usize) -> Result<Vec<ExpiredMessage>> {
        let mut statement = self.connection.prepare(
            "SELECT chat, id, media_path FROM messages
             WHERE expires_at IS NOT NULL AND expires_at <= ?1 AND kept = 0
             ORDER BY expires_at LIMIT ?2",
        )?;
        let rows = statement.query_map(params![now, limit as i64], |row| {
            Ok(ExpiredMessage {
                chat: row.get(0)?,
                id: row.get(1)?,
                media_path: row.get::<_, Option<String>>(2)?.map(PathBuf::from),
            })
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Chat, Content, Delivery, Message};

    const CHAT: &str = "chat@s.whatsapp.net";

    fn archive_with(ids: &[&str]) -> Archive {
        let archive = Archive::in_memory().unwrap();
        archive
            .upsert_chat(&Chat::new(CHAT.into(), "Chat".into()))
            .unwrap();
        for (index, id) in ids.iter().enumerate() {
            archive
                .insert_message(
                    &Message {
                        id: (*id).into(),
                        chat: CHAT.into(),
                        sender: CHAT.into(),
                        sender_name: None,
                        from_me: false,
                        timestamp: index as i64 + 1,
                        content: Content::text("synthetic"),
                        status: Delivery::None,
                        quoted: None,
                        reactions: Vec::new(),
                        edited: false,
                        thumbnail: None,
                        mentions: Vec::new(),
                        forwarded: false,
                        delivered_at: None,
                        read_at: None,
                    },
                    None,
                )
                .unwrap();
        }
        archive
    }

    fn due(archive: &Archive, now: i64) -> Vec<String> {
        archive
            .expired_messages(now, 100)
            .unwrap()
            .into_iter()
            .map(|message| message.id)
            .collect()
    }

    #[test]
    fn only_messages_with_a_timer_that_ran_out_are_due() {
        let archive = archive_with(&["plain", "soon", "later"]);
        archive.set_expires_at(CHAT, "soon", Some(100)).unwrap();
        archive.set_expires_at(CHAT, "later", Some(500)).unwrap();
        assert!(due(&archive, 99).is_empty());
        assert_eq!(due(&archive, 100), ["soon"]);
        assert_eq!(due(&archive, 1_000), ["soon", "later"]);
    }

    #[test]
    fn a_kept_message_never_runs_out_and_resumes_when_unkept() {
        let archive = archive_with(&["a"]);
        archive.set_expires_at(CHAT, "a", Some(100)).unwrap();
        assert!(archive.set_kept(CHAT, "a", true).unwrap());
        assert!(archive.is_kept(CHAT, "a").unwrap());
        assert!(due(&archive, 1_000).is_empty());
        // A replay of the message cannot move a kept message's time.
        assert!(!archive.set_expires_at(CHAT, "a", Some(5)).unwrap());
        assert!(archive.set_kept(CHAT, "a", false).unwrap());
        assert_eq!(due(&archive, 1_000), ["a"]);
    }

    #[test]
    fn setting_the_same_time_again_changes_nothing() {
        let archive = archive_with(&["a"]);
        assert!(archive.set_expires_at(CHAT, "a", Some(100)).unwrap());
        assert!(!archive.set_expires_at(CHAT, "a", Some(100)).unwrap());
    }

    #[test]
    fn deleting_the_expired_message_leaves_nothing_due() {
        let archive = archive_with(&["a"]);
        archive.set_expires_at(CHAT, "a", Some(1)).unwrap();
        archive.delete_message(CHAT, "a").unwrap();
        assert!(due(&archive, 10).is_empty());
        assert!(!archive.is_kept(CHAT, "a").unwrap());
    }
}
