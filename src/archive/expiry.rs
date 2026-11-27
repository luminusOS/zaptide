//! Disappearing messages. Each message that carries a timer remembers when it
//! runs out, so changing the chat timer later never reaches back to older
//! messages. A message someone chose to keep never runs out.

use super::{Archive, Result};
use rusqlite::params;
use std::path::{Path, PathBuf};

pub const INDEX: &str = "
CREATE INDEX IF NOT EXISTS messages_expiring ON messages(expires_at)
    WHERE expires_at IS NOT NULL AND kept = 0;
CREATE TABLE IF NOT EXISTS expiry_media_cleanup (
    name TEXT PRIMARY KEY NOT NULL,
    identity TEXT NOT NULL,
    retry_after INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS expiry_media_by_retry ON expiry_media_cleanup(retry_after);
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
    #[cfg(test)]
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

    /// Delete the exact due row and record its file for cleanup in the same
    /// transaction. No file may be removed when the transaction fails.
    pub fn delete_expired_message(
        &self,
        message: &ExpiredMessage,
        now: i64,
        cleanup: Option<(&str, &str)>,
    ) -> Result<bool> {
        let transaction = self.connection.unchecked_transaction()?;
        let removed = transaction.execute(
            "DELETE FROM messages
             WHERE chat = ?1 AND id = ?2 AND expires_at <= ?3 AND kept = 0",
            params![message.chat, message.id, now],
        )? > 0;
        if removed && let Some((name, identity)) = cleanup {
            transaction.execute(
                "INSERT INTO expiry_media_cleanup (name, identity, retry_after) VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET
                    identity = excluded.identity, retry_after = excluded.retry_after",
                params![name, identity, now],
            )?;
        }
        transaction.commit()?;
        Ok(removed)
    }

    pub fn pending_expiry_media(&self, now: i64, limit: usize) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT name, identity FROM expiry_media_cleanup WHERE retry_after <= ?1
             ORDER BY retry_after, rowid LIMIT ?2",
        )?;
        let rows = statement.query_map(params![now, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect()
    }

    pub fn finish_expiry_media(&self, name: &str, retry_after: Option<i64>) -> Result<()> {
        match retry_after {
            Some(at) => self.connection.execute(
                "UPDATE expiry_media_cleanup SET retry_after = ?2 WHERE name = ?1",
                params![name, at],
            )?,
            None => self.connection.execute(
                "DELETE FROM expiry_media_cleanup WHERE name = ?1",
                params![name],
            )?,
        };
        Ok(())
    }

    pub fn expiry_media_has_exact_reference(
        &self,
        managed: &Path,
        resolved: &Path,
    ) -> Result<bool> {
        self.connection
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM messages WHERE media_path IN (?1, ?2))")?
            .query_row(params![managed.to_str(), resolved.to_str()], |row| {
                row.get(0)
            })
    }

    pub fn visit_expiry_media_references(
        &self,
        mut visit: impl FnMut(PathBuf) -> std::io::Result<bool>,
    ) -> anyhow::Result<()> {
        let mut statement = self.connection.prepare_cached(
            "SELECT DISTINCT media_path FROM messages WHERE media_path IS NOT NULL",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0).map(PathBuf::from))?;
        for row in rows {
            if !visit(row?)? {
                break;
            }
        }
        Ok(())
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
