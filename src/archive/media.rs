use super::*;

pub(super) const SCHEMA: &str = "
CREATE INDEX IF NOT EXISTS messages_media_path ON messages(media_path) WHERE media_path IS NOT NULL;
CREATE INDEX IF NOT EXISTS messages_sticker_path ON messages(kind, from_me, media_path, timestamp DESC)
    WHERE raw IS NOT NULL;
";

impl Archive {
    pub fn set_media_path(&self, chat: &str, id: &str, path: &Path) -> Result<Option<Message>> {
        self.put_media_path(chat, id, Some(path))
    }

    pub fn clear_media_path(&self, chat: &str, id: &str) -> Result<Option<Message>> {
        self.put_media_path(chat, id, None)
    }

    fn put_media_path(&self, chat: &str, id: &str, path: Option<&Path>) -> Result<Option<Message>> {
        let Some(mut message) = self.message(chat, id)? else {
            return Ok(None);
        };
        let Some(media) = message.content.media_mut() else {
            return Ok(None);
        };
        media.path = path.map(Path::to_path_buf);
        self.set_content(chat, id, &message.content, message.edited)?;
        Ok(Some(message))
    }

    #[cfg(test)]
    pub fn media_paths(&self) -> Result<Vec<(String, String, std::path::PathBuf)>> {
        let mut statement = self
            .connection
            .prepare("SELECT chat, id, media_path FROM messages WHERE media_path IS NOT NULL")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                std::path::PathBuf::from(row.get::<_, String>(2)?),
            ))
        })?;
        rows.collect()
    }

    /// The next `limit` rows after `rowid` with their attachment path, if
    /// any, so a relocation pass can resume where it stopped.
    pub fn media_paths_after(&self, after: i64, limit: usize) -> Result<Vec<MediaPathRow>> {
        let mut statement = self.connection.prepare(
            "SELECT rowid, chat, id, json_extract(content, '$.media.path') FROM messages
             WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after, limit as i64], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, Option<String>>(3)?
                    .map(std::path::PathBuf::from),
            ))
        })?;
        rows.collect()
    }

    /// Downloaded files that may be deleted to free space: not stickers, which
    /// the picker reuses, and only messages that can fetch the file again. A
    /// pending or failed send has no raw message, and its file is the only copy.
    pub fn expirable_media(&self) -> Result<Vec<(String, String, std::path::PathBuf)>> {
        let mut statement = self.connection.prepare(
            "SELECT chat, id, media_path
             FROM messages
             WHERE media_path IS NOT NULL
               AND raw IS NOT NULL
               AND status NOT IN (?1, ?2)
               AND kind <> 'sticker'",
        )?;
        let rows = statement.query_map(
            params![
                status_rank(Delivery::Pending),
                status_rank(Delivery::Failed)
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    std::path::PathBuf::from(row.get::<_, String>(2)?),
                ))
            },
        )?;
        rows.collect()
    }

    pub fn recent_stickers(&self, limit: usize) -> Result<Vec<ArchivedSticker>> {
        let mut statement = self.connection.prepare(
            "SELECT media_path, MAX(timestamp), raw
             FROM messages
             WHERE kind = 'sticker' AND media_path IS NOT NULL
               AND from_me = 1
               -- A pending or failed send has no raw message to resend.
               AND raw IS NOT NULL
             GROUP BY media_path
             ORDER BY 2 DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok(ArchivedSticker {
                last_used: row.get(1)?,
                path: std::path::PathBuf::from(row.get::<_, String>(0)?),
                raw: row.get(2)?,
            })
        })?;
        Ok(rows
            .flatten()
            .filter(|sticker| sticker.path.exists())
            .collect())
    }

    pub fn stickers_without_file(&self, limit: usize) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT chat, id FROM messages
             WHERE kind = 'sticker'
               AND from_me = 1
               AND media_path IS NULL
               AND raw IS NOT NULL
             ORDER BY timestamp DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect()
    }

    /// Upserts a recent phone sticker, preserving the latest use time.
    pub fn upsert_phone_sticker(
        &self,
        hash: &str,
        raw: &[u8],
        last_used: i64,
        weight: f32,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO stickers (hash, raw, last_used, weight) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(hash) DO UPDATE SET
                 raw = excluded.raw,
                 last_used = MAX(stickers.last_used, excluded.last_used),
                 weight = excluded.weight",
            params![hash, raw, last_used, weight as f64],
        )?;
        Ok(())
    }

    pub fn phone_stickers(&self) -> Result<Vec<PhoneSticker>> {
        let mut statement = self.connection.prepare(
            "SELECT hash, raw, last_used, path FROM stickers
             ORDER BY last_used DESC, weight DESC
             LIMIT 120",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(PhoneSticker {
                hash: row.get(0)?,
                raw: row.get(1)?,
                last_used: row.get(2)?,
                path: row
                    .get::<_, Option<String>>(3)?
                    .map(std::path::PathBuf::from),
            })
        })?;
        rows.collect()
    }

    pub fn set_sticker_path(&self, hash: &str, path: &Path) -> Result<()> {
        self.connection.execute(
            "UPDATE stickers SET path = ?2 WHERE hash = ?1",
            params![hash, path.to_string_lossy()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_media_queries_match_legacy_json_expressions() {
        let archive = Archive::in_memory().unwrap();
        let rows = [
            (
                "image",
                r#"{"kind":"image","media":{"path":"/fixture/image"}}"#,
                0,
                2,
                Some(vec![1u8]),
            ),
            (
                "sticker",
                r#"{"kind":"sticker","media":{"path":"/fixture/sticker"}}"#,
                1,
                2,
                Some(vec![2]),
            ),
            (
                "missing",
                r#"{"kind":"sticker","media":{"path":null}}"#,
                1,
                2,
                Some(vec![3]),
            ),
            (
                "absent",
                r#"{"kind":"sticker","media":{}}"#,
                1,
                2,
                Some(vec![4]),
            ),
            ("text", r#"{"kind":"text","text":"fixture"}"#, 0, 2, None),
            (
                "pending",
                r#"{"kind":"video","media":{"path":"/fixture/video"}}"#,
                1,
                1,
                Some(vec![5]),
            ),
            (
                "failed",
                r#"{"kind":"audio","media":{"path":"/fixture/audio"}}"#,
                1,
                6,
                Some(vec![6]),
            ),
            (
                "no_raw",
                r#"{"kind":"document","media":{"path":"/fixture/doc"}}"#,
                1,
                2,
                None,
            ),
        ];
        for (i, (id, content, from_me, status, raw)) in rows.into_iter().enumerate() {
            archive.connection.execute("INSERT INTO messages(chat,id,sender,from_me,timestamp,content,status,raw) VALUES ('fixture',?1,'fixture',?2,?3,?4,?5,?6)", params![id, from_me, i as i64, content, status, raw]).unwrap();
        }
        for (old, new) in [
            (
                "SELECT id FROM messages WHERE json_extract(content, '$.media.path') IS NOT NULL",
                "SELECT id FROM messages WHERE media_path IS NOT NULL",
            ),
            (
                "SELECT id FROM messages WHERE json_extract(content, '$.media.path') IS NOT NULL AND raw IS NOT NULL AND status NOT IN (1,6) AND json_extract(content, '$.kind') <> 'sticker'",
                "SELECT id FROM messages WHERE media_path IS NOT NULL AND raw IS NOT NULL AND status NOT IN (1,6) AND kind <> 'sticker'",
            ),
            (
                "SELECT id FROM messages WHERE json_extract(content, '$.kind') = 'sticker' AND json_extract(content, '$.media.path') IS NOT NULL AND from_me = 1 AND raw IS NOT NULL",
                "SELECT id FROM messages WHERE kind = 'sticker' AND media_path IS NOT NULL AND from_me = 1 AND raw IS NOT NULL",
            ),
            (
                "SELECT id FROM messages WHERE json_extract(content, '$.kind') = 'sticker' AND json_extract(content, '$.media.path') IS NULL AND from_me = 1 AND raw IS NOT NULL",
                "SELECT id FROM messages WHERE kind = 'sticker' AND media_path IS NULL AND from_me = 1 AND raw IS NOT NULL",
            ),
        ] {
            let ids = |sql: &str| {
                let mut statement = archive.connection.prepare(sql).unwrap();
                let mut ids = statement
                    .query_map([], |r| r.get::<_, String>(0))
                    .unwrap()
                    .collect::<Result<Vec<_>>>()
                    .unwrap();
                ids.sort();
                ids
            };
            assert_eq!(ids(old), ids(new));
        }
    }

    #[test]
    fn media_columns_migrate_idempotently_and_index_queries() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA_FOR_LEGACY).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 42;")
            .unwrap();
        let archive = Archive::prepare(connection).unwrap();
        let version: i64 = archive
            .connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 42);
        let archive = Archive::prepare(archive.connection).unwrap();
        assert_eq!(archive.media_paths().unwrap().len(), 1);
        assert_eq!(
            archive.stickers_without_file(10).unwrap(),
            vec![("chat".into(), "missing".into())]
        );
        assert_eq!(archive.expirable_media().unwrap().len(), 1);
        for (query, index) in [
            (
                "SELECT chat, id, media_path FROM messages WHERE media_path IS NOT NULL",
                "messages_media_path",
            ),
            (
                "SELECT chat, id, media_path FROM messages WHERE media_path IS NOT NULL AND raw IS NOT NULL AND status NOT IN (1, 6) AND kind <> 'sticker'",
                "messages_media_path",
            ),
            (
                "SELECT media_path, MAX(timestamp), raw FROM messages WHERE kind = 'sticker' AND media_path IS NOT NULL AND from_me = 1 AND raw IS NOT NULL GROUP BY media_path ORDER BY 2 DESC LIMIT 80",
                "messages_sticker_path",
            ),
            (
                "SELECT chat, id FROM messages WHERE kind = 'sticker' AND from_me = 1 AND media_path IS NULL AND raw IS NOT NULL ORDER BY timestamp DESC LIMIT 80",
                "messages_sticker_path",
            ),
        ] {
            let mut statement = archive
                .connection
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap();
            let plan = statement
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            println!("{query}: {plan:?}");
            assert!(plan.iter().any(|line| line.contains(index)), "{plan:?}");
        }
    }

    const SCHEMA_FOR_LEGACY: &str = r#"
        CREATE TABLE messages (chat TEXT NOT NULL, id TEXT NOT NULL, sender TEXT NOT NULL,
            sender_name TEXT, from_me INTEGER NOT NULL, timestamp INTEGER NOT NULL,
            content TEXT NOT NULL, status INTEGER NOT NULL DEFAULT 0, quoted TEXT,
            reactions TEXT NOT NULL DEFAULT '[]', edited INTEGER NOT NULL DEFAULT 0,
            raw BLOB, PRIMARY KEY(chat, id));
        INSERT INTO messages(chat,id,sender,from_me,timestamp,content,status,raw) VALUES
            ('chat','image','fixture',0,1,'{"kind":"image","media":{"path":"/synthetic/image"}}',2,x'01'),
            ('chat','missing','fixture',1,2,'{"kind":"sticker","media":{"path":null}}',2,x'02'),
            ('chat','text','fixture',0,3,'{"kind":"text","text":"fixture"}',2,x'03');
    "#;
}
