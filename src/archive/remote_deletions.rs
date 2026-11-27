//! Durable queue of deletions that a person confirmed in this window and that
//! still have to reach the phone and the other linked devices.
//!
//! A row exists only because a confirmed request removed the matching local
//! rows in the same savepoint. Nothing derives a row from data the archive
//! lacks, so a reset, a corrupted file or an incomplete history sync can
//! never produce one. The queue is cleared with the rest of the archive on
//! unlinking.

use super::{Archive, Result};
use rusqlite::params;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS remote_deletions (
    key TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    chat TEXT NOT NULL,
    message_id TEXT NOT NULL,
    participant TEXT,
    from_me INTEGER NOT NULL,
    bound INTEGER NOT NULL,
    requested_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0
);
";

const SAVEPOINT: &str = "archive_atomic";

/// What the other devices are asked to delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteDeletionKind {
    /// One message, "delete for me".
    Message,
    /// Every message of a chat up to a bound; the chat stays in the list.
    Clear,
    /// Every message of a chat up to a bound, and the chat itself.
    Chat,
}

impl RemoteDeletionKind {
    fn name(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Clear => "clear",
            Self::Chat => "chat",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "message" => Some(Self::Message),
            "clear" => Some(Self::Clear),
            "chat" => Some(Self::Chat),
            _ => None,
        }
    }
}

/// One confirmed deletion waiting to be sent.
///
/// For [`RemoteDeletionKind::Message`] the message fields name the deleted
/// message and `bound` is its timestamp. For the chat kinds they name the
/// newest message the archive held when the person confirmed, and `bound` is
/// its timestamp: nothing newer than what the person saw is ever requested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteDeletion {
    pub kind: RemoteDeletionKind,
    pub chat: String,
    pub message_id: String,
    /// The sender of a group message from someone else.
    pub participant: Option<String>,
    pub from_me: bool,
    pub bound: i64,
}

impl RemoteDeletion {
    /// Identity of the request. Confirming the same deletion twice names the
    /// same row, so a repeated request or a retry never queues a second one.
    pub fn key(&self) -> String {
        match self.kind {
            RemoteDeletionKind::Message => format!("message:{}:{}", self.chat, self.message_id),
            RemoteDeletionKind::Clear => format!("clear:{}", self.chat),
            RemoteDeletionKind::Chat => format!("chat:{}", self.chat),
        }
    }
}

/// The newest message a chat holds, which bounds a chat-level deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewestMessage {
    pub id: String,
    pub sender: String,
    pub from_me: bool,
    pub timestamp: i64,
}

impl Archive {
    /// Runs `work` in a savepoint that rolls back when it fails. Savepoints
    /// nest, so this is safe inside an archive batch.
    pub fn atomically<T>(&self, work: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        self.connection
            .execute_batch(&format!("SAVEPOINT {SAVEPOINT}"))?;
        match work(self) {
            Ok(value) => {
                self.connection
                    .execute_batch(&format!("RELEASE {SAVEPOINT}"))?;
                Ok(value)
            }
            Err(error) => {
                let _ = self
                    .connection
                    .execute_batch(&format!("ROLLBACK TO {SAVEPOINT}; RELEASE {SAVEPOINT}"));
                Err(error)
            }
        }
    }

    /// The newest archived message that can bound a phone-side chat action.
    /// Local transcript notices cannot supply a message range.
    pub fn newest_message(&self, chat: &str) -> Result<Option<NewestMessage>> {
        let chat = &*self.resolve_chat(chat)?;
        let mut statement = self.connection.prepare_cached(
            "SELECT id, sender, from_me, timestamp FROM messages
             WHERE chat = ?1 AND kind IS NOT 'system'
             ORDER BY timestamp DESC, rowid DESC LIMIT 1",
        )?;
        let mut rows = statement.query_map(params![chat], |row| {
            Ok(NewestMessage {
                id: row.get(0)?,
                sender: row.get(1)?,
                from_me: row.get(2)?,
                timestamp: row.get(3)?,
            })
        })?;
        rows.next().transpose()
    }

    /// Queues a confirmed deletion. Returns whether the queue changed: a
    /// repeat of a queued request changes nothing, and a chat-level request
    /// replaces its queued twin only when it reaches a newer message.
    pub fn queue_remote_deletion(&self, deletion: &RemoteDeletion, now: i64) -> Result<bool> {
        let changed = self.connection.execute(
            "INSERT INTO remote_deletions
                 (key, kind, chat, message_id, participant, from_me, bound, requested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(key) DO UPDATE SET
                 message_id = excluded.message_id,
                 participant = excluded.participant,
                 from_me = excluded.from_me,
                 bound = excluded.bound,
                 attempts = 0
             WHERE excluded.bound > remote_deletions.bound",
            params![
                deletion.key(),
                deletion.kind.name(),
                deletion.chat,
                deletion.message_id,
                deletion.participant,
                deletion.from_me,
                deletion.bound,
                now,
            ],
        )?;
        Ok(changed > 0)
    }

    /// The oldest queued deletion. A row with an unknown kind is never
    /// returned; it leaves with the expiry sweep.
    pub fn next_remote_deletion(&self) -> Result<Option<RemoteDeletion>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT kind, chat, message_id, participant, from_me, bound
             FROM remote_deletions ORDER BY requested_at, rowid",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                RemoteDeletion {
                    kind: RemoteDeletionKind::Message,
                    chat: row.get(1)?,
                    message_id: row.get(2)?,
                    participant: row.get(3)?,
                    from_me: row.get(4)?,
                    bound: row.get(5)?,
                },
            ))
        })?;
        for row in rows {
            let (kind, deletion) = row?;
            if let Some(kind) = RemoteDeletionKind::from_name(&kind) {
                return Ok(Some(RemoteDeletion { kind, ..deletion }));
            }
        }
        Ok(None)
    }

    /// Removes a deletion that reached the other devices.
    pub fn finish_remote_deletion(&self, key: &str, bound: i64) -> Result<()> {
        self.connection.execute(
            "DELETE FROM remote_deletions WHERE key = ?1 AND bound = ?2",
            params![key, bound],
        )?;
        Ok(())
    }

    /// Counts a failed attempt and gives up on the deletion once it has
    /// failed `max_attempts` times. Returns whether it was given up. The
    /// local deletion stays; only its propagation is abandoned.
    pub fn fail_remote_deletion(&self, key: &str, bound: i64, max_attempts: u32) -> Result<bool> {
        self.connection.execute(
            "UPDATE remote_deletions SET attempts = attempts + 1 WHERE key = ?1 AND bound = ?2",
            params![key, bound],
        )?;
        let removed = self.connection.execute(
            "DELETE FROM remote_deletions WHERE key = ?1 AND bound = ?2 AND attempts >= ?3",
            params![key, bound, max_attempts],
        )?;
        Ok(removed > 0)
    }

    /// Drops deletions queued before `before` (Unix seconds). A request that
    /// old no longer says what the person wants today.
    pub fn expire_remote_deletions(&self, before: i64) -> Result<usize> {
        self.connection.execute(
            "DELETE FROM remote_deletions WHERE requested_at < ?1",
            params![before],
        )
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

    fn deletion(kind: RemoteDeletionKind, id: &str, bound: i64) -> RemoteDeletion {
        RemoteDeletion {
            kind,
            chat: CHAT.to_owned(),
            message_id: id.to_owned(),
            participant: None,
            from_me: false,
            bound,
        }
    }

    #[test]
    fn a_repeated_request_queues_one_row() {
        let archive = archive_with_messages(&[]);
        let request = deletion(RemoteDeletionKind::Message, "m1", 1);
        assert!(archive.queue_remote_deletion(&request, 10).expect("queue"));
        assert!(!archive.queue_remote_deletion(&request, 11).expect("repeat"));
        assert_eq!(
            archive.next_remote_deletion().expect("next"),
            Some(request.clone())
        );
        archive
            .finish_remote_deletion(&request.key(), request.bound)
            .expect("finish");
        assert_eq!(archive.next_remote_deletion().expect("next"), None);
    }

    #[test]
    fn a_newer_chat_request_replaces_an_older_one_but_not_the_reverse() {
        let archive = archive_with_messages(&[]);
        let older = deletion(RemoteDeletionKind::Chat, "m1", 1);
        let newer = deletion(RemoteDeletionKind::Chat, "m5", 5);
        assert!(archive.queue_remote_deletion(&older, 10).expect("older"));
        assert!(archive.queue_remote_deletion(&newer, 11).expect("newer"));
        assert!(!archive.queue_remote_deletion(&older, 12).expect("again"));
        let queued = archive.next_remote_deletion().expect("next").expect("row");
        assert_eq!(queued.message_id, "m5");
        assert_eq!(queued.bound, 5);
    }

    #[test]
    fn a_failed_older_attempt_does_not_consume_the_replacements_retry() {
        let archive = archive_with_messages(&[]);
        let older = deletion(RemoteDeletionKind::Chat, "m1", 1);
        let newer = deletion(RemoteDeletionKind::Chat, "m2", 2);
        archive.queue_remote_deletion(&older, 10).unwrap();
        archive.queue_remote_deletion(&newer, 11).unwrap();

        assert!(
            !archive
                .fail_remote_deletion(&older.key(), older.bound, 1)
                .unwrap()
        );
        assert_eq!(archive.next_remote_deletion().unwrap(), Some(newer));
    }

    #[test]
    fn a_clear_and_a_delete_of_one_chat_are_separate_requests() {
        let archive = archive_with_messages(&[]);
        let clear = deletion(RemoteDeletionKind::Clear, "m1", 1);
        let delete = deletion(RemoteDeletionKind::Chat, "m1", 1);
        assert!(archive.queue_remote_deletion(&clear, 10).expect("clear"));
        assert!(archive.queue_remote_deletion(&delete, 11).expect("delete"));
        assert_ne!(clear.key(), delete.key());
    }

    #[test]
    fn deletions_leave_in_the_order_they_were_requested() {
        let archive = archive_with_messages(&[]);
        let first = deletion(RemoteDeletionKind::Message, "a", 1);
        let second = deletion(RemoteDeletionKind::Message, "b", 2);
        archive.queue_remote_deletion(&first, 10).expect("first");
        archive.queue_remote_deletion(&second, 10).expect("second");
        assert_eq!(archive.next_remote_deletion().unwrap(), Some(first.clone()));
        archive
            .finish_remote_deletion(&first.key(), first.bound)
            .unwrap();
        assert_eq!(archive.next_remote_deletion().unwrap(), Some(second));
    }

    #[test]
    fn a_deletion_is_abandoned_after_its_last_attempt() {
        let archive = archive_with_messages(&[]);
        let request = deletion(RemoteDeletionKind::Message, "m1", 1);
        archive.queue_remote_deletion(&request, 10).expect("queue");
        assert!(
            !archive
                .fail_remote_deletion(&request.key(), request.bound, 3)
                .unwrap()
        );
        assert!(
            !archive
                .fail_remote_deletion(&request.key(), request.bound, 3)
                .unwrap()
        );
        assert!(archive.next_remote_deletion().unwrap().is_some());
        assert!(
            archive
                .fail_remote_deletion(&request.key(), request.bound, 3)
                .unwrap()
        );
        assert_eq!(archive.next_remote_deletion().unwrap(), None);
    }

    #[test]
    fn old_requests_expire_and_recent_ones_stay() {
        let archive = archive_with_messages(&[]);
        let old = deletion(RemoteDeletionKind::Message, "old", 1);
        let recent = deletion(RemoteDeletionKind::Message, "recent", 2);
        archive.queue_remote_deletion(&old, 100).expect("old");
        archive.queue_remote_deletion(&recent, 200).expect("recent");
        assert_eq!(archive.expire_remote_deletions(150).expect("expire"), 1);
        assert_eq!(archive.next_remote_deletion().unwrap(), Some(recent));
    }

    #[test]
    fn a_row_with_an_unknown_kind_is_never_returned() {
        let archive = archive_with_messages(&[]);
        archive
            .execute_batch_for_test(
                "INSERT INTO remote_deletions
                     (key, kind, chat, message_id, from_me, bound, requested_at)
                 VALUES ('odd', 'wipe-everything', 'chat@s.whatsapp.net', 'x', 0, 1, 1)",
            )
            .expect("insert");
        assert_eq!(archive.next_remote_deletion().expect("next"), None);
        assert_eq!(archive.expire_remote_deletions(2).expect("expire"), 1);
    }

    #[test]
    fn the_newest_message_bounds_a_chat_and_an_empty_chat_has_none() {
        let archive = archive_with_messages(&[1, 3, 2]);
        let newest = archive.newest_message(CHAT).expect("newest").expect("row");
        assert_eq!(newest.id, "m3");
        assert_eq!(newest.timestamp, 3);
        let empty = archive_with_messages(&[]);
        assert_eq!(empty.newest_message(CHAT).expect("newest"), None);
    }

    #[test]
    fn transcript_notices_do_not_advance_the_phone_message_range() {
        use crate::model::{Content, SystemEvent};

        for timestamps in [&[1][..], &[]] {
            let archive = archive_with_messages(timestamps);
            let mut notice = message(CHAT, "local-notice", 2, false);
            notice.content = Content::System {
                event: SystemEvent::Timer { seconds: 86_400 },
            };
            archive.insert_message(&notice, None).expect("insert");

            let newest = archive.newest_message(CHAT).expect("newest");
            assert_eq!(
                newest.map(|row| row.id),
                timestamps.first().map(|_| "m1".into())
            );
            let chat = archive.chat(CHAT).expect("chat").expect("row");
            assert_eq!(chat.last_activity, timestamps.first().copied().unwrap_or(0));
        }
    }

    #[test]
    fn a_failed_step_rolls_the_whole_savepoint_back() {
        let archive = archive_with_messages(&[1]);
        let result: Result<()> = archive.atomically(|archive| {
            archive.delete_message(CHAT, "m1")?;
            archive.queue_remote_deletion(&deletion(RemoteDeletionKind::Message, "m1", 1), 10)?;
            Err(rusqlite::Error::QueryReturnedNoRows)
        });
        assert!(result.is_err());
        assert!(archive.message(CHAT, "m1").expect("read").is_some());
        assert_eq!(archive.next_remote_deletion().expect("next"), None);
    }

    #[test]
    fn unlinking_clears_the_queue() {
        let archive = archive_with_messages(&[1]);
        archive
            .queue_remote_deletion(&deletion(RemoteDeletionKind::Chat, "m1", 1), 10)
            .expect("queue");
        archive.clear().expect("clear");
        assert_eq!(archive.next_remote_deletion().expect("next"), None);
    }
}
