//! SQLite archive of chats, messages, contacts, and stickers.
//!
//! Each message keeps its raw protobuf because attachment download keys may be
//! needed long after history sync.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{Chat, ChatKind, Content, Delivery, Message};

mod contacts;
mod encryption;
pub use encryption::{archive_key_identity, copy_archive_key, forget_archive_key};
mod media;
mod polls;
mod receipts;
mod row;
pub use polls::PollVote;

#[derive(Clone, Debug)]
pub struct PhoneSticker {
    pub hash: String,
    pub raw: Vec<u8>,
    pub last_used: i64,
    pub path: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ArchivedSticker {
    pub last_used: i64,
    pub path: std::path::PathBuf,
    pub raw: Option<Vec<u8>>,
}

pub struct Archive {
    connection: Connection,
}

pub type Result<T> = std::result::Result<T, rusqlite::Error>;
type PendingQuotedMessage = (String, String, String, Vec<u8>);
type FailedQuotedMessage = (String, Vec<u8>);

struct StoredMessageProjection {
    status: i64,
    content: String,
    edited: bool,
    raw: Option<Vec<u8>>,
    mentions: String,
}

fn preserve_media_path(existing: &Content, incoming: &mut Content) {
    let (existing_media, incoming_media) = match (existing, incoming) {
        (Content::Image { media: old, .. }, Content::Image { media: new, .. })
        | (Content::Video { media: old, .. }, Content::Video { media: new, .. })
        | (Content::Audio { media: old, .. }, Content::Audio { media: new, .. })
        | (Content::Document { media: old, .. }, Content::Document { media: new, .. })
        | (Content::Sticker { media: old, .. }, Content::Sticker { media: new, .. }) => (old, new),
        _ => return,
    };
    if existing_media.path.is_some() {
        incoming_media.path = existing_media.path.clone();
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS chats (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    last_activity INTEGER NOT NULL DEFAULT 0,
    unread INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    pinned INTEGER NOT NULL DEFAULT 0,
    muted_until INTEGER,
    locked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS messages (
    chat TEXT NOT NULL,
    id TEXT NOT NULL,
    sender TEXT NOT NULL,
    sender_name TEXT,
    from_me INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    content TEXT NOT NULL,
    status INTEGER NOT NULL DEFAULT 0,
    quoted TEXT,
    reactions TEXT NOT NULL DEFAULT '[]',
    edited INTEGER NOT NULL DEFAULT 0,
    raw BLOB,
    PRIMARY KEY (chat, id)
);
CREATE INDEX IF NOT EXISTS messages_by_time ON messages (chat, timestamp);
CREATE INDEX IF NOT EXISTS messages_by_id ON messages (id);
CREATE TABLE IF NOT EXISTS contacts (
    id TEXT PRIMARY KEY,
    full_name TEXT,
    push_name TEXT
);
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS lids (
    lid TEXT PRIMARY KEY,
    pn TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS stickers (
    hash TEXT PRIMARY KEY,
    raw BLOB NOT NULL,
    last_used INTEGER NOT NULL DEFAULT 0,
    weight REAL NOT NULL DEFAULT 0,
    path TEXT
);
CREATE TABLE IF NOT EXISTS group_receipts (
    chat TEXT NOT NULL,
    id TEXT NOT NULL,
    recipient TEXT NOT NULL,
    expected INTEGER NOT NULL DEFAULT 0,
    status INTEGER NOT NULL DEFAULT 0,
    delivered_at INTEGER,
    read_at INTEGER,
    played_at INTEGER,
    PRIMARY KEY (chat, id, recipient)
);
CREATE TRIGGER IF NOT EXISTS delete_group_receipts AFTER DELETE ON messages BEGIN
    DELETE FROM group_receipts WHERE chat = OLD.chat AND id = OLD.id;
END;
";

const CHAT_COLUMNS: &str =
    "c.id, c.name, c.kind, c.last_activity, c.unread, c.archived, c.pinned, c.muted_until,
                    m.from_me, m.sender_name, m.content, m.status, m.sender, c.participants, c.read_only,
                    c.pinned_at, c.ephemeral_expiration, c.locked";

const MIGRATIONS: &[(&str, &str, &str)] = &[
    ("messages", "thumbnail", "BLOB"),
    ("messages", "mentions", "TEXT NOT NULL DEFAULT '[]'"),
    ("chats", "participants", "TEXT NOT NULL DEFAULT '[]'"),
    ("chats", "read_only", "INTEGER NOT NULL DEFAULT 0"),
    ("messages", "forwarded", "INTEGER NOT NULL DEFAULT 0"),
    ("messages", "delivered_at", "INTEGER"),
    ("messages", "read_at", "INTEGER"),
    ("chats", "read_through", "INTEGER"),
    ("chats", "pending_read", "INTEGER"),
    ("chats", "ephemeral_expiration", "INTEGER"),
    ("chats", "ephemeral_setting_timestamp", "INTEGER"),
    ("chats", "pinned_at", "INTEGER NOT NULL DEFAULT 0"),
    ("chats", "pin_updated_at", "INTEGER"),
    ("chats", "mute_updated_at", "INTEGER"),
    ("chats", "locked", "INTEGER NOT NULL DEFAULT 0"),
    ("chats", "lock_updated_at", "INTEGER"),
];
const CHAT_JOIN: &str = "FROM chats c
             LEFT JOIN messages m ON m.chat = c.id AND m.rowid = (
                 SELECT rowid FROM messages WHERE chat = c.id ORDER BY timestamp DESC, rowid DESC LIMIT 1
             )";

const MESSAGE_COLUMNS: &str = "id, sender, sender_name, from_me, timestamp, content, status, quoted, reactions, edited, thumbnail, mentions, forwarded, delivered_at, read_at";

fn status_rank(status: Delivery) -> i64 {
    match status {
        Delivery::None => 0,
        Delivery::Pending => 1,
        Delivery::Sent => 2,
        Delivery::Delivered => 3,
        Delivery::Read => 4,
        Delivery::Played => 5,
        Delivery::Failed => 6,
    }
}

fn stamp_column(status: Delivery) -> Option<&'static str> {
    match status {
        Delivery::Delivered => Some("delivered_at"),
        Delivery::Read | Delivery::Played => Some("read_at"),
        _ => None,
    }
}

fn status_from_rank(rank: i64) -> Delivery {
    match rank {
        1 => Delivery::Pending,
        2 => Delivery::Sent,
        3 => Delivery::Delivered,
        4 => Delivery::Read,
        5 => Delivery::Played,
        6 => Delivery::Failed,
        _ => Delivery::None,
    }
}

fn kind_name(kind: ChatKind) -> &'static str {
    match kind {
        ChatKind::Direct => "direct",
        ChatKind::Group => "group",
        ChatKind::Broadcast => "broadcast",
    }
}

fn kind_from_name(name: &str) -> ChatKind {
    match name {
        "group" => ChatKind::Group,
        "broadcast" => ChatKind::Broadcast,
        _ => ChatKind::Direct,
    }
}

impl Archive {
    /// Unlocks the on-disk archive with its OS keyring key, migrating plaintext
    /// archives before their first encrypted use. Never falls back to plaintext.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let key = encryption::key_for(path)?;
        Self::open_with_key(path, &key)
    }

    fn open_with_key(path: &Path, key: &[u8; 32]) -> anyhow::Result<Self> {
        Ok(Self::prepare(encryption::open(path, key)?)?)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    #[cfg(test)]
    pub(crate) fn execute_batch_for_test(&self, sql: &str) -> Result<()> {
        self.connection.execute_batch(sql)
    }

    fn prepare(connection: Connection) -> Result<Self> {
        connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(SCHEMA)?;
        transaction.execute_batch(polls::SCHEMA)?;
        for (table, column, definition) in MIGRATIONS {
            let columns = transaction
                .prepare(&format!("PRAGMA table_info({table})"))?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>>>()?;
            let exists = columns.iter().any(|name| name == column);
            if !exists {
                transaction.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                ))?;
            }
        }
        transaction.execute_batch(
            "CREATE INDEX IF NOT EXISTS messages_quoted_status ON messages(status, from_me)
             WHERE quoted IS NOT NULL AND raw IS NOT NULL;
             CREATE INDEX IF NOT EXISTS messages_answer_parent ON messages(
                 chat, (CASE WHEN json_valid(quoted) THEN json_extract(quoted, '$.id') END), status)
             WHERE from_me = 1 AND quoted IS NOT NULL AND raw IS NOT NULL;",
        )?;
        transaction.commit()?;
        Ok(Self { connection })
    }

    /// Creates a chat or replaces a phone-number title with a better name.
    pub fn upsert_chat(&self, chat: &Chat) -> Result<()> {
        self.connection.execute(
            "INSERT INTO chats (id, name, kind, last_activity, unread, archived, pinned, muted_until, pinned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                last_activity = MAX(last_activity, excluded.last_activity),
                archived = excluded.archived,
                pinned = CASE WHEN pin_updated_at IS NULL THEN excluded.pinned ELSE pinned END,
                pinned_at = CASE WHEN pin_updated_at IS NULL THEN excluded.pinned_at ELSE pinned_at END,
                muted_until = CASE WHEN mute_updated_at IS NULL THEN excluded.muted_until ELSE muted_until END",
            params![
                chat.id,
                chat.name,
                kind_name(chat.kind),
                chat.last_activity,
                chat.unread,
                chat.archived,
                chat.pinned,
                chat.muted_until,
                chat.pinned_at,
            ],
        )?;
        Ok(())
    }

    pub fn ensure_chat(&self, id: &str, name: &str) -> Result<()> {
        self.connection.execute(
            "INSERT OR IGNORE INTO chats (id, name, kind) VALUES (?1, ?2, ?3)",
            params![id, name, kind_name(ChatKind::from_id(id))],
        )?;
        Ok(())
    }

    pub fn set_group_info(
        &self,
        id: &str,
        name: Option<&str>,
        participants: &[String],
        read_only: bool,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET name = COALESCE(?2, name), participants = ?3, read_only = ?4 WHERE id = ?1",
            params![
                id,
                name,
                serde_json::to_string(participants).unwrap_or_else(|_| "[]".into()),
                read_only
            ],
        )?;
        Ok(())
    }

    pub fn rename_chat(&self, id: &str, name: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    pub fn set_archived(&self, id: &str, archived: bool) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET archived = ?2 WHERE id = ?1",
            params![id, archived],
        )?;
        Ok(())
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<()> {
        self.set_pinned_at(id, pinned, jiff::Timestamp::now().as_millisecond())
    }

    /// Apply app-state in timestamp order. A later history chunk has no state
    /// version and must not overwrite a pin/unpin already received from sync.
    pub fn set_pinned_at(&self, id: &str, pinned: bool, timestamp: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET pinned = ?2, pinned_at = CASE WHEN ?2 THEN ?3 ELSE 0 END,
                pin_updated_at = ?3 WHERE id = ?1
                AND (pin_updated_at IS NULL OR pin_updated_at <= ?3)",
            params![id, pinned, timestamp],
        )?;
        Ok(())
    }

    pub fn set_muted(&self, id: &str, until: Option<i64>) -> Result<()> {
        self.set_muted_at(id, until, jiff::Timestamp::now().as_millisecond())
    }
    /// Keep mute/unmute actions across history replay, including actions that
    /// precede the initial chat snapshot and older app-state replay.
    pub fn set_muted_at(&self, id: &str, until: Option<i64>, timestamp: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET muted_until = ?2, mute_updated_at = ?3 WHERE id = ?1
                AND (mute_updated_at IS NULL OR mute_updated_at <= ?3)",
            params![id, until, timestamp],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub fn set_locked(&self, id: &str, locked: bool) -> Result<()> {
        self.set_locked_at(id, locked, jiff::Timestamp::now().as_millisecond())
    }

    /// Records history metadata only until app-state provides its version.
    pub fn set_locked_snapshot(&self, id: &str, locked: bool) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET locked = ?2 WHERE id = ?1 AND lock_updated_at IS NULL",
            params![id, locked],
        )?;
        Ok(())
    }

    /// Apply lock state in timestamp order, like pin and mute, so an old
    /// replay cannot undo a lock change just received from the phone.
    pub fn set_locked_at(&self, id: &str, locked: bool, timestamp: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET locked = ?2, lock_updated_at = ?3 WHERE id = ?1
                AND (lock_updated_at IS NULL OR lock_updated_at <= ?3)",
            params![id, locked, timestamp],
        )?;
        Ok(())
    }

    /// Applies disappearing-message metadata unless a newer setting is stored.
    pub fn set_ephemeral(&self, id: &str, expiration: u32, setting_timestamp: i64) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE chats SET ephemeral_expiration = ?2, ephemeral_setting_timestamp = ?3
             WHERE id = ?1 AND (ephemeral_setting_timestamp IS NULL OR ephemeral_setting_timestamp <= ?3)",
            params![id, expiration, setting_timestamp],
        )? > 0)
    }

    /// Returns the chat timer, including zero for an explicitly disabled timer.
    pub fn ephemeral_expiration(&self, id: &str) -> Result<Option<u32>> {
        self.connection
            .query_row(
                "SELECT ephemeral_expiration FROM chats WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map(Option::flatten)
    }

    pub fn mark_read(&self, id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET unread = 0,
             read_through = MAX(COALESCE(read_through, 0), last_activity) WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// A read on another device covers messages up to its position, not newer
    /// arrivals. Keep the position across restarts and history replays.
    pub fn mark_read_through(&self, id: &str, timestamp: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET read_through = MAX(COALESCE(read_through, 0), ?2),
             unread = MIN(unread, (SELECT COUNT(*) FROM messages
                 WHERE chat = ?1 AND from_me = 0
                 AND timestamp > MAX(COALESCE(read_through, 0), ?2))) WHERE id = ?1",
            params![id, timestamp],
        )?;
        Ok(())
    }

    /// A message id disambiguates rapid messages with the same second-level
    /// timestamp. A receipt for the first must leave the later messages unread.
    pub fn mark_read_to(&self, chat: &str, message: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET
             read_through = MAX(COALESCE(read_through, 0),
                 (SELECT timestamp FROM messages WHERE chat = ?1 AND id = ?2)),
             unread = MIN(unread, (SELECT COUNT(*) FROM messages m
                 JOIN messages boundary ON boundary.chat = m.chat AND boundary.id = ?2
                 WHERE m.chat = ?1 AND m.from_me = 0
                 AND (m.timestamp > boundary.timestamp
                      OR (m.timestamp = boundary.timestamp AND m.rowid > boundary.rowid))))
             WHERE id = ?1 AND EXISTS(SELECT 1 FROM messages WHERE chat = ?1 AND id = ?2)",
            params![chat, message],
        )?;
        Ok(())
    }

    pub fn read_through(&self, id: &str) -> Result<Option<i64>> {
        self.connection
            .query_row(
                "SELECT read_through FROM chats WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map(Option::flatten)
    }

    pub fn queue_read_sync(&self, id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET pending_read = read_through WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn pending_reads(&self) -> Result<Vec<(String, i64)>> {
        self.connection
            .prepare("SELECT id, pending_read FROM chats WHERE pending_read IS NOT NULL")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    }

    pub fn finish_read_sync(&self, id: &str, through: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET pending_read = NULL WHERE id = ?1 AND pending_read <= ?2",
            params![id, through],
        )?;
        Ok(())
    }

    /// The chat holding an incoming message, when exactly one chat has that id.
    /// Receipts can name a peer by a privacy id that is not mapped yet.
    pub fn incoming_chat_of(&self, id: &str) -> Result<Option<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT chat FROM messages WHERE id = ?1 AND from_me = 0 LIMIT 2")?;
        let chats = statement
            .query_map(params![id], |row| row.get(0))?
            .collect::<Result<Vec<String>>>()?;
        Ok(match chats.as_slice() {
            [chat] => Some(chat.clone()),
            _ => None,
        })
    }

    pub fn history_unread(&self, id: &str, unread: u32) -> Result<u32> {
        let Some(through) = self.read_through(id)? else {
            return Ok(unread);
        };
        let remaining: u32 = self.connection.query_row(
            "SELECT COUNT(*) FROM messages WHERE chat = ?1 AND from_me = 0 AND timestamp > ?2",
            params![id, through],
            |row| row.get(0),
        )?;
        Ok(unread.min(remaining))
    }

    pub fn set_unread(&self, id: &str, unread: u32) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET unread = ?2 WHERE id = ?1",
            params![id, unread],
        )?;
        Ok(())
    }

    pub fn chats(&self) -> Result<Vec<Chat>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {CHAT_COLUMNS} {CHAT_JOIN} ORDER BY c.last_activity DESC"
        ))?;
        let rows = statement.query_map([], row::chat_from_row)?;
        rows.collect()
    }

    pub fn chat(&self, id: &str) -> Result<Option<Chat>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {CHAT_COLUMNS} {CHAT_JOIN} WHERE c.id = ?1"
        ))?;
        statement
            .query_row(params![id], row::chat_from_row)
            .optional()
    }

    pub fn bump_unread(&self, id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE chats SET unread = unread + 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn unread_incoming(&self, chat: &str, limit: u32) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT id, sender FROM messages WHERE chat = ?1 AND from_me = 0
             AND timestamp >= COALESCE((SELECT read_through FROM chats WHERE id = ?1), -1)
             ORDER BY timestamp DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![chat, i64::from(limit)], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect()
    }

    /// Upserts a message, preserves the furthest delivery state, and updates
    /// chat activity. `raw` contains attachment metadata.
    pub fn insert_message(&self, message: &Message, raw: Option<&[u8]>) -> Result<()> {
        let existing: Option<StoredMessageProjection> = self
            .connection
            .query_row(
                "SELECT status, content, edited, raw, mentions FROM messages WHERE chat = ?1 AND id = ?2",
                params![message.chat, message.id],
                |row| {
                    Ok(StoredMessageProjection {
                        status: row.get(0)?,
                        content: row.get(1)?,
                        edited: row.get(2)?,
                        raw: row.get(3)?,
                        mentions: row.get(4)?,
                    })
                },
            )
            .optional()?;
        let status = match existing.as_ref().map(|row| row.status) {
            Some(rank)
                if rank == status_rank(Delivery::Failed)
                    && matches!(
                        message.status,
                        Delivery::Sent | Delivery::Delivered | Delivery::Read | Delivery::Played
                    ) =>
            {
                status_rank(message.status)
            }
            Some(rank)
                if message.status != Delivery::Failed && rank > status_rank(message.status) =>
            {
                rank
            }
            _ => status_rank(message.status),
        };
        let reactions = self.merged_reactions(message)?;
        let mut content = self
            .keep_answer(&message.chat, &message.id, &message.content)
            .into_owned();
        let mut edited = message.edited;
        let mut mentions = serde_json::to_string(&message.mentions).unwrap_or_default();
        let same_raw = existing
            .as_ref()
            .and_then(|row| row.raw.as_deref())
            .zip(raw)
            .is_some_and(|(old, new)| old == new);
        if let Some(existing) = existing
            && let Ok(existing_content) = serde_json::from_str::<Content>(&existing.content)
        {
            if same_raw
                && (matches!(&existing_content, Content::Revoked)
                    || existing.edited && !message.edited)
                && !matches!(&content, Content::Revoked)
            {
                content = existing_content;
                edited = existing.edited;
                mentions = existing.mentions;
            } else if same_raw {
                preserve_media_path(&existing_content, &mut content);
            }
        }
        self.connection.execute(
            "INSERT INTO messages (chat, id, sender, sender_name, from_me, timestamp, content, status, quoted, reactions, edited, raw, thumbnail, mentions, forwarded, delivered_at, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(chat, id) DO UPDATE SET
                sender_name = COALESCE(excluded.sender_name, sender_name),
                content = excluded.content,
                status = excluded.status,
                quoted = COALESCE(excluded.quoted, quoted),
                reactions = excluded.reactions,
                edited = ?18,
                raw = COALESCE(excluded.raw, raw),
                thumbnail = COALESCE(excluded.thumbnail, thumbnail),
                mentions = excluded.mentions,
                forwarded = excluded.forwarded,
                delivered_at = COALESCE(delivered_at, excluded.delivered_at),
                read_at = COALESCE(read_at, excluded.read_at)",
            params![
                message.chat,
                message.id,
                message.sender,
                message.sender_name,
                message.from_me,
                message.timestamp,
                serde_json::to_string(&content).unwrap_or_default(),
                status,
                message
                    .quoted
                    .as_ref()
                    .map(|quoted| serde_json::to_string(quoted).unwrap_or_default()),
                serde_json::to_string(&reactions).unwrap_or_default(),
                edited,
                raw,
                message.thumbnail.as_deref(),
                mentions,
                message.forwarded,
                message.delivered_at,
                message.read_at,
                edited,
            ],
        )?;
        self.connection.execute(
            "UPDATE chats SET last_activity = MAX(last_activity, ?2) WHERE id = ?1",
            params![message.chat, message.timestamp],
        )?;
        Ok(())
    }

    /// A buttons or list message decoded again (history replay, re-derive)
    /// has no record of the answer sent from here; keep the one already stored.
    fn keep_answer<'a>(
        &self,
        chat: &str,
        id: &str,
        content: &'a Content,
    ) -> std::borrow::Cow<'a, Content> {
        if !matches!(content, Content::Buttons { .. } | Content::List { .. })
            || content.answer().is_some()
        {
            return std::borrow::Cow::Borrowed(content);
        }
        let same_kind =
            |stored: &Content| std::mem::discriminant(stored) == std::mem::discriminant(content);
        let stored = self
            .connection
            .query_row(
                "SELECT content FROM messages WHERE chat = ?1 AND id = ?2",
                params![chat, id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Content>(&json).ok());
        stored
            .as_ref()
            .filter(|stored| same_kind(stored))
            .and_then(Content::answer)
            .filter(|answer| content.choice(answer).is_some())
            .and_then(|answer| content.with_answer(Some(answer.to_owned())))
            .map_or(std::borrow::Cow::Borrowed(content), std::borrow::Cow::Owned)
    }

    /// History rows often omit reactions. Keep any already stored when the
    /// incoming list is empty (wipe protection). A non-empty list is the
    /// current snapshot, so write it unchanged.
    fn merged_reactions(&self, message: &Message) -> Result<Vec<crate::model::Reaction>> {
        let incoming = &message.reactions;
        if incoming.is_empty()
            && let Some(existing) = self.message(&message.chat, &message.id)?
        {
            return Ok(existing.reactions);
        }
        Ok(incoming.clone())
    }

    /// Returns up to `limit` messages before an optional timestamp/id boundary,
    /// in ascending order.
    pub fn messages(
        &self,
        chat: &str,
        before: Option<(i64, &str)>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS}
             FROM messages
             WHERE chat = ?1 AND (timestamp < ?2 OR (timestamp = ?2 AND rowid <
                 (SELECT rowid FROM messages WHERE chat = ?1 AND id = ?3)))
             ORDER BY timestamp DESC, rowid DESC
             LIMIT ?4"
        ))?;
        let (before_time, before_id) = before.unwrap_or((i64::MAX, ""));
        let rows = statement
            .query_map(params![chat, before_time, before_id, limit as i64], |row| {
                row::message_from_row(chat, row)
            })?;
        let mut messages: Vec<Message> = rows.collect::<Result<_>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// Returns messages from `from` through `before`, ascending and limited.
    pub fn messages_range(
        &self,
        chat: &str,
        from: i64,
        before: (i64, &str),
        limit: usize,
    ) -> Result<Vec<Message>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS}
             FROM messages
             WHERE chat = ?1 AND timestamp >= ?2 AND (timestamp < ?3 OR (timestamp = ?3 AND rowid <
                 (SELECT rowid FROM messages WHERE chat = ?1 AND id = ?4)))
             ORDER BY timestamp ASC, rowid ASC
             LIMIT ?5"
        ))?;
        let rows = statement.query_map(
            params![chat, from, before.0, before.1, limit as i64],
            |row| row::message_from_row(chat, row),
        )?;
        rows.collect()
    }

    /// Returns raw messages for re-deriving fields in newer versions.
    pub fn rows_with_raw(&self) -> Result<Vec<(String, String, Vec<u8>)>> {
        let mut statement = self
            .connection
            .prepare("SELECT chat, id, raw FROM messages WHERE raw IS NOT NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect()
    }

    /// Pending quoted replies used to recover interrupted interactive sends.
    pub fn pending_quoted_messages(&self) -> Result<Vec<PendingQuotedMessage>> {
        let mut statement = self.connection.prepare(
            "SELECT chat, id, quoted, raw FROM messages WHERE status = ?1 AND from_me = 1 AND quoted IS NOT NULL AND raw IS NOT NULL"
        )?;
        let rows = statement.query_map(params![status_rank(Delivery::Pending)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        Ok(rows
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(chat, id, quoted, raw)| {
                let quoted = serde_json::from_str::<crate::model::Quoted>(&quoted)
                    .ok()?
                    .id;
                Some((chat, id, quoted, raw))
            })
            .collect())
    }

    /// Failed quoted replies: used only to clear an earlier attempt when retrying.
    pub fn failed_quoted_messages(
        &self,
        chat: &str,
        quoted_id: &str,
    ) -> Result<Vec<FailedQuotedMessage>> {
        let mut statement = self.connection.prepare(
            "SELECT id, raw FROM messages WHERE chat = ?1 AND status = ?2 AND from_me = 1 AND quoted IS NOT NULL AND raw IS NOT NULL
             AND (CASE WHEN json_valid(quoted) THEN json_extract(quoted, '$.id') END) = ?3"
        )?;
        let rows = statement.query_map(
            params![chat, status_rank(Delivery::Failed), quoted_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        rows.collect()
    }

    /// Accepted responses whose completion callback may have been lost on exit.
    pub fn confirmed_quoted_messages(&self) -> Result<Vec<(String, String, Vec<u8>)>> {
        let mut statement = self.connection.prepare(
            "SELECT m.chat, parent.id, m.raw FROM messages m
             JOIN messages parent ON parent.chat = m.chat AND parent.id =
                 (CASE WHEN json_valid(m.quoted) THEN json_extract(m.quoted, '$.id') END)
             WHERE m.from_me = 1 AND m.status BETWEEN ?1 AND ?2
                 AND m.quoted IS NOT NULL AND m.raw IS NOT NULL
                 AND (CASE WHEN json_valid(parent.content) THEN json_extract(parent.content, '$.kind') END) IN ('buttons', 'list')
                 AND (CASE WHEN json_valid(parent.content) THEN json_extract(parent.content, '$.answered') END) IS NULL"
        )?;
        let rows = statement.query_map(
            params![status_rank(Delivery::Sent), status_rank(Delivery::Played)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )?;
        rows.collect()
    }

    /// Accepted responses to a parent that may arrive after the response.
    pub fn confirmed_answers_for(&self, chat: &str, parent: &str) -> Result<Vec<Vec<u8>>> {
        let mut statement = self.connection.prepare(
            "SELECT raw FROM messages WHERE chat = ?1 AND from_me = 1
             AND status BETWEEN ?3 AND ?4 AND quoted IS NOT NULL AND raw IS NOT NULL
             AND (CASE WHEN json_valid(quoted) THEN json_extract(quoted, '$.id') END) = ?2",
        )?;
        let rows = statement.query_map(
            params![
                chat,
                parent,
                status_rank(Delivery::Sent),
                status_rank(Delivery::Played)
            ],
            |row| row.get(0),
        )?;
        rows.collect()
    }

    /// Replaces protobuf-derived fields while preserving local file state.
    pub fn set_derived(
        &self,
        chat: &str,
        id: &str,
        content: &Content,
        mentions: &[crate::model::MentionRef],
        thumbnail: Option<&[u8]>,
        forwarded: bool,
    ) -> Result<()> {
        let content = self.keep_answer(chat, id, content);
        self.connection.execute(
            "UPDATE messages SET content = ?3, mentions = ?4, thumbnail = COALESCE(?5, thumbnail), forwarded = ?6
             WHERE chat = ?1 AND id = ?2",
            params![
                chat,
                id,
                serde_json::to_string(&content).unwrap_or_default(),
                serde_json::to_string(mentions).unwrap_or_default(),
                thumbnail,
                forwarded
            ],
        )?;
        Ok(())
    }

    pub fn delete_message(&self, chat: &str, id: &str) -> Result<bool> {
        let deleted = self.connection.execute(
            "DELETE FROM messages WHERE chat = ?1 AND id = ?2",
            params![chat, id],
        )?;
        Ok(deleted > 0)
    }

    pub fn message(&self, chat: &str, id: &str) -> Result<Option<Message>> {
        self.connection
            .query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE chat = ?1 AND id = ?2"),
                params![chat, id],
                |row| row::message_from_row(chat, row),
            )
            .optional()
    }

    /// Returns the earliest message for phone-history requests.
    pub fn oldest(&self, chat: &str) -> Result<Option<Message>> {
        let id: Option<String> = self
            .connection
            .query_row(
                "SELECT id FROM messages WHERE chat = ?1 ORDER BY timestamp ASC, rowid ASC LIMIT 1",
                params![chat],
                |row| row.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.message(chat, &id),
            None => Ok(None),
        }
    }

    /// Returns a message's raw protobuf for attachment downloads.
    pub fn raw(&self, chat: &str, id: &str) -> Result<Option<Vec<u8>>> {
        self.connection
            .query_row(
                "SELECT raw FROM messages WHERE chat = ?1 AND id = ?2",
                params![chat, id],
                |row| row.get(0),
            )
            .optional()
            .map(Option::flatten)
    }

    /// Advances delivery state, except that `Failed` may replace it. Stores the
    /// first timestamp for each delivery stage.
    pub fn set_status(&self, chat: &str, id: &str, status: Delivery, at: i64) -> Result<bool> {
        let rank = status_rank(status);
        let changed = if status == Delivery::Failed {
            self.connection.execute(
                "UPDATE messages SET status = ?3 WHERE chat = ?1 AND id = ?2",
                params![chat, id, rank],
            )?
        } else if let Some(column) = stamp_column(status) {
            self.connection.execute(
                &format!(
                    "UPDATE messages SET status = ?3, {column} = COALESCE({column}, ?4)
                     WHERE chat = ?1 AND id = ?2 AND status < ?3"
                ),
                params![chat, id, rank, at],
            )?
        } else {
            self.connection.execute(
                "UPDATE messages SET status = ?3 WHERE chat = ?1 AND id = ?2 AND status < ?3",
                params![chat, id, rank],
            )?
        };
        Ok(changed > 0)
    }

    /// Advances outgoing messages through `timestamp` to `status` and returns changed ids.
    pub fn advance_statuses(
        &self,
        chat: &str,
        up_to: i64,
        status: Delivery,
        at: i64,
    ) -> Result<Vec<String>> {
        let rank = status_rank(status);
        let mut statement = self.connection.prepare(
            "SELECT id FROM messages WHERE chat = ?1 AND from_me = 1 AND timestamp <= ?2 AND status > 0 AND status < ?3",
        )?;
        let ids: Vec<String> = statement
            .query_map(params![chat, up_to, rank], |row| row.get(0))?
            .collect::<Result<_>>()?;
        if let Some(column) = stamp_column(status) {
            self.connection.execute(
                &format!(
                    "UPDATE messages SET status = ?3, {column} = COALESCE({column}, ?4)
                     WHERE chat = ?1 AND from_me = 1 AND timestamp <= ?2 AND status > 0 AND status < ?3"
                ),
                params![chat, up_to, rank, at],
            )?;
        } else {
            self.connection.execute(
                "UPDATE messages SET status = ?3 WHERE chat = ?1 AND from_me = 1 AND timestamp <= ?2 AND status > 0 AND status < ?3",
                params![chat, up_to, rank],
            )?;
        }
        Ok(ids)
    }

    pub fn set_content(
        &self,
        chat: &str,
        id: &str,
        content: &Content,
        edited: bool,
    ) -> Result<bool> {
        let changed = self.connection.execute(
            "UPDATE messages SET content = ?3, edited = ?4 WHERE chat = ?1 AND id = ?2",
            params![
                chat,
                id,
                serde_json::to_string(content).unwrap_or_default(),
                edited
            ],
        )?;
        Ok(changed > 0)
    }

    /// Replaces an edited text body and its mention metadata.
    pub fn set_edited_text(
        &self,
        chat: &str,
        id: &str,
        content: &Content,
        mentions: &[crate::model::MentionRef],
    ) -> Result<bool> {
        let changed = self.connection.execute(
            "UPDATE messages SET content = ?3, mentions = ?4, edited = 1 WHERE chat = ?1 AND id = ?2",
            params![
                chat,
                id,
                serde_json::to_string(content).unwrap_or_default(),
                serde_json::to_string(mentions).unwrap_or_default(),
            ],
        )?;
        Ok(changed > 0)
    }

    /// Upserts a reaction, or removes it when the emoji is empty.
    pub fn set_reaction(
        &self,
        chat: &str,
        id: &str,
        sender: &str,
        from_me: bool,
        emoji: &str,
    ) -> Result<Option<Message>> {
        let Some(mut message) = self.message(chat, id)? else {
            return Ok(None);
        };
        message
            .reactions
            .retain(|reaction| reaction.sender != sender);
        if !emoji.is_empty() {
            message.reactions.push(crate::model::Reaction {
                sender: sender.to_owned(),
                from_me,
                emoji: emoji.to_owned(),
            });
        }
        self.connection.execute(
            "UPDATE messages SET reactions = ?3 WHERE chat = ?1 AND id = ?2",
            params![
                chat,
                id,
                serde_json::to_string(&message.reactions).unwrap_or_default()
            ],
        )?;
        Ok(Some(message))
    }

    /// Clears all archived data during unlinking.
    pub fn clear(&self) -> Result<()> {
        self.connection
            .execute_batch("PRAGMA synchronous = FULL;")?;
        let clear = (|| {
            let transaction = self.connection.unchecked_transaction()?;
            transaction.execute_batch(
                "DELETE FROM poll_history; DELETE FROM poll_votes; DELETE FROM polls; DELETE FROM group_receipts; DELETE FROM messages; DELETE FROM chats; DELETE FROM contacts; DELETE FROM meta WHERE key <> 'logout_cleanup_required'; DELETE FROM lids; DELETE FROM stickers;",
            )?;
            transaction.commit()
        })();
        let restore = self
            .connection
            .execute_batch("PRAGMA synchronous = NORMAL;");
        clear?;
        restore?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod sticker_tests {
    use super::*;
    use crate::model::{Content, Delivery, Media, MediaState};

    fn sticker(chat: &str, id: &str, timestamp: i64, path: Option<&str>) -> Message {
        Message {
            id: id.into(),
            chat: chat.into(),
            sender: chat.into(),
            sender_name: None,
            from_me: true,
            timestamp,
            content: Content::Sticker {
                media: Media {
                    mime: "image/webp".into(),
                    size: 10,
                    width: Some(512),
                    height: Some(512),
                    path: path.map(std::path::PathBuf::from),
                    state: MediaState::Idle,
                },
                animated: false,
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

    #[test]
    fn phone_stickers_keep_the_latest_use_and_their_file() {
        let archive = Archive::in_memory().expect("opens");
        archive
            .upsert_phone_sticker("aa", b"one", 100, 0.5)
            .expect("stored");
        archive
            .upsert_phone_sticker("bb", b"two", 300, 0.1)
            .expect("stored");
        // Older repeated use does not lower the last-used time.
        archive
            .upsert_phone_sticker("aa", b"one", 50, 0.9)
            .expect("stored");
        let list = archive.phone_stickers().expect("lists");
        assert_eq!(
            list.iter().map(|s| s.hash.as_str()).collect::<Vec<_>>(),
            ["bb", "aa"]
        );
        assert_eq!(list[1].last_used, 100);
        assert!(list.iter().all(|s| s.path.is_none()));
        archive
            .set_sticker_path("aa", Path::new("/tmp/aa.webp"))
            .expect("filed");
        let list = archive.phone_stickers().expect("lists");
        assert_eq!(list[1].path.as_deref(), Some(Path::new("/tmp/aa.webp")));
    }

    #[test]
    fn unfetched_stickers_are_listed_for_the_picker_and_fetched_ones_are_not() {
        let archive = Archive::in_memory().expect("opens");
        archive.ensure_chat("a@s.whatsapp.net", "A").expect("chat");
        archive
            .insert_message(&sticker("a@s.whatsapp.net", "s1", 10, None), Some(b"raw"))
            .expect("inserted");
        archive
            .insert_message(
                &sticker("a@s.whatsapp.net", "s2", 20, Some("/nowhere/s2.webp")),
                Some(b"raw"),
            )
            .expect("inserted");
        archive
            .insert_message(
                &Message {
                    from_me: false,
                    ..sticker("a@s.whatsapp.net", "received", 30, None)
                },
                Some(b"raw"),
            )
            .expect("received");
        let missing = archive.stickers_without_file(10).expect("lists");
        assert_eq!(
            missing,
            vec![("a@s.whatsapp.net".to_owned(), "s1".to_owned())]
        );
        // Exclude missing local files.
        assert!(archive.recent_stickers(10).expect("lists").is_empty());
    }

    #[test]
    fn recent_stickers_skip_sends_without_a_raw_message() {
        let file = std::env::temp_dir().join(format!("zaptide-recent-{}.webp", std::process::id()));
        std::fs::write(&file, b"webp").expect("file");
        let path = file.to_str().expect("utf-8");
        let archive = Archive::in_memory().expect("opens");
        archive.ensure_chat("a@s.whatsapp.net", "A").expect("chat");
        archive
            .insert_message(
                &sticker("a@s.whatsapp.net", "s1", 10, Some(path)),
                Some(b"raw"),
            )
            .expect("inserted");
        // A newer pending send of the same picker file has no raw message yet.
        archive
            .insert_message(&sticker("a@s.whatsapp.net", "s2", 20, Some(path)), None)
            .expect("inserted");
        let received =
            std::env::temp_dir().join(format!("zaptide-received-{}.webp", std::process::id()));
        std::fs::write(&received, b"received").expect("file");
        archive
            .insert_message(
                &Message {
                    from_me: false,
                    ..sticker("a@s.whatsapp.net", "received", 30, received.to_str())
                },
                Some(b"raw"),
            )
            .expect("received");
        let recent = archive.recent_stickers(10).expect("lists");
        std::fs::remove_file(&file).ok();
        std::fs::remove_file(&received).ok();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].raw.as_deref(), Some(&b"raw"[..]));
    }
}

#[cfg(test)]
mod media_path_tests {
    use super::*;
    use crate::model::{Content, Delivery, Media, MediaState};

    fn picture(id: &str) -> Message {
        Message {
            id: id.into(),
            chat: "a@s.whatsapp.net".into(),
            sender: "a@s.whatsapp.net".into(),
            sender_name: None,
            from_me: false,
            timestamp: 1,
            content: Content::Image {
                media: Media {
                    mime: "image/jpeg".into(),
                    size: 10,
                    width: None,
                    height: None,
                    path: None,
                    state: MediaState::Idle,
                },
                caption: None,
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

    #[test]
    fn attachment_paths_can_be_listed_moved_and_forgotten() {
        let archive = Archive::in_memory().expect("opens");
        archive.ensure_chat("a@s.whatsapp.net", "A").expect("chat");
        archive
            .insert_message(&picture("p1"), None)
            .expect("inserted");
        assert!(archive.media_paths().expect("lists").is_empty());
        archive
            .set_media_path("a@s.whatsapp.net", "p1", Path::new("/old/media/p1.jpg"))
            .expect("filed");
        let listed = archive.media_paths().expect("lists");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].2, Path::new("/old/media/p1.jpg"));
        archive
            .clear_media_path("a@s.whatsapp.net", "p1")
            .expect("cleared");
        assert!(archive.media_paths().expect("lists").is_empty());
    }
}
