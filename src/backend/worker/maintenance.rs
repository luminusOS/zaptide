use super::*;

impl Worker {
    /// Repairs moved attachment paths or clears missing files for redownload,
    /// over the next `limit` rows. Returns true once the pass has ended.
    pub(super) fn relocate_step(&mut self, relocation: &mut Relocation, limit: usize) -> bool {
        let dir = self.dirs.media_cache_dir();
        let rows = match self.archive.media_paths_after(relocation.after, limit) {
            Ok(rows) => rows,
            Err(error) => {
                log::warn!("could not list attachments: {error}");
                return true;
            }
        };
        let Some(&(last, ..)) = rows.last() else {
            if relocation.moved + relocation.forgotten > 0 {
                log::info!(
                    "attachments: {} re-pointed to {}, {} to fetch again",
                    relocation.moved,
                    dir.display(),
                    relocation.forgotten
                );
                self.emit(Event::RefreshTranscript);
            }
            return true;
        };
        relocation.after = last;
        for (_, chat, id, path) in rows {
            let Some(path) = path.filter(|path| !path.exists()) else {
                continue;
            };
            let candidate = path.file_name().map(|name| dir.join(name));
            match candidate.filter(|candidate| candidate.exists()) {
                Some(candidate) => {
                    if self.archive.set_media_path(&chat, &id, &candidate).is_ok() {
                        relocation.moved += 1;
                    }
                }
                None => {
                    if self.archive.clear_media_path(&chat, &id).is_ok() {
                        relocation.forgotten += 1;
                    }
                }
            }
        }
        false
    }

    /// Starts the startup upkeep that [`Self::maintain`] runs a step at a time.
    pub(super) fn start_maintenance(&mut self) {
        self.maintenance = Some(match self.start_backfill() {
            Some(backfill) => Maintenance::Backfill(backfill),
            None => Maintenance::ConfirmAnswers,
        });
    }

    /// Runs the next bounded step of startup upkeep.
    pub(super) fn maintain(&mut self) {
        self.maintenance_deferred = 0;
        self.maintenance = match self.maintenance.take() {
            Some(Maintenance::Backfill(mut backfill)) => {
                if self.backfill_step(&mut backfill, MAINTENANCE_BATCH) {
                    Some(Maintenance::ConfirmAnswers)
                } else {
                    Some(Maintenance::Backfill(backfill))
                }
            }
            // One indexed query over outgoing replies to unanswered buttons or
            // lists; short enough for a single step.
            Some(Maintenance::ConfirmAnswers) => {
                self.reconcile_confirmed_answers();
                Some(Maintenance::RelocateMedia(Relocation::default()))
            }
            Some(Maintenance::RelocateMedia(mut relocation)) => {
                if self.relocate_step(&mut relocation, MAINTENANCE_BATCH) {
                    None
                } else {
                    Some(Maintenance::RelocateMedia(relocation))
                }
            }
            None => None,
        };
    }

    #[cfg(test)]
    pub(super) fn backfill(&mut self) {
        if let Some(mut backfill) = self.start_backfill() {
            while !self.backfill_step(&mut backfill, MAINTENANCE_BATCH) {}
        }
    }

    /// Resumes an interrupted pass of the current version from its saved
    /// position, since the rows before it are already re-derived.
    pub(super) fn start_backfill(&self) -> Option<Backfill> {
        let stored = self.archive.meta("derived").ok().flatten();
        let stored_version = stored
            .as_deref()
            .and_then(|value| value.parse::<u32>().ok());
        log::info!(
            "archive re-derivation version: stored {:?}, target {DERIVED_VERSION}",
            stored_version
        );
        if stored.as_deref() == Some(DERIVED_VERSION) {
            return None;
        }
        let after = self
            .archive
            .meta(DERIVED_CURSOR)
            .ok()
            .flatten()
            .and_then(|cursor| {
                cursor
                    .strip_prefix(DERIVED_VERSION)?
                    .strip_prefix(':')?
                    .parse()
                    .ok()
            })
            .unwrap_or(0);
        Some(Backfill {
            after,
            updated: 0,
            failed: 0,
            retry_required: false,
            started: Instant::now(),
        })
    }

    /// Re-derives the next `limit` stored rows and returns true once the
    /// pass has ended. Each row is read fresh and written within this call,
    /// with no await between, so what events changed between steps is the
    /// state it starts from; edited content and revoked rows are preserved.
    pub(super) fn backfill_step(&mut self, backfill: &mut Backfill, limit: usize) -> bool {
        let batch = self.begin_batch();
        let rows = match batch.archive.derived_sources(backfill.after, limit) {
            Ok(rows) => rows,
            Err(error) => {
                log::warn!("could not read the archive for re-deriving: {error}");
                log::info!("archive re-derivation failed rows: unknown (batch read failed)");
                return true;
            }
        };
        let Some(last) = rows.last() else {
            if !batch.finish() {
                backfill.retry_required = true;
            }
            self.finish_backfill(backfill);
            return true;
        };
        backfill.after = last.rowid;
        let batch_size = rows.len();
        let mut updated = 0;
        let mut failed = 0;
        for source in rows {
            // Malformed or unsupported raw messages have no derived projection to retry.
            let Ok(message) = wa::Message::decode_from_slice(&source.raw) else {
                continue;
            };
            let base = base_message(&message);
            let Some(mut content) = classify(base) else {
                continue;
            };
            if matches!(source.content, Content::Revoked) {
                continue;
            }
            let mentions = if source.edited {
                content = source.content.clone();
                source.mentions.clone()
            } else {
                if let (Some(new), Some(old)) = (content.media_mut(), source.content.media()) {
                    new.path = old.path.clone();
                }
                batch.mentions_of(&mentioned_of(base))
            };
            let thumbnail = thumbnail_of(base);
            match batch.archive.set_derived_if_unchanged(
                &source,
                &content,
                &mentions,
                thumbnail.as_deref(),
                forwarded_of(base),
            ) {
                Ok(true) => updated += 1,
                Ok(false) => {}
                Err(_error) => failed += 1,
            }
        }
        if batch.finish() {
            backfill.updated += updated;
            backfill.failed += failed;
            backfill.retry_required |= failed != 0;
        } else {
            backfill.retry_required = true;
            backfill.failed += batch_size;
        }
        // A failed row freezes the cursor before it, so the next launch retries it.
        if !backfill.retry_required {
            let cursor = format!("{DERIVED_VERSION}:{}", backfill.after);
            if let Err(error) = self.archive.set_meta(DERIVED_CURSOR, &cursor) {
                log::debug!("could not save the re-derive position: {error}");
            }
        }
        false
    }

    pub(super) fn finish_backfill(&mut self, backfill: &Backfill) {
        log::info!("archive re-derivation failed rows: {}", backfill.failed);
        if backfill.retry_required {
            log::warn!("some archived messages could not be re-derived; will retry later");
            return;
        }
        if self.archive.set_meta("derived", DERIVED_VERSION).is_err() {
            log::warn!("could not save the archive re-derivation version");
            return;
        }
        if backfill.updated > 0 {
            log::info!(
                "re-derived {} archived messages in {:.1?}",
                backfill.updated,
                backfill.started.elapsed()
            );
            self.emit_chats();
            // Pages sent while the pass ran may hold rows it re-derived since.
            self.emit(Event::RefreshTranscript);
        }
    }
}
