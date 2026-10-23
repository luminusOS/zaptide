//! Starred messages. A star is stored apart from its message because the
//! phone's settings can arrive before the history that holds the message.

use super::{Archive, MESSAGE_COLUMNS, Result, row};
use crate::model::Message;
use rusqlite::params;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS stars (
    chat TEXT NOT NULL, id TEXT NOT NULL, starred INTEGER NOT NULL, updated_at INTEGER NOT NULL,
    PRIMARY KEY (chat, id)
);
CREATE TRIGGER IF NOT EXISTS delete_stars AFTER DELETE ON messages BEGIN
    DELETE FROM stars WHERE chat = OLD.chat AND id = OLD.id;
END;
";

/// A starred message with the chat it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct StarredMessage {
    pub chat: String,
    pub message: Message,
}

impl Archive {
    /// Records a star unless a newer change for the same message is already
    /// stored, so replayed history cannot undo a later star or unstar.
    /// Returns whether the stored state changed.
    pub fn set_starred(
        &self,
        chat: &str,
        id: &str,
        starred: bool,
        updated_at: i64,
    ) -> Result<bool> {
        let changed = self.connection.execute(
            "INSERT INTO stars (chat, id, starred, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (chat, id) DO UPDATE SET starred = excluded.starred,
                 updated_at = excluded.updated_at
             WHERE excluded.updated_at >= stars.updated_at AND excluded.starred != stars.starred",
            params![chat, id, starred, updated_at],
        )?;
        Ok(changed > 0)
    }

    /// Ids of the starred messages of one chat.
    pub fn starred_ids(&self, chat: &str) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM stars WHERE chat = ?1 AND starred = 1")?;
        statement
            .query_map(params![chat], |row| row.get(0))?
            .collect()
    }

    /// Starred messages of one chat, newest first. A locked chat lists none,
    /// as it shows nothing in the chat list.
    pub fn starred_messages(&self, chat: &str, limit: usize) -> Result<Vec<StarredMessage>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS}, chat
             FROM messages
             WHERE chat = ?1
               AND EXISTS (SELECT 1 FROM stars
                           WHERE stars.chat = messages.chat AND stars.id = messages.id
                             AND stars.starred = 1)
               AND NOT COALESCE((SELECT locked FROM chats WHERE chats.id = messages.chat), 0)
             ORDER BY timestamp DESC, rowid DESC
             LIMIT ?2"
        ))?;
        let chat_column = MESSAGE_COLUMNS.split(',').count();
        let rows = statement.query_map(params![chat, limit as i64], |row| {
            let chat: String = row.get(chat_column)?;
            let message = row::message_from_row(&chat, row)?;
            Ok(StarredMessage { chat, message })
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Chat, Content, Delivery};

    fn archive() -> Archive {
        Archive::in_memory().unwrap()
    }

    fn message(id: &str, timestamp: i64) -> Message {
        Message {
            id: id.into(),
            chat: "chat@s.whatsapp.net".into(),
            sender: "chat@s.whatsapp.net".into(),
            sender_name: None,
            from_me: false,
            timestamp,
            content: Content::text(id),
            status: Delivery::None,
            quoted: None,
            reactions: Vec::new(),
            edited: false,
            thumbnail: None,
            mentions: Vec::new(),
            forwarded: false,
            delivered_at: None,
            read_at: None,
        }
    }

    #[test]
    fn a_newer_change_wins_over_replayed_history() {
        let archive = archive();
        assert!(archive.set_starred("c", "m", true, 20).unwrap());
        assert!(!archive.set_starred("c", "m", false, 10).unwrap());
        assert_eq!(archive.starred_ids("c").unwrap(), ["m"]);
        assert!(archive.set_starred("c", "m", false, 30).unwrap());
        assert!(archive.starred_ids("c").unwrap().is_empty());
    }

    #[test]
    fn repeating_the_same_state_changes_nothing() {
        let archive = archive();
        assert!(archive.set_starred("c", "m", true, 1).unwrap());
        assert!(!archive.set_starred("c", "m", true, 2).unwrap());
    }

    #[test]
    fn a_star_can_arrive_before_its_message() {
        let archive = archive();
        archive
            .set_starred("chat@s.whatsapp.net", "late", true, 5)
            .unwrap();
        assert!(
            archive
                .starred_messages("chat@s.whatsapp.net", 10)
                .unwrap()
                .is_empty()
        );
        archive
            .upsert_chat(&Chat::new("chat@s.whatsapp.net".into(), "Chat".into()))
            .unwrap();
        archive.insert_message(&message("late", 100), None).unwrap();
        let starred = archive.starred_messages("chat@s.whatsapp.net", 10).unwrap();
        assert_eq!(starred.len(), 1);
        assert_eq!(starred[0].chat, "chat@s.whatsapp.net");
        assert_eq!(starred[0].message.id, "late");
    }

    #[test]
    fn starred_messages_are_newest_first_and_skip_unstarred_ones() {
        let archive = archive();
        archive
            .upsert_chat(&Chat::new("chat@s.whatsapp.net".into(), "Chat".into()))
            .unwrap();
        for (id, at) in [("old", 1), ("plain", 2), ("new", 3)] {
            archive.insert_message(&message(id, at), None).unwrap();
        }
        archive
            .set_starred("chat@s.whatsapp.net", "old", true, 1)
            .unwrap();
        archive
            .set_starred("chat@s.whatsapp.net", "new", true, 1)
            .unwrap();
        let ids: Vec<_> = archive
            .starred_messages("chat@s.whatsapp.net", 10)
            .unwrap()
            .into_iter()
            .map(|starred| starred.message.id)
            .collect();
        assert_eq!(ids, ["new", "old"]);
    }

    #[test]
    fn a_chat_lists_only_its_own_starred_messages() {
        let archive = archive();
        for chat in ["chat@s.whatsapp.net", "other@s.whatsapp.net"] {
            archive
                .upsert_chat(&Chat::new(chat.into(), "Chat".into()))
                .unwrap();
        }
        let mut other = message("elsewhere", 5);
        other.chat = "other@s.whatsapp.net".into();
        archive.insert_message(&message("here", 4), None).unwrap();
        archive.insert_message(&other, None).unwrap();
        archive
            .set_starred("chat@s.whatsapp.net", "here", true, 1)
            .unwrap();
        archive
            .set_starred("other@s.whatsapp.net", "elsewhere", true, 1)
            .unwrap();
        let ids: Vec<_> = archive
            .starred_messages("chat@s.whatsapp.net", 10)
            .unwrap()
            .into_iter()
            .map(|starred| starred.message.id)
            .collect();
        assert_eq!(ids, ["here"]);
    }

    #[test]
    fn locked_chats_never_list_their_starred_messages() {
        let archive = archive();
        archive
            .upsert_chat(&Chat::new("chat@s.whatsapp.net".into(), "Chat".into()))
            .unwrap();
        archive.set_locked("chat@s.whatsapp.net", true).unwrap();
        archive.insert_message(&message("m", 1), None).unwrap();
        archive
            .set_starred("chat@s.whatsapp.net", "m", true, 1)
            .unwrap();
        assert!(
            archive
                .starred_messages("chat@s.whatsapp.net", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn deleting_a_message_drops_its_star() {
        let archive = archive();
        archive.insert_message(&message("m", 1), None).unwrap();
        archive
            .set_starred("chat@s.whatsapp.net", "m", true, 1)
            .unwrap();
        archive.delete_message("chat@s.whatsapp.net", "m").unwrap();
        assert!(
            archive
                .starred_ids("chat@s.whatsapp.net")
                .unwrap()
                .is_empty()
        );
    }
}
