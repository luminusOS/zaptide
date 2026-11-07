//! Periodic removal of old downloaded attachments. The window sends the
//! account's retention period (`Command::SetMediaRetention`); while it is off
//! nothing is scheduled. Files go on a blocking thread, and the archive forgets
//! them here, so a message offers its download again instead of a missing file.

use super::*;
use crate::backend::MediaSwept;
use crate::media_retention;

const FIRST_SWEEP: Duration = Duration::from_secs(30);
const SWEEP_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Default)]
pub(super) struct Retention {
    days: Option<u32>,
    next: Option<Instant>,
    pub(super) running: bool,
}

impl Worker {
    pub(super) fn set_media_retention(&mut self, days: Option<u32>) {
        if self.retention.days == days {
            return;
        }
        self.retention.days = days;
        self.retention.next = days.map(|_| Instant::now() + FIRST_SWEEP);
    }

    pub(super) fn pump_media_retention(&mut self) {
        let Some(due) = self.retention.next else {
            return;
        };
        if self.retention.running || Instant::now() < due {
            return;
        }
        self.retention.next = Some(Instant::now() + SWEEP_EVERY);
        self.start_media_sweep();
    }

    fn start_media_sweep(&mut self) {
        let Some(days) = self.retention.days else {
            return;
        };
        let rows = match self.archive.expirable_media() {
            Ok(rows) => rows,
            Err(error) => {
                log::warn!("could not list downloads for cleanup: {error}");
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        self.retention.running = true;
        let dir = self.dirs.media_cache_dir();
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            let swept = tokio::task::spawn_blocking(move || {
                let report = media_retention::sweep(
                    &dir,
                    rows.iter().map(|(_, _, path)| path.clone()),
                    media_retention::retention_period(days),
                    std::time::SystemTime::now(),
                    media_retention::is_held,
                );
                let removed: HashSet<&PathBuf> = report.removed.iter().collect();
                let cleared = rows
                    .iter()
                    .filter(|(_, _, path)| removed.contains(path))
                    .cloned()
                    .collect();
                MediaSwept {
                    cleared,
                    bytes: report.bytes,
                    failed: report.failed,
                    more: report.more,
                }
            })
            .await
            .unwrap_or(MediaSwept {
                cleared: Vec::new(),
                bytes: 0,
                failed: 0,
                more: false,
            });
            let _ = Completion {
                generation: session_generation,
                done: Done::MediaSwept(swept),
            }
            .send(&commands);
        });
    }

    pub(super) fn media_swept(&mut self, swept: MediaSwept) {
        let mut forgotten = 0;
        for (chat, id, path) in &swept.cleared {
            // A download that finished meanwhile put a new file in the same place.
            if path.exists() {
                continue;
            }
            if matches!(self.archive.clear_media_path(chat, id), Ok(Some(_))) {
                forgotten += 1;
                self.emit_message(chat, id);
            }
        }
        if !swept.cleared.is_empty() || swept.failed > 0 {
            log::info!(
                "cleanup: removed {} downloaded files ({} KiB), {forgotten} messages updated, {} could not be removed",
                swept.cleared.len(),
                swept.bytes / 1024,
                swept.failed
            );
        }
        if swept.more && self.retention.days.is_some() {
            self.retention.next = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::worker;
    use super::*;
    use crate::model::{Content, Media};

    const CHAT: &str = "1@s.whatsapp.net";

    fn file_message(id: &str, path: &Path, sticker: bool) -> crate::model::Message {
        let mut message = crate::archive::tests::message(CHAT, id, 100, false);
        let media = Media {
            mime: "image/jpeg".into(),
            size: 9,
            width: Some(2),
            height: Some(2),
            path: Some(path.to_path_buf()),
            state: Default::default(),
        };
        message.content = if sticker {
            Content::Sticker {
                media,
                animated: false,
            }
        } else {
            Content::Image {
                caption: None,
                media,
            }
        };
        message
    }

    fn aged(path: &Path, days: u32) {
        std::fs::write(path, b"synthetic").unwrap();
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(86_400) * days)
            .unwrap();
    }

    #[tokio::test]
    async fn an_old_download_goes_and_its_message_offers_the_download_again() {
        let (mut worker, events, mut inbox, _wa) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let media = worker.dirs.media_cache_dir();
        std::fs::create_dir_all(&media).unwrap();
        let (old, recent, sticker, playing) = (
            media.join("old.jpg"),
            media.join("recent.jpg"),
            media.join("sticker.webp"),
            media.join("playing.jpg"),
        );
        aged(&old, 45);
        aged(&recent, 3);
        aged(&sticker, 45);
        aged(&playing, 45);
        let _held = crate::media_retention::hold([playing.clone()]);
        worker.archive.ensure_chat(CHAT, "A").unwrap();
        for (id, path, is_sticker) in [
            ("old", &old, false),
            ("recent", &recent, false),
            ("sticker", &sticker, true),
            ("playing", &playing, false),
        ] {
            worker
                .archive
                .insert_message(&file_message(id, path, is_sticker), Some(b"raw"))
                .unwrap();
        }

        worker.set_media_retention(Some(30));
        worker.retention.next = Some(Instant::now());
        worker.pump_media_retention();
        let Some(
            command @ Command::Completed(Completion {
                done: Done::MediaSwept(_),
                ..
            }),
        ) = inbox.recv().await
        else {
            panic!("the cleanup reports back");
        };
        worker.handle_command(command).await;

        assert!(!old.exists());
        assert!(recent.exists());
        assert!(sticker.exists());
        assert!(playing.exists());
        let stored = |id: &str| worker.archive.message(CHAT, id).unwrap().unwrap();
        assert_eq!(stored("old").content.media().unwrap().path, None);
        assert_eq!(stored("recent").content.media().unwrap().path, Some(recent));
        assert_eq!(
            stored("sticker").content.media().unwrap().path,
            Some(sticker)
        );
        assert!(matches!(
            crate::native_media::attachment_action(&stored("old")),
            Some(crate::native_media::NativeMediaAction::Download { .. })
        ));
        assert!(matches!(
            events.try_recv(),
            Ok(Event::MessageUpdated(updated)) if updated.id == "old"
        ));
        assert!(events.try_recv().is_err());
        assert!(!worker.retention.running);
    }

    #[tokio::test]
    async fn nothing_is_scheduled_or_removed_while_cleanup_is_off() {
        let (mut worker, _events, mut inbox, _wa) = worker();
        let root = tempfile::tempdir().unwrap();
        worker.dirs = AppDirs::under(root.path());
        let media = worker.dirs.media_cache_dir();
        std::fs::create_dir_all(&media).unwrap();
        let old = media.join("old.jpg");
        aged(&old, 400);
        worker.archive.ensure_chat(CHAT, "A").unwrap();
        worker
            .archive
            .insert_message(&file_message("old", &old, false), Some(b"raw"))
            .unwrap();

        worker.pump_media_retention();
        assert!(worker.retention.next.is_none());

        worker.set_media_retention(Some(30));
        worker.set_media_retention(None);
        worker.retention.next = None;
        worker.pump_media_retention();

        assert!(inbox.try_recv().is_err());
        assert!(old.exists());
    }

    #[tokio::test]
    async fn changing_the_period_schedules_a_new_run() {
        let (mut worker, _events, _inbox, _wa) = worker();
        worker.set_media_retention(Some(30));
        assert!(worker.retention.next.is_some());
        worker.retention.next = None;
        worker.set_media_retention(Some(30));
        assert!(worker.retention.next.is_none());
        worker.set_media_retention(Some(60));
        assert!(worker.retention.next.is_some());
    }

    #[tokio::test]
    async fn a_file_downloaded_again_in_the_meantime_is_kept() {
        let (mut worker, events, _inbox, _wa) = worker();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("again.jpg");
        std::fs::write(&path, b"synthetic").unwrap();
        worker.archive.ensure_chat(CHAT, "A").unwrap();
        worker
            .archive
            .insert_message(&file_message("again", &path, false), Some(b"raw"))
            .unwrap();

        worker.media_swept(MediaSwept {
            cleared: vec![(CHAT.into(), "again".into(), path.clone())],
            bytes: 0,
            failed: 0,
            more: false,
        });

        let stored = worker.archive.message(CHAT, "again").unwrap().unwrap();
        assert_eq!(stored.content.media().unwrap().path, Some(path));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_missing_file_already_shows_the_download_again() {
        let (worker, _events, _inbox, _wa) = worker();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("gone.jpg");
        worker.archive.ensure_chat(CHAT, "A").unwrap();
        worker
            .archive
            .insert_message(&file_message("gone", &path, false), Some(b"raw"))
            .unwrap();

        let stored = worker.archive.message(CHAT, "gone").unwrap().unwrap();
        assert!(matches!(
            crate::native_media::attachment_action(&stored),
            Some(crate::native_media::NativeMediaAction::Download { .. })
        ));
    }
}
