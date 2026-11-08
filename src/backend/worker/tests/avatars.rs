use super::*;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use whatsapp_rust::features::ProfilePicture;

/// Stands in for WhatsApp and the picture servers: every contact has a
/// picture, `current` is its id, and lookups and downloads take a moment.
#[derive(Default)]
struct FakePictures {
    current: String,
    in_flight: AtomicUsize,
    most_in_flight: AtomicUsize,
    lookups: Mutex<Vec<(String, bool, Option<String>)>>,
    downloads: AtomicUsize,
}

impl FakePictures {
    fn with_picture(id: &str) -> Arc<Self> {
        Arc::new(Self {
            current: id.into(),
            ..Default::default()
        })
    }
}

impl PictureSource for Arc<FakePictures> {
    async fn lookup(
        &self,
        jid: &Jid,
        preview: bool,
        existing_id: Option<&str>,
    ) -> Result<ProfilePictureLookup, String> {
        let now = self.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        self.most_in_flight.fetch_max(now, Ordering::AcqRel);
        self.lookups.lock().unwrap().push((
            jid.to_string(),
            preview,
            existing_id.map(str::to_owned),
        ));
        tokio::time::sleep(Duration::from_millis(5)).await;
        if existing_id == Some(self.current.as_str()) {
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
            return Ok(ProfilePictureLookup::Unchanged);
        }
        Ok(ProfilePictureLookup::Found(ProfilePicture {
            id: self.current.clone(),
            url: format!("https://pictures.invalid/{jid}"),
            direct_path: None,
            hash: None,
        }))
    }

    async fn download(&self, url: String) -> Result<Vec<u8>, String> {
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.downloads.fetch_add(1, Ordering::AcqRel);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        Ok(url.into_bytes())
    }
}

fn contact(index: usize) -> Jid {
    format!("1555{index:07}@s.whatsapp.net").parse().unwrap()
}

#[tokio::test]
async fn a_full_chat_list_fetches_at_most_four_pictures_at_once() {
    let source = FakePictures::with_picture("pic-1");
    let permits = Arc::new(tokio::sync::Semaphore::new(AVATAR_FETCHES));
    let mut fetches = tokio::task::JoinSet::new();
    for index in 0..40 {
        let (source, permits) = (source.clone(), permits.clone());
        fetches.spawn(async move {
            fetch_picture(&source, &permits, || true, &[contact(index)], false, None).await
        });
    }
    while let Some(fetched) = fetches.join_next().await {
        assert!(matches!(
            fetched.unwrap(),
            Ok(FetchedPicture::Downloaded { .. })
        ));
    }
    assert_eq!(source.downloads.load(Ordering::Acquire), 40);
    assert_eq!(
        source.most_in_flight.load(Ordering::Acquire),
        AVATAR_FETCHES
    );
}

#[tokio::test]
async fn an_unchanged_picture_id_skips_the_download() {
    let source = FakePictures::with_picture("pic-1");
    let permits = tokio::sync::Semaphore::new(AVATAR_FETCHES);
    let jid = contact(1);
    let known = KnownPicture {
        jid: jid.to_string(),
        preview: true,
        id: "pic-1".into(),
    };

    let fetched = fetch_picture(
        &source,
        &permits,
        || true,
        std::slice::from_ref(&jid),
        false,
        Some(&known),
    )
    .await;
    assert!(matches!(fetched, Ok(FetchedPicture::Unchanged)));
    assert_eq!(source.downloads.load(Ordering::Acquire), 0);
    assert_eq!(
        source.lookups.lock().unwrap().as_slice(),
        [(jid.to_string(), true, Some("pic-1".to_owned()))]
    );

    let changed = KnownPicture {
        id: "pic-0".into(),
        ..known.clone()
    };
    match fetch_picture(
        &source,
        &permits,
        || true,
        std::slice::from_ref(&jid),
        false,
        Some(&changed),
    )
    .await
    {
        Ok(FetchedPicture::Downloaded { picture, .. }) => assert_eq!(picture, Some(known.clone())),
        _ => panic!("a new picture is downloaded"),
    }
    // The id of a preview says nothing about the full-size picture.
    match fetch_picture(&source, &permits, || true, &[jid], true, Some(&known)).await {
        Ok(FetchedPicture::Downloaded { picture, .. }) => {
            assert_eq!(picture.map(|picture| picture.preview), Some(false));
        }
        _ => panic!("the full picture is downloaded"),
    }
    assert_eq!(source.downloads.load(Ordering::Acquire), 2);
}

#[test]
fn a_picture_id_survives_the_cache_and_a_damaged_one_is_ignored() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("contact.jpg");
    let known = KnownPicture {
        jid: contact(1).to_string(),
        preview: false,
        id: "pic-1".into(),
    };
    std::fs::write(KnownPicture::sidecar(&path), known.encode()).unwrap();
    assert_eq!(KnownPicture::read(&path), Some(known));
    std::fs::write(KnownPicture::sidecar(&path), "garbage").unwrap();
    assert_eq!(KnownPicture::read(&path), None);
}

#[test]
fn stale_pictures_go_stale_spread_over_the_half_day_before_the_old_limit() {
    let ids = (0..1000)
        .map(|index| format!("1555{index:07}@s.whatsapp.net"))
        .collect::<Vec<_>>();
    let stale = |age: Duration| ids.iter().filter(|id| !avatar_is_fresh(id, age)).count();
    let hour = Duration::from_secs(60 * 60);
    assert_eq!(
        stale(AVATAR_FRESH - AVATAR_SPREAD - hour),
        0,
        "nothing before 12 h"
    );
    let first_hour = stale(AVATAR_FRESH - AVATAR_SPREAD + hour);
    assert!((20..=150).contains(&first_hour), "{first_hour} stale");
    assert!(stale(AVATAR_FRESH - hour) < ids.len());
    assert_eq!(
        stale(AVATAR_FRESH),
        ids.len(),
        "none outlives the old limit"
    );
    assert!(ids.iter().all(|id| avatar_spread(id) == avatar_spread(id)));
}

#[tokio::test]
async fn a_request_made_stale_while_queued_asks_nothing() {
    let source = FakePictures::with_picture("pic-1");
    let permits = tokio::sync::Semaphore::new(AVATAR_FETCHES);
    let fetched = fetch_picture(&source, &permits, || false, &[contact(1)], false, None).await;
    assert!(fetched.is_err());
    assert!(source.lookups.lock().unwrap().is_empty());
    assert_eq!(source.downloads.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn a_picture_and_its_id_are_written_together_or_not_at_all() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("avatars").join("contact.jpg");
    let sidecar = KnownPicture::sidecar(&path);
    let known = KnownPicture {
        jid: contact(1).to_string(),
        preview: true,
        id: "pic-2".into(),
    };
    let (session, avatar, lock) = (
        AtomicU64::new(3),
        AtomicU64::new(1),
        tokio::sync::Mutex::new(()),
    );

    // A picture update between the lookup and the write.
    avatar.store(2, Ordering::Release);
    let written = write_avatar_cache(
        &path,
        b"new",
        Some(&known),
        3,
        &session,
        (1, &avatar),
        &lock,
    )
    .await;
    assert!(written.is_err());
    assert!(!path.exists() && !sidecar.exists());

    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"old").unwrap();
    std::fs::write(&sidecar, "stale id").unwrap();
    let written = write_avatar_cache(&path, b"new", None, 3, &session, (2, &avatar), &lock).await;
    assert_eq!(written.as_deref(), Ok(path.as_path()));
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    assert!(!sidecar.exists(), "an id from the old picture never stays");

    write_avatar_cache(
        &path,
        b"newer",
        Some(&known),
        3,
        &session,
        (2, &avatar),
        &lock,
    )
    .await
    .unwrap();
    assert_eq!(KnownPicture::read(&path), Some(known));
}

#[tokio::test]
async fn keeping_a_confirmed_picture_marks_it_fresh_without_rewriting_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("contact.jpg");
    std::fs::write(&path, b"picture").unwrap();
    let old = std::time::SystemTime::now() - 2 * AVATAR_FRESH;
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let (session, avatar, lock) = (
        AtomicU64::new(3),
        AtomicU64::new(1),
        tokio::sync::Mutex::new(()),
    );

    let kept = keep_session_cache_file(&path, 3, &session, (1, &avatar), &lock).await;
    assert_eq!(kept.as_deref(), Ok(path.as_path()));
    assert_eq!(std::fs::read(&path).unwrap(), b"picture");
    let age = std::fs::metadata(&path)
        .unwrap()
        .modified()
        .unwrap()
        .elapsed()
        .unwrap();
    assert!(avatar_is_fresh("contact", age));

    avatar.store(2, Ordering::Release);
    assert!(
        keep_session_cache_file(&path, 3, &session, (1, &avatar), &lock)
            .await
            .is_err(),
        "a picture update in the meantime wins"
    );
}
