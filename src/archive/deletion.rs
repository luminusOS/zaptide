//! Clearing and deleting whole chats. Both remove messages up through a
//! timestamp bound rather than unconditionally, so a clear or delete that
//! raced with a new incoming message never removes that newer message —
//! the same bound WhatsApp's own apps apply. Existing `AFTER DELETE ON
//! messages` triggers already cascade into stars, polls, and group receipts.

use super::{Archive, Result};
use rusqlite::params;

impl Archive {
    /// Deletes messages with `timestamp <= through` (`i64::MAX` for
    /// everything currently in the chat, the same sentinel
    /// `finish_read_sync` uses), skipping starred ones when `keep_starred`.
    /// Idempotent: replaying an older or already-applied clear removes
    /// nothing further. Returns how many messages remain in the chat
    /// afterward.
    pub fn clear_chat_messages(&self, id: &str, through: i64, keep_starred: bool) -> Result<i64> {
        let id = &*self.resolve_chat(id)?;
        if keep_starred {
            self.connection.execute(
                "DELETE FROM messages WHERE chat = ?1 AND timestamp <= ?2
                 AND NOT EXISTS (
                     SELECT 1 FROM stars
                     WHERE stars.chat = messages.chat AND stars.id = messages.id
                         AND stars.starred = 1
                 )",
                params![id, through],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM messages WHERE chat = ?1 AND timestamp <= ?2",
                params![id, through],
            )?;
        }
        // No preview survives a clear that reached the newest message; an
        // older bound, or kept starred messages, can leave one behind.
        self.connection.execute(
            "UPDATE chats SET last_activity =
                 COALESCE((SELECT MAX(timestamp) FROM messages
                           WHERE chat = ?1 AND kind IS NOT 'system'), 0)
             WHERE id = ?1",
            params![id],
        )?;
        self.connection.query_row(
            "SELECT COUNT(*) FROM messages WHERE chat = ?1",
            params![id],
            |row| row.get(0),
        )
    }

    /// Clears a chat through `through` (always, starred included — WhatsApp's
    /// own `DeleteChatAction` has no starred-message exception) and drops its
    /// row once nothing is left. A message newer than `through` keeps the
    /// chat in the list, same as on the phone. The row reappears, with
    /// default preferences, the next time `ensure_chat` sees activity for
    /// it.
    pub fn delete_chat(&self, id: &str, through: i64) -> Result<bool> {
        let id = &*self.resolve_chat(id)?;
        let remaining = self.clear_chat_messages(id, through, false)?;
        if remaining == 0 {
            self.connection
                .execute("DELETE FROM chats WHERE id = ?1", params![id])?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::tests::message;

    const CHAT: &str = "chat@s.whatsapp.net";

    fn archive_with_messages(timestamps: &[i64]) -> Archive {
        let archive = Archive::in_memory().expect("opens");
        archive.ensure_chat(CHAT, "A").expect("chat");
        for &timestamp in timestamps {
            archive
                .insert_message(
                    &message(CHAT, &format!("m{timestamp}"), timestamp, false),
                    None,
                )
                .expect("insert");
        }
        archive
    }

    #[test]
    fn clearing_keeps_messages_newer_than_the_bound() {
        let archive = archive_with_messages(&[1, 2, 3]);
        let remaining = archive.clear_chat_messages(CHAT, 2, false).expect("clear");
        assert_eq!(remaining, 1);
        assert!(archive.message(CHAT, "m1").expect("read").is_none());
        assert!(archive.message(CHAT, "m2").expect("read").is_none());
        assert!(archive.message(CHAT, "m3").expect("read").is_some());
    }

    #[test]
    fn clearing_everything_can_keep_starred_messages() {
        let archive = archive_with_messages(&[1, 2]);
        archive.set_starred(CHAT, "m1", true, 1).expect("star");
        let remaining = archive
            .clear_chat_messages(CHAT, i64::MAX, true)
            .expect("clear");
        assert_eq!(remaining, 1);
        assert!(archive.message(CHAT, "m1").expect("read").is_some());
        assert!(archive.message(CHAT, "m2").expect("read").is_none());
    }

    #[test]
    fn clearing_starred_messages_when_not_kept() {
        let archive = archive_with_messages(&[1]);
        archive.set_starred(CHAT, "m1", true, 1).expect("star");
        let remaining = archive
            .clear_chat_messages(CHAT, i64::MAX, false)
            .expect("clear");
        assert_eq!(remaining, 0);
    }

    #[test]
    fn clearing_recomputes_the_last_activity_preview() {
        let archive = archive_with_messages(&[1, 2, 3]);
        archive.clear_chat_messages(CHAT, 2, false).expect("clear");
        assert_eq!(archive.chat(CHAT).expect("chat").unwrap().last_activity, 3);
        archive
            .clear_chat_messages(CHAT, i64::MAX, false)
            .expect("clear");
        assert_eq!(archive.chat(CHAT).expect("chat").unwrap().last_activity, 0);
    }

    #[test]
    fn clearing_twice_removes_nothing_further() {
        let archive = archive_with_messages(&[1, 2]);
        assert_eq!(
            archive
                .clear_chat_messages(CHAT, i64::MAX, false)
                .expect("clear"),
            0
        );
        assert_eq!(
            archive
                .clear_chat_messages(CHAT, i64::MAX, false)
                .expect("clear again"),
            0
        );
    }

    #[test]
    fn deleting_a_chat_with_a_newer_message_keeps_the_chat() {
        let archive = archive_with_messages(&[1, 2]);
        let removed = archive.delete_chat(CHAT, 1).expect("delete");
        assert!(!removed, "a message newer than the bound should survive");
        assert!(archive.chat(CHAT).expect("chat").is_some());
        assert!(archive.message(CHAT, "m2").expect("read").is_some());
    }

    #[test]
    fn deleting_a_chat_drops_its_row_once_nothing_remains() {
        let archive = archive_with_messages(&[1, 2]);
        let removed = archive.delete_chat(CHAT, i64::MAX).expect("delete");
        assert!(removed);
        assert!(archive.chat(CHAT).expect("chat").is_none());
    }

    #[test]
    fn a_deleted_chat_reappears_on_new_activity() {
        let archive = archive_with_messages(&[1]);
        archive.delete_chat(CHAT, i64::MAX).expect("delete");
        assert!(archive.chat(CHAT).expect("chat").is_none());
        archive.ensure_chat(CHAT, "A").expect("chat recreated");
        archive
            .insert_message(&message(CHAT, "m2", 2, false), None)
            .expect("insert");
        assert!(archive.chat(CHAT).expect("chat").is_some());
    }
}
