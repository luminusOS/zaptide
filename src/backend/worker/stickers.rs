use super::*;

impl Worker {
    /// Downloads missing recent and archived stickers for the picker.
    pub(super) fn fetch_missing_stickers(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let phone = match self.archive.phone_stickers() {
            Ok(list) => list,
            Err(error) => {
                log::warn!("could not list the phone's stickers: {error}");
                Vec::new()
            }
        };
        let dir = self.dirs.sticker_cache_dir();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let session_cache_lock = self.session_cache_lock.clone();
        for sticker in phone.into_iter().filter(|sticker| sticker.path.is_none()) {
            if !self.sticker_fetches.insert(sticker.hash.clone()) {
                continue;
            }
            let Ok(meta) = wa::StickerMetadata::decode_from_slice(&sticker.raw) else {
                self.sticker_fetches.remove(&sticker.hash);
                continue;
            };
            let client = client.clone();
            let commands = self.commands.clone();
            let dir = dir.clone();
            let hash = sticker.hash;
            let session_generation_shared = session_generation_shared.clone();
            let session_cache_lock = session_cache_lock.clone();
            tokio::spawn(async move {
                let result = async {
                    if session_generation_shared.load(Ordering::Acquire) != session_generation {
                        return Err(
                            "The linked account changed during the sticker download".to_owned()
                        );
                    }
                    let bytes = client
                        .download(&PhoneSticker(meta))
                        .await
                        .map_err(|error| error.to_string())?;
                    let path = dir.join(format!("{hash}.webp"));
                    write_session_cache_file(
                        &dir,
                        &path,
                        &bytes,
                        session_generation,
                        &session_generation_shared,
                        None,
                        &session_cache_lock,
                    )
                    .await
                }
                .await;
                let _ = Completion {
                    generation: session_generation,
                    done: Done::StickerFetched { hash, result },
                }
                .send(&commands);
            });
        }
        match self.archive.stickers_without_file(STICKER_FETCH_LIMIT) {
            Ok(list) => {
                for (chat, id) in list {
                    let Some(raw) = self.archive.raw(&chat, &id).ok().flatten() else {
                        continue;
                    };
                    let fingerprint = message_raw_fingerprint(&raw);
                    if self
                        .sticker_downloads
                        .insert((chat.clone(), id.clone(), fingerprint))
                    {
                        self.download(chat, id);
                    }
                }
            }
            Err(error) => log::warn!("could not list unfetched stickers: {error}"),
        }
    }

    /// Returns distinct downloaded stickers by most recent use.
    pub(super) fn emit_stickers(&mut self) {
        self.stickers_dirty = false;
        let mut seen = HashSet::new();
        let mut list: Vec<(i64, PathBuf)> = Vec::new();
        if let Ok(phone) = self.archive.phone_stickers() {
            for sticker in phone {
                if let Some(path) = sticker.path
                    && path.exists()
                    && seen.insert(sticker.hash)
                {
                    list.push((sticker.last_used, path));
                }
            }
        }
        match self.archive.recent_stickers(80) {
            Ok(rows) => {
                for sticker in rows {
                    let hash = sticker
                        .raw
                        .as_deref()
                        .and_then(|raw| wa::Message::decode_from_slice(raw).ok())
                        .and_then(|message| {
                            let base = message.get_base_message();
                            let sticker = base.sticker_message.as_option()?;
                            sticker_hash(
                                sticker.file_sha256.as_deref(),
                                sticker.file_enc_sha256.as_deref(),
                            )
                        })
                        .unwrap_or_else(|| sticker.path.display().to_string());
                    if seen.insert(hash) {
                        list.push((sticker.last_used, sticker.path));
                    }
                }
            }
            Err(error) => log::warn!("could not list stickers: {error}"),
        }
        list.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        self.emit(Event::Stickers {
            saved: self.saved_stickers(),
            packs: self.sticker_packs(),
            recent: list.into_iter().map(|(_, path)| path).collect(),
        });
    }

    pub(super) fn flush_stickers(&mut self) {
        if self.stickers_dirty {
            self.emit_stickers();
        }
    }

    /// Root directory for imported sticker packs.
    pub(super) fn packs_dir(&self) -> PathBuf {
        self.dirs.saved_sticker_dir().join("packs")
    }

    /// Returns imported packs, newest first, with files in name order.
    pub(super) fn sticker_packs(&self) -> Vec<crate::model::StickerPack> {
        let Ok(entries) = std::fs::read_dir(self.packs_dir()) else {
            return Vec::new();
        };
        let mut packs: Vec<(std::time::SystemTime, crate::model::StickerPack)> = entries
            .flatten()
            .filter_map(|entry| {
                let dir = entry.path();
                if !dir.is_dir() {
                    return None;
                }
                let mut stickers: Vec<PathBuf> = std::fs::read_dir(&dir)
                    .ok()?
                    .flatten()
                    .map(|file| file.path())
                    .filter(|path| {
                        path.extension()
                            .is_some_and(|extension| extension == "webp")
                    })
                    .collect();
                if stickers.is_empty() {
                    return None;
                }
                stickers.sort();
                let when = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                Some((
                    when,
                    crate::model::StickerPack {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        dir,
                        stickers,
                        local: true,
                    },
                ))
            })
            .collect();
        packs.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        packs.into_iter().map(|(_, pack)| pack).collect()
    }

    /// Returns saved sticker files, newest first.
    pub(super) fn saved_stickers(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.dirs.saved_sticker_dir()) else {
            return Vec::new();
        };
        let mut saved: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                path.extension()
                    .is_some_and(|extension| extension == "webp")
                    .then(|| {
                        let when = entry
                            .metadata()
                            .and_then(|metadata| metadata.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        (when, path)
                    })
            })
            .collect();
        saved.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        saved.into_iter().map(|(_, path)| path).collect()
    }
}
