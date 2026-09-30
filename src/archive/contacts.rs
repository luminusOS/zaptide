use super::*;
use crate::model::Contact;

impl Archive {
    /// Stores a privacy id mapping and carries early mute/pin/lock sync to the
    /// canonical chat. Returns whether that chat's preferences were touched.
    pub fn put_lid(&self, lid: &str, pn: &str) -> Result<bool> {
        self.connection.execute(
            "INSERT INTO lids (lid, pn) VALUES (?1, ?2) ON CONFLICT(lid) DO UPDATE SET pn = excluded.pn",
            params![lid, pn],
        )?;
        self.merge_group_recipient(&format!("{lid}@lid"), &format!("{pn}@s.whatsapp.net"))?;
        let changed = self.connection.execute(
            "INSERT INTO chats (id, name, kind, pinned, pinned_at, pin_updated_at,
                muted_until, mute_updated_at, locked, lock_updated_at)
             SELECT ?2, ?3, 'direct', pinned, pinned_at, pin_updated_at,
                muted_until, mute_updated_at, locked, lock_updated_at FROM chats WHERE id = ?1
                AND (pin_updated_at IS NOT NULL OR mute_updated_at IS NOT NULL
                    OR lock_updated_at IS NOT NULL OR locked)
             ON CONFLICT(id) DO UPDATE SET
                pinned = CASE WHEN excluded.pin_updated_at >= COALESCE(pin_updated_at, -1)
                    THEN excluded.pinned ELSE pinned END,
                pinned_at = CASE WHEN excluded.pin_updated_at >= COALESCE(pin_updated_at, -1)
                    THEN excluded.pinned_at ELSE pinned_at END,
                pin_updated_at = NULLIF(MAX(COALESCE(pin_updated_at, -1), COALESCE(excluded.pin_updated_at, -1)), -1),
                muted_until = CASE WHEN excluded.mute_updated_at >= COALESCE(mute_updated_at, -1)
                    THEN excluded.muted_until ELSE muted_until END,
                mute_updated_at = NULLIF(MAX(COALESCE(mute_updated_at, -1), COALESCE(excluded.mute_updated_at, -1)), -1),
                locked = CASE WHEN excluded.lock_updated_at >= COALESCE(lock_updated_at, -1)
                    THEN excluded.locked
                    WHEN lock_updated_at IS NULL AND excluded.lock_updated_at IS NULL
                    THEN MAX(locked, excluded.locked) ELSE locked END,
                lock_updated_at = NULLIF(MAX(COALESCE(lock_updated_at, -1), COALESCE(excluded.lock_updated_at, -1)), -1)",
            params![format!("{lid}@lid"), format!("{pn}@s.whatsapp.net"), pn],
        )?;
        Ok(changed > 0)
    }

    pub fn lids(&self) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare("SELECT lid, pn FROM lids")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    pub fn upsert_contact(&self, contact: &Contact) -> Result<()> {
        self.connection.execute(
            "INSERT INTO contacts (id, full_name, push_name) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                full_name = COALESCE(excluded.full_name, full_name),
                push_name = COALESCE(excluded.push_name, push_name)",
            params![contact.id, contact.full_name, contact.push_name],
        )?;
        Ok(())
    }

    pub fn contact(&self, id: &str) -> Result<Option<Contact>> {
        self.connection
            .query_row(
                "SELECT id, full_name, push_name FROM contacts WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Contact {
                        id: row.get(0)?,
                        full_name: row.get(1)?,
                        push_name: row.get(2)?,
                    })
                },
            )
            .optional()
    }

    pub fn contacts(&self) -> Result<Vec<Contact>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, full_name, push_name FROM contacts")?;
        let rows = statement.query_map([], |row| {
            Ok(Contact {
                id: row.get(0)?,
                full_name: row.get(1)?,
                push_name: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}
