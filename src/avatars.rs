use super::*;

impl Worker {
    pub(super) fn avatar_file(&self, id: &str, full: bool) -> PathBuf {
        self.dirs.avatar_file(id, full)
    }

    /// Removes a cached picture and its id; callers hold `session_cache_lock`.
    pub(super) fn remove_avatar_files(&self, id: &str, full: bool) {
        let path = self.avatar_file(id, full);
        let _ = std::fs::remove_file(KnownPicture::sidecar(&path));
        let _ = std::fs::remove_file(path);
    }

    pub(super) fn fetch_avatar(&mut self, id: String, full: bool) {
        self.queue_avatar(id, full, false);
    }

    /// The picture of the chat the person just opened goes before the rest.
    pub(super) fn fetch_avatar_first(&mut self, id: String) {
        self.queue_avatar(id, false, true);
    }

    /// Answers from the cache when it can, else queues the lookup. Queued
    /// lookups start in order, `AVATAR_FETCHES` at a time, so the first
    /// pictures arrive one by one instead of everything arriving together.
    fn queue_avatar(&mut self, id: String, full: bool, first: bool) {
        let key = (id.clone(), full);
        if self.avatar_active.contains(&key) {
            return;
        }
        let path = self.avatar_file(&id, full);
        let cached = std::fs::metadata(&path).ok();
        if let Some(metadata) = &cached
            && metadata
                .modified()
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| avatar_is_fresh(&id, age))
        {
            let path = (metadata.len() > 0).then_some(path);
            self.emit(Event::Avatar { id, full, path });
            return;
        }
        // A picture that is only due for a refresh still shows at once; the
        // lookup below replaces it if it changed, and a missing connection
        // no longer leaves the list without pictures.
        if cached.as_ref().is_some_and(|metadata| metadata.len() > 0) {
            self.emit(Event::Avatar {
                id: id.clone(),
                full,
                path: Some(path.clone()),
            });
        }
        match self.avatar_backlog.iter().position(|queued| *queued == key) {
            Some(position) if first => {
                self.avatar_backlog.remove(position);
                self.avatar_backlog.push_front(key);
            }
            Some(_) => {}
            None if first => self.avatar_backlog.push_front(key),
            None => self.avatar_backlog.push_back(key),
        }
        self.pump_avatars();
    }

    /// Starts queued lookups while there is a free slot.
    pub(super) fn pump_avatars(&mut self) {
        while self.avatar_active.len() < AVATAR_FETCHES {
            let Some((id, full)) = self.avatar_backlog.pop_front() else {
                break;
            };
            self.start_avatar(id, full);
        }
    }

    fn start_avatar(&mut self, id: String, full: bool) {
        let path = self.avatar_file(&id, full);
        let cached = std::fs::metadata(&path).ok();
        // Try both of our ids for our profile picture.
        let candidates: Vec<Jid> = if self.is_me(&id) || id == self.me() {
            [self.identity.me_pn.clone(), self.identity.me_lid.clone()]
                .into_iter()
                .flatten()
                .filter_map(|id| Self::jid_of(&id))
                .collect()
        } else {
            Self::jid_of(&id).into_iter().collect()
        };
        if candidates.is_empty() {
            self.emit(Event::Avatar {
                id,
                full,
                path: None,
            });
            return;
        }
        let connected = self
            .client
            .as_ref()
            .is_some_and(|client| client.is_socket_connected());
        let Some(client) = self.client.clone().filter(|_| connected) else {
            // Defer profile-picture lookup until connected.
            self.pending_avatars.entry((id, full)).or_insert(0);
            return;
        };
        let known = cached
            .filter(|metadata| metadata.len() > 0)
            .and_then(|_| KnownPicture::read(&path));
        let permits = self.avatar_permits.clone();
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        let session_generation_shared = self.session_generation_shared.clone();
        let avatar_generation_shared = self.avatar_generation(&id, full);
        let avatar_generation = avatar_generation_shared.load(Ordering::Acquire);
        let session_cache_lock = self.session_cache_lock.clone();
        self.avatar_active.insert((id.clone(), full));
        tokio::spawn(async move {
            let fetched = async {
                let source = ClientPictures(client);
                let still_wanted = || {
                    session_generation_shared.load(Ordering::Acquire) == session_generation
                        && avatar_generation_shared.load(Ordering::Acquire) == avatar_generation
                };
                let fetched = fetch_picture(
                    &source,
                    &permits,
                    still_wanted,
                    &candidates,
                    full,
                    known.as_ref(),
                )
                .await?;
                let (bytes, picture) = match fetched {
                    FetchedPicture::Missing => return Ok(None),
                    FetchedPicture::Unchanged => {
                        return keep_session_cache_file(
                            &path,
                            session_generation,
                            &session_generation_shared,
                            (avatar_generation, &avatar_generation_shared),
                            &session_cache_lock,
                        )
                        .await
                        .map(Some);
                    }
                    FetchedPicture::Downloaded { bytes, picture } => (bytes, picture),
                };
                let path = write_avatar_cache(
                    &path,
                    &bytes,
                    picture.as_ref(),
                    session_generation,
                    &session_generation_shared,
                    (avatar_generation, &avatar_generation_shared),
                    &session_cache_lock,
                )
                .await?;
                Ok::<Option<PathBuf>, String>(Some(path))
            }
            .await;
            match fetched {
                Ok(path) => {
                    let _ = Completion {
                        generation: session_generation,
                        done: Done::AvatarFetched {
                            id,
                            full,

                            avatar_generation,
                            path,
                        },
                    }
                    .send(&commands);
                }
                Err(_error) => {
                    log::debug!("profile picture lookup failed");
                    let _ = Completion {
                        generation: session_generation,
                        done: Done::AvatarFailed {
                            id,
                            full,

                            avatar_generation,
                        },
                    }
                    .send(&commands);
                }
            }
        });
    }

    pub(super) fn avatar_generation(&mut self, id: &str, full: bool) -> Arc<AtomicU64> {
        self.avatar_generations
            .entry((id.to_owned(), full))
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone()
    }

    pub(super) fn invalidate_avatar_generations(&mut self, id: &str) {
        for full in [false, true] {
            self.avatar_generation(id, full)
                .fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Retries deferred or failed profile-picture requests.
    pub(super) fn retry_avatars(&mut self) {
        if !self
            .client
            .as_ref()
            .is_some_and(|client| client.is_socket_connected())
        {
            return;
        }
        let due: Vec<(String, bool)> = self.pending_avatars.keys().cloned().collect();
        for (id, full) in due {
            let attempts = self
                .pending_avatars
                .remove(&(id.clone(), full))
                .unwrap_or(0);
            if attempts >= 3 {
                self.emit(Event::Avatar {
                    id,
                    full,
                    path: None,
                });
                continue;
            }
            self.fetch_avatar(id, full);
        }
    }
}
