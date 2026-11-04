//! Call history. A call is filed as an ordinary message of its chat, so the
//! transcript pages it with everything else; this lists them across chats.

use super::{Archive, Result};
use crate::model::Content;
use crate::model::call::CallEntry;
use rusqlite::params;

/// The partial index `messages_calls` is built on this exact text, and SQLite
/// only uses it for a query whose expression matches it character for character.
const KIND: &str = "CASE WHEN json_valid(content) THEN json_extract(content, '$.kind') END";

pub fn schema() -> String {
    format!(
        "CREATE INDEX IF NOT EXISTS messages_calls ON messages(timestamp) WHERE {KIND} = 'call';"
    )
}

impl Archive {
    /// Files a call, replacing an earlier record of the same call with the
    /// time and side the newer report gives: the phone's call history is
    /// authoritative over what this device saw live.
    pub fn insert_call(&self, message: &crate::model::Message) -> Result<()> {
        self.insert_message(message, None)?;
        self.connection.execute(
            &format!(
                "UPDATE messages SET timestamp = ?3, from_me = ?4, sender = ?5
                 WHERE chat = ?1 AND id = ?2 AND {KIND} = 'call'"
            ),
            params![
                message.chat,
                message.id,
                message.timestamp,
                message.from_me,
                message.sender
            ],
        )?;
        Ok(())
    }

    /// The latest calls of unlocked chats, newest first.
    pub fn calls(&self, limit: usize) -> Result<Vec<CallEntry>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT chat, id, from_me, timestamp, content FROM messages
             WHERE {KIND} = 'call'
               AND NOT COALESCE((SELECT locked FROM chats WHERE chats.id = messages.chat), 0)
             ORDER BY timestamp DESC, rowid DESC
             LIMIT ?1"
        ))?;
        let rows = statement.query_map(params![limit as i64], |row| {
            let content: String = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, i64>(3)?,
                content,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (chat, id, from_me, timestamp, content) = row?;
            if let Ok(Content::Call {
                video,
                outcome,
                seconds,
                ..
            }) = serde_json::from_str(&content)
            {
                entries.push(CallEntry {
                    chat,
                    id,
                    video,
                    outgoing: from_me,
                    outcome,
                    seconds,
                    timestamp,
                });
            }
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::call::CallOutcome;
    use crate::model::{Chat, Delivery, Message};

    fn call(chat: &str, id: &str, timestamp: i64, outgoing: bool) -> Message {
        Message {
            id: id.into(),
            chat: chat.into(),
            sender: chat.into(),
            sender_name: None,
            from_me: outgoing,
            timestamp,
            content: Content::Call {
                video: false,
                outgoing,
                outcome: CallOutcome::Completed,
                seconds: Some(138),
            },
            status: Delivery::None,
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

    fn text(chat: &str, id: &str, timestamp: i64) -> Message {
        Message {
            content: Content::text("hello"),
            ..call(chat, id, timestamp, false)
        }
    }

    #[test]
    fn lists_only_calls_newest_first_across_chats() {
        let archive = Archive::in_memory().unwrap();
        for chat in ["a@s.whatsapp.net", "b@s.whatsapp.net"] {
            archive
                .upsert_chat(&Chat::new(chat.into(), "Chat".into()))
                .unwrap();
        }
        archive
            .insert_message(&call("a@s.whatsapp.net", "old", 10, true), None)
            .unwrap();
        archive
            .insert_message(&text("a@s.whatsapp.net", "words", 20), None)
            .unwrap();
        archive
            .insert_message(&call("b@s.whatsapp.net", "new", 30, false), None)
            .unwrap();

        let calls = archive.calls(10).unwrap();
        let ids: Vec<_> = calls.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["new", "old"]);
        assert_eq!(calls[0].chat, "b@s.whatsapp.net");
        assert!(!calls[0].outgoing);
        assert!(calls[1].outgoing);
        assert_eq!(calls[0].seconds, Some(138));
    }

    #[test]
    fn the_history_query_uses_the_call_index_instead_of_scanning_messages() {
        let archive = Archive::in_memory().unwrap();
        let plan: Vec<String> = archive
            .connection
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT id FROM messages WHERE {KIND} = 'call' ORDER BY timestamp DESC LIMIT 5"
            ))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert!(
            plan.iter().any(|step| step.contains("messages_calls")),
            "{plan:?}"
        );
    }

    #[test]
    fn locked_chats_keep_their_calls_out_of_the_history() {
        let archive = Archive::in_memory().unwrap();
        archive
            .upsert_chat(&Chat::new("a@s.whatsapp.net".into(), "Chat".into()))
            .unwrap();
        archive
            .insert_message(&call("a@s.whatsapp.net", "call", 10, false), None)
            .unwrap();
        archive.set_locked("a@s.whatsapp.net", true).unwrap();
        assert!(archive.calls(10).unwrap().is_empty());
    }

    #[test]
    fn the_limit_keeps_the_newest_calls() {
        let archive = Archive::in_memory().unwrap();
        for index in 0..5 {
            archive
                .insert_message(
                    &call("a@s.whatsapp.net", &format!("call{index}"), index, false),
                    None,
                )
                .unwrap();
        }
        let ids: Vec<_> = archive
            .calls(2)
            .unwrap()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(ids, ["call4", "call3"]);
    }

    #[test]
    fn a_resynced_call_replaces_the_stored_one_instead_of_duplicating_it() {
        let archive = Archive::in_memory().unwrap();
        archive
            .insert_message(&call("a@s.whatsapp.net", "same", 10, false), None)
            .unwrap();
        let mut again = call("a@s.whatsapp.net", "same", 10, false);
        again.content = Content::Call {
            video: false,
            outgoing: false,
            outcome: CallOutcome::Missed,
            seconds: None,
        };
        archive.insert_message(&again, None).unwrap();
        let calls = archive.calls(10).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].outcome, CallOutcome::Missed);
    }

    #[test]
    fn a_resynced_call_takes_the_phones_time_and_side() {
        let archive = Archive::in_memory().unwrap();
        archive
            .upsert_chat(&Chat::new("a@s.whatsapp.net".into(), "Chat".into()))
            .unwrap();
        archive
            .insert_call(&call("a@s.whatsapp.net", "same", 10, false))
            .unwrap();
        archive
            .insert_call(&call("a@s.whatsapp.net", "same", 99, true))
            .unwrap();
        let stored = archive
            .message("a@s.whatsapp.net", "same")
            .unwrap()
            .unwrap();
        assert_eq!((stored.timestamp, stored.from_me), (99, true));
        assert_eq!(archive.calls(10).unwrap().len(), 1);
    }
}
