use super::*;

impl Archive {
    pub fn set_media_path(&self, chat: &str, id: &str, path: &Path) -> Result<Option<Message>> {
        self.put_media_path(chat, id, Some(path))
    }

    /// Clears an attachment path so it can be downloaded again.
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

    pub fn media_paths(&self) -> Result<Vec<(String, String, std::path::PathBuf)>> {
        let mut statement = self.connection.prepare(
            "SELECT chat, id, json_extract(content, '$.media.path') AS path
             FROM messages WHERE path IS NOT NULL",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                std::path::PathBuf::from(row.get::<_, String>(2)?),
            ))
        })?;
        rows.collect()
    }

    /// Returns downloaded stickers we sent for the picker, newest first.
    pub fn recent_stickers(&self, limit: usize) -> Result<Vec<ArchivedSticker>> {
        let mut statement = self.connection.prepare(
            "SELECT json_extract(content, '$.media.path') AS path, MAX(timestamp), raw
             FROM messages
             WHERE json_extract(content, '$.kind') = 'sticker' AND path IS NOT NULL
               AND from_me = 1
               -- A pending or failed send has no raw message to resend.
               AND raw IS NOT NULL
             GROUP BY path
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

    /// Returns undownloaded stickers we sent, newest first.
    pub fn stickers_without_file(&self, limit: usize) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT chat, id FROM messages
             WHERE json_extract(content, '$.kind') = 'sticker'
               AND from_me = 1
               AND json_extract(content, '$.media.path') IS NULL
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

    /// Returns recent phone stickers by latest use.
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
