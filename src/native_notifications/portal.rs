//! Notifications through the desktop portal, version 2.
//!
//! GNotification has no field for a sound, and a Flatpak cannot reach the
//! services that take one directly. Portal version 2 accepts a sound with the
//! notification and lets the desktop decide whether to play it, which keeps Do
//! Not Disturb and the per-app sound setting in charge. Desktops whose portal
//! reports an older version keep using GNotification, without a sound.
//!
//! ashpd carries the call, the version and the notification fields, but its
//! `Notification::sound` sends a bare descriptor where the portal expects
//! `("file-descriptor", h)` and cannot say `silent`, so the sound is added to
//! the body here.

use std::future::Future;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ashpd::desktop::Icon;
use ashpd::desktop::notification::{Notification, NotificationProxy};
use ashpd::zvariant::{Fd, Type, Value, as_value};
use serde::Serialize;

use crate::notification_sound::{SOUND, SoundRequest};

/// First interface version that accepts a `sound`.
const SOUND_VERSION: u32 = 2;
const UNKNOWN_VERSION: u32 = u32::MAX;
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Only a sandboxed app is identified by the portal without extra registration.
pub fn sandboxed() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

fn takes_sound(version: u32) -> bool {
    version != UNKNOWN_VERSION && version >= SOUND_VERSION
}

/// Whether the portal takes a sound, found out once without blocking.
#[derive(Clone)]
pub struct Support {
    version: Arc<AtomicU32>,
    probing: Arc<AtomicBool>,
}

impl Default for Support {
    fn default() -> Self {
        Self {
            version: Arc::new(AtomicU32::new(UNKNOWN_VERSION)),
            probing: Arc::default(),
        }
    }
}

impl Support {
    /// Starts asking for the interface version unless it is known or already
    /// being asked. A portal that cannot be reached is asked again later.
    pub fn probe(&self) {
        if self.version.load(Ordering::Acquire) != UNKNOWN_VERSION
            || self.probing.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let support = self.clone();
        crate::portal::spawn(async move {
            match tokio::time::timeout(CALL_TIMEOUT, NotificationProxy::new()).await {
                Ok(Ok(proxy)) => support.version.store(proxy.version(), Ordering::Release),
                Ok(Err(error)) => log::debug!("could not read the notification portal: {error}"),
                Err(_) => log::debug!("the notification portal did not answer"),
            }
            support.probing.store(false, Ordering::Release);
        });
    }

    pub fn supports_sound(&self) -> bool {
        takes_sound(self.version.load(Ordering::Acquire))
    }
}

pub struct Request {
    pub id: String,
    pub title: String,
    pub body: String,
    /// PNG bytes.
    pub icon: Option<Vec<u8>>,
    /// Action the desktop activates when the notification is clicked.
    pub default_action: String,
    pub default_action_target: String,
    pub sound: SoundRequest,
}

/// What the portal is told about the sound: a sealed file, or `silent`.
enum Sound {
    File(OwnedFd),
    Silent,
}

impl Sound {
    fn of(request: SoundRequest) -> std::io::Result<Self> {
        match request {
            SoundRequest::Play => Ok(Self::File(sealed_memory_file(c"zaptide-sound", SOUND)?)),
            SoundRequest::Silent => Ok(Self::Silent),
        }
    }

    fn value(&self) -> Value<'_> {
        match self {
            Self::File(descriptor) => {
                Value::new(("file-descriptor", Value::from(Fd::from(descriptor))))
            }
            Self::Silent => Value::from("silent"),
        }
    }
}

/// The `AddNotification` body: ashpd's notification and the sound it cannot
/// express.
#[derive(Serialize, Type)]
#[zvariant(signature = "dict", crate = "ashpd::zvariant")]
struct Body<'a> {
    #[serde(flatten)]
    notification: Notification,
    #[serde(with = "as_value")]
    sound: Value<'a>,
}

fn notification(request: &Request) -> std::io::Result<Notification> {
    let mut notification = Notification::new(&request.title)
        .body(request.body.as_str())
        .default_action(request.default_action.as_str())
        .default_action_target(request.default_action_target.as_str());
    if let Some(icon) = &request.icon {
        notification = notification.icon(Icon::FileDescriptor(sealed_memory_file(
            c"zaptide-icon",
            icon,
        )?));
    }
    Ok(notification)
}

async fn send(proxy: &NotificationProxy, request: Request) -> Result<(), String> {
    let sound = Sound::of(request.sound).map_err(|error| error.to_string())?;
    let body = Body {
        notification: notification(&request).map_err(|error| error.to_string())?,
        sound: sound.value(),
    };
    tokio::time::timeout(
        CALL_TIMEOUT,
        proxy.call_method("AddNotification", &(request.id.as_str(), body)),
    )
    .await
    .map_err(|_| "the portal did not answer".to_owned())?
    .map(drop)
    .map_err(|error| error.to_string())
}

async fn send_to_desktop(request: Request) -> Result<(), String> {
    let proxy = tokio::time::timeout(CALL_TIMEOUT, NotificationProxy::new())
        .await
        .map_err(|_| "the portal did not answer".to_owned())?
        .map_err(|error| error.to_string())?;
    send(&proxy, request).await
}

/// Runs `outcome` on the portal runtime and reports a failure to `on_error` on
/// the calling thread, which must be running a main loop.
fn report_failure(
    outcome: impl Future<Output = Result<(), String>> + Send + 'static,
    on_error: impl FnOnce(String) + 'static,
) {
    let (failure, reported) = async_channel::bounded(1);
    crate::portal::spawn(async move {
        let _ = failure.send(outcome.await).await;
    });
    relm4::gtk::glib::spawn_future_local(async move {
        if let Ok(Err(reason)) = reported.recv().await {
            on_error(reason);
        }
    });
}

/// Sends the notification, replacing one with the same ID. `on_error` runs on
/// the calling thread with the reason when the portal refuses it or cannot be
/// reached.
pub fn add_notification(request: Request, on_error: impl FnOnce(String) + 'static) {
    report_failure(send_to_desktop(request), on_error);
}

/// An anonymous file holding `bytes`, sealed against any later change. The
/// portal refuses descriptors it cannot trust to stay the same while it reads.
fn sealed_memory_file(name: &std::ffi::CStr, bytes: &[u8]) -> std::io::Result<OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};

    // SAFETY: `name` is NUL terminated and the flags are valid for this call.
    let raw =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` is a new descriptor that nothing else owns.
    let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.write_all(bytes)?;
    // The reader shares this offset, so leave it at the start.
    file.seek(SeekFrom::Start(0))?;
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    // SAFETY: `file` owns a valid descriptor and F_ADD_SEALS takes an integer.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file.into())
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::fd::{AsFd, AsRawFd};
    use std::sync::Mutex;

    use ashpd::zbus;
    use zbus::zvariant::OwnedValue;

    use super::*;

    fn read_all(descriptor: OwnedFd) -> Vec<u8> {
        let mut bytes = Vec::new();
        std::fs::File::from(descriptor)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    fn request(sound: SoundRequest, icon: Option<&[u8]>) -> Request {
        Request {
            id: "token-1".into(),
            title: "Ada".into(),
            body: "New message".into(),
            icon: icon.map(<[u8]>::to_vec),
            default_action: "app.open-chat".into(),
            default_action_target: "token-1".into(),
            sound,
        }
    }

    #[test]
    fn only_version_two_and_later_take_a_sound() {
        assert!(!takes_sound(UNKNOWN_VERSION));
        assert!(!takes_sound(0));
        assert!(!takes_sound(1));
        assert!(takes_sound(2));
        assert!(takes_sound(3));
    }

    #[test]
    fn support_is_unknown_until_the_version_is_read() {
        let support = Support::default();
        assert!(!support.supports_sound());
        support.version.store(1, Ordering::Release);
        assert!(!support.supports_sound());
        support.version.store(2, Ordering::Release);
        assert!(support.supports_sound());
    }

    #[test]
    fn a_sealed_file_keeps_its_bytes_and_refuses_changes() {
        let descriptor = sealed_memory_file(c"test", b"hello").unwrap();
        // SAFETY: valid descriptor, F_GET_SEALS takes no argument.
        let seals = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GET_SEALS) };
        for seal in [
            libc::F_SEAL_SEAL,
            libc::F_SEAL_SHRINK,
            libc::F_SEAL_GROW,
            libc::F_SEAL_WRITE,
        ] {
            assert_eq!(seals & seal, seal);
        }
        let mut writer = std::fs::File::from(descriptor.try_clone().unwrap());
        assert!(writer.write_all(b"x").is_err());
        assert_eq!(read_all(descriptor), b"hello");
    }

    struct Received {
        id: String,
        notification: std::collections::HashMap<String, OwnedValue>,
    }

    /// A portal on a private peer-to-peer connection, so nothing touches the
    /// session bus.
    struct FakePortal {
        version: u32,
        refuse: bool,
        received: Arc<Mutex<Vec<Received>>>,
    }

    #[zbus::interface(
        name = "org.freedesktop.portal.Notification",
        introspection_docs = false,
        crate = "ashpd::zbus"
    )]
    impl FakePortal {
        #[zbus(property, name = "version")]
        fn version(&self) -> u32 {
            self.version
        }

        fn add_notification(
            &self,
            id: String,
            notification: std::collections::HashMap<String, OwnedValue>,
        ) -> zbus::fdo::Result<()> {
            if self.refuse {
                return Err(zbus::fdo::Error::Failed("refused".into()));
            }
            self.received
                .lock()
                .unwrap()
                .push(Received { id, notification });
            Ok(())
        }
    }

    async fn connect(portal: FakePortal) -> zbus::Result<zbus::Connection> {
        let (server_end, client_end) = tokio::net::UnixStream::pair()?;
        let guid = zbus::Guid::generate();
        let server = zbus::connection::Builder::unix_stream(server_end)
            .server(guid)?
            .p2p()
            .serve_at("/org/freedesktop/portal/desktop", portal)?
            .build();
        let client = zbus::connection::Builder::unix_stream(client_end)
            .p2p()
            .build();
        let (server, client) = tokio::try_join!(server, client)?;
        std::mem::forget(server);
        Ok(client)
    }

    async fn proxy(portal: FakePortal) -> NotificationProxy {
        NotificationProxy::with_connection(connect(portal).await.unwrap())
            .await
            .unwrap()
    }

    fn fake(version: u32, refuse: bool) -> (FakePortal, Arc<Mutex<Vec<Received>>>) {
        let received = Arc::default();
        (
            FakePortal {
                version,
                refuse,
                received: Arc::clone(&received),
            },
            received,
        )
    }

    fn version_of(version: u32) -> u32 {
        let (portal, _) = fake(version, false);
        crate::portal::run(async move { proxy(portal).await.version() }).unwrap()
    }

    #[test]
    fn the_portal_version_is_read_through_the_proxy() {
        assert_eq!(version_of(1), 1);
        assert_eq!(version_of(2), 2);
        assert!(!takes_sound(version_of(1)));
        assert!(takes_sound(version_of(2)));
    }

    fn deliver(request: Request, version: u32, refuse: bool) -> (Result<(), String>, Received) {
        let (portal, received) = fake(version, refuse);
        let outcome =
            crate::portal::run(async move { send(&proxy(portal).await, request).await }).unwrap();
        let received = received.lock().unwrap().pop();
        (
            outcome,
            received.unwrap_or(Received {
                id: String::new(),
                notification: Default::default(),
            }),
        )
    }

    /// The descriptor named by a serialized `(sv)`, read back as bytes.
    fn descriptor_bytes(value: &OwnedValue) -> Vec<u8> {
        fn find(value: &Value<'_>) -> Option<Vec<u8>> {
            match value {
                Value::Structure(structure) => {
                    let fields = structure.fields();
                    assert_eq!(<&str>::try_from(&fields[0]).unwrap(), "file-descriptor");
                    find(&fields[1])
                }
                Value::Value(inner) => find(inner),
                Value::Fd(descriptor) => {
                    let owned = descriptor.as_fd().try_clone_to_owned().ok()?;
                    Some(read_all(owned))
                }
                _ => None,
            }
        }
        find(value).expect("a file descriptor")
    }

    #[test]
    fn the_portal_receives_the_notification_with_its_sound_and_icon() {
        let (outcome, received) = deliver(request(SoundRequest::Play, Some(b"png")), 2, false);
        outcome.unwrap();
        assert_eq!(received.id, "token-1");
        let fields = &received.notification;
        assert_eq!(<&str>::try_from(&fields["title"]).unwrap(), "Ada");
        assert_eq!(<&str>::try_from(&fields["body"]).unwrap(), "New message");
        assert_eq!(
            <&str>::try_from(&fields["default-action"]).unwrap(),
            "app.open-chat"
        );
        assert_eq!(
            <&str>::try_from(&fields["default-action-target"]).unwrap(),
            "token-1"
        );
        assert_eq!(descriptor_bytes(&fields["icon"]), b"png");
        assert_eq!(descriptor_bytes(&fields["sound"]), SOUND);
    }

    #[test]
    fn ashpd_sends_a_sound_as_a_bare_descriptor_which_is_why_the_body_adds_its_own() {
        let (portal, received) = fake(2, false);
        crate::portal::run(async move {
            let sound = sealed_memory_file(c"zaptide-sound", SOUND).unwrap();
            proxy(portal)
                .await
                .add_notification(
                    "token-1",
                    Notification::new("Ada").sound::<&OwnedFd>(Some(&sound)),
                )
                .await
        })
        .unwrap()
        .unwrap();
        let received = received.lock().unwrap();
        let sound = &received[0].notification["sound"];
        assert!(matches!(&**sound, Value::Fd(_)), "{sound:?}");
    }

    #[test]
    fn a_silent_request_names_no_sound_file() {
        let (outcome, received) = deliver(request(SoundRequest::Silent, None), 2, false);
        outcome.unwrap();
        let fields = &received.notification;
        assert_eq!(<&str>::try_from(&fields["sound"]).unwrap(), "silent");
        assert!(!fields.contains_key("icon"));
    }

    #[test]
    fn a_refused_notification_reports_why() {
        let (outcome, _) = deliver(request(SoundRequest::Silent, None), 2, true);
        assert!(outcome.unwrap_err().contains("refused"));
    }

    #[test]
    fn a_failure_is_reported_on_the_calling_thread() {
        let context = relm4::gtk::glib::MainContext::new();
        let _owner = context.acquire().unwrap();
        context
            .with_thread_default(|| {
                let reasons = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let sink = reasons.clone();
                let thread = std::thread::current().id();
                report_failure(async { Err("refused".into()) }, move |reason| {
                    assert_eq!(std::thread::current().id(), thread);
                    sink.borrow_mut().push(reason);
                });
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while reasons.borrow().is_empty() {
                    assert!(std::time::Instant::now() < deadline, "timed out");
                    context.iteration(false);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                assert_eq!(*reasons.borrow(), ["refused"]);
            })
            .unwrap();
    }

    #[test]
    fn a_delivered_notification_reports_nothing() {
        let context = relm4::gtk::glib::MainContext::new();
        let _owner = context.acquire().unwrap();
        context
            .with_thread_default(|| {
                let reported = std::rc::Rc::new(std::cell::Cell::new(false));
                let flag = reported.clone();
                report_failure(async { Ok(()) }, move |_| flag.set(true));
                let until = std::time::Instant::now() + std::time::Duration::from_millis(200);
                while std::time::Instant::now() < until {
                    context.iteration(false);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                assert!(!reported.get());
            })
            .unwrap();
    }

    #[test]
    #[ignore = "talks to the session's notification portal; sends nothing"]
    fn the_session_portal_reports_its_version() {
        let version = crate::portal::run(async {
            tokio::time::timeout(CALL_TIMEOUT, NotificationProxy::new())
                .await
                .ok()
                .and_then(Result::ok)
                .map(|proxy| proxy.version())
        })
        .unwrap();
        eprintln!(
            "the notification portal reports version {version:?}, sound: {}",
            version.is_some_and(takes_sound)
        );
    }
}
