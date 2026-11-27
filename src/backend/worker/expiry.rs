//! Disappearing messages: changing a chat's timer, removing messages whose
//! time has run out, and letting go of pins that ran out.
//!
//! Removal is local. It never queues a deletion for other devices, because
//! their copy runs out on its own clock, and a message someone kept stays.

use super::*;
use crate::model::SystemEvent;
use rustix::fd::OwnedFd;
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;

/// How often the archive is checked for messages that ran out.
const CHECK_EVERY: Duration = Duration::from_secs(30);
/// Most messages removed in one pass, so a long-closed app catches up in steps.
const BATCH: usize = 200;
const MEDIA_RETRY_SECONDS: i64 = 60;

#[derive(Default)]
pub(super) struct Expiry {
    next: Option<Instant>,
}

pub(super) struct KeepChange {
    pub(super) target: String,
    pub(super) kept: bool,
    at_ms: i64,
}

impl KeepChange {
    pub(super) fn order(&self) -> (i64, bool) {
        (self.at_ms, !self.kept)
    }
}

fn kept_flag(kind: wa::KeepType) -> Option<bool> {
    match kind {
        wa::KeepType::KeepForAll => Some(true),
        wa::KeepType::UndoKeepForAll => Some(false),
        _ => None,
    }
}

pub(super) fn keep_change(base: &wa::Message, fallback_ms: i64) -> Option<KeepChange> {
    let keep = base.keep_in_chat_message.as_option()?;
    let key = keep.key.as_option()?;
    let target = key.id.as_ref().filter(|id| !id.is_empty())?.clone();
    let kept = kept_flag(keep.keep_type?)?;
    let at_ms = keep
        .timestamp_ms
        .filter(|at| *at > 0)
        .unwrap_or(fallback_ms);
    (at_ms > 0).then_some(KeepChange {
        target,
        kept,
        at_ms,
    })
}

pub(super) fn history_keep_state(info: &wa::WebMessageInfo, id: &str) -> Option<KeepChange> {
    let snapshot = info.keep_in_chat.as_option()?;
    let key = snapshot.key.as_option();
    if key
        .and_then(|key| key.id.as_deref())
        .is_some_and(|target| target != id)
    {
        return None;
    }
    let at_ms = [
        snapshot.client_timestamp_ms,
        snapshot.server_timestamp_ms,
        snapshot
            .server_timestamp
            .map(|seconds| seconds.saturating_mul(1000)),
    ]
    .into_iter()
    .flatten()
    .find(|at| *at > 0)?;
    Some(KeepChange {
        target: id.to_owned(),
        kept: kept_flag(snapshot.keep_type?)?,
        at_ms,
    })
}

struct ExpiryCache {
    root: PathBuf,
    dir: OwnedFd,
}

impl ExpiryCache {
    fn open(cache: &Path) -> Option<Self> {
        let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let parent = cache.parent()?.canonicalize().ok()?;
        let parent_dir = fs::open(&parent, flags, Mode::empty()).ok()?;
        let account = cache.file_name()?;
        let cache_dir = fs::openat(parent_dir, account, flags, Mode::empty()).ok()?;
        let dir = fs::openat(cache_dir, "media", flags, Mode::empty()).ok()?;
        Some(Self {
            root: parent.join(account).join("media"),
            dir,
        })
    }

    fn stat(&self, name: &str) -> rustix::io::Result<Stat> {
        fs::statat(&self.dir, name, AtFlags::SYMLINK_NOFOLLOW)
    }

    fn candidate<'a>(&self, path: &'a Path) -> Option<(&'a str, String)> {
        let name = path.file_name()?.to_str()?;
        if path.parent()?.canonicalize().ok()? != self.root {
            return None;
        }
        // A later stat cannot prove that the file is still the expired
        // attachment if we could not identify it before deleting the row.
        let stat = match self.stat(name) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => return None,
            Err(error) => {
                log::warn!("leaving an expired attachment that could not be identified: {error}");
                return None;
            }
        };
        (FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile)
            .then(|| (name, file_identity(&stat)))
    }

    fn file_matches(&self, name: &str, identity: &str) -> rustix::io::Result<bool> {
        match self.stat(name) {
            Ok(stat) => Ok(
                FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                    && file_identity(&stat) == identity,
            ),
            Err(Errno::NOENT) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn remove(&self, name: &str, identity: &str) -> rustix::io::Result<()> {
        if !self.file_matches(name, identity)? {
            return Ok(());
        }
        match fs::unlinkat(&self.dir, name, AtFlags::empty()) {
            Err(Errno::NOENT) => Ok(()),
            result => result,
        }
    }
}

// Retrying a path after a restart must not remove a replacement file.
fn file_identity(stat: &Stat) -> String {
    format!(
        "{}:{}:{}:{}",
        stat.st_dev, stat.st_ino, stat.st_ctime, stat.st_ctime_nsec
    )
}

impl Worker {
    /// `Command::SetDisappearing`: the new timer applies once WhatsApp took it.
    pub(super) fn set_disappearing(&mut self, chat: ChatId, seconds: u32) {
        if !crate::model::TIMER_CHOICES.contains(&seconds) {
            return;
        }
        let chat = self.canonical_str(&chat);
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error(
                "Could not change disappearing messages while disconnected".to_owned(),
            ));
            return;
        };
        let at = jiff::Timestamp::now().as_second();
        let commands = self.commands.clone();
        let generation = self.session_generation;
        tokio::spawn(async move {
            let result = if jid.is_group() {
                client
                    .groups()
                    .set_ephemeral(jid, seconds)
                    .await
                    .map(|()| String::new())
                    .map_err(|error| error.to_string())
            } else {
                client
                    .set_chat_disappearing_timer(jid, seconds)
                    .await
                    .map(|sent| sent.message_id.into_string())
                    .map_err(|error| error.to_string())
            };
            let _ = Completion {
                generation,
                done: Done::DisappearingSet {
                    chat,
                    seconds,
                    at,
                    result,
                },
            }
            .send(&commands);
        });
    }

    pub(super) fn disappearing_set(
        &mut self,
        chat: ChatId,
        seconds: u32,
        at: i64,
        result: Result<String, String>,
    ) {
        let stanza = match result {
            Ok(stanza) => stanza,
            Err(error) => {
                log::warn!("could not change the disappearing timer: {error}");
                self.emit(Event::Error(
                    "Could not change disappearing messages".to_owned(),
                ));
                return;
            }
        };
        let accepted = self
            .archive
            .set_ephemeral(&chat, seconds, at)
            .unwrap_or(false);
        // A group tells everyone, including us, with a notification that
        // carries its own line; a 1:1 chat has only what we just sent.
        if accepted && !stanza.is_empty() {
            let me = self.me();
            self.store_system_line(
                &chat,
                &stanza,
                me,
                None,
                true,
                at,
                SystemEvent::Timer { seconds },
            );
        }
        self.emit_chat(&chat);
    }

    /// `Command::SetKept`: keeps a disappearing message for everyone in the
    /// chat, or lets it run out again. The archive changes once WhatsApp
    /// took the request, in `keep_set`.
    pub(super) fn set_kept(&mut self, chat: ChatId, id: String, kept: bool) {
        let chat = self.canonical_str(&chat);
        let state = self.archive.keep_state(&chat, &id);
        let message = self.archive.message(&chat, &id);
        if state.is_err() || message.is_err() {
            self.report_keep_archive_error();
            return;
        }
        let (Ok(Some((deadline, current))), Ok(Some(message))) = (state, message) else {
            return;
        };
        if current == kept
            || !current && deadline <= jiff::Timestamp::now().as_second()
            || matches!(message.content, Content::Revoked | Content::System { .. })
            || message.content.view_once()
        {
            return;
        }
        let (Some(client), Some(jid)) = (self.client.clone(), Self::jid_of(&chat)) else {
            self.emit(Event::Error(
                "Could not change Keep in Chat while disconnected".to_owned(),
            ));
            return;
        };
        let participant = (jid.is_group() && !message.from_me)
            .then(|| Self::jid_of(&message.sender))
            .flatten();
        let commands = self.commands.clone();
        let generation = self.session_generation;
        tokio::spawn(async move {
            let result = async {
                let message_id = whatsapp_rust::MessageId::new(&id).map_err(|_| ())?;
                let target = whatsapp_rust::MessageRef::new(
                    &jid,
                    message_id,
                    participant.as_ref(),
                    message.from_me,
                )
                .map_err(|_| ())?;
                let sent = client.keep_message(&target, kept).await.map_err(|_| ())?;
                keep_change(&sent.message, 0)
                    .map(|change| change.at_ms)
                    .ok_or(())
            }
            .await;
            let _ = Completion {
                generation,
                done: Done::KeepSet {
                    chat,
                    id,
                    kept,
                    result,
                },
            }
            .send(&commands);
        });
    }

    pub(super) fn keep_set(
        &mut self,
        chat: ChatId,
        id: String,
        kept: bool,
        result: Result<i64, ()>,
    ) {
        let chat = self.canonical_str(&chat);
        match result {
            Ok(at_ms) => {
                let change = KeepChange {
                    target: id,
                    kept,
                    at_ms,
                };
                self.apply_live_keep(&chat, change, true);
            }
            Err(()) => {
                self.emit(Event::Error("Could not change Keep in Chat".to_owned()));
            }
        }
    }

    pub(super) fn store_keep(
        &self,
        chat: &str,
        change: &KeepChange,
    ) -> crate::archive::Result<Option<i64>> {
        // The add-on key uses the sender's address space. Only the envelope
        // identifies this account's chat; in a DM the nested JID may be ours.
        self.archive
            .set_kept(chat, &change.target, change.kept, change.at_ms)
    }

    pub(super) fn apply_live_keep(&self, chat: &str, change: KeepChange, local: bool) {
        match self.store_keep(chat, &change) {
            Ok(Some(deadline)) => self.emit_keep(chat, change.target, change.kept, deadline, local),
            Ok(None) => {}
            Err(_) => self.report_keep_archive_error(),
        }
        if self.archive.prune_pending_keeps().is_err() {
            self.report_keep_archive_error();
        }
    }

    fn report_keep_archive_error(&self) {
        log::warn!("could not save a Keep in Chat change");
        self.emit(Event::Error(
            "Could not save Keep in Chat. Request chat history after fixing archive access.".into(),
        ));
    }

    pub(super) fn emit_keep(&self, chat: &str, id: String, kept: bool, deadline: i64, local: bool) {
        let overdue = deadline <= jiff::Timestamp::now().as_second();
        let restored = if kept && overdue {
            self.updated_message(chat, &id).map(Box::new)
        } else {
            None
        };
        self.emit(Event::KeptChanged {
            chat: chat.to_owned(),
            id: id.clone(),
            kept,
            deadline,
            local,
            restored,
        });
        if overdue {
            if !kept {
                self.emit(Event::MessageDeleted {
                    chat: chat.to_owned(),
                    id,
                });
            }
            self.emit_chat(chat);
            self.emit_pin(chat);
        }
    }

    /// Called on every tick; does its work at most every half minute.
    pub(super) fn pump_expiry(&mut self) {
        let now = Instant::now();
        if self.expiry.next.is_some_and(|due| now < due) {
            return;
        }
        self.expiry.next = Some(now + CHECK_EVERY);
        if !self.privacy.privacy_ready {
            return;
        }
        self.remove_expired();
        if self.archive.prune_pending_keeps().is_err() {
            log::warn!("could not prune pending Keep in Chat changes");
        }
    }

    fn remove_expired(&mut self) {
        let now = jiff::Timestamp::now();
        let seconds = now.as_second();
        // Only the account's managed cache proves ownership. Configured and
        // former download folders can also contain files the user owns.
        let cache = ExpiryCache::open(&self.dirs.cache);
        let mut touched = std::collections::BTreeSet::new();
        match self.archive.expired_messages(seconds, BATCH) {
            Ok(due) => {
                for message in due {
                    let candidate = cache
                        .as_ref()
                        .and_then(|cache| cache.candidate(message.media_path.as_deref()?));
                    let cleanup = candidate
                        .as_ref()
                        .map(|(name, identity)| (*name, identity.as_str()));
                    match self
                        .archive
                        .delete_expired_message(&message, seconds, cleanup)
                    {
                        Ok(true) => {
                            self.emit(Event::MessageDeleted {
                                chat: message.chat.clone(),
                                id: message.id,
                            });
                            touched.insert(message.chat);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            log::warn!("could not remove an expired message: {error}");
                            break;
                        }
                    }
                }
            }
            Err(error) => log::warn!("could not list expired messages: {error}"),
        }
        if let Some(cache) = cache {
            self.remove_expired_media(&cache, seconds);
        }
        match self.archive.expire_pins(now.as_millisecond()) {
            Ok(chats) => touched.extend(chats),
            Err(error) => log::warn!("could not end expired pins: {error}"),
        }
        for chat in touched {
            self.emit_chat(&chat);
            self.emit_pin(&chat);
        }
    }

    fn remove_expired_media(&self, cache: &ExpiryCache, now: i64) {
        let pending = match self.archive.pending_expiry_media(now, BATCH) {
            Ok(pending) if pending.is_empty() => return,
            Ok(pending) => pending,
            Err(error) => {
                log::warn!("could not list expired attachments: {error}");
                return;
            }
        };
        let managed = self.dirs.media_cache_dir();
        let mut candidates = HashMap::new();
        let retry_at = now.saturating_add(MEDIA_RETRY_SECONDS);
        for (name, identity) in pending {
            if Path::new(&name).file_name() != Some(name.as_ref()) {
                self.finish_expiry_media(&name, None);
                continue;
            }
            let path = cache.root.join(&name);
            match self
                .archive
                .expiry_media_has_exact_reference(&managed.join(&name), &path)
            {
                Ok(true) => self.finish_expiry_media(&name, None),
                Ok(false) if crate::media_retention::while_unheld(&path, || ()).is_none() => {
                    self.finish_expiry_media(&name, Some(retry_at));
                }
                Ok(false) => match cache.file_matches(&name, &identity) {
                    Ok(true) => {
                        candidates.insert(path, (name, identity));
                    }
                    Ok(false) => self.finish_expiry_media(&name, None),
                    Err(error) => {
                        log::warn!("could not inspect an expired attachment: {error}");
                        self.finish_expiry_media(&name, Some(retry_at));
                    }
                },
                Err(error) => log::warn!("could not check attachment reference: {error}"),
            }
        }
        if candidates.is_empty() {
            return;
        }
        // Exact references use the archive's path index. Only files still
        // eligible for unlinking need the complete check for path aliases.
        let mut aliased = Vec::new();
        let aliases = self.archive.visit_expiry_media_references(|path| {
            match path.canonicalize() {
                Ok(path) => {
                    if let Some((name, _)) = candidates.remove(&path) {
                        aliased.push(name);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) => {}
                Err(error) => return Err(error),
            }
            Ok(!candidates.is_empty())
        });
        for name in aliased {
            self.finish_expiry_media(&name, None);
        }
        if let Err(error) = aliases {
            log::warn!("could not check attachment aliases: {error}");
            return;
        }
        for (path, (name, identity)) in candidates {
            // Take the cache writer lock before the viewer hold lock. Never
            // wait on a writer in the worker tick; the remaining queue is durable.
            let Ok(cache_write) = self.session_cache_lock.try_lock() else {
                break;
            };
            let retry_after = match crate::media_retention::while_unheld(&path, || {
                cache.remove(&name, &identity)
            }) {
                Some(Ok(())) => None,
                Some(Err(error)) => {
                    log::warn!("could not remove an expired attachment: {error}");
                    Some(retry_at)
                }
                None => Some(retry_at),
            };
            drop(cache_write);
            self.finish_expiry_media(&name, retry_after);
        }
    }

    fn finish_expiry_media(&self, name: &str, retry_after: Option<i64>) {
        if let Err(error) = self.archive.finish_expiry_media(name, retry_after) {
            log::warn!("could not update expired attachment cleanup: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::worker;
    use super::*;

    const CHAT: &str = "123-456@g.us";

    fn insert_attachment(worker: &Worker, id: &str, path: &Path) {
        worker.archive.ensure_chat(CHAT, "Synthetic chat").unwrap();
        let mut message = crate::archive::tests::message(CHAT, id, 20, false);
        message.content = Content::Image {
            caption: None,
            media: crate::model::Media {
                mime: "image/jpeg".into(),
                size: 9,
                width: None,
                height: None,
                path: Some(path.to_path_buf()),
                state: Default::default(),
                view_once: false,
            },
        };
        worker.archive.insert_message(&message, None).unwrap();
    }

    #[test]
    fn expiry_unlinks_a_cache_attachment_but_leaves_a_custom_download() {
        let (mut worker, _, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let cache = worker.dirs.media_cache_dir();
        let downloads = root.path().join("Downloads");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir(&downloads).unwrap();
        worker.set_media_download_dir(Some(downloads.clone()));
        let owned = cache.join("owned.jpg");
        let exported = downloads.join("exported.jpg");
        for (id, path) in [("owned", &owned), ("exported", &exported)] {
            std::fs::write(path, b"synthetic").unwrap();
            insert_attachment(&worker, id, path);
            worker.archive.set_expires_at(CHAT, id, Some(80)).unwrap();
        }

        worker.pump_expiry();

        assert!(!owned.exists());
        assert_eq!(std::fs::read(exported).unwrap(), b"synthetic");
        assert!(worker.archive.message(CHAT, "owned").unwrap().is_none());
        assert!(worker.archive.message(CHAT, "exported").unwrap().is_none());
    }

    #[test]
    fn expiry_recognizes_an_attachment_referenced_via_an_alternate_path() {
        let (mut worker, _, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let media_dir = worker.dirs.media_cache_dir();
        std::fs::create_dir_all(&media_dir).unwrap();
        let path = media_dir.join("shared.jpg");
        std::fs::write(&path, b"synthetic").unwrap();
        let alternate = media_dir.join("../media/shared.jpg");
        insert_attachment(&worker, "expired", &path);
        insert_attachment(&worker, "permanent", &alternate);
        worker
            .archive
            .set_expires_at(CHAT, "expired", Some(80))
            .unwrap();

        worker.pump_expiry();

        assert!(worker.archive.message(CHAT, "expired").unwrap().is_none());
        assert_eq!(std::fs::read(alternate).unwrap(), b"synthetic");
    }

    #[test]
    fn expiry_defers_unlinking_while_a_cache_writer_holds_the_lock() {
        let (mut worker, _, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let media_dir = worker.dirs.media_cache_dir();
        std::fs::create_dir_all(&media_dir).unwrap();
        let path = media_dir.join("attachment.jpg");
        std::fs::write(&path, b"synthetic").unwrap();
        insert_attachment(&worker, "expired", &path);
        worker
            .archive
            .set_expires_at(CHAT, "expired", Some(80))
            .unwrap();
        let cache_lock = worker.session_cache_lock.clone();
        let writer = cache_lock.try_lock().unwrap();

        worker.pump_expiry();

        assert!(worker.archive.message(CHAT, "expired").unwrap().is_none());
        assert_eq!(std::fs::read(&path).unwrap(), b"synthetic");
        drop(writer);
        let cache = ExpiryCache::open(&worker.dirs.cache).unwrap();
        worker.remove_expired_media(&cache, jiff::Timestamp::now().as_second());
        assert!(!path.exists());
    }

    #[test]
    fn expiry_respects_held_files_and_retries_after_reopening_the_archive() {
        let (mut worker, _, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let db = root.path().join("synthetic.db");
        let key = [42; 32];
        worker.archive = Archive::open_encrypted_for_test(&db, &key).unwrap();
        let media_dir = worker.dirs.media_cache_dir();
        std::fs::create_dir_all(&media_dir).unwrap();
        let path = media_dir.join("held.jpg");
        std::fs::write(&path, b"synthetic").unwrap();
        insert_attachment(&worker, "expired", &path);
        worker
            .archive
            .set_expires_at(CHAT, "expired", Some(80))
            .unwrap();
        let held = crate::media_retention::hold([path.clone()]);

        worker.pump_expiry();

        assert_eq!(std::fs::read(&path).unwrap(), b"synthetic");
        assert!(worker.archive.message(CHAT, "expired").unwrap().is_none());
        drop(held);
        drop(worker);
        let (mut reopened, _, _, _) = super::super::tests::worker();
        reopened.dirs = AppDirs::under(root.path());
        reopened.archive = Archive::open_encrypted_for_test(&db, &key).unwrap();
        let cache = ExpiryCache::open(&reopened.dirs.cache).unwrap();
        let later = jiff::Timestamp::now().as_second() + MEDIA_RETRY_SECONDS;
        reopened.remove_expired_media(&cache, later);

        assert!(!path.exists());
        assert!(
            reopened
                .archive
                .pending_expiry_media(later, 1)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn expiry_does_not_remove_a_new_file_at_a_queued_path() {
        let (mut worker, _, _, _) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let media_dir = worker.dirs.media_cache_dir();
        std::fs::create_dir_all(&media_dir).unwrap();
        let path = media_dir.join("held.jpg");
        std::fs::write(&path, b"synthetic").unwrap();
        insert_attachment(&worker, "expired", &path);
        worker
            .archive
            .set_expires_at(CHAT, "expired", Some(80))
            .unwrap();
        let held = crate::media_retention::hold([path.clone()]);
        worker.pump_expiry();
        drop(held);

        let replacement = media_dir.join("replacement.jpg");
        std::fs::write(&replacement, b"new attachment").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let cache = ExpiryCache::open(&worker.dirs.cache).unwrap();
        let later = jiff::Timestamp::now().as_second() + MEDIA_RETRY_SECONDS;
        worker.remove_expired_media(&cache, later);

        assert_eq!(std::fs::read(&path).unwrap(), b"new attachment");
        assert!(
            worker
                .archive
                .pending_expiry_media(later, 1)
                .unwrap()
                .is_empty()
        );
    }
}
