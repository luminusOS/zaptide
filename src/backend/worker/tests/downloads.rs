use super::*;
use std::io::{Read, Write};
use whatsapp_rust::download::{DownloadParams, MediaDownloader, MediaHost, MediaRoute};
use whatsapp_rust::http::{HttpClient, HttpRequest, HttpResponse};
use whatsapp_rust::wacore::net::StreamingHttpResponse;

type Body = Box<dyn Fn() -> Box<dyn Read + Send> + Send + Sync>;

/// A CDN that answers every request with the same body, streamed.
struct FakeCdn(u16, Body);

#[whatsapp_rust::async_trait]
impl HttpClient for FakeCdn {
    async fn execute(&self, _request: HttpRequest) -> anyhow::Result<HttpResponse> {
        let mut body = Vec::new();
        (self.1)().read_to_end(&mut body)?;
        Ok(HttpResponse {
            status_code: self.0,
            body,
        })
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn execute_streaming(&self, _request: HttpRequest) -> anyhow::Result<StreamingHttpResponse> {
        Ok(StreamingHttpResponse {
            status_code: self.0,
            body: (self.1)(),
        })
    }
}

fn downloader(body: Body) -> MediaDownloader {
    downloader_answering(200, body)
}

fn downloader_answering(status: u16, body: Body) -> MediaDownloader {
    MediaDownloader::new(
        Arc::new(FakeCdn(status, body)),
        Arc::new(whatsapp_rust::TokioRuntime),
        MediaRoute::unauthenticated(vec![MediaHost::new("cdn.invalid")]),
    )
}

fn encrypted(plaintext: &[u8]) -> (DownloadParams, Vec<u8>) {
    let media = whatsapp_rust::wacore::upload::encrypt_media(plaintext, MediaType::Document)
        .expect("synthetic media encrypts");
    let params = DownloadParams::encrypted(
        "/v/t62.fixture/document",
        &media.media_key,
        &media.file_sha256,
        &media.file_enc_sha256,
        plaintext.len() as u64,
        MediaType::Document,
    );
    (params, media.data_to_upload)
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.map(|entry| entry.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

fn shared(body: Vec<u8>) -> Body {
    let body = Arc::new(body);
    Box::new(move || Box::new(std::io::Cursor::new(body.as_ref().clone())))
}

#[tokio::test]
async fn a_streamed_download_lands_in_the_staging_file_chunk_by_chunk() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path().join("media");
    let staging = download_staging_path(&dir);
    let generation = AtomicU64::new(3);
    let lock = tokio::sync::Mutex::new(());
    let chunk = |index: usize| vec![(index % 251) as u8; 4096];

    let result = stream_session_cache_file(
        &dir,
        &staging,
        3,
        &generation,
        &lock,
        |mut file| async move {
            for index in 0..300 {
                file.write_all(&chunk(index))?;
            }
            Ok::<_, std::io::Error>(file)
        },
    )
    .await;

    assert_eq!(result.unwrap(), staging);
    let expected: Vec<u8> = (0..300).flat_map(chunk).collect();
    assert_eq!(std::fs::read(&staging).unwrap(), expected);
}

#[tokio::test]
async fn a_verified_library_download_streams_the_plaintext_to_disk() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path().to_owned();
    let staging = download_staging_path(&dir);
    let generation = AtomicU64::new(1);
    let lock = tokio::sync::Mutex::new(());
    let plaintext: Vec<u8> = (0..200_000u32).map(|n| (n * 7 % 256) as u8).collect();
    let (params, body) = encrypted(&plaintext);
    let downloader = downloader(shared(body));

    let result = stream_session_cache_file(&dir, &staging, 1, &generation, &lock, |file| async {
        downloader.download_to_writer(&params, file).await
    })
    .await;

    assert_eq!(result.unwrap(), staging);
    assert_eq!(std::fs::read(&staging).unwrap(), plaintext);
}

#[tokio::test]
async fn a_download_that_fails_verification_leaves_no_file_behind() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path().to_owned();
    let staging = download_staging_path(&dir);
    let generation = AtomicU64::new(1);
    let lock = tokio::sync::Mutex::new(());
    let plaintext = vec![0x42; 100_000];
    let (params, mut body) = encrypted(&plaintext);
    let last = body.len() - 1;
    body[last] ^= 1;
    let downloader = downloader(shared(body));

    let result = stream_session_cache_file(&dir, &staging, 1, &generation, &lock, |file| async {
        downloader.download_to_writer(&params, file).await
    })
    .await;

    assert!(matches!(result, Err(StagedDownload::Fetch(_))));
    assert!(!staging.exists());
    assert_eq!(files_in(&dir), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn a_cdn_refusal_keeps_its_status_through_the_staging_file() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    let generation = AtomicU64::new(1);
    let lock = tokio::sync::Mutex::new(());
    let (params, _) = encrypted(b"expired");

    for (status, reupload) in [(404, true), (410, true), (500, false)] {
        let downloader = downloader_answering(status, shared(Vec::new()));
        let staging = download_staging_path(dir);
        let result =
            stream_session_cache_file(dir, &staging, 1, &generation, &lock, |file| async {
                downloader.download_to_writer(&params, file).await
            })
            .await;
        let Err(StagedDownload::Fetch(error)) = result else {
            panic!("status {status} should fail the fetch");
        };
        assert!(error.to_string().contains(&status.to_string()), "{error}");
        assert_eq!(
            failures::needs_reupload(&anyhow::Error::from(error)),
            reupload,
            "{status}"
        );
        assert!(files_in(dir).is_empty());
    }
}

#[tokio::test]
async fn dropping_a_download_future_removes_its_partial_file() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path();
    let staging = download_staging_path(dir);
    let generation = AtomicU64::new(1);
    let lock = tokio::sync::Mutex::new(());
    let (started, ready) = tokio::sync::oneshot::channel();
    let mut future = Box::pin(stream_session_cache_file(
        dir,
        &staging,
        1,
        &generation,
        &lock,
        |mut file| async move {
            file.write_all(b"partial").unwrap();
            started.send(()).unwrap();
            std::future::pending::<Result<std::fs::File, std::io::Error>>().await
        },
    ));
    tokio::select! {
        result = &mut future => panic!("download finished unexpectedly: {result:?}"),
        result = ready => result.unwrap(),
    }
    assert_eq!(std::fs::read(&staging).unwrap(), b"partial");
    drop(future);
    assert!(files_in(dir).is_empty());
}

#[tokio::test]
async fn a_failed_stream_removes_what_it_wrote() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path().to_owned();
    let staging = download_staging_path(&dir);
    let generation = AtomicU64::new(1);
    let lock = tokio::sync::Mutex::new(());

    let result = stream_session_cache_file(
        &dir,
        &staging,
        1,
        &generation,
        &lock,
        |mut file| async move {
            file.write_all(b"unverified bytes")?;
            Err::<std::fs::File, _>(std::io::Error::other("connection reset"))
        },
    )
    .await;

    assert!(
        matches!(&result, Err(StagedDownload::Fetch(error)) if error.to_string() == "connection reset")
    );
    assert_eq!(files_in(&dir), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn an_account_change_while_streaming_discards_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path().to_owned();
    let staging = download_staging_path(&dir);
    let generation = AtomicU64::new(1);
    let lock = tokio::sync::Mutex::new(());

    let result = stream_session_cache_file(&dir, &staging, 1, &generation, &lock, |mut file| {
        let generation = &generation;
        async move {
            file.write_all(b"old account")?;
            generation.store(2, Ordering::Release);
            Ok::<_, std::io::Error>(file)
        }
    })
    .await;

    assert!(matches!(result, Err(StagedDownload::Local(_))));
    assert_eq!(files_in(&dir), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn a_stale_stream_does_not_recreate_a_cleared_cache() {
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path().join("media");
    let staging = download_staging_path(&dir);
    let generation = AtomicU64::new(9);
    let lock = tokio::sync::Mutex::new(());
    let mut fetched = false;

    let result = stream_session_cache_file(&dir, &staging, 8, &generation, &lock, |file| {
        fetched = true;
        async move { Ok::<_, std::io::Error>(file) }
    })
    .await;

    assert!(matches!(result, Err(StagedDownload::Local(_))));
    assert!(!fetched);
    assert!(!dir.exists());
}

fn peak_resident_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap()
}

/// Run alone (`-- --ignored --test-threads=1`): VmHWM is the whole process's
/// peak, so other tests running beside it would blur the figure.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measures process peak memory; run alone"]
async fn streaming_a_large_download_keeps_peak_memory_flat() {
    const SIZE: u64 = 256 * 1024 * 1024;
    let directory = tempfile::tempdir().unwrap();
    let dir = directory.path().to_owned();
    let encrypted_path = dir.join("body.enc");
    let info = whatsapp_rust::wacore::upload::encrypt_media_streaming(
        std::io::repeat(0x5a).take(SIZE),
        std::io::BufWriter::new(std::fs::File::create(&encrypted_path).unwrap()),
        MediaType::Document,
    )
    .unwrap();
    let params = DownloadParams::encrypted(
        "/v/t62.fixture/large",
        &info.media_key,
        &info.file_sha256,
        &info.file_enc_sha256,
        SIZE,
        MediaType::Document,
    );
    let body_path = encrypted_path.clone();
    let downloader = downloader(Box::new(move || {
        Box::new(std::io::BufReader::new(
            std::fs::File::open(&body_path).unwrap(),
        ))
    }));
    let generation = AtomicU64::new(1);
    let lock = tokio::sync::Mutex::new(());
    let staging = download_staging_path(&dir);

    let before = peak_resident_kib();
    let result = stream_session_cache_file(&dir, &staging, 1, &generation, &lock, |file| async {
        downloader.download_to_writer(&params, file).await
    })
    .await;
    let streamed = peak_resident_kib();
    assert_eq!(result.unwrap(), staging);
    assert_eq!(std::fs::metadata(&staging).unwrap().len(), SIZE);

    let in_memory = downloader.download(&params).await.unwrap();
    let buffered = peak_resident_kib();
    assert_eq!(in_memory.len() as u64, SIZE);
    eprintln!(
        "VmHWM kB: before {before}, after streaming {streamed} (+{}), after in-memory {buffered} (+{})",
        streamed - before,
        buffered - streamed
    );
    assert!(streamed - before < 32 * 1024, "streaming grew the peak");
}
