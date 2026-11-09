//! Delivery of chat notifications, with the notification sound where the
//! desktop can take it.
//!
//! A sandboxed build goes through the notification portal; any other build
//! talks to the desktop notification service directly. Both fall back to
//! GNotification when the desktop refuses or cannot take the sound.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use super::{OPEN_CHAT_ACTION, freedesktop, portal};
use crate::i18n::gettext;
use crate::notification_sound::SoundRequest;
use gtk::glib::variant::ToVariant;
use gtk::prelude::*;
use relm4::gtk;

/// Edge, in pixels, of the picture sent as `image-data`. The raw RGBA crosses
/// D-Bus with every notification, and servers draw it far smaller.
const IMAGE_DATA_EDGE: u32 = 128;

pub(super) struct Request {
    pub token: String,
    pub title: String,
    pub body: String,
    pub icon: Option<Vec<u8>>,
    /// The same notification for GNotification, used as is or as the fallback.
    pub notification: gtk::gio::Notification,
}

/// Shows and withdraws notifications by token, without a sound of its own.
pub(super) trait Port {
    fn show(&self, request: Request);
    fn withdraw(&self, token: &str);
}

/// Shows and withdraws notifications that say what to do about the sound.
trait SoundPort {
    fn show(&self, request: Request, sound: SoundRequest);
    fn withdraw(&self, token: &str);
}

/// Sends chat notifications through the sound-capable transport while the
/// person wants the sound, and through plain GNotification otherwise, as
/// before the sound existed. A token that changes transport is withdrawn from
/// the old one first, so it is never shown twice.
pub(super) struct Notifier {
    plain: Rc<dyn Port>,
    with_sound: Rc<dyn SoundPort>,
    on_sound: Rc<RefCell<HashSet<String>>>,
}

impl Notifier {
    /// `sound` is `None` when the person turned the sound off.
    pub fn show(&self, request: Request, sound: Option<SoundRequest>) {
        match sound {
            Some(sound) => {
                if self.on_sound.borrow_mut().insert(request.token.clone()) {
                    self.plain.withdraw(&request.token);
                }
                self.with_sound.show(request, sound);
            }
            None => {
                if self.on_sound.borrow_mut().remove(&request.token) {
                    self.with_sound.withdraw(&request.token);
                }
                self.plain.show(request);
            }
        }
    }

    pub fn withdraw(&self, token: &str) {
        if self.on_sound.borrow_mut().remove(token) {
            self.with_sound.withdraw(token);
        } else {
            self.plain.withdraw(token);
        }
    }
}

#[derive(Clone)]
struct Gio(gtk::Application);
impl Port for Gio {
    fn show(&self, request: Request) {
        self.0
            .send_notification(Some(&request.token), &request.notification);
    }
    fn withdraw(&self, token: &str) {
        self.0.withdraw_notification(token);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Route {
    Gio,
    Portal,
    /// The first portal delivery of a token that may already be on screen
    /// through GNotification.
    PortalAfterGio,
}

/// Pins a token to one transport once the portal version is known, so a
/// replacement never lands beside its predecessor. Until then, notifications
/// go through GNotification without pinning.
fn route(modes: &mut HashMap<String, bool>, token: &str, sound_support: Option<bool>) -> Route {
    if let Some(&portal) = modes.get(token) {
        return if portal { Route::Portal } else { Route::Gio };
    }
    let Some(portal) = sound_support else {
        return Route::Gio;
    };
    modes.insert(token.to_owned(), portal);
    if portal {
        Route::PortalAfterGio
    } else {
        Route::Gio
    }
}

struct Portal {
    gio: Gio,
    support: portal::Support,
    queue: portal::Queue,
    /// Tokens pinned to a transport: true for the portal.
    modes: RefCell<HashMap<String, bool>>,
}
impl SoundPort for Portal {
    fn show(&self, request: Request, sound: SoundRequest) {
        self.support.probe();
        let route = route(
            &mut self.modes.borrow_mut(),
            &request.token,
            self.support.sound_support(),
        );
        match route {
            Route::Gio => {
                self.gio.show(request);
                return;
            }
            Route::PortalAfterGio => self.gio.withdraw(&request.token),
            Route::Portal => (),
        }
        let fallback = self.gio.clone();
        let portal_request = portal::Request {
            id: request.token.clone(),
            title: request.title.clone(),
            body: request.body.clone(),
            icon: request.icon.clone(),
            default_action: format!("app.{OPEN_CHAT_ACTION}"),
            default_action_target: request.token.clone(),
            sound,
        };
        self.queue
            .add(portal_request, move || fallback.show(request));
    }
    fn withdraw(&self, token: &str) {
        // `route` records a token only once it may go through the portal.
        let portal = self.modes.borrow_mut().remove(token).unwrap_or(false);
        let gio = self.gio.clone();
        let id = token.to_owned();
        self.queue.remove(token, portal, move || gio.withdraw(&id));
    }
}

pub(super) fn image_data(icon: &[u8]) -> Result<ashpd::zbus::zvariant::OwnedValue, String> {
    let crate::native_media::ThumbnailResult::Image(thumbnail) =
        crate::native_media::decode_thumbnail(icon)
    else {
        return Err("notification icon could not be decoded".into());
    };
    let image = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.rgba)
        .ok_or("notification icon has an invalid size")?;
    let image = image::DynamicImage::ImageRgba8(image)
        .thumbnail(IMAGE_DATA_EDGE, IMAGE_DATA_EDGE)
        .into_rgba8();
    let width = i32::try_from(image.width()).map_err(|e| e.to_string())?;
    let height = i32::try_from(image.height()).map_err(|e| e.to_string())?;
    ashpd::zbus::zvariant::Value::new((
        width,
        height,
        width * 4,
        true,
        8_i32,
        4_i32,
        image.into_raw(),
    ))
    .try_into()
    .map_err(|e: ashpd::zbus::zvariant::Error| e.to_string())
}

type SharedSession = Arc<tokio::sync::Mutex<Option<freedesktop::Session>>>;

/// Talks to `org.freedesktop.Notifications` directly, which takes a sound file
/// and reports which notifications were clicked or closed.
struct Native {
    gio: Rc<dyn Port>,
    queue: portal::Queue,
    session: SharedSession,
    /// Tokens the server refused, shown through GNotification until withdrawn.
    fallback: Rc<RefCell<HashSet<String>>>,
    /// Tokens the server holds, whose next failure is ambiguous.
    delivered: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Tokens shown and not withdrawn since.
    active: Arc<std::sync::Mutex<HashSet<String>>>,
    events: async_channel::Sender<freedesktop::Event>,
    connection: Option<ashpd::zbus::Connection>,
    /// The application ID, which lets the desktop apply its per-app settings.
    desktop_entry: String,
}
impl SoundPort for Native {
    fn show(&self, request: Request, sound: SoundRequest) {
        if self.fallback.borrow().contains(&request.token) {
            self.gio.show(request);
            return;
        }
        let token = request.token.clone();
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token.clone());
        let content = Content {
            title: request.title.clone(),
            body: request.body.clone(),
            icon: request.icon.clone(),
            sound,
            open_label: gettext("Open"),
            desktop_entry: self.desktop_entry.clone(),
        };
        let id = token.clone();
        let session = self.session.clone();
        let events = self.events.clone();
        let connection = self.connection.clone();
        let delivered = self.delivered.clone();
        let active = self.active.clone();
        let fallback = self.fallback.clone();
        let gio = self.gio.clone();
        self.queue.enqueue(
            &token,
            false,
            Box::new(move || {
                Box::pin(portal::on_runtime(async move {
                    let previously_delivered = delivered
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains(&id);
                    let result = async {
                        let mut session = session.lock().await;
                        let session = current_session(&mut session, connection, events).await?;
                        notify(session, &id, content).await
                    }
                    .await;
                    if result.is_ok()
                        && active
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .contains(&id)
                    {
                        delivered
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(id);
                    }
                    if previously_delivered && result.is_err() {
                        return Err(portal::AMBIGUOUS.to_owned());
                    }
                    result
                }))
            }),
            Box::new(move |result, current| {
                if result.is_err() && current {
                    fallback.borrow_mut().insert(request.token.clone());
                    gio.show(request);
                }
            }),
        );
    }
    fn withdraw(&self, token: &str) {
        self.gio.withdraw(token);
        self.fallback.borrow_mut().remove(token);
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
        self.delivered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
        let id = token.to_owned();
        let session = self.session.clone();
        self.queue.enqueue(
            token,
            true,
            Box::new(move || {
                Box::pin(async move {
                    portal::on_runtime(async move {
                        if let Some(session) = session.lock().await.as_ref() {
                            session
                                .withdraw(&id)
                                .await
                                .map_err(|_| portal::AMBIGUOUS.to_owned())?;
                        }
                        Ok(())
                    })
                    .await
                })
            }),
            Box::new(|_, _| {}),
        );
    }
}

/// What [`notify`] sends, gathered on the GTK thread.
struct Content {
    title: String,
    body: String,
    icon: Option<Vec<u8>>,
    sound: SoundRequest,
    open_label: String,
    desktop_entry: String,
}

/// The session with the current server owner, opened again after the server
/// restarted or changed. Its click and close events go to `events`.
async fn current_session(
    slot: &mut Option<freedesktop::Session>,
    connection: Option<ashpd::zbus::Connection>,
    events: async_channel::Sender<freedesktop::Event>,
) -> Result<&freedesktop::Session, String> {
    if let Some(existing) = slot.as_ref()
        && !existing.is_current().await
    {
        *slot = None;
    }
    if slot.is_none() {
        let connection = match connection {
            Some(connection) => connection,
            None => ashpd::zbus::Connection::session()
                .await
                .map_err(|e| e.to_string())?,
        };
        let (session, receiver) = freedesktop::Session::new(connection)
            .await
            .map_err(|e| e.to_string())?;
        crate::portal::spawn(async move {
            while let Ok(event) = receiver.recv().await {
                if events.send(event).await.is_err() {
                    break;
                }
            }
        });
        *slot = Some(session);
    }
    slot.as_ref()
        .ok_or_else(|| "notification session unavailable".into())
}

async fn notify(session: &freedesktop::Session, id: &str, content: Content) -> Result<(), String> {
    if !session.supports("actions") {
        return Err("notification actions unavailable".into());
    }
    let mut hints = HashMap::new();
    hints.insert(
        "desktop-entry".into(),
        ashpd::zbus::zvariant::Str::from(content.desktop_entry).into(),
    );
    if let Some(icon) = content.icon {
        hints.insert("image-data".into(), image_data(&icon)?);
    }
    match content.sound {
        SoundRequest::Silent => {
            hints.insert("suppress-sound".into(), true.into());
        }
        SoundRequest::Play if session.supports("sound") => {
            let path = crate::notification_sound::native_sound_file().map_err(|e| e.to_string())?;
            let path = path.to_str().ok_or("sound path is not UTF-8")?;
            hints.insert(
                "sound-file".into(),
                ashpd::zbus::zvariant::Str::from(path).into(),
            );
        }
        SoundRequest::Play => {}
    }
    let body_format = if session.supports("body-markup") {
        super::BodyFormat::Markup
    } else {
        super::BodyFormat::Plain
    };
    let body = super::notification_body(&content.body, body_format);
    session
        .show(
            id,
            freedesktop::Notification {
                app_name: "ZapTide",
                replaces_id: 0,
                app_icon: "",
                summary: &content.title,
                body: &body,
                actions: vec!["default".into(), content.open_label],
                hints,
                expire_timeout: -1,
            },
        )
        .await
        .map(drop)
        .map_err(|error| portal::classify(&error))
}

#[derive(Debug, PartialEq, Eq)]
enum BackendChoice {
    Portal,
    Native,
}

fn backend_choice(sandboxed: bool) -> BackendChoice {
    if sandboxed {
        BackendChoice::Portal
    } else {
        BackendChoice::Native
    }
}

fn handle_event(
    event: freedesktop::Event,
    queue: &portal::Queue,
    active: &std::sync::Mutex<HashSet<String>>,
    delivered: &std::sync::Mutex<HashSet<String>>,
    activate: impl FnOnce(&str),
    closed: impl FnOnce(&str),
) {
    match event {
        freedesktop::Event::Activated(token) => activate(&token),
        freedesktop::Event::Closed(token) if !queue.has_pending(&token) => {
            active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&token);
            delivered
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&token);
            closed(&token);
        }
        freedesktop::Event::Closed(_) => {}
    }
}

/// The chat notification sender for this build. `closed` receives the token of
/// a notification the person dismissed, where the desktop reports it.
pub(super) fn create(
    application: &gtk::Application,
    sandboxed: bool,
    closed: impl Fn(&str) + 'static,
) -> Notifier {
    let gio = Gio(application.clone());
    let on_sound: Rc<RefCell<HashSet<String>>> = Rc::default();
    let dismissed = on_sound.clone();
    let closed = move |token: &str| {
        dismissed.borrow_mut().remove(token);
        closed(token);
    };
    let with_sound: Rc<dyn SoundPort> = if backend_choice(sandboxed) == BackendChoice::Portal {
        let support = portal::Support::default();
        support.probe();
        Rc::new(Portal {
            gio: gio.clone(),
            support,
            queue: portal::Queue::default(),
            modes: RefCell::new(HashMap::new()),
        })
    } else {
        native(application, gio.clone(), closed)
    };
    Notifier {
        plain: Rc::new(gio),
        with_sound,
        on_sound,
    }
}

fn native(
    application: &gtk::Application,
    gio: Gio,
    closed: impl Fn(&str) + 'static,
) -> Rc<dyn SoundPort> {
    let (events, receiver) = async_channel::unbounded();
    let queue = portal::Queue::default();
    let event_queue = queue.clone();
    let desktop_entry = application
        .application_id()
        .map(String::from)
        .unwrap_or_default();
    let application = application.clone();
    let active: Arc<std::sync::Mutex<HashSet<String>>> = Arc::default();
    let delivered: Arc<std::sync::Mutex<HashSet<String>>> = Arc::default();
    let active_events = active.clone();
    let delivered_events = delivered.clone();
    gtk::glib::spawn_future_local(async move {
        while let Ok(event) = receiver.recv().await {
            handle_event(
                event,
                &event_queue,
                &active_events,
                &delivered_events,
                |token| application.activate_action(OPEN_CHAT_ACTION, Some(&token.to_variant())),
                |token| closed(token),
            );
        }
    });
    Rc::new(Native {
        gio: Rc::new(gio),
        queue,
        session: Arc::default(),
        fallback: Rc::default(),
        delivered,
        active,
        events,
        connection: None,
        desktop_entry,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ashpd::zbus::{self, zvariant::OwnedValue};

    type Wire = (String, Vec<String>, HashMap<String, OwnedValue>);

    struct FakeServer(async_channel::Sender<Wire>);

    #[zbus::interface(name = "org.freedesktop.Notifications", crate = "ashpd::zbus")]
    impl FakeServer {
        fn get_capabilities(&self) -> Vec<String> {
            vec!["actions".into(), "sound".into(), "body-markup".into()]
        }
        #[allow(
            clippy::too_many_arguments,
            reason = "D-Bus Notify requires eight separate wire arguments"
        )]
        fn notify(
            &self,
            _app: String,
            _replace: u32,
            _icon: String,
            title: String,
            body: String,
            actions: Vec<String>,
            hints: HashMap<String, OwnedValue>,
            _timeout: i32,
        ) -> zbus::fdo::Result<u32> {
            self.0.try_send((body, actions, hints)).unwrap();
            match title.as_str() {
                "refuse" => Err(zbus::fdo::Error::Failed("refused".into())),
                "no-reply" => Err(zbus::fdo::Error::NoReply("unknown".into())),
                _ => Ok(42),
            }
        }
        fn close_notification(&self, _id: u32) {}
    }

    struct FakeGio(Rc<RefCell<usize>>);
    impl Port for FakeGio {
        fn show(&self, _: Request) {
            *self.0.borrow_mut() += 1;
        }
        fn withdraw(&self, _: &str) {}
    }

    struct BusGuard(std::process::Child);
    impl Drop for BusGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct BlockingServer {
        wire: async_channel::Sender<(char, u32)>,
        gate: async_channel::Receiver<()>,
    }
    #[zbus::interface(name = "org.freedesktop.Notifications", crate = "ashpd::zbus")]
    impl BlockingServer {
        fn get_capabilities(&self) -> Vec<String> {
            vec!["actions".into()]
        }
        #[allow(
            clippy::too_many_arguments,
            reason = "D-Bus Notify requires eight separate wire arguments"
        )]
        async fn notify(
            &self,
            _app: String,
            replace: u32,
            _icon: String,
            _title: String,
            _body: String,
            _actions: Vec<String>,
            _hints: HashMap<String, OwnedValue>,
            _timeout: i32,
        ) -> u32 {
            self.wire.send(('n', replace)).await.unwrap();
            self.gate.recv().await.unwrap();
            42
        }
        fn close_notification(&self, id: u32) {
            self.wire.try_send(('c', id)).unwrap();
        }
    }

    #[test]
    fn native_queue_waits_for_inflight_notify_before_close() {
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (wire, received) = async_channel::unbounded();
                    let (gate, release) = async_channel::bounded(1);
                    let (setup, ready) = async_channel::bounded(1);
                    crate::portal::spawn(async move {
                        let (a, b) = tokio::net::UnixStream::pair().unwrap();
                        let server = zbus::connection::Builder::unix_stream(a)
                            .server(zbus::Guid::generate())
                            .unwrap()
                            .p2p()
                            .serve_at(
                                "/org/freedesktop/Notifications",
                                BlockingServer {
                                    wire,
                                    gate: release,
                                },
                            )
                            .unwrap()
                            .build();
                        let client = zbus::connection::Builder::unix_stream(b).p2p().build();
                        let (server, connection) = tokio::try_join!(server, client).unwrap();
                        let (session, _) =
                            freedesktop::Session::with_owner(connection, ":peer".into(), true)
                                .await
                                .unwrap();
                        setup.send((session, server)).await.unwrap();
                    });
                    let (session, _server) = ready.recv().await.unwrap();
                    let (events, _) = async_channel::unbounded();
                    let native = Native {
                        gio: Rc::new(FakeGio(Rc::default())),
                        queue: portal::Queue::default(),
                        session: Arc::new(tokio::sync::Mutex::new(Some(session))),
                        fallback: Rc::default(),
                        delivered: Arc::default(),
                        active: Arc::default(),
                        events,
                        connection: None,
                        desktop_entry: "test.app".into(),
                    };
                    let request = || Request {
                        token: "one".into(),
                        title: "title".into(),
                        body: "body".into(),
                        icon: None,
                        notification: gtk::gio::Notification::new("title"),
                    };
                    native.show(request(), SoundRequest::Silent);
                    assert_eq!(received.recv().await.unwrap(), ('n', 0));
                    native.show(request(), SoundRequest::Silent);
                    native.withdraw("one");
                    assert!(received.try_recv().is_err());
                    gate.send(()).await.unwrap();
                    assert_eq!(received.recv().await.unwrap(), ('c', 42));
                    assert!(received.try_recv().is_err());
                })
            })
            .unwrap();
    }

    struct RestartServer {
        wire: async_channel::Sender<(char, u32)>,
        actions: bool,
    }
    #[zbus::interface(name = "org.freedesktop.Notifications", crate = "ashpd::zbus")]
    impl RestartServer {
        fn get_capabilities(&self) -> Vec<String> {
            if self.actions {
                vec!["actions".into()]
            } else {
                vec![]
            }
        }
        #[allow(
            clippy::too_many_arguments,
            reason = "D-Bus Notify requires eight separate wire arguments"
        )]
        fn notify(
            &self,
            _app: String,
            replace: u32,
            _icon: String,
            _title: String,
            _body: String,
            _actions: Vec<String>,
            _hints: HashMap<String, OwnedValue>,
            _timeout: i32,
        ) -> u32 {
            self.wire.try_send(('n', replace)).unwrap();
            42
        }
        fn close_notification(&self, id: u32) {
            self.wire.try_send(('c', id)).unwrap();
        }
    }

    #[test]
    fn native_recreates_session_after_private_bus_owner_restart() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let _bus = BusGuard(child);
        let address = address.trim().to_owned();
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async move {
                    let (wire, received) = async_channel::unbounded();
                    let (setup, ready) = async_channel::bounded(1);
                    let bus_address = address.clone();
                    let first_wire = wire.clone();
                    crate::portal::spawn(async move {
                        let server = zbus::connection::Builder::address(bus_address.as_str())
                            .unwrap()
                            .name("org.freedesktop.Notifications")
                            .unwrap()
                            .serve_at(
                                "/org/freedesktop/Notifications",
                                RestartServer {
                                    wire: first_wire,
                                    actions: true,
                                },
                            )
                            .unwrap()
                            .build()
                            .await
                            .unwrap();
                        let connection = zbus::connection::Builder::address(bus_address.as_str())
                            .unwrap()
                            .build()
                            .await
                            .unwrap();
                        setup.send((server, connection)).await.unwrap();
                    });
                    let (first, connection) = ready.recv().await.unwrap();
                    let fallback = Rc::new(RefCell::new(0));
                    let (events, _) = async_channel::unbounded();
                    let native = Native {
                        gio: Rc::new(FakeGio(fallback.clone())),
                        queue: portal::Queue::default(),
                        session: Arc::default(),
                        fallback: Rc::default(),
                        delivered: Arc::default(),
                        active: Arc::default(),
                        events,
                        connection: Some(connection),
                        desktop_entry: "test.app".into(),
                    };
                    let request = |token: &str| Request {
                        token: token.into(),
                        title: "title".into(),
                        body: "body".into(),
                        icon: None,
                        notification: gtk::gio::Notification::new("title"),
                    };
                    native.show(request("old"), SoundRequest::Silent);
                    assert_eq!(received.recv().await.unwrap(), ('n', 0));
                    while native.queue.has_pending("old") {
                        gtk::glib::timeout_future(std::time::Duration::from_millis(5)).await;
                    }
                    let (same_done, same_ready) = async_channel::bounded(1);
                    let same_server = first.clone();
                    crate::portal::spawn(async move {
                        same_server
                            .release_name("org.freedesktop.Notifications")
                            .await
                            .unwrap();
                        same_server
                            .request_name("org.freedesktop.Notifications")
                            .await
                            .unwrap();
                        same_done.send(()).await.unwrap();
                    });
                    same_ready.recv().await.unwrap();
                    native.show(request("old"), SoundRequest::Silent);
                    assert_eq!(received.recv().await.unwrap(), ('n', 0));
                    while native.queue.has_pending("old") {
                        gtk::glib::timeout_future(std::time::Duration::from_millis(5)).await;
                    }
                    let (setup, ready) = async_channel::bounded(1);
                    let next_wire = wire.clone();
                    let next_address = address.clone();
                    crate::portal::spawn(async move {
                        first
                            .release_name("org.freedesktop.Notifications")
                            .await
                            .unwrap();
                        let server = zbus::connection::Builder::address(next_address.as_str())
                            .unwrap()
                            .name("org.freedesktop.Notifications")
                            .unwrap()
                            .serve_at(
                                "/org/freedesktop/Notifications",
                                RestartServer {
                                    wire: next_wire,
                                    actions: true,
                                },
                            )
                            .unwrap()
                            .build()
                            .await
                            .unwrap();
                        setup.send(server).await.unwrap();
                    });
                    let second = ready.recv().await.unwrap();
                    native.show(request("new"), SoundRequest::Silent);
                    assert_eq!(received.recv().await.unwrap(), ('n', 0));
                    native.withdraw("old");
                    gtk::glib::timeout_future(std::time::Duration::from_millis(150)).await;
                    assert!(received.try_recv().is_err());
                    assert_eq!(*fallback.borrow(), 0);
                    while native.queue.has_pending("new") {
                        gtk::glib::timeout_future(std::time::Duration::from_millis(5)).await;
                    }
                    let (setup, ready) = async_channel::bounded(1);
                    crate::portal::spawn(async move {
                        second
                            .release_name("org.freedesktop.Notifications")
                            .await
                            .unwrap();
                        let server = zbus::connection::Builder::address(address.as_str())
                            .unwrap()
                            .name("org.freedesktop.Notifications")
                            .unwrap()
                            .serve_at(
                                "/org/freedesktop/Notifications",
                                RestartServer {
                                    wire,
                                    actions: false,
                                },
                            )
                            .unwrap()
                            .build()
                            .await
                            .unwrap();
                        setup.send(server).await.unwrap();
                    });
                    let _third = ready.recv().await.unwrap();
                    native.show(request("unsupported"), SoundRequest::Silent);
                    while native.queue.has_pending("unsupported") {
                        gtk::glib::timeout_future(std::time::Duration::from_millis(5)).await;
                    }
                    assert_eq!(*fallback.borrow(), 1);
                    native.show(request("unsupported"), SoundRequest::Silent);
                    assert_eq!(*fallback.borrow(), 2);
                    assert!(received.try_recv().is_err());
                })
            })
            .unwrap();
    }

    #[test]
    fn native_adapter_sends_rgba_sound_and_silence_over_private_peer() {
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (setup_tx, setup_rx) = async_channel::bounded(1);
                    let (wire_tx, wire_rx) = async_channel::unbounded();
                    crate::portal::spawn(async move {
                        let (a, b) = tokio::net::UnixStream::pair().unwrap();
                        let server = zbus::connection::Builder::unix_stream(a)
                            .server(zbus::Guid::generate())
                            .unwrap()
                            .p2p()
                            .serve_at("/org/freedesktop/Notifications", FakeServer(wire_tx))
                            .unwrap()
                            .build();
                        let client = zbus::connection::Builder::unix_stream(b).p2p().build();
                        let (server, connection) = tokio::try_join!(server, client).unwrap();
                        let (session, _events) =
                            freedesktop::Session::with_owner(connection, ":peer".into(), true)
                                .await
                                .unwrap();
                        setup_tx.send((session, server)).await.unwrap();
                    });
                    let (session, _server) = setup_rx.recv().await.unwrap();
                    let fallback = Rc::new(RefCell::new(0));
                    let (events, _receiver) = async_channel::unbounded();
                    let native = Native {
                        gio: Rc::new(FakeGio(fallback.clone())),
                        queue: portal::Queue::default(),
                        session: Arc::new(tokio::sync::Mutex::new(Some(session))),
                        fallback: Rc::default(),
                        delivered: Arc::default(),
                        active: Arc::default(),
                        events,
                        connection: None,
                        desktop_entry: "test.app".into(),
                    };
                    let mut png = std::io::Cursor::new(Vec::new());
                    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                        2,
                        1,
                        image::Rgba([1, 2, 3, 255]),
                    ))
                    .write_to(&mut png, image::ImageFormat::Png)
                    .unwrap();
                    let request = |token: &str| Request {
                        token: token.into(),
                        title: "title".into(),
                        body: "<hello> &".into(),
                        icon: Some(png.get_ref().clone()),
                        notification: gtk::gio::Notification::new("title"),
                    };
                    native.show(request("one"), SoundRequest::Play);
                    let (body, actions, hints) = wire_rx.recv().await.unwrap();
                    assert_eq!(body, "&lt;hello&gt; &amp;");
                    assert_eq!(actions, ["default", "Open"]);
                    let structure =
                        <&zbus::zvariant::Structure<'_>>::try_from(&hints["image-data"]).unwrap();
                    let fields = structure.fields();
                    assert_eq!(i32::try_from(&fields[0]).unwrap(), 128);
                    assert_eq!(i32::try_from(&fields[1]).unwrap(), 64);
                    assert_eq!(i32::try_from(&fields[2]).unwrap(), 512);
                    let pixels = Vec::<u8>::try_from(fields[6].try_clone().unwrap()).unwrap();
                    assert_eq!(pixels.len(), 128 * 64 * 4);
                    let (pixels, remainder) = pixels.as_chunks::<4>();
                    assert!(remainder.is_empty());
                    assert!(pixels.iter().all(|pixel| *pixel == [1, 2, 3, 255]));
                    assert_eq!(
                        <&str>::try_from(&hints["desktop-entry"]).unwrap(),
                        "test.app"
                    );
                    let path = <&str>::try_from(&hints["sound-file"]).unwrap();
                    assert_eq!(
                        std::fs::read(path).unwrap(),
                        crate::notification_sound::SOUND
                    );
                    native.show(request("two"), SoundRequest::Silent);
                    let (_, _, hints) = wire_rx.recv().await.unwrap();
                    assert!(bool::try_from(&hints["suppress-sound"]).unwrap());
                    assert!(!hints.contains_key("sound-file") && !hints.contains_key("sound-name"));
                    assert_eq!(*fallback.borrow(), 0);
                    let mut refused = request("refused");
                    refused.title = "refuse".into();
                    native.show(refused, SoundRequest::Silent);
                    wire_rx.recv().await.unwrap();
                    while native.queue.has_pending("refused") {
                        gtk::glib::timeout_future(std::time::Duration::from_millis(5)).await;
                    }
                    assert_eq!(*fallback.borrow(), 1);
                    let mut ambiguous = request("ambiguous");
                    ambiguous.title = "no-reply".into();
                    native.show(ambiguous, SoundRequest::Silent);
                    wire_rx.recv().await.unwrap();
                    gtk::glib::timeout_future(std::time::Duration::from_millis(100)).await;
                    assert_eq!(*fallback.borrow(), 1);
                    assert!(native.queue.has_pending("ambiguous"));
                    native.withdraw("ambiguous");
                    assert!(!native.queue.has_pending("ambiguous"));
                })
            })
            .unwrap();
    }

    #[derive(Default)]
    struct Recorder(RefCell<Vec<String>>);
    impl Recorder {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut self.0.borrow_mut())
        }
    }
    impl Port for Recorder {
        fn show(&self, request: Request) {
            self.0.borrow_mut().push(format!("show {}", request.token));
        }
        fn withdraw(&self, token: &str) {
            self.0.borrow_mut().push(format!("withdraw {token}"));
        }
    }
    impl SoundPort for Recorder {
        fn show(&self, request: Request, sound: SoundRequest) {
            self.0
                .borrow_mut()
                .push(format!("show {} {sound:?}", request.token));
        }
        fn withdraw(&self, token: &str) {
            self.0.borrow_mut().push(format!("withdraw {token}"));
        }
    }

    fn plain_request(token: &str) -> Request {
        Request {
            token: token.into(),
            title: "title".into(),
            body: "body".into(),
            icon: None,
            notification: gtk::gio::Notification::new("title"),
        }
    }

    #[test]
    fn a_turned_off_sound_keeps_plain_notifications_and_switching_never_shows_twice() {
        let plain = Rc::new(Recorder::default());
        let with_sound = Rc::new(Recorder::default());
        let notifier = Notifier {
            plain: plain.clone(),
            with_sound: with_sound.clone(),
            on_sound: Rc::default(),
        };
        notifier.show(plain_request("chat"), None);
        assert_eq!(plain.take(), ["show chat"]);
        assert!(with_sound.take().is_empty());

        notifier.show(plain_request("chat"), Some(SoundRequest::Play));
        assert_eq!(plain.take(), ["withdraw chat"]);
        notifier.show(plain_request("chat"), Some(SoundRequest::Silent));
        assert!(plain.take().is_empty());
        assert_eq!(with_sound.take(), ["show chat Play", "show chat Silent"]);

        notifier.show(plain_request("chat"), None);
        assert_eq!(with_sound.take(), ["withdraw chat"]);
        assert_eq!(plain.take(), ["show chat"]);

        notifier.withdraw("chat");
        assert_eq!(plain.take(), ["withdraw chat"]);
        assert!(with_sound.take().is_empty());
        notifier.show(plain_request("chat"), Some(SoundRequest::Play));
        plain.take();
        notifier.withdraw("chat");
        assert_eq!(with_sound.take(), ["show chat Play", "withdraw chat"]);
        assert!(plain.take().is_empty());
    }

    #[test]
    fn a_token_is_pinned_only_once_the_portal_version_is_known() {
        let mut modes = HashMap::new();
        assert_eq!(route(&mut modes, "early", None), Route::Gio);
        assert!(modes.is_empty());
        assert_eq!(
            route(&mut modes, "early", Some(true)),
            Route::PortalAfterGio
        );
        assert_eq!(route(&mut modes, "early", Some(true)), Route::Portal);
        assert_eq!(route(&mut modes, "old-portal", Some(false)), Route::Gio);
        assert_eq!(route(&mut modes, "old-portal", Some(true)), Route::Gio);
        assert_eq!(route(&mut modes, "early", None), Route::Portal);
    }

    #[test]
    fn sandbox_selection_does_not_depend_on_host_services() {
        assert_eq!(backend_choice(true), BackendChoice::Portal);
        assert_eq!(backend_choice(false), BackendChoice::Native);
    }

    #[test]
    fn closed_drain_forgets_only_idle_token_and_preserves_pending_replacement() {
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let mut tokens = super::super::ActivationTokens::default();
                    tokens.insert(
                        "dismissed".into(),
                        (crate::account::AccountId(1), "one".into()),
                    );
                    tokens.insert(
                        "replacing".into(),
                        (crate::account::AccountId(2), "two".into()),
                    );
                    let tokens = Rc::new(RefCell::new(tokens));
                    let queue = portal::Queue::default();
                    let (_gate, receiver) = async_channel::bounded::<()>(1);
                    queue.enqueue(
                        "replacing",
                        false,
                        Box::new(move || {
                            Box::pin(async move {
                                let _ = receiver.recv().await;
                                Ok(())
                            })
                        }),
                        Box::new(|_, _| {}),
                    );
                    let active = std::sync::Mutex::new(HashSet::from([
                        "dismissed".into(),
                        "replacing".into(),
                    ]));
                    let delivered = std::sync::Mutex::new(HashSet::from([
                        "dismissed".into(),
                        "replacing".into(),
                    ]));
                    let (sender, incoming) = async_channel::unbounded();
                    sender
                        .send(freedesktop::Event::Closed("dismissed".into()))
                        .await
                        .unwrap();
                    sender
                        .send(freedesktop::Event::Closed("replacing".into()))
                        .await
                        .unwrap();
                    for _ in 0..2 {
                        handle_event(
                            incoming.recv().await.unwrap(),
                            &queue,
                            &active,
                            &delivered,
                            |_| panic!("closed must not activate"),
                            |token| {
                                tokens.borrow_mut().activate(token);
                            },
                        );
                    }
                    assert!(tokens.borrow().get("dismissed").is_none());
                    assert!(tokens.borrow().get("replacing").is_some());
                    assert!(!active.lock().unwrap().contains("dismissed"));
                    assert!(delivered.lock().unwrap().contains("replacing"));
                })
            })
            .unwrap();
    }
}
