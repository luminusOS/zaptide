//! Disappearing messages. Each message that carries a timer remembers when it
//! runs out, so changing the chat timer later never reaches back to older
//! messages. A message someone chose to keep never runs out.

use super::{Archive, Result};
use rusqlite::{OptionalExtension, params};
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
CREATE TABLE IF NOT EXISTS message_keeps (
    chat TEXT NOT NULL,
    id TEXT NOT NULL,
    kept INTEGER NOT NULL,
    at_ms INTEGER NOT NULL,
    PRIMARY KEY (chat, id)
);
CREATE INDEX IF NOT EXISTS message_keeps_by_time ON message_keeps(at_ms);
CREATE TRIGGER IF NOT EXISTS forget_chat_keeps AFTER DELETE ON chats
BEGIN
    DELETE FROM message_keeps WHERE chat = OLD.id;
END;
";

const PENDING_KEEPS: i64 = 2048;

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

    /// Applies an ordered Keep change, even if its target has not arrived.
    /// Returns the target deadline if a timed row changed.
    pub fn set_kept(&self, chat: &str, id: &str, kept: bool, at_ms: i64) -> Result<Option<i64>> {
        self.atomically(|archive| {
            let won = archive.connection.execute(
                "INSERT INTO message_keeps (chat, id, kept, at_ms) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(chat, id) DO UPDATE SET kept = excluded.kept, at_ms = excluded.at_ms
                 WHERE excluded.at_ms > message_keeps.at_ms
                    OR (excluded.at_ms = message_keeps.at_ms AND excluded.kept < message_keeps.kept)",
                params![chat, id, kept, at_ms],
            )? != 0;
            if won {
                archive
                    .connection
                    .query_row(
                        "UPDATE messages SET kept = ?3
                         WHERE chat = ?1 AND id = ?2 AND expires_at IS NOT NULL AND kept != ?3
                         RETURNING expires_at",
                        params![chat, id, kept],
                        |row| row.get(0),
                    )
                    .optional()
            } else {
                Ok(None)
            }
        })
    }

    pub(super) fn merge_keeps(&self, alias: &str, canonical: &str) -> Result<bool> {
        let moved = self.connection.execute(
            "INSERT INTO message_keeps (chat, id, kept, at_ms)
             SELECT ?2, id, kept, at_ms FROM message_keeps WHERE chat = ?1
             ON CONFLICT(chat, id) DO UPDATE SET kept = excluded.kept, at_ms = excluded.at_ms
             WHERE excluded.at_ms > message_keeps.at_ms
                OR (excluded.at_ms = message_keeps.at_ms AND excluded.kept < message_keeps.kept)",
            params![alias, canonical],
        )?;
        let changed = self.connection.execute(
            "UPDATE messages SET kept = k.kept FROM message_keeps k
             WHERE messages.chat = ?1 AND k.chat = ?1 AND k.id = messages.id
               AND expires_at IS NOT NULL AND messages.kept != k.kept",
            [canonical],
        )?;
        self.connection
            .execute("DELETE FROM message_keeps WHERE chat = ?1", [alias])?;
        Ok(moved > 0 || changed > 0)
    }

    /// Bounds state for targets with no timed row, including expired targets.
    pub fn prune_pending_keeps(&self) -> Result<()> {
        self.connection.execute(
            "DELETE FROM message_keeps WHERE rowid IN (
                SELECT k.rowid FROM message_keeps k
                WHERE NOT EXISTS (
                    SELECT 1 FROM messages m WHERE m.chat = k.chat AND m.id = k.id
                      AND m.expires_at IS NOT NULL
                )
                ORDER BY k.at_ms DESC, k.rowid DESC LIMIT -1 OFFSET ?1
            )",
            [PENDING_KEEPS],
        )?;
        Ok(())
    }

    /// The original deadline and kept state of a timed message.
    pub fn keep_state(&self, chat: &str, id: &str) -> Result<Option<(i64, bool)>> {
        self.connection
            .prepare_cached(
                "SELECT expires_at, kept FROM messages
                 WHERE chat = ?1 AND id = ?2 AND expires_at IS NOT NULL",
            )?
            .query_row(params![chat, id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
    }

    /// Keep state for only the messages on the current page or live update.
    pub(crate) fn keep_states(
        &self,
        chat: &str,
        messages: &[crate::model::Message],
    ) -> Result<Vec<(String, i64, bool)>> {
        let ids: Vec<_> = messages.iter().map(|message| &message.id).collect();
        let ids = serde_json::to_string(&ids)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        self.connection
            .prepare_cached(
                "SELECT m.id, m.expires_at, m.kept FROM json_each(?2) AS requested
                 JOIN messages m ON m.chat = ?1 AND m.id = requested.value
                 WHERE m.expires_at IS NOT NULL",
            )?
            .query_map(params![chat, ids], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect()
    }

    #[cfg(test)]
    pub fn is_kept(&self, chat: &str, id: &str) -> Result<bool> {
        Ok(self.keep_state(chat, id)?.is_some_and(|(_, kept)| kept))
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
        assert_eq!(archive.set_kept(CHAT, "a", true, 1).unwrap(), Some(100));
        assert!(archive.is_kept(CHAT, "a").unwrap());
        assert!(due(&archive, 1_000).is_empty());
        // A replay of the message cannot move a kept message's time.
        assert!(!archive.set_expires_at(CHAT, "a", Some(5)).unwrap());
        assert_eq!(archive.set_kept(CHAT, "a", false, 2).unwrap(), Some(100));
        assert_eq!(due(&archive, 1_000), ["a"]);
    }

    #[test]
    fn pending_keep_pruning_keeps_the_newest_changes_and_known_targets() {
        let archive = archive_with(&["known"]);
        archive.set_expires_at(CHAT, "known", Some(100)).unwrap();
        archive.set_kept(CHAT, "known", true, 1).unwrap();
        archive
            .connection
            .execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i <= ?2)
                 INSERT INTO message_keeps SELECT ?1, CAST(i AS TEXT), 1, i FROM n",
                params![CHAT, PENDING_KEEPS],
            )
            .unwrap();

        archive.prune_pending_keeps().unwrap();

        let (count, oldest_pending): (i64, i64) = archive
            .connection
            .query_row(
                "SELECT COUNT(*), MIN(at_ms) FROM message_keeps WHERE id != 'known'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((count, oldest_pending), (PENDING_KEEPS, 2));
        assert!(archive.is_kept(CHAT, "known").unwrap());
    }

    #[test]
    fn privacy_id_merge_preserves_the_newer_pending_unkeep() {
        const ALIAS: &str = "123456@lid";
        let archive = archive_with(&["target"]);
        archive.set_expires_at(CHAT, "target", Some(100)).unwrap();
        archive.set_kept(CHAT, "target", true, 2).unwrap();
        archive.set_kept(ALIAS, "target", false, 3).unwrap();

        assert!(archive.put_lid("123456", "chat").unwrap());

        assert_eq!(
            archive.keep_state(CHAT, "target").unwrap(),
            Some((100, false))
        );
        assert_eq!(archive.set_kept(CHAT, "target", true, 2).unwrap(), None);
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
