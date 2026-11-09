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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::rc::{Rc, Weak};
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
const WITHDRAW_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
pub(super) const AMBIGUOUS: &str = "ambiguous notification transport";

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

    /// Whether the portal takes a sound, or `None` until its version is read.
    pub fn sound_support(&self) -> Option<bool> {
        let version = self.version.load(Ordering::Acquire);
        (version != UNKNOWN_VERSION).then(|| takes_sound(version))
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
    proxy
        .call_method("AddNotification", &(request.id.as_str(), body))
        .await
        .map(drop)
        .map_err(|error| classify(&error))
}

/// The reason a D-Bus call failed. Only an error reply proves the desktop
/// refused; a missing reply or a broken connection leaves unknown whether it
/// acted, which is reported as [`AMBIGUOUS`].
pub(super) fn classify(error: &ashpd::zbus::Error) -> String {
    match error {
        ashpd::zbus::Error::MethodError(name, _, _)
            if !matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.NoReply"
                    | "org.freedesktop.DBus.Error.Timeout"
                    | "org.freedesktop.DBus.Error.Disconnected"
            ) =>
        {
            error.to_string()
        }
        _ => AMBIGUOUS.to_owned(),
    }
}

async fn send_to_desktop(request: Request) -> Result<(), String> {
    let proxy = tokio::time::timeout(CALL_TIMEOUT, NotificationProxy::new())
        .await
        .map_err(|_| "the portal did not answer".to_owned())?
        .map_err(|error| error.to_string())?;
    send(&proxy, request).await
}

type LocalTask = Pin<Box<dyn Future<Output = Result<(), String>>>>;
type Start = Box<dyn FnOnce() -> LocalTask>;
/// Receives the outcome and whether the operation is still the newest for its ID.
type Finish = Box<dyn FnOnce(Result<(), String>, bool)>;
type Lanes = Rc<RefCell<HashMap<String, Lane>>>;

struct Operation {
    generation: u64,
    start: Start,
    finish: Finish,
}

struct Lane {
    generation: Rc<Cell<u64>>,
    /// Operations that have run to the end, to tell a stuck lane from a busy one.
    finished: Rc<Cell<u64>>,
    pending: Rc<RefCell<Option<Operation>>>,
    quarantined: Rc<Cell<bool>>,
    abort: futures_util::future::AbortHandle,
}

impl Lane {
    fn spawn(lanes: Weak<RefCell<HashMap<String, Lane>>>, id: &str) -> Self {
        let generation = Rc::new(Cell::new(0));
        let finished = Rc::new(Cell::new(0));
        let pending = Rc::new(RefCell::new(None));
        let quarantined = Rc::new(Cell::new(false));
        let (abort, registration) = futures_util::future::AbortHandle::new_pair();
        relm4::gtk::glib::spawn_future_local(futures_util::future::Abortable::new(
            run_lane(
                lanes,
                id.to_owned(),
                generation.clone(),
                finished.clone(),
                pending.clone(),
                quarantined.clone(),
            ),
            registration,
        ));
        Self {
            generation,
            finished,
            pending,
            quarantined,
            abort,
        }
    }
}

async fn run_lane(
    lanes: Weak<RefCell<HashMap<String, Lane>>>,
    id: String,
    generation: Rc<Cell<u64>>,
    finished: Rc<Cell<u64>>,
    pending: Rc<RefCell<Option<Operation>>>,
    quarantined: Rc<Cell<bool>>,
) {
    loop {
        let Some(operation) = pending.borrow_mut().take() else {
            if let Some(lanes) = lanes.upgrade() {
                lanes.borrow_mut().remove(&id);
            }
            return;
        };
        let result = (operation.start)().await;
        finished.set(finished.get().wrapping_add(1));
        if matches!(&result, Err(reason) if reason == AMBIGUOUS) {
            quarantined.set(true);
            return;
        }
        (operation.finish)(result, operation.generation == generation.get());
    }
}

/// Runs notification operations in order, in one lane per notification ID.
///
/// A lane runs one operation at a time. While one is in flight only the newest
/// request waits behind it: each add or removal replaces the waiting one and
/// drops its payload, and `finish` learns whether its operation is still the
/// newest, so a superseded add never falls back. A lane is abandoned, with
/// whatever waits in it, when a removal is still waiting after the grace
/// period or nothing in the lane has finished since that removal was queued.
///
/// An operation that fails with [`AMBIGUOUS`] may or may not have reached the
/// desktop, so its lane is quarantined: nothing more runs for that ID, and the
/// next removal drops the lane without asking the desktop. A notification the
/// desktop showed late may then stay on screen.
#[derive(Clone)]
pub struct Queue {
    lanes: Lanes,
    grace: std::time::Duration,
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            lanes: Lanes::default(),
            grace: WITHDRAW_GRACE,
        }
    }
}

impl Queue {
    #[cfg(test)]
    fn with_grace(grace: std::time::Duration) -> Self {
        Self {
            grace,
            ..Self::default()
        }
    }

    pub(super) fn has_pending(&self, id: &str) -> bool {
        self.lanes.borrow().contains_key(id)
    }

    pub(super) fn enqueue(&self, id: &str, remove: bool, start: Start, finish: Finish) {
        let mut lanes = self.lanes.borrow_mut();
        if remove && lanes.get(id).is_some_and(|lane| lane.quarantined.get()) {
            if let Some(lane) = lanes.remove(id) {
                lane.abort.abort();
            }
            return;
        }
        let lane = lanes
            .entry(id.to_owned())
            .or_insert_with(|| Lane::spawn(Rc::downgrade(&self.lanes), id));
        let generation = lane.generation.get().wrapping_add(1);
        lane.generation.set(generation);
        *lane.pending.borrow_mut() = Some(Operation {
            generation,
            start,
            finish,
        });
        if remove {
            let finished = lane.finished.get();
            self.abandon_after_grace(id, generation, finished, Rc::downgrade(&lane.generation));
        }
    }

    fn abandon_after_grace(&self, id: &str, generation: u64, finished: u64, lane: Weak<Cell<u64>>) {
        let lanes = Rc::downgrade(&self.lanes);
        let grace = self.grace;
        let id = id.to_owned();
        relm4::gtk::glib::spawn_future_local(async move {
            relm4::gtk::glib::timeout_future(grace).await;
            let Some(lanes) = lanes.upgrade() else {
                return;
            };
            let mut lanes = lanes.borrow_mut();
            let stuck = lanes.get(&id).is_some_and(|current| {
                lane.upgrade()
                    .is_some_and(|lane| Rc::ptr_eq(&lane, &current.generation))
                    && (current.generation.get() == generation
                        || current.finished.get() == finished)
            });
            if stuck && let Some(lane) = lanes.remove(&id) {
                lane.abort.abort();
            }
        });
    }

    /// Sends `request` through the portal; `fallback` runs if the portal
    /// refuses it and no newer request for its ID has arrived.
    pub fn add(&self, request: Request, fallback: impl FnOnce() + 'static) {
        let id = request.id.clone();
        self.enqueue(
            &id,
            false,
            Box::new(move || Box::pin(on_runtime(send_to_desktop(request)))),
            Box::new(move |result, current| {
                if result.is_err() && current {
                    fallback();
                }
            }),
        );
    }

    /// Runs `withdraw` at once, then removes the portal's copy behind any
    /// operation in flight. `portal_enabled` is false when the ID never went
    /// through the portal.
    pub fn remove(&self, id: &str, portal_enabled: bool, withdraw: impl FnOnce()) {
        withdraw();
        let token = id.to_owned();
        self.enqueue(
            id,
            true,
            Box::new(move || {
                Box::pin(async move {
                    if !portal_enabled {
                        return Ok(());
                    }
                    on_runtime(async move {
                        let proxy = tokio::time::timeout(CALL_TIMEOUT, NotificationProxy::new())
                            .await
                            .map_err(|_| "the portal did not answer".to_owned())?
                            .map_err(|error| error.to_string())?;
                        proxy
                            .call_method("RemoveNotification", &(token.as_str(),))
                            .await
                            .map(drop)
                            .map_err(|error| classify(&error))
                    })
                    .await
                })
            }),
            Box::new(|_, _| {}),
        );
    }
}

pub(super) async fn on_runtime(
    future: impl Future<Output = Result<(), String>> + Send + 'static,
) -> Result<(), String> {
    let (sender, receiver) = async_channel::bounded(1);
    crate::portal::spawn(async move {
        tokio::select! {
            result = future => { let _ = sender.send(result).await; }
            _ = sender.closed() => {}
        }
    });
    receiver.recv().await.map_err(|_| AMBIGUOUS.to_owned())?
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

    const SHORT_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

    #[derive(Clone, Default)]
    struct QueuedFake {
        remote: Rc<RefCell<HashMap<String, String>>>,
        fallbacks: Rc<RefCell<Vec<String>>>,
    }

    impl QueuedFake {
        fn add(
            &self,
            queue: &Queue,
            message: (&str, &str),
            gate: async_channel::Receiver<()>,
            started: async_channel::Sender<()>,
            done: async_channel::Sender<()>,
            fail: bool,
        ) {
            let (id, body) = message;
            let key = id.to_owned();
            let value = body.to_owned();
            let remote = self.remote.clone();
            let fallbacks = self.fallbacks.clone();
            queue.enqueue(
                id,
                false,
                Box::new(move || {
                    Box::pin(async move {
                        started.send(()).await.unwrap();
                        gate.recv().await.unwrap();
                        if fail {
                            return Err("refused".into());
                        }
                        remote.borrow_mut().insert(key, value);
                        Ok(())
                    })
                }),
                Box::new(move |result, current| {
                    if result.is_err() && current {
                        fallbacks.borrow_mut().push("fallback".into());
                    }
                    done.try_send(()).unwrap();
                }),
            );
        }

        fn remove(&self, queue: &Queue, id: &str, done: async_channel::Sender<()>) {
            let key = id.to_owned();
            let remote = self.remote.clone();
            queue.enqueue(
                id,
                true,
                Box::new(move || {
                    Box::pin(async move {
                        remote.borrow_mut().remove(&key);
                        Ok(())
                    })
                }),
                Box::new(move |_, _| {
                    done.try_send(()).unwrap();
                }),
            );
        }
    }

    #[test]
    fn withdrawal_releases_a_call_that_never_replies() {
        let context = relm4::gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let fake = QueuedFake::default();
                    let queue = Queue::with_grace(SHORT_GRACE);
                    let (_gate, release) = async_channel::bounded(1);
                    let (started, started_rx) = async_channel::bounded(1);
                    let (done, done_rx) = async_channel::bounded(2);
                    fake.add(
                        &queue,
                        ("chat", "old"),
                        release,
                        started,
                        done.clone(),
                        false,
                    );
                    started_rx.recv().await.unwrap();
                    let payload = Rc::new(vec![0u8; 1024]);
                    let weak = Rc::downgrade(&payload);
                    queue.enqueue(
                        "chat",
                        true,
                        Box::new(move || {
                            Box::pin(async move {
                                drop(payload);
                                Ok(())
                            })
                        }),
                        Box::new(|_, _| panic!("abandoned remove ran")),
                    );
                    on_runtime(async {
                        tokio::time::sleep(SHORT_GRACE * 2).await;
                        Ok(())
                    })
                    .await
                    .unwrap();
                    assert!(!queue.lanes.borrow().contains_key("chat"));
                    assert!(weak.upgrade().is_none());
                    assert!(done_rx.try_recv().is_err());
                })
            })
            .unwrap();
    }

    #[test]
    fn an_add_that_replaced_a_withdrawal_behind_a_hung_call_is_released_after_grace() {
        let context = relm4::gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let fake = QueuedFake::default();
                    let queue = Queue::with_grace(SHORT_GRACE);
                    let (_gate, release) = async_channel::bounded(1);
                    let (started, started_rx) = async_channel::bounded(1);
                    let (done, done_rx) = async_channel::bounded(2);
                    fake.add(
                        &queue,
                        ("chat", "old"),
                        release,
                        started,
                        done.clone(),
                        false,
                    );
                    started_rx.recv().await.unwrap();
                    fake.remove(&queue, "chat", done.clone());
                    let payload = Rc::new(());
                    let weak = Rc::downgrade(&payload);
                    queue.enqueue(
                        "chat",
                        false,
                        Box::new(move || {
                            Box::pin(async move {
                                drop(payload);
                                Ok(())
                            })
                        }),
                        Box::new(|_, _| panic!("abandoned add ran")),
                    );
                    relm4::gtk::glib::timeout_future(SHORT_GRACE * 2).await;
                    assert!(!queue.has_pending("chat"));
                    assert!(weak.upgrade().is_none());
                    assert!(done_rx.try_recv().is_err());
                })
            })
            .unwrap();
    }

    #[test]
    fn pending_replacements_release_superseded_payloads_and_preserve_withdrawal() {
        let context = relm4::gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    for withdraw in [false, true] {
                        let fake = QueuedFake::default();
                        let queue = Queue::default();
                        let (gate, release) = async_channel::bounded(1);
                        let (started, started_rx) = async_channel::bounded(1);
                        let (done, done_rx) = async_channel::bounded(2);
                        fake.add(
                            &queue,
                            ("chat", "old"),
                            release,
                            started,
                            done.clone(),
                            false,
                        );
                        started_rx.recv().await.unwrap();
                        let mut payloads = Vec::new();
                        for number in 0..1000 {
                            let payload = Rc::new(format!("message-{number}"));
                            payloads.push(Rc::downgrade(&payload));
                            let remote = fake.remote.clone();
                            let done = done.clone();
                            queue.enqueue(
                                "chat",
                                false,
                                Box::new(move || {
                                    Box::pin(async move {
                                        remote
                                            .borrow_mut()
                                            .insert("chat".into(), (*payload).clone());
                                        Ok(())
                                    })
                                }),
                                Box::new(move |_, _| {
                                    done.try_send(()).unwrap();
                                }),
                            );
                        }
                        assert_eq!(
                            payloads
                                .iter()
                                .filter(|payload| payload.strong_count() != 0)
                                .count(),
                            1
                        );
                        if withdraw {
                            fake.remove(&queue, "chat", done.clone());
                            assert!(payloads.iter().all(|payload| payload.strong_count() == 0));
                        }
                        gate.send(()).await.unwrap();
                        done_rx.recv().await.unwrap();
                        done_rx.recv().await.unwrap();
                        assert_eq!(
                            fake.remote.borrow().get("chat").map(String::as_str),
                            if withdraw { None } else { Some("message-999") }
                        );
                    }
                })
            })
            .unwrap();
    }

    #[test]
    fn blocked_adds_are_ordered_before_replacement_and_withdrawal() {
        let context = relm4::gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    for remove in [false, true] {
                        let fake = QueuedFake::default();
                        let queue = Queue::default();
                        let (gate, release) = async_channel::bounded(1);
                        let (started, started_rx) = async_channel::bounded(2);
                        let (done, done_rx) = async_channel::bounded(2);
                        fake.add(
                            &queue,
                            ("chat", "old"),
                            release,
                            started.clone(),
                            done.clone(),
                            false,
                        );
                        started_rx.recv().await.unwrap();
                        if remove {
                            fake.remove(&queue, "chat", done.clone());
                        } else {
                            let (next_gate, next_release) = async_channel::bounded(1);
                            next_gate.send(()).await.unwrap();
                            fake.add(
                                &queue,
                                ("chat", "new"),
                                next_release,
                                started,
                                done.clone(),
                                false,
                            );
                        }
                        gate.send(()).await.unwrap();
                        done_rx.recv().await.unwrap();
                        done_rx.recv().await.unwrap();
                        assert_eq!(
                            fake.remote.borrow().get("chat").map(String::as_str),
                            if remove { None } else { Some("new") }
                        );
                    }
                })
            })
            .unwrap();
    }

    #[test]
    fn an_add_after_a_waiting_withdrawal_replaces_it() {
        let context = relm4::gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let fake = QueuedFake::default();
                    let queue = Queue::default();
                    let (gate, release) = async_channel::bounded(1);
                    let (started, started_rx) = async_channel::bounded(2);
                    let (done, done_rx) = async_channel::bounded(2);
                    fake.add(
                        &queue,
                        ("chat", "old"),
                        release,
                        started.clone(),
                        done.clone(),
                        false,
                    );
                    started_rx.recv().await.unwrap();
                    fake.remove(&queue, "chat", done.clone());
                    let (next_gate, next_release) = async_channel::bounded(1);
                    next_gate.send(()).await.unwrap();
                    fake.add(
                        &queue,
                        ("chat", "new"),
                        next_release,
                        started,
                        done.clone(),
                        false,
                    );
                    gate.send(()).await.unwrap();
                    done_rx.recv().await.unwrap();
                    done_rx.recv().await.unwrap();
                    assert_eq!(
                        fake.remote.borrow().get("chat").map(String::as_str),
                        Some("new")
                    );
                })
            })
            .unwrap();
    }

    #[test]
    fn failed_old_add_never_falls_back_after_withdrawal_or_replacement() {
        let context = relm4::gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    for remove in [false, true] {
                        let fake = QueuedFake::default();
                        let queue = Queue::default();
                        let (gate, release) = async_channel::bounded(1);
                        let (started, started_rx) = async_channel::bounded(2);
                        let (done, done_rx) = async_channel::bounded(2);
                        fake.add(
                            &queue,
                            ("chat", "old"),
                            release,
                            started.clone(),
                            done.clone(),
                            true,
                        );
                        started_rx.recv().await.unwrap();
                        if remove {
                            fake.remove(&queue, "chat", done.clone());
                        } else {
                            let (next_gate, next_release) = async_channel::bounded(1);
                            next_gate.send(()).await.unwrap();
                            fake.add(
                                &queue,
                                ("chat", "new"),
                                next_release,
                                started,
                                done.clone(),
                                false,
                            );
                        }
                        gate.send(()).await.unwrap();
                        done_rx.recv().await.unwrap();
                        done_rx.recv().await.unwrap();
                        assert!(fake.fallbacks.borrow().is_empty());
                        assert_eq!(
                            fake.remote.borrow().get("chat").map(String::as_str),
                            if remove { None } else { Some("new") }
                        );
                    }
                })
            })
            .unwrap();
    }

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
        assert_eq!(support.sound_support(), None);
        support.version.store(1, Ordering::Release);
        assert_eq!(support.sound_support(), Some(false));
        support.version.store(2, Ordering::Release);
        assert_eq!(support.sound_support(), Some(true));
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
        race: Option<Race>,
    }

    struct Race {
        started: async_channel::Sender<()>,
        release: async_channel::Receiver<()>,
        remote: Arc<Mutex<HashMap<String, String>>>,
        fail: bool,
        no_reply: bool,
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

        async fn add_notification(
            &self,
            id: String,
            notification: std::collections::HashMap<String, OwnedValue>,
        ) -> zbus::fdo::Result<()> {
            let body = notification
                .get("body")
                .and_then(|value| <&str>::try_from(value).ok())
                .unwrap_or("")
                .to_owned();
            if let Some(race) = &self.race {
                if body == "old" {
                    race.started.send(()).await.unwrap();
                    race.release.recv().await.unwrap();
                    if race.no_reply {
                        return Err(zbus::fdo::Error::NoReply("ambiguous".into()));
                    }
                    if race.fail {
                        return Err(zbus::fdo::Error::Failed("refused".into()));
                    }
                }
                race.remote.lock().unwrap().insert(id.clone(), body);
            }
            if self.refuse {
                return Err(zbus::fdo::Error::Failed("refused".into()));
            }
            self.received
                .lock()
                .unwrap()
                .push(Received { id, notification });
            Ok(())
        }

        fn remove_notification(&self, id: String) {
            if let Some(race) = &self.race {
                race.remote.lock().unwrap().remove(&id);
            }
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

    fn queue_dbus_add(
        queue: &Queue,
        connection: zbus::Connection,
        body: &str,
        done: async_channel::Sender<bool>,
    ) {
        let mut item = request(SoundRequest::Silent, None);
        item.body = body.to_owned();
        queue.enqueue(
            "token-1",
            false,
            Box::new(move || {
                Box::pin(async move {
                    on_runtime(async move {
                        let proxy = NotificationProxy::with_connection(connection)
                            .await
                            .map_err(|e| e.to_string())?;
                        send(&proxy, item).await
                    })
                    .await
                })
            }),
            Box::new(move |result, current| {
                done.try_send(result.is_err() && current).unwrap();
            }),
        );
    }

    fn queue_dbus_remove(
        queue: &Queue,
        connection: zbus::Connection,
        done: async_channel::Sender<bool>,
    ) {
        queue.enqueue(
            "token-1",
            true,
            Box::new(move || {
                Box::pin(async move {
                    on_runtime(async move {
                        let proxy = NotificationProxy::with_connection(connection)
                            .await
                            .map_err(|e| e.to_string())?;
                        proxy
                            .call_method("RemoveNotification", &("token-1",))
                            .await
                            .map(drop)
                            .map_err(|error| classify(&error))
                    })
                    .await
                })
            }),
            Box::new(move |_, _| {
                done.try_send(false).unwrap();
            }),
        );
    }

    #[test]
    fn removal_outside_the_portal_withdraws_locally_without_waiting_for_it() {
        let context = relm4::gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let queue = Queue::with_grace(SHORT_GRACE);
                    let visible = Rc::new(Cell::new(true));
                    let hide = visible.clone();
                    queue.remove("token-1", false, move || hide.set(false));
                    assert!(!visible.get());
                    on_runtime(async {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        Ok(())
                    })
                    .await
                    .unwrap();
                    assert!(
                        !queue.lanes.borrow().contains_key("token-1"),
                        "unsupported removal must not wait for a portal"
                    );

                    let (_gate, gate_rx) = async_channel::bounded::<()>(1);
                    let (started, started_rx) = async_channel::bounded(1);
                    queue.enqueue(
                        "token-2",
                        false,
                        Box::new(move || {
                            Box::pin(async move {
                                started.send(()).await.unwrap();
                                let _ = gate_rx.recv().await;
                                Ok(())
                            })
                        }),
                        Box::new(|_, _| {}),
                    );
                    started_rx.recv().await.unwrap();
                    visible.set(true);
                    let hide = visible.clone();
                    queue.remove("token-2", false, move || hide.set(false));
                    assert!(!visible.get());
                    on_runtime(async {
                        tokio::time::sleep(SHORT_GRACE * 2).await;
                        Ok(())
                    })
                    .await
                    .unwrap();
                    assert!(!visible.get());
                    assert!(!queue.lanes.borrow().contains_key("token-2"));
                })
            })
            .unwrap();
    }

    #[test]
    fn ambiguous_dbus_reply_quarantines_and_cleanup_releases_the_token() {
        let context = relm4::gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (started, started_rx) = async_channel::bounded(1);
                    let (release, release_rx) = async_channel::bounded(1);
                    let remote = Arc::new(Mutex::new(HashMap::new()));
                    let (mut portal, _) = fake(2, false);
                    portal.race = Some(Race {
                        started,
                        release: release_rx,
                        remote: remote.clone(),
                        fail: false,
                        no_reply: true,
                    });
                    let connection =
                        crate::portal::run(async move { connect(portal).await.unwrap() }).unwrap();
                    let queue = Queue::default();
                    let (done, done_rx) = async_channel::bounded(2);
                    let visible = Rc::new(Cell::new(true));
                    queue_dbus_add(&queue, connection.clone(), "old", done.clone());
                    started_rx.recv().await.unwrap();
                    release.send(()).await.unwrap();
                    on_runtime(async {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        Ok(())
                    })
                    .await
                    .unwrap();
                    assert!(
                        queue
                            .lanes
                            .borrow()
                            .get("token-1")
                            .unwrap()
                            .quarantined
                            .get()
                    );
                    let mut payloads = Vec::new();
                    for _ in 0..1000 {
                        let payload = Rc::new(vec![0u8; 1024]);
                        payloads.push(Rc::downgrade(&payload));
                        queue.enqueue(
                            "token-1",
                            false,
                            Box::new(move || {
                                Box::pin(async move {
                                    drop(payload);
                                    Ok(())
                                })
                            }),
                            Box::new(|_, _| panic!("quarantined operation ran")),
                        );
                    }
                    assert_eq!(
                        payloads
                            .iter()
                            .filter(|payload| payload.strong_count() != 0)
                            .count(),
                        1
                    );
                    let hide = visible.clone();
                    queue.remove("token-1", true, move || hide.set(false));
                    assert!(
                        !visible.get(),
                        "GIO fallback must be withdrawn during quarantine"
                    );
                    drop(connection);
                    assert!(!queue.lanes.borrow().contains_key("token-1"));
                    assert!(payloads.iter().all(|payload| payload.strong_count() == 0));
                    assert!(done_rx.try_recv().is_err());
                    assert!(remote.lock().unwrap().is_empty());
                    queue.enqueue(
                        "new-token",
                        false,
                        Box::new(|| Box::pin(async { Ok(()) })),
                        Box::new(move |result, current| {
                            assert!(result.is_ok() && current);
                            done.try_send(false).unwrap();
                        }),
                    );
                    assert!(!done_rx.recv().await.unwrap());
                })
            })
            .unwrap();
    }

    #[test]
    fn queued_dbus_add_waits_past_old_deadline_before_replacement_or_remove() {
        let context = relm4::gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    for (remove, fail) in
                        [(false, false), (true, false), (false, true), (true, true)]
                    {
                        let (started, started_rx) = async_channel::bounded(1);
                        let (release, release_rx) = async_channel::bounded(1);
                        let remote = Arc::new(Mutex::new(HashMap::from([(
                            "other-account".into(),
                            "keep".into(),
                        )])));
                        let (mut portal, _) = fake(2, false);
                        portal.race = Some(Race {
                            started,
                            release: release_rx,
                            remote: remote.clone(),
                            fail,
                            no_reply: false,
                        });
                        let connection =
                            crate::portal::run(async move { connect(portal).await.unwrap() })
                                .unwrap();
                        let queue = Queue::default();
                        let (done, done_rx) = async_channel::bounded(2);
                        queue_dbus_add(&queue, connection.clone(), "old", done.clone());
                        started_rx.recv().await.unwrap();
                        if remove {
                            queue_dbus_remove(&queue, connection, done.clone());
                        } else {
                            queue_dbus_add(&queue, connection, "new", done.clone());
                        }
                        on_runtime(async {
                            tokio::time::sleep(std::time::Duration::from_millis(5_200)).await;
                            Ok(())
                        })
                        .await
                        .unwrap();
                        assert!(done_rx.try_recv().is_err());
                        release.send(()).await.unwrap();
                        assert!(!done_rx.recv().await.unwrap());
                        assert!(!done_rx.recv().await.unwrap());
                        let state = remote.lock().unwrap();
                        assert_eq!(
                            state.get("token-1").map(String::as_str),
                            if remove { None } else { Some("new") }
                        );
                        assert_eq!(state.get("other-account").map(String::as_str), Some("keep"));
                    }
                })
            })
            .unwrap();
    }

    fn fake(version: u32, refuse: bool) -> (FakePortal, Arc<Mutex<Vec<Received>>>) {
        let received = Arc::default();
        (
            FakePortal {
                version,
                refuse,
                received: Arc::clone(&received),
                race: None,
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
