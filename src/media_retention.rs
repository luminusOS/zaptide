//! Removes downloaded attachments once they are older than the retention
//! period the user chose.
//!
//! Only files that an archived message points to, directly inside the media
//! cache folder, are ever removed. Messages stay in the archive, and an
//! attachment that WhatsApp still keeps can be downloaded again.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

pub const MIN_DAYS: u32 = 1;
pub const MAX_DAYS: u32 = 3650;
pub const DEFAULT_DAYS: u32 = 90;

/// A run removes at most this many files, so that clearing the archive's
/// references afterwards never holds the worker for long.
pub const BATCH: usize = 500;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

pub fn retention_period(days: u32) -> Duration {
    DAY * days.clamp(MIN_DAYS, MAX_DAYS)
}

/// A downloaded file and what the policy needs to know about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub age: Duration,
    pub size: u64,
    pub in_use: bool,
}

/// The entries to remove: older than `keep` and not in use. Oldest first.
pub fn expired(entries: &[Entry], keep: Duration) -> Vec<&Entry> {
    let mut old: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.age > keep && !entry.in_use)
        .collect();
    old.sort_by_key(|entry| std::cmp::Reverse(entry.age));
    old
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Paths exactly as the archive stores them.
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
    pub failed: usize,
    /// More files were due than this run's limit allowed.
    pub more: bool,
}

/// Removes the expired files among `referenced` from `media_dir`.
///
/// A path is considered only when its parent folder is `media_dir` itself
/// after resolving links, and only when the file is a regular file rather
/// than a link, so nothing outside the cache can be reached through the
/// archive or through a planted link. A file that cannot be removed is
/// counted and skipped.
pub fn sweep(
    media_dir: &Path,
    referenced: impl IntoIterator<Item = PathBuf>,
    keep: Duration,
    now: SystemTime,
    in_use: impl Fn(&Path) -> bool,
) -> Report {
    let Ok(root) = media_dir.canonicalize() else {
        return Report::default();
    };
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    let mut originals: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for path in referenced {
        let Some(file) = contained_file(&root, &path) else {
            continue;
        };
        originals.entry(file.clone()).or_default().push(path);
        if !seen.insert(file.clone()) {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&file) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let held = originals[&file].iter().any(|path| in_use(path)) || in_use(&file);
        entries.push(Entry {
            age: now.duration_since(modified).unwrap_or_default(),
            size: metadata.len(),
            in_use: held,
            path: file,
        });
    }

    let mut report = Report::default();
    let due = expired(&entries, keep);
    report.more = due.len() > BATCH;
    for entry in due.into_iter().take(BATCH) {
        match std::fs::remove_file(&entry.path) {
            Ok(()) => report.bytes += entry.size,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                report.failed += 1;
                continue;
            }
        }
        report
            .removed
            .extend(originals.remove(&entry.path).unwrap_or_default());
    }
    report
}

/// `path` re-rooted in the resolved `root`, if it names a file directly in it.
fn contained_file(root: &Path, path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let parent = path.parent()?.canonicalize().ok()?;
    (parent == root).then(|| root.join(name))
}

static HELD: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn held() -> std::sync::MutexGuard<'static, Vec<PathBuf>> {
    crate::shared_resource::lock(&HELD)
}

/// Keeps files from being cleaned up while it lives: a viewer showing them or
/// a clip playing.
#[must_use = "the files are released when this is dropped"]
pub struct Hold(Vec<PathBuf>);

pub fn hold(paths: impl IntoIterator<Item = PathBuf>) -> Hold {
    let paths: Vec<PathBuf> = paths.into_iter().collect();
    held().extend(paths.iter().cloned());
    Hold(paths)
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut all = held();
        for path in &self.0 {
            if let Some(index) = all.iter().position(|held| held == path) {
                all.swap_remove(index);
            }
        }
    }
}

pub fn is_held(path: &Path) -> bool {
    held().iter().any(|held| held == path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const KEEP: Duration = Duration::from_secs(30 * 24 * 60 * 60);

    fn entry(name: &str, days: u64, in_use: bool) -> Entry {
        Entry {
            path: PathBuf::from(name),
            age: DAY * days as u32,
            size: 1,
            in_use,
        }
    }

    fn names(entries: Vec<&Entry>) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.path.to_str().unwrap())
            .collect()
    }

    #[test]
    fn only_files_older_than_the_period_are_due() {
        let entries = [
            entry("new", 1, false),
            entry("edge", 30, false),
            entry("old", 31, false),
        ];
        assert_eq!(names(expired(&entries, KEEP)), ["old"]);
    }

    #[test]
    fn files_in_use_are_never_due() {
        let entries = [entry("playing", 400, true), entry("idle", 400, false)];
        assert_eq!(names(expired(&entries, KEEP)), ["idle"]);
    }

    #[test]
    fn the_oldest_files_come_first() {
        let entries = [
            entry("a", 40, false),
            entry("b", 90, false),
            entry("c", 60, false),
        ];
        assert_eq!(names(expired(&entries, KEEP)), ["b", "c", "a"]);
    }

    #[test]
    fn the_period_is_bounded() {
        assert_eq!(retention_period(0), DAY * MIN_DAYS);
        assert_eq!(retention_period(u32::MAX), DAY * MAX_DAYS);
        assert_eq!(retention_period(30), KEEP);
    }

    fn write_aged(path: &Path, days: u64) {
        std::fs::write(path, b"synthetic").unwrap();
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - DAY * days as u32)
            .unwrap();
    }

    fn run(dir: &Path, paths: &[&Path]) -> Report {
        sweep(
            dir,
            paths.iter().map(|path| path.to_path_buf()),
            KEEP,
            SystemTime::now(),
            |_| false,
        )
    }

    #[test]
    fn old_files_go_and_new_ones_stay() {
        let root = tempfile::tempdir().unwrap();
        let (old, new) = (root.path().join("old.jpg"), root.path().join("new.jpg"));
        write_aged(&old, 45);
        write_aged(&new, 2);

        let report = run(root.path(), &[&old, &new]);

        assert_eq!(report.removed, std::slice::from_ref(&old));
        assert_eq!(report.bytes, 9);
        assert!(!old.exists());
        assert!(new.exists());
    }

    #[test]
    fn files_the_archive_does_not_name_are_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let (named, staged) = (
            root.path().join("a.jpg"),
            root.path().join(".download-1.tmp"),
        );
        write_aged(&named, 90);
        write_aged(&staged, 90);

        run(root.path(), &[&named]);

        assert!(!named.exists());
        assert!(staged.exists());
    }

    #[test]
    fn a_missing_folder_is_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("media");
        let report = run(&missing, &[&missing.join("a.jpg")]);
        assert_eq!(report, Report::default());
    }

    #[test]
    fn a_path_outside_the_folder_is_never_removed() {
        let root = tempfile::tempdir().unwrap();
        let media = root.path().join("media");
        let other = root.path().join("stickers");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let outside = other.join("saved.webp");
        write_aged(&outside, 400);
        let traversal = media.join("..").join("stickers").join("saved.webp");

        let report = run(&media, &[&outside, &traversal]);

        assert!(report.removed.is_empty());
        assert!(outside.exists());
    }

    #[test]
    fn a_nested_folder_is_not_the_media_folder() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("a.jpg");
        write_aged(&file, 400);

        run(root.path(), &[&file]);

        assert!(file.exists());
    }

    #[test]
    fn a_link_is_not_followed_out_of_the_folder() {
        let root = tempfile::tempdir().unwrap();
        let media = root.path().join("media");
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let target = elsewhere.join("private.bin");
        write_aged(&target, 400);
        let link = media.join("a.jpg");
        symlink(&target, &link).unwrap();
        let folder_link = root.path().join("alias");
        symlink(&elsewhere, &folder_link).unwrap();

        let report = run(&media, &[&link, &folder_link.join("private.bin")]);

        assert!(report.removed.is_empty());
        assert!(target.exists());
        assert!(link.symlink_metadata().is_ok());
    }

    #[test]
    fn a_media_folder_reached_through_a_link_still_works() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let alias = root.path().join("alias");
        symlink(&real, &alias).unwrap();
        let file = real.join("a.jpg");
        write_aged(&file, 400);

        let report = run(&alias, &[&alias.join("a.jpg")]);

        assert_eq!(report.removed, [alias.join("a.jpg")]);
        assert!(!file.exists());
    }

    #[test]
    fn a_directory_named_like_media_is_kept() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("a.jpg");
        std::fs::create_dir_all(&folder).unwrap();

        let report = run(root.path(), &[&folder]);

        assert!(report.removed.is_empty());
        assert!(folder.is_dir());
    }

    #[test]
    fn a_failure_does_not_stop_the_run() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let (first, second) = (root.path().join("a.jpg"), root.path().join("b.jpg"));
        write_aged(&first, 100);
        write_aged(&second, 50);
        let mut permissions = std::fs::metadata(root.path()).unwrap().permissions();
        permissions.set_mode(0o500);
        std::fs::set_permissions(root.path(), permissions.clone()).unwrap();
        let blocked = std::fs::File::create(root.path().join("probe")).is_err();
        let report = run(root.path(), &[&first, &second]);
        permissions.set_mode(0o700);
        std::fs::set_permissions(root.path(), permissions).unwrap();

        if blocked {
            assert_eq!(report.failed, 2);
            assert!(report.removed.is_empty());
            let report = run(root.path(), &[&first, &second]);
            assert_eq!(report.removed.len(), 2);
        } else {
            assert_eq!(report.removed.len(), 2);
        }
    }

    #[test]
    fn messages_sharing_one_file_are_all_reported() {
        let root = tempfile::tempdir().unwrap();
        let (first, missing) = (root.path().join("a.jpg"), root.path().join("b.jpg"));
        write_aged(&first, 100);
        let report = run(root.path(), &[&first, &first, &missing]);

        assert_eq!(report.removed, [first.clone(), first]);
        assert_eq!(report.failed, 0);
    }

    #[test]
    fn files_in_use_stay() {
        let root = tempfile::tempdir().unwrap();
        let (busy, idle) = (root.path().join("busy.mp4"), root.path().join("idle.mp4"));
        write_aged(&busy, 100);
        write_aged(&idle, 100);

        let report = sweep(
            root.path(),
            [busy.clone(), idle.clone()],
            KEEP,
            SystemTime::now(),
            |path| path.ends_with("busy.mp4"),
        );

        assert_eq!(report.removed, [idle]);
        assert!(busy.exists());
    }

    #[test]
    fn a_run_is_limited_and_says_so() {
        let root = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..BATCH + 3)
            .map(|index| {
                let path = root.path().join(format!("{index}.bin"));
                write_aged(&path, 100);
                path
            })
            .collect();

        let first = run_all(root.path(), &paths);
        assert_eq!((first.removed.len(), first.more), (BATCH, true));
        let second = run_all(root.path(), &paths);
        assert_eq!((second.removed.len(), second.more), (3, false));
    }

    fn run_all(dir: &Path, paths: &[PathBuf]) -> Report {
        sweep(dir, paths.to_vec(), KEEP, SystemTime::now(), |_| false)
    }

    #[test]
    fn holds_are_counted_and_released() {
        let path = PathBuf::from("/synthetic/held-twice.mp4");
        let first = hold([path.clone()]);
        let second = hold([path.clone()]);
        assert!(is_held(&path));
        drop(first);
        assert!(is_held(&path));
        drop(second);
        assert!(!is_held(&path));
    }
}
