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

    /// Lists distinct recent and saved stickers for the picker, newest first.
    /// Files with the same content count once, so a sticker from multiple
    /// sources appears only once while retaining each collection membership.
    pub(super) fn emit_stickers(&mut self) {
        self.stickers_dirty = false;
        // (last use, file, starred, recent)
        let mut candidates: Vec<(i64, PathBuf, bool, bool)> = Vec::new();
        if let Ok(phone) = self.archive.phone_stickers() {
            for sticker in phone {
                if let Some(path) = sticker.path
                    && path.exists()
                {
                    candidates.push((sticker.last_used, path, false, true));
                }
            }
        }
        match self.archive.recent_stickers(80) {
            Ok(rows) => {
                for sticker in rows {
                    candidates.push((sticker.last_used, sticker.path, false, true));
                }
            }
            Err(error) => log::warn!("could not list stickers: {error}"),
        }
        match self.archive.starred_stickers(STICKER_FETCH_LIMIT * 4) {
            Ok(rows) => {
                for sticker in rows {
                    candidates.push((sticker.last_used, sticker.path, true, false));
                }
            }
            Err(error) => log::warn!("could not list starred stickers: {error}"),
        }
        for (when, path) in self.folder_stickers() {
            candidates.push((when, path, true, false));
        }
        candidates.sort_by_key(|(when, _, _, _)| std::cmp::Reverse(*when));
        let saved_dir = self.dirs.saved_sticker_dir();
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut distinct: Vec<(PathBuf, bool, bool)> = Vec::new();
        for (_, path, starred, recent) in candidates {
            let key =
                sticker_content_key(&path).unwrap_or_else(|| format!("path:{}", path.display()));
            match seen.get(&key) {
                Some(&index) => {
                    if path.parent() == Some(saved_dir.as_path()) {
                        distinct[index].0 = path;
                    }
                    distinct[index].1 |= starred;
                    distinct[index].2 |= recent;
                }
                None => {
                    seen.insert(key, distinct.len());
                    distinct.push((path, starred, recent));
                }
            }
        }
        let starred = distinct
            .iter()
            .filter(|(_, starred, _)| *starred)
            .map(|(path, _, _)| path.clone())
            .collect();
        self.emit(Event::Stickers {
            saved: starred,
            packs: self.sticker_packs(),
            recent: distinct
                .into_iter()
                .filter(|(_, _, recent)| *recent)
                .map(|(path, _, _)| path)
                .collect(),
        });
    }

    /// Deletes matching saved-folder copies without changing message stars.
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
        self.emit_stickers();
    }

    /// Unstars matching messages in unlocked chats without deleting files.
    pub(super) fn unstar_sticker(&mut self, path: PathBuf) {
        let Some(key) = sticker_content_key(&path) else {
            return;
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::worker::tests::worker;

    #[test]
    fn saved_copy_remains_removable_when_a_recent_duplicate_is_newer() {
        let (mut worker, events, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let saved = worker.dirs.saved_sticker_dir().join("saved.webp");
        let cached = worker.dirs.sticker_cache_dir().join("recent.webp");
        std::fs::create_dir_all(worker.dirs.saved_sticker_dir()).unwrap();
        std::fs::create_dir_all(worker.dirs.sticker_cache_dir()).unwrap();
        std::fs::write(&saved, b"synthetic sticker").unwrap();
        std::fs::write(&cached, b"synthetic sticker").unwrap();
        worker
            .archive
            .upsert_phone_sticker("recent", b"metadata", i64::MAX, 1.0)
            .unwrap();
        worker.archive.set_sticker_path("recent", &cached).unwrap();

        worker.emit_stickers();
        let Event::Stickers {
            saved: favorites,
            recent,
            ..
        } = events.try_recv().unwrap()
        else {
            panic!("expected stickers");
        };
        assert_eq!(favorites, vec![saved.clone()]);
        assert_eq!(recent, vec![saved.clone()]);
        worker.remove_sticker(saved.clone());
        assert!(!saved.exists());
        assert!(cached.exists());
        let Event::Stickers {
            saved: favorites,
            recent,
            ..
        } = events.try_recv().unwrap()
        else {
            panic!("expected refreshed stickers");
        };
        assert!(favorites.is_empty());
        assert_eq!(recent, vec![cached]);
    }

    #[test]
    fn removing_saved_duplicate_leaves_starred_message_available() {
        let (mut worker, events, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let saved = worker.dirs.saved_sticker_dir().join("saved.webp");
        let cached = worker.dirs.sticker_cache_dir().join("starred.webp");
        std::fs::create_dir_all(worker.dirs.saved_sticker_dir()).unwrap();
        std::fs::create_dir_all(worker.dirs.sticker_cache_dir()).unwrap();
        std::fs::write(&saved, b"synthetic sticker").unwrap();
        std::fs::write(&cached, b"synthetic sticker").unwrap();
        star_sticker(&worker, "visible@s.whatsapp.net", &cached, false);
        worker.emit_stickers();
        let Event::Stickers {
            saved: favorites, ..
        } = events.try_recv().unwrap()
        else {
            panic!("expected stickers");
        };
        assert_eq!(favorites, vec![saved.clone()]);
        worker.remove_sticker(saved);
        let Event::Stickers {
            saved: favorites, ..
        } = events.try_recv().unwrap()
        else {
            panic!("expected refreshed stickers");
        };
        assert_eq!(favorites, vec![cached]);
        assert_eq!(
            worker
                .archive
                .starred_ids("visible@s.whatsapp.net")
                .unwrap(),
            ["sticker"]
        );
    }

    fn star_sticker(worker: &Worker, chat: &str, path: &Path, locked: bool) {
        let mut message = crate::archive::tests::message(chat, "sticker", 100, false);
        message.content = Content::Sticker {
            media: crate::model::Media {
                mime: "image/webp".into(),
                size: 10,
                width: Some(2),
                height: Some(2),
                path: Some(path.to_path_buf()),
                state: Default::default(),
            },
            animated: false,
        };
        worker.archive.ensure_chat(chat, "Fixture").unwrap();
        worker.archive.insert_message(&message, None).unwrap();
        worker.archive.set_locked(chat, locked).unwrap();
        worker
            .archive
            .set_starred(chat, "sticker", true, 1)
            .unwrap();
    }

    #[test]
    fn removing_saved_stickers_preserves_message_stars_and_cached_files() {
        let (mut worker, _events, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let saved = worker.dirs.saved_sticker_dir().join("saved.webp");
        let duplicate = worker.dirs.saved_sticker_dir().join("duplicate.webp");
        let unrelated = worker.dirs.saved_sticker_dir().join("unrelated.webp");
        let cached = worker.dirs.sticker_cache_dir().join("cached.webp");
        std::fs::create_dir_all(worker.dirs.saved_sticker_dir()).unwrap();
        std::fs::create_dir_all(worker.dirs.sticker_cache_dir()).unwrap();
        for path in [&saved, &duplicate, &cached] {
            std::fs::write(path, b"synthetic sticker").unwrap();
        }
        std::fs::write(&unrelated, b"another sticker").unwrap();
        star_sticker(&worker, "visible@s.whatsapp.net", &cached, false);

        worker.remove_sticker(saved.clone());

        assert!(!saved.exists());
        assert!(!duplicate.exists());
        assert!(unrelated.exists());
        assert!(cached.exists());
        assert_eq!(
            worker
                .archive
                .starred_ids("visible@s.whatsapp.net")
                .unwrap(),
            ["sticker"]
        );
    }

    #[test]
    fn sticker_unstar_candidates_exclude_locked_chats() {
        let (worker, _events, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sticker.webp");
        std::fs::write(&path, b"synthetic sticker").unwrap();
        star_sticker(&worker, "visible@s.whatsapp.net", &path, false);
        star_sticker(&worker, "locked@s.whatsapp.net", &path, true);

        assert_eq!(
            worker.archive.starred_sticker_keys().unwrap(),
            vec![("visible@s.whatsapp.net".into(), "sticker".into(), path)]
        );
    }

    #[tokio::test]
    async fn unstarring_stickers_preserves_saved_files_and_locked_stars() {
        let (mut worker, _events, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let saved = worker.dirs.saved_sticker_dir().join("saved.webp");
        let cached = worker.dirs.sticker_cache_dir().join("cached.webp");
        let other = worker.dirs.sticker_cache_dir().join("other.webp");
        std::fs::create_dir_all(worker.dirs.saved_sticker_dir()).unwrap();
        std::fs::create_dir_all(worker.dirs.sticker_cache_dir()).unwrap();
        for path in [&saved, &cached] {
            std::fs::write(path, b"synthetic sticker").unwrap();
        }
        std::fs::write(&other, b"another sticker").unwrap();
        star_sticker(&worker, "visible@s.whatsapp.net", &cached, false);
        star_sticker(&worker, "duplicate@s.whatsapp.net", &cached, false);
        star_sticker(&worker, "locked@s.whatsapp.net", &cached, true);
        star_sticker(&worker, "unrelated@s.whatsapp.net", &other, false);

        worker
            .handle_command(Command::UnstarSticker(cached.clone()))
            .await;

        assert!(saved.exists());
        assert!(cached.exists());
        assert!(other.exists());
        for chat in ["visible@s.whatsapp.net", "duplicate@s.whatsapp.net"] {
            assert!(worker.archive.starred_ids(chat).unwrap().is_empty());
        }
        for chat in ["locked@s.whatsapp.net", "unrelated@s.whatsapp.net"] {
            assert_eq!(worker.archive.starred_ids(chat).unwrap(), ["sticker"]);
        }
    }
}
