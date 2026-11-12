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
        // Starred stickers belong to the picker whoever sent them.
        match self
            .archive
            .starred_stickers_without_file(STICKER_FETCH_LIMIT)
        {
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
            Err(error) => log::warn!("could not list unfetched starred stickers: {error}"),
        }
    }

    /// Lists every distinct sticker for the picker, newest first, and the
    /// starred ones among them. Files with the same content count once, so a
    /// sticker that arrived in several chats or was copied in by hand does not
    /// repeat.
    pub(super) fn emit_stickers(&mut self) {
        self.stickers_dirty = false;
        // (last use, file, starred)
        let mut candidates: Vec<(i64, PathBuf, bool)> = Vec::new();
        if let Ok(phone) = self.archive.phone_stickers() {
            for sticker in phone {
                if let Some(path) = sticker.path
                    && path.exists()
                {
                    candidates.push((sticker.last_used, path, false));
                }
            }
        }
        match self.archive.recent_stickers(80) {
            Ok(rows) => {
                for sticker in rows {
                    candidates.push((sticker.last_used, sticker.path, false));
                }
            }
            Err(error) => log::warn!("could not list stickers: {error}"),
        }
        match self.archive.starred_stickers(STICKER_FETCH_LIMIT * 4) {
            Ok(rows) => {
                for sticker in rows {
                    candidates.push((sticker.last_used, sticker.path, true));
                }
            }
            Err(error) => log::warn!("could not list starred stickers: {error}"),
        }
        for (when, path) in self.folder_stickers() {
            candidates.push((when, path, false));
        }
        candidates.sort_by_key(|(when, _, _)| std::cmp::Reverse(*when));
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut distinct: Vec<(PathBuf, bool)> = Vec::new();
        for (_, path, starred) in candidates {
            let key = sticker_content_key(&path)
                .unwrap_or_else(|| format!("path:{}", path.display()));
            match seen.get(&key) {
                Some(&index) => distinct[index].1 |= starred,
                None => {
                    seen.insert(key, distinct.len());
                    distinct.push((path, starred));
                }
            }
        }
        let starred = distinct
            .iter()
            .filter(|(_, starred)| *starred)
            .map(|(path, _)| path.clone())
            .collect();
        self.emit(Event::Stickers {
            saved: starred,
            packs: self.sticker_packs(),
            recent: distinct.into_iter().map(|(path, _)| path).collect(),
        });
    }

    /// Removes a sticker from the picker: deletes copies the person put in the
    /// stickers folder and unstars every starred message with the same file.
    pub(super) fn remove_sticker(&mut self, path: PathBuf) {
        let Some(key) = sticker_content_key(&path) else {
            return;
        };
        for (_, file) in self.folder_stickers() {
            if sticker_content_key(&file).as_deref() == Some(key.as_str())
                && let Err(error) = std::fs::remove_file(&file)
            {
                log::warn!("could not remove a saved sticker: {error}");
            }
        }
        match self.archive.starred_sticker_keys() {
            Ok(rows) => {
                for (chat, id, file) in rows {
                    if sticker_content_key(&file).as_deref() == Some(key.as_str()) {
                        self.set_starred(chat, id, false);
                    }
                }
            }
            Err(error) => log::warn!("could not list starred stickers: {error}"),
        }
        self.emit_stickers();
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

    /// Stickers the person copied into the account's stickers folder
    /// (`<state>/stickers`), as (modified time in seconds, file), newest first.
    /// The folder is created so it can be found and filled.
    pub(super) fn folder_stickers(&self) -> Vec<(i64, PathBuf)> {
        let dir = self.dirs.saved_sticker_dir();
        let _ = std::fs::create_dir_all(&dir);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut saved: Vec<(i64, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let is_webp = path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("webp"));
                (is_webp && path.is_file()).then(|| {
                    let when = entry
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |elapsed| elapsed.as_secs() as i64);
                    (when, path)
                })
            })
            .collect();
        saved.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        saved
    }
}

/// Hash of a sticker file's bytes, cached by size and modification time.
fn sticker_content_key(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::sync::{Mutex, OnceLock};
    type Entry = (u64, Option<std::time::SystemTime>, String);
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    let metadata = std::fs::metadata(path).ok()?;
    let stamp = (metadata.len(), metadata.modified().ok());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(cache) = cache.lock()
        && let Some((len, modified, key)) = cache.get(path)
        && (*len, *modified) == stamp
    {
        return Some(key.clone());
    }
    let key = hex::encode(Sha256::digest(std::fs::read(path).ok()?));
    if let Ok(mut cache) = cache.lock() {
        cache.insert(path.to_path_buf(), (stamp.0, stamp.1, key.clone()));
    }
    Some(key)
}
