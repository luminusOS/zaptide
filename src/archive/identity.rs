use super::*;

impl Archive {
    pub(super) fn resolve_chat<'a>(&self, chat: &'a str) -> Result<std::borrow::Cow<'a, str>> {
        let Some(lid) = chat.strip_suffix("@lid") else {
            return Ok(std::borrow::Cow::Borrowed(chat));
        };
        let phone: Option<String> = self
            .connection
            .query_row("SELECT pn FROM lids WHERE lid = ?1", [lid], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(match phone {
            Some(phone) => std::borrow::Cow::Owned(format!("{phone}@s.whatsapp.net")),
            None => std::borrow::Cow::Borrowed(chat),
        })
    }

    pub(super) fn merge_lid_chat(&self, alias: &str, canonical: &str) -> Result<bool> {
        let unread = if self
            .connection
            .query_row("SELECT 1 FROM chats WHERE id = ?1", [alias], |_| Ok(()))
            .optional()?
            .is_some()
        {
            Some(self.merged_identity_unread(alias, canonical)?)
        } else {
            None
        };
        let phone = canonical
            .strip_suffix("@s.whatsapp.net")
            .unwrap_or(canonical);
        let changed = self.connection.execute(
            "INSERT INTO chats (id, name, kind, last_activity, unread, archived,
                pinned, pinned_at, muted_until,
                read_through, pending_read, ephemeral_expiration, ephemeral_setting_timestamp)
             SELECT ?2, name, 'direct', last_activity, unread, archived,
                pinned, pinned_at, muted_until,
                read_through, pending_read, ephemeral_expiration, ephemeral_setting_timestamp
             FROM chats WHERE id = ?1
             ON CONFLICT(id) DO UPDATE SET
                name = CASE WHEN chats.name IN ('', ?2, ?3) THEN excluded.name ELSE chats.name END,
                last_activity = MAX(chats.last_activity, excluded.last_activity),
                archived = MAX(chats.archived, excluded.archived),
                pinned = CASE WHEN chats.pin_updated_at IS NULL
                    THEN MAX(chats.pinned, excluded.pinned) ELSE chats.pinned END,
                pinned_at = CASE WHEN chats.pin_updated_at IS NULL
                    THEN MAX(chats.pinned_at, excluded.pinned_at) ELSE chats.pinned_at END,
                muted_until = CASE WHEN chats.mute_updated_at IS NULL
                    THEN COALESCE(chats.muted_until, excluded.muted_until) ELSE chats.muted_until END,
                read_through = NULLIF(MAX(COALESCE(chats.read_through, -1),
                    COALESCE(excluded.read_through, -1)), -1),
                pending_read = NULLIF(MAX(COALESCE(chats.pending_read, -1),
                    COALESCE(excluded.pending_read, -1)), -1),
                ephemeral_expiration = CASE
                    WHEN excluded.ephemeral_setting_timestamp > COALESCE(chats.ephemeral_setting_timestamp, -1)
                    THEN excluded.ephemeral_expiration
                    ELSE COALESCE(chats.ephemeral_expiration, excluded.ephemeral_expiration) END,
                ephemeral_setting_timestamp = NULLIF(MAX(COALESCE(chats.ephemeral_setting_timestamp, -1),
                    COALESCE(excluded.ephemeral_setting_timestamp, -1)), -1)",
            params![alias, canonical, phone],
        )?;
        let contacts = self.connection.execute(
            "INSERT INTO contacts (id, full_name, push_name)
             SELECT ?2, full_name, push_name FROM contacts WHERE id = ?1
             ON CONFLICT(id) DO UPDATE SET
                full_name = COALESCE(contacts.full_name, excluded.full_name),
                push_name = COALESCE(contacts.push_name, excluded.push_name)",
            params![alias, canonical],
        )?;
        let collisions = {
            let mut statement = self.connection.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages WHERE chat = ?1
                 AND id IN (SELECT id FROM messages WHERE chat = ?2)
                 ORDER BY timestamp, rowid"
            ))?;
            statement
                .query_map(params![alias, canonical], |row| {
                    row::message_from_row(canonical, row)
                })?
                .collect::<Result<Vec<_>>>()?
        };
        for mut message in collisions {
            let mut stored = self
                .message(canonical, &message.id)?
                .ok_or(rusqlite::Error::InvalidQuery)?;
            if stored.from_me != message.from_me {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if message.sender == alias {
                message.sender = canonical.to_owned();
            }
            let prefer_alias = !matches!(stored.content, Content::Revoked)
                && (matches!(message.content, Content::Revoked)
                    || message.edited && !stored.edited);
            let (primary, secondary) = if prefer_alias {
                (&mut message, &stored)
            } else {
                (&mut stored, &message)
            };
            let primary_content = primary.content.clone();
            preserve_media_path(&secondary.content, &mut primary.content);
            preserve_media_path(&primary_content, &mut primary.content);
            let secondary_content = serde_json::to_string(&secondary.content).unwrap_or_default();
            primary.content = keep_answer(Some(&secondary_content), &primary.content).into_owned();
            let confirmed = [primary.status, secondary.status]
                .into_iter()
                .filter(|status| {
                    matches!(
                        status,
                        Delivery::Sent | Delivery::Delivered | Delivery::Read | Delivery::Played
                    )
                })
                .max();
            primary.status = confirmed.unwrap_or(primary.status.max(secondary.status));
            for reaction in primary.reactions.iter_mut() {
                if reaction.sender == alias {
                    reaction.sender = canonical.to_owned();
                }
            }
            for reaction in &secondary.reactions {
                let mut reaction = reaction.clone();
                if reaction.sender == alias {
                    reaction.sender = canonical.to_owned();
                }
                if !primary
                    .reactions
                    .iter()
                    .any(|known| known.sender == reaction.sender)
                {
                    primary.reactions.push(reaction);
                }
            }
            let raw = if prefer_alias {
                self.raw_exact(alias, &primary.id)?
                    .or(self.raw_exact(canonical, &primary.id)?)
            } else {
                self.raw_exact(canonical, &primary.id)?
                    .or(self.raw_exact(alias, &primary.id)?)
            };
            self.connection.execute(
                "UPDATE messages SET content = ?3, edited = ?4, mentions = ?5,
                    status = ?6, reactions = ?7, raw = ?8,
                    sender_name = COALESCE(?9, ?10), quoted = COALESCE(?11, ?12),
                    thumbnail = COALESCE(?13, ?14), forwarded = ?15,
                    delivered_at = COALESCE(delivered_at, ?16),
                    read_at = COALESCE(read_at, ?17),
                    sender = CASE WHEN sender = ?18 THEN ?1 ELSE sender END
                 WHERE chat = ?1 AND id = ?2",
                params![
                    canonical,
                    primary.id,
                    serde_json::to_string(&primary.content).unwrap_or_default(),
                    primary.edited,
                    serde_json::to_string(&primary.mentions).unwrap_or_default(),
                    status_rank(primary.status),
                    serde_json::to_string(&primary.reactions).unwrap_or_default(),
                    raw,
                    primary.sender_name,
                    secondary.sender_name,
                    primary
                        .quoted
                        .as_ref()
                        .map(|quoted| serde_json::to_string(quoted).unwrap_or_default()),
                    secondary
                        .quoted
                        .as_ref()
                        .map(|quoted| serde_json::to_string(quoted).unwrap_or_default()),
                    primary.thumbnail,
                    secondary.thumbnail,
                    primary.forwarded || secondary.forwarded,
                    primary.delivered_at.or(secondary.delivered_at),
                    primary.read_at.or(secondary.read_at),
                    alias
                ],
            )?;
        }
        let moved = self.connection.execute(
            "UPDATE OR IGNORE messages SET chat = ?2,
                sender = CASE WHEN sender = ?1 THEN ?2 ELSE sender END WHERE chat = ?1",
            params![alias, canonical],
        )?;
        self.connection.execute(
            "INSERT INTO stars (chat, id, starred, updated_at)
             SELECT ?2, id, starred, updated_at FROM stars WHERE chat = ?1
             ON CONFLICT(chat, id) DO UPDATE SET starred = excluded.starred, updated_at = excluded.updated_at
             WHERE excluded.updated_at > stars.updated_at",
            params![alias, canonical],
        )?;
        let merged_pins = self.connection.execute(
            "INSERT INTO pins (chat, id, pinned, pinned_by, pinned_at_ms, expires_at_ms)
             SELECT ?2, id, pinned, pinned_by, pinned_at_ms, expires_at_ms FROM pins WHERE chat = ?1
             ON CONFLICT(chat, id) DO UPDATE SET pinned = excluded.pinned,
                 pinned_by = excluded.pinned_by, pinned_at_ms = excluded.pinned_at_ms,
                 expires_at_ms = excluded.expires_at_ms
             WHERE excluded.pinned_at_ms > pins.pinned_at_ms
                OR (excluded.pinned_at_ms = pins.pinned_at_ms AND excluded.pinned < pins.pinned)",
            params![alias, canonical],
        )?;
        self.connection.execute(
            "INSERT OR IGNORE INTO polls (chat, id, creator, secret)
             SELECT ?2, id, creator, secret FROM polls WHERE chat = ?1",
            params![alias, canonical],
        )?;
        self.connection.execute(
            "INSERT OR IGNORE INTO poll_history (chat, id)
             SELECT ?2, id FROM poll_history WHERE chat = ?1",
            params![alias, canonical],
        )?;
        self.connection.execute(
            "INSERT INTO poll_votes (chat, poll, voter, sender, update_id, at, from_me, choices, encrypted, attempted)
             SELECT ?2, poll, voter, sender, update_id, at, from_me, choices, encrypted, attempted
             FROM poll_votes WHERE chat = ?1
             ON CONFLICT(chat, poll, voter) DO UPDATE SET
                sender = excluded.sender, update_id = excluded.update_id, at = excluded.at,
                from_me = excluded.from_me, choices = excluded.choices,
                encrypted = excluded.encrypted, attempted = excluded.attempted
             WHERE (excluded.at, excluded.update_id) > (poll_votes.at, poll_votes.update_id)
                OR ((excluded.at, excluded.update_id) = (poll_votes.at, poll_votes.update_id)
                    AND poll_votes.choices IS NULL AND excluded.choices IS NOT NULL)",
            params![alias, canonical],
        )?;
        self.connection.execute(
            "INSERT INTO group_receipts (chat, id, recipient, expected, status, delivered_at, read_at, played_at)
             SELECT ?2, id, recipient, expected, status, delivered_at, read_at, played_at
             FROM group_receipts WHERE chat = ?1
             ON CONFLICT(chat, id, recipient) DO UPDATE SET
                expected = MAX(group_receipts.expected, excluded.expected),
                status = MAX(group_receipts.status, excluded.status),
                delivered_at = COALESCE(group_receipts.delivered_at, excluded.delivered_at),
                read_at = COALESCE(group_receipts.read_at, excluded.read_at),
                played_at = COALESCE(group_receipts.played_at, excluded.played_at)",
            params![alias, canonical],
        )?;
        for table in [
            "stars",
            "pins",
            "polls",
            "poll_history",
            "poll_votes",
            "group_receipts",
            "messages",
        ] {
            self.connection
                .execute(&format!("DELETE FROM {table} WHERE chat = ?1"), [alias])?;
        }
        self.connection
            .execute("DELETE FROM chats WHERE id = ?1", [alias])?;
        self.connection
            .execute("DELETE FROM contacts WHERE id = ?1", [alias])?;
        if let Some(unread) = unread {
            self.set_unread(canonical, unread)?;
        }
        Ok(changed > 0 || contacts > 0 || moved > 0 || merged_pins > 0)
    }

    fn merged_identity_unread(&self, alias: &str, canonical: &str) -> Result<u32> {
        // Keep unknown unread rows conservatively; only archived IDs can be deduplicated.
        self.connection.query_row(
            "WITH incoming AS (
                SELECT m.id, m.timestamp, c.unread,
                    ROW_NUMBER() OVER (PARTITION BY m.chat ORDER BY m.timestamp DESC, m.rowid DESC) AS position
                FROM messages m JOIN chats c ON c.id = m.chat
                WHERE m.chat IN (?1, ?2) AND m.from_me = 0
             ), unread_ids AS (
                SELECT DISTINCT id FROM incoming candidate
                WHERE position <= unread
                    AND timestamp >= COALESCE((SELECT MAX(read_through) FROM chats WHERE id IN (?1, ?2)), -1)
                    AND NOT EXISTS (SELECT 1 FROM incoming seen
                        WHERE seen.id = candidate.id AND seen.position > seen.unread)
             )
             SELECT (SELECT COUNT(*) FROM unread_ids) + COALESCE((
                SELECT MAX(MAX(c.unread - (SELECT COUNT(*) FROM messages m
                    WHERE m.chat = c.id AND m.from_me = 0), 0))
                FROM chats c WHERE c.id IN (?1, ?2)), 0)",
            params![alias, canonical], |row| row.get(0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::tests::message;
    use crate::model::Reaction;

    const PHONE: &str = "15550001001@s.whatsapp.net";
    const LID: &str = "900001@lid";

    #[test]
    fn identity_merge_keeps_confirmed_delivery_and_reactions_from_both_histories() {
        for confirmed_chat in [PHONE, LID] {
            let archive = Archive::in_memory().unwrap();
            let mut confirmed = message(confirmed_chat, "shared", 10, true);
            confirmed.status = Delivery::Read;
            confirmed.read_at = Some(12);
            confirmed.reactions.push(Reaction {
                sender: PHONE.into(),
                from_me: false,
                emoji: "👍".into(),
            });
            let mut failed = message(
                if confirmed_chat == PHONE { LID } else { PHONE },
                "shared",
                10,
                true,
            );
            failed.status = Delivery::Failed;
            failed.reactions.push(Reaction {
                sender: "me@s.whatsapp.net".into(),
                from_me: true,
                emoji: "❤️".into(),
            });
            archive
                .insert_message(&confirmed, Some(b"confirmed"))
                .unwrap();
            archive.insert_message(&failed, None).unwrap();

            archive.put_lid("900001", "15550001001").unwrap();

            let merged = archive.message(PHONE, "shared").unwrap().unwrap();
            assert_eq!(merged.status, Delivery::Read);
            assert_eq!(merged.read_at, Some(12));
            assert_eq!(merged.reactions.len(), 2);
            assert_eq!(
                archive.raw(PHONE, "shared").unwrap().as_deref(),
                Some(b"confirmed".as_slice())
            );
        }
    }

    #[test]
    fn identity_merge_preserves_edits_and_revocations_with_missing_or_different_raw() {
        for updated_chat in [PHONE, LID] {
            for raw in [None, Some(b"older".as_slice())] {
                for content in [Content::text("edited text"), Content::Revoked] {
                    let archive = Archive::in_memory().unwrap();
                    let mut updated = message(updated_chat, "shared", 10, false);
                    updated.content = content.clone();
                    updated.edited = !matches!(content, Content::Revoked);
                    updated.mentions.push(crate::model::MentionRef {
                        user: "123".into(),
                        id: "123@s.whatsapp.net".into(),
                        name: None,
                    });
                    let older = message(
                        if updated_chat == PHONE { LID } else { PHONE },
                        "shared",
                        10,
                        false,
                    );
                    archive.insert_message(&updated, Some(b"updated")).unwrap();
                    archive.insert_message(&older, raw).unwrap();

                    archive.put_lid("900001", "15550001001").unwrap();

                    let merged = archive.message(PHONE, "shared").unwrap().unwrap();
                    assert_eq!(merged.content, content);
                    assert_eq!(merged.edited, updated.edited);
                    assert_eq!(merged.mentions, updated.mentions);
                }
            }
        }
    }

    #[test]
    fn identity_merge_repairs_preexisting_mappings_when_reopening() {
        let archive = Archive::in_memory().unwrap();
        archive.ensure_chat(PHONE, "Phone").unwrap();
        archive.ensure_chat(LID, "Contact").unwrap();
        archive
            .insert_message(&message(PHONE, "sent", 10, true), Some(b"sent raw"))
            .unwrap();
        archive
            .insert_message(&message(LID, "reply", 11, false), Some(b"reply raw"))
            .unwrap();
        archive
            .connection
            .execute("INSERT INTO lids VALUES ('900001', '15550001001')", [])
            .unwrap();
        let repaired = Archive::prepare(archive.connection).unwrap();
        assert_eq!(repaired.chats().unwrap().len(), 1);
        assert_eq!(repaired.messages(PHONE, None, 10).unwrap().len(), 2);
        assert_eq!(
            repaired.raw(PHONE, "reply").unwrap().as_deref(),
            Some(b"reply raw".as_slice())
        );
        let reopened = Archive::prepare(repaired.connection).unwrap();
        assert_eq!(reopened.messages(PHONE, None, 10).unwrap().len(), 2);
    }

    #[test]
    fn identity_merge_retains_stars_poll_keys_and_newest_votes() {
        let archive = Archive::in_memory().unwrap();
        archive.ensure_chat(PHONE, "Phone").unwrap();
        archive.ensure_chat(LID, "Contact").unwrap();
        archive
            .insert_message(&message(PHONE, "poll", 10, true), None)
            .unwrap();
        archive
            .insert_message(&message(LID, "poll", 10, true), None)
            .unwrap();
        archive.set_starred(PHONE, "poll", false, 10).unwrap();
        archive.set_starred(LID, "poll", true, 11).unwrap();
        archive.save_poll(LID, "poll", LID, &[7; 32]).unwrap();
        archive.mark_poll_history(LID, "poll").unwrap();
        let mut vote = PollVote {
            chat: PHONE.into(),
            poll: "poll".into(),
            voter: LID.into(),
            sender: LID.into(),
            update_id: "a".into(),
            at: 20,
            from_me: false,
            choices: Some(vec![0]),
            encrypted: None,
        };
        archive.save_poll_vote(&vote).unwrap();
        vote.chat = LID.into();
        vote.update_id = "z".into();
        vote.choices = Some(Vec::new());
        archive.save_poll_vote(&vote).unwrap();
        archive.put_lid("900001", "15550001001").unwrap();
        assert_eq!(
            archive.poll_key(PHONE, "poll").unwrap(),
            Some((LID.into(), vec![7; 32]))
        );
        assert!(archive.has_poll_history(PHONE, "poll").unwrap());
        let votes = archive.poll_votes(PHONE, "poll").unwrap();
        assert_eq!(votes.len(), 1);
        assert_eq!(votes[0].update_id, "z");
        assert_eq!(votes[0].sender, LID);
        assert_eq!(votes[0].choices, Some(Vec::new()));
        let star: bool = archive
            .connection
            .query_row(
                "SELECT starred FROM stars WHERE chat = ?1 AND id = 'poll'",
                [PHONE],
                |row| row.get(0),
            )
            .unwrap();
        assert!(star);
    }

    #[test]
    fn identity_merge_preserves_downloaded_media_even_when_raw_differs() {
        for downloaded_chat in [PHONE, LID] {
            let archive = Archive::in_memory().unwrap();
            archive.ensure_chat(PHONE, "Phone").unwrap();
            archive.ensure_chat(LID, "Contact").unwrap();
            for chat in [PHONE, LID] {
                let mut row = message(chat, "media", 10, false);
                row.content = Content::Image {
                    caption: None,
                    media: crate::model::Media {
                        mime: "image/jpeg".into(),
                        size: 10,
                        width: None,
                        height: None,
                        path: (chat == downloaded_chat).then(|| "/synthetic/image.jpg".into()),
                        state: Default::default(),
                        view_once: false,
                    },
                };
                archive.insert_message(&row, Some(chat.as_bytes())).unwrap();
            }
            archive.put_lid("900001", "15550001001").unwrap();
            let row = archive.message(PHONE, "media").unwrap().unwrap();
            assert_eq!(
                row.content.media().unwrap().path.as_deref(),
                Some(std::path::Path::new("/synthetic/image.jpg"))
            );
        }
    }

    #[test]
    fn identity_merge_reconciles_unread_without_double_counting_shared_messages() {
        for (shared, expected) in [(false, 2), (true, 1)] {
            let archive = Archive::in_memory().unwrap();
            archive.ensure_chat(PHONE, "Phone").unwrap();
            archive.ensure_chat(LID, "Contact").unwrap();
            archive
                .insert_message(&message(PHONE, "one", 10, false), None)
                .unwrap();
            archive
                .insert_message(
                    &message(LID, if shared { "one" } else { "two" }, 10, false),
                    None,
                )
                .unwrap();
            archive.set_unread(PHONE, 1).unwrap();
            archive.set_unread(LID, 1).unwrap();
            archive.put_lid("900001", "15550001001").unwrap();
            assert_eq!(archive.chat(PHONE).unwrap().unwrap().unread, expected);
        }
    }

    #[test]
    fn identity_merge_preserves_same_second_unread_and_original_paging_order() {
        let archive = Archive::in_memory().unwrap();
        archive.ensure_chat(PHONE, "Phone").unwrap();
        archive.ensure_chat(LID, "Contact").unwrap();
        archive
            .insert_message(&message(LID, "read", 10, false), None)
            .unwrap();
        archive
            .insert_message(&message(PHONE, "unread", 10, false), None)
            .unwrap();
        archive.set_unread(PHONE, 1).unwrap();
        archive.mark_read(LID).unwrap();
        let original_rowid: i64 = archive
            .connection
            .query_row(
                "SELECT rowid FROM messages WHERE chat = ?1 AND id = 'read'",
                [LID],
                |row| row.get(0),
            )
            .unwrap();
        archive.put_lid("900001", "15550001001").unwrap();
        assert_eq!(archive.chat(PHONE).unwrap().unwrap().unread, 1);
        let moved_rowid: i64 = archive
            .connection
            .query_row(
                "SELECT rowid FROM messages WHERE chat = ?1 AND id = 'read'",
                [PHONE],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(original_rowid, moved_rowid);
        let older = archive.messages(PHONE, Some((10, "unread")), 1).unwrap();
        assert_eq!(older[0].id, "read");
    }

    #[test]
    fn identity_merge_applies_read_positions_and_retains_unarchived_unread_count() {
        let archive = Archive::in_memory().unwrap();
        archive.ensure_chat(PHONE, "Phone").unwrap();
        archive.ensure_chat(LID, "Contact").unwrap();
        archive
            .insert_message(&message(LID, "old", 10, false), None)
            .unwrap();
        archive.set_unread(LID, 1).unwrap();
        archive.mark_read_through(PHONE, 20).unwrap();
        archive.put_lid("900001", "15550001001").unwrap();
        assert_eq!(archive.chat(PHONE).unwrap().unwrap().unread, 0);

        archive.ensure_chat(LID, "Contact").unwrap();
        archive.set_unread(LID, 3).unwrap();
        archive.put_lid("900001", "15550001001").unwrap();
        assert_eq!(archive.chat(PHONE).unwrap().unwrap().unread, 3);
    }

    #[test]
    fn identity_merge_failure_rolls_back_mapping_messages_and_children() {
        let archive = Archive::in_memory().unwrap();
        archive.ensure_chat(PHONE, "Phone").unwrap();
        archive.ensure_chat(LID, "Contact").unwrap();
        archive
            .insert_message(&message(LID, "reply", 10, false), Some(b"raw"))
            .unwrap();
        archive.set_starred(LID, "reply", true, 11).unwrap();
        archive
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_merge BEFORE DELETE ON chats
             WHEN OLD.id = '900001@lid' BEGIN SELECT RAISE(ABORT, 'fixture failure'); END;",
            )
            .unwrap();

        assert!(archive.put_lid("900001", "15550001001").is_err());
        assert!(archive.lids().unwrap().is_empty());
        assert_eq!(archive.chats().unwrap().len(), 2);
        assert!(archive.message(PHONE, "reply").unwrap().is_none());
        assert!(archive.message(LID, "reply").unwrap().is_some());
        let starred: bool = archive
            .connection
            .query_row(
                "SELECT starred FROM stars WHERE chat = ?1 AND id = 'reply'",
                [LID],
                |row| row.get(0),
            )
            .unwrap();
        assert!(starred);
    }
}
