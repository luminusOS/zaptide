//! Pinned messages. A pin is stored apart from its message, like a star,
//! because the pin can arrive before the history that holds the message, and
//! an unpin is kept as a marker so the order messages arrive in cannot undo a
//! later change.

use std::collections::HashSet;

use super::{Archive, Result};
use crate::model::{Content, Message};
use rusqlite::params;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pins (
    chat TEXT NOT NULL, id TEXT NOT NULL, pinned INTEGER NOT NULL,
    pinned_by TEXT NOT NULL DEFAULT '', pinned_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER,
    PRIMARY KEY (chat, id)
);
CREATE TRIGGER IF NOT EXISTS delete_pins AFTER DELETE ON messages BEGIN
    DELETE FROM pins WHERE chat = OLD.chat AND id = OLD.id;
END;
";

/// A message pinned in its chat, with who pinned it and until when.
#[derive(Clone, Debug, PartialEq)]
pub struct PinnedMessage {
    pub message: Message,
    pub pinned_by: String,
    pub pinned_at_ms: i64,
    pub expires_at_ms: Option<i64>,
}

impl Archive {
    /// Records the newest change for a message, preferring an unpin on a tie.
    /// Returns whether the stored state changed.
    pub fn set_pin(
        &self,
        chat: &str,
        id: &str,
        pinned: bool,
        pinned_by: &str,
        at_ms: i64,
        expires_at_ms: Option<i64>,
    ) -> Result<bool> {
        let changed = self.connection.execute(
            "INSERT INTO pins (chat, id, pinned, pinned_by, pinned_at_ms, expires_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (chat, id) DO UPDATE SET pinned = excluded.pinned,
                 pinned_by = excluded.pinned_by, pinned_at_ms = excluded.pinned_at_ms,
                 expires_at_ms = excluded.expires_at_ms
             WHERE excluded.pinned_at_ms > pins.pinned_at_ms
                OR (excluded.pinned_at_ms = pins.pinned_at_ms AND excluded.pinned < pins.pinned)",
            params![chat, id, pinned, pinned_by, at_ms, expires_at_ms],
        )?;
        Ok(changed > 0)
    }

    /// Unexpired pin targets, including messages that have not arrived yet.
    pub(crate) fn active_pin_targets(&self, chat: &str, now_ms: i64) -> Result<HashSet<String>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id FROM pins
             WHERE chat = ?1 AND pinned = 1 AND (expires_at_ms IS NULL OR expires_at_ms > ?2)",
        )?;
        statement
            .query_map(params![chat, now_ms], |row| row.get(0))?
            .collect()
    }

    /// The message a chat shows in its pinned bar: the newest pin that has not
    /// run out and whose message is still here and not deleted.
    pub fn pinned_message(&self, chat: &str, now_ms: i64) -> Result<Option<PinnedMessage>> {
        let mut statement = self.connection.prepare(
            "SELECT id, pinned_by, pinned_at_ms, expires_at_ms FROM pins
             WHERE chat = ?1 AND pinned = 1 AND (expires_at_ms IS NULL OR expires_at_ms > ?2)
             ORDER BY pinned_at_ms DESC",
        )?;
        let pins = statement
            .query_map(params![chat, now_ms], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>>>()?;
        for (id, pinned_by, pinned_at_ms, expires_at_ms) in pins {
            let Some(message) = self.message(chat, &id)? else {
                continue;
            };
            if matches!(message.content, Content::Revoked) {
                continue;
            }
            return Ok(Some(PinnedMessage {
                message,
                pinned_by,
                pinned_at_ms,
                expires_at_ms,
            }));
        }
        Ok(None)
    }

    /// Turns off the pins whose time has run out and returns the chats that
    /// had one, so their pinned bar can be refreshed.
    pub fn expire_pins(&self, now_ms: i64) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT chat FROM pins
             WHERE pinned = 1 AND expires_at_ms IS NOT NULL AND expires_at_ms <= ?1",
        )?;
        let chats = statement
            .query_map(params![now_ms], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>>>()?;
        if !chats.is_empty() {
            self.connection.execute(
                "UPDATE pins SET pinned = 0
                 WHERE pinned = 1 AND expires_at_ms IS NOT NULL AND expires_at_ms <= ?1",
                params![now_ms],
            )?;
        }
        Ok(chats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Chat, Delivery};

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

    fn pinned_id(archive: &Archive, now_ms: i64) -> Option<String> {
        archive
            .pinned_message(CHAT, now_ms)
            .unwrap()
            .map(|pin| pin.message.id)
    }

    #[test]
    fn the_newest_pin_is_the_one_shown() {
        let archive = archive_with(&["a", "b"]);
        assert!(archive.set_pin(CHAT, "a", true, "x", 10, None).unwrap());
        assert!(archive.set_pin(CHAT, "b", true, "x", 20, None).unwrap());
        assert_eq!(pinned_id(&archive, 30).as_deref(), Some("b"));
        assert!(archive.set_pin(CHAT, "b", false, "x", 25, None).unwrap());
        assert_eq!(pinned_id(&archive, 30).as_deref(), Some("a"));
    }

    #[test]
    fn an_older_change_cannot_undo_a_newer_one() {
        let archive = archive_with(&["a"]);
        assert!(archive.set_pin(CHAT, "a", false, "x", 20, None).unwrap());
        // The pin that came before the unpin, replayed from history.
        assert!(!archive.set_pin(CHAT, "a", true, "x", 10, None).unwrap());
        assert_eq!(pinned_id(&archive, 30), None);
        assert!(!archive.set_pin(CHAT, "a", false, "x", 20, None).unwrap());
    }

    #[test]
    fn pin_state_equal_time_unpin_wins_in_both_replay_orders() {
        for first in [true, false] {
            let archive = archive_with(&["a"]);
            assert!(archive.set_pin(CHAT, "a", first, "x", 10, None).unwrap());
            assert_eq!(
                archive.set_pin(CHAT, "a", !first, "y", 10, None).unwrap(),
                first
            );
            assert_eq!(pinned_id(&archive, 20), None);
        }
    }

    #[test]
    fn pin_state_equal_time_unpin_wins_on_either_side_of_identity_merge() {
        const LID: &str = "111@lid";
        const PHONE: &str = "222@s.whatsapp.net";
        for lid_pinned in [true, false] {
            let archive = Archive::in_memory().unwrap();
            for chat in [LID, PHONE] {
                archive
                    .upsert_chat(&Chat::new(chat.into(), "Chat".into()))
                    .unwrap();
                archive
                    .insert_message(&crate::archive::tests::message(chat, "a", 1, true), None)
                    .unwrap();
            }
            archive
                .set_pin(LID, "a", lid_pinned, "x", 10, None)
                .unwrap();
            archive
                .set_pin(PHONE, "a", !lid_pinned, "y", 10, None)
                .unwrap();

            archive.put_lid("111", "222").unwrap();

            assert!(archive.message(PHONE, "a").unwrap().is_some());
            assert!(archive.pinned_message(PHONE, 20).unwrap().is_none());
        }
    }

    #[test]
    fn a_pin_can_arrive_before_its_message() {
        let archive = archive_with(&[]);
        archive.set_pin(CHAT, "late", true, "x", 5, None).unwrap();
        assert_eq!(pinned_id(&archive, 6), None);
        let archive_message = archive_with(&["late"]);
        archive_message
            .set_pin(CHAT, "late", true, "x", 5, None)
            .unwrap();
        assert_eq!(pinned_id(&archive_message, 6).as_deref(), Some("late"));
    }

    #[test]
    fn an_expired_pin_is_hidden_and_turned_off_once() {
        let archive = archive_with(&["a"]);
        archive
            .set_pin(CHAT, "a", true, "x", 10, Some(1_000))
            .unwrap();
        assert_eq!(pinned_id(&archive, 999).as_deref(), Some("a"));
        assert_eq!(pinned_id(&archive, 1_000), None);
        assert_eq!(archive.expire_pins(1_000).unwrap(), [CHAT]);
        assert!(archive.expire_pins(1_001).unwrap().is_empty());
    }

    #[test]
    fn deleting_the_message_drops_its_pin() {
        let archive = archive_with(&["a"]);
        archive.set_pin(CHAT, "a", true, "x", 10, None).unwrap();
        archive.delete_message(CHAT, "a").unwrap();
        assert_eq!(pinned_id(&archive, 20), None);
        let rows: i64 = archive
            .connection
            .query_row("SELECT COUNT(*) FROM pins", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn a_message_deleted_for_everyone_is_not_shown_as_pinned() {
        let archive = archive_with(&["a"]);
        archive.set_pin(CHAT, "a", true, "x", 10, None).unwrap();
        archive
            .set_content(CHAT, "a", &Content::Revoked, false)
            .unwrap();
        assert_eq!(pinned_id(&archive, 20), None);
    }
}
