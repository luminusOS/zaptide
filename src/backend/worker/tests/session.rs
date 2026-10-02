use super::worker;
use super::*;

#[test]
fn logout_cleanup_removes_session_sidecars_and_account_caches() {
    let root = tempfile::tempdir().expect("temporary account root");
    let dirs = AppDirs::under(root.path());
    dirs.ensure().expect("app directories");
    let session = dirs.session_db();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut path = session.clone().into_os_string();
        path.push(suffix);
        std::fs::write(path, b"synthetic session fixture").expect("session fixture");
    }
    for directory in [
        dirs.avatar_cache_dir(),
        dirs.media_cache_dir(),
        dirs.sticker_cache_dir(),
    ] {
        std::fs::create_dir_all(&directory).expect("cache directory");
        std::fs::write(directory.join("fixture"), b"synthetic cache fixture")
            .expect("cache fixture");
    }
    let saved_sticker = dirs.saved_sticker_dir().join("user-saved.webp");
    std::fs::create_dir_all(dirs.saved_sticker_dir()).expect("saved sticker directory");
    std::fs::write(&saved_sticker, b"user-owned sticker").expect("saved sticker fixture");
    let archive = Archive::in_memory().expect("archive");
    archive
        .ensure_chat("synthetic@s.whatsapp.net", "Synthetic")
        .expect("synthetic chat");

    clear_logged_out_data(&dirs, &archive).expect("logout cleanup");

    assert!(archive.chats().expect("empty archive").is_empty());
    assert!(!session.exists());
    assert!(!dirs.avatar_cache_dir().exists());
    assert!(!dirs.media_cache_dir().exists());
    assert!(!dirs.sticker_cache_dir().exists());
    assert!(saved_sticker.exists(), "user-saved stickers are user data");
}

#[test]
fn stale_attachment_outbound_cannot_use_new_session_or_archive_content() {
    let (mut worker, events, _, _) = worker();
    worker.session_generation = 2;
    let row = crate::archive::tests::message("chat", "old-account-message", 1, true);
    worker.outbound("chat".into(), 1, row, Vec::new());

    assert!(
        worker
            .archive
            .message("chat", "old-account-message")
            .expect("archive query")
            .is_none()
    );
    assert!(events.try_iter().next().is_none());

    let (sent, mut result) = mpsc::unbounded_channel();
    worker.outbound_batch(
        "chat".into(),
        1,
        crate::archive::tests::message("chat", "old-account-batch", 2, true),
        Vec::new(),
        sent,
    );
    assert_eq!(result.try_recv(), Ok(false));
    assert!(
        worker
            .archive
            .message("chat", "old-account-batch")
            .expect("archive query")
            .is_none()
    );
}

#[tokio::test]
async fn stale_contact_lookup_cannot_write_into_a_new_session() {
    let (mut worker, events, _, _) = worker();
    worker.session_generation = 4;

    worker
        .handle_command(Command::ContactChecked {
            session_generation: 3,
            phone: "15551234567".into(),
            full_name: Some("Old account contact".into()),
            first_name: Some("Old".into()),
            to_phone: true,
            registered: true,
        })
        .await;

    assert!(
        worker
            .archive
            .contact("15551234567@s.whatsapp.net")
            .expect("archive query")
            .is_none()
    );
    assert!(events.try_iter().next().is_none());
}

#[tokio::test]
async fn stale_contact_save_and_me_info_cannot_repopulate_new_session() {
    let (mut worker, events, _, _) = worker();
    worker.session_generation = 7;

    worker
        .handle_command(Command::ContactSaved {
            session_generation: 6,
            id: "15551234567@s.whatsapp.net".into(),
            name: "Old account contact".into(),
            error: None,
        })
        .await;
    worker
        .handle_command(Command::MeInfo {
            session_generation: 6,
            about: Some("Old account status".into()),
        })
        .await;

    assert!(
        worker
            .archive
            .contact("15551234567@s.whatsapp.net")
            .expect("archive query")
            .is_none()
    );
    assert!(
        worker
            .archive
            .meta("me_about")
            .expect("archive metadata")
            .is_none()
    );
    assert!(events.try_iter().next().is_none());
}

#[tokio::test]
async fn stale_download_cannot_recreate_cleared_account_cache() {
    let root = tempfile::tempdir().expect("temporary cache root");
    let cache = root.path().join("media");
    let path = cache.join("old-account-image.jpg");
    let session_generation = AtomicU64::new(9);
    let cache_lock = tokio::sync::Mutex::new(());

    let result = write_session_cache_file(
        &cache,
        &path,
        b"fixture",
        8,
        &session_generation,
        None,
        &cache_lock,
    )
    .await;

    assert!(result.is_err());
    assert!(!path.exists());
    assert!(!cache.exists());
}

#[tokio::test]
async fn invalidated_avatar_fetch_cannot_restore_old_profile_picture() {
    let root = tempfile::tempdir().expect("temporary avatar cache");
    let cache = root.path().join("avatars");
    let path = cache.join("contact.jpg");
    let session_generation = AtomicU64::new(5);
    let avatar_generation = AtomicU64::new(8);
    let cache_lock = tokio::sync::Mutex::new(());

    let result = write_session_cache_file(
        &cache,
        &path,
        b"old avatar",
        5,
        &session_generation,
        Some((7, &avatar_generation)),
        &cache_lock,
    )
    .await;

    assert!(result.is_err());
    assert!(!path.exists());
    assert!(!cache.exists());
}
