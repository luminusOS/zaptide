use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use super::{OPEN_CHAT_ACTION, freedesktop, portal};
use crate::notification_sound::SoundRequest;
use gtk::glib::variant::ToVariant;
use gtk::prelude::*;
use relm4::gtk;

pub(super) struct Request {
    pub token: String,
    pub title: String,
    pub body: String,
    pub icon: Option<Vec<u8>>,
    pub sound: SoundRequest,
    pub notification: gtk::gio::Notification,
}

pub(super) trait Port {
    fn show(&self, request: Request);
    fn withdraw(&self, token: &str);
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

struct Portal {
    gio: Gio,
    support: portal::Support,
    queue: portal::Queue,
    modes: RefCell<HashMap<String, bool>>,
}
impl Port for Portal {
    fn show(&self, request: Request) {
        self.support.probe();
        let supported = *self
            .modes
            .borrow_mut()
            .entry(request.token.clone())
            .or_insert_with(|| self.support.supports_sound());
        let fallback = self.gio.clone();
        let portal_request = portal::Request {
            id: request.token.clone(),
            title: request.title.clone(),
            body: request.body.clone(),
            icon: request.icon.clone(),
            default_action: format!("app.{OPEN_CHAT_ACTION}"),
            default_action_target: request.token.clone(),
            sound: request.sound,
        };
        self.queue
            .add(portal_request, supported, move || fallback.show(request));
    }
    fn withdraw(&self, token: &str) {
        let enabled = self.modes.borrow_mut().remove(token).unwrap_or(true);
        let gio = self.gio.clone();
        let id = token.to_owned();
        self.queue.remove(token, enabled, move || gio.withdraw(&id));
    }
}

pub(super) fn image_data(icon: &[u8]) -> Result<ashpd::zbus::zvariant::OwnedValue, String> {
    let crate::native_media::ThumbnailResult::Image(image) =
        crate::native_media::decode_thumbnail(icon)
    else {
        return Err("notification icon could not be decoded".into());
    };
    let width = i32::try_from(image.width).map_err(|e| e.to_string())?;
    let height = i32::try_from(image.height).map_err(|e| e.to_string())?;
    ashpd::zbus::zvariant::Value::new((width, height, width * 4, true, 8_i32, 4_i32, image.rgba))
        .try_into()
        .map_err(|e: ashpd::zbus::zvariant::Error| e.to_string())
}

type SharedSession = Arc<tokio::sync::Mutex<Option<freedesktop::Session>>>;
struct Native {
    gio: Rc<dyn Port>,
    queue: portal::Queue,
    session: SharedSession,
    fallback: Rc<RefCell<HashSet<String>>>,
    delivered: Arc<std::sync::Mutex<HashSet<String>>>,
    active: Arc<std::sync::Mutex<HashSet<String>>>,
    events: async_channel::Sender<freedesktop::Event>,
    connection: Option<ashpd::zbus::Connection>,
}
impl Port for Native {
    fn show(&self, request: Request) {
        if self.fallback.borrow().contains(&request.token) {
            self.gio.show(request);
            return;
        }
        let token = request.token.clone();
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token.clone());
        let id = token.clone();
        let title = request.title.clone();
        let body = request.body.clone();
        let icon = request.icon.clone();
        let sound = request.sound;
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
                Box::pin(async move {
                    portal::on_runtime(async move {
                        let previously_delivered = delivered
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .contains(&id);
                        let result: Result<(), String> = async {
                            let mut session = session.lock().await;
                            if let Some(existing) = session.as_ref()
                                && !existing.is_current().await
                            {
                                *session = None;
                            }
                            if session.is_none() {
                                let connection = match connection {
                                    Some(connection) => connection,
                                    None => ashpd::zbus::Connection::session()
                                        .await
                                        .map_err(|e| e.to_string())?,
                                };
                                let (new_session, receiver) = freedesktop::Session::new(connection)
                                    .await
                                    .map_err(|e| e.to_string())?;
                                crate::portal::spawn(async move {
                                    while let Ok(event) = receiver.recv().await {
                                        if events.send(event).await.is_err() {
                                            break;
                                        }
                                    }
                                });
                                *session = Some(new_session);
                            }
                            let Some(session) = session.as_ref() else {
                                return Err("notification session unavailable".into());
                            };
                            if !session.capabilities().iter().any(|cap| cap == "actions") {
                                return Err("notification actions unavailable".into());
                            }
                            let mut hints = HashMap::new();
                            if let Some(icon) = icon {
                                hints.insert("image-data".into(), image_data(&icon)?);
                            }
                            match sound {
                                SoundRequest::Silent => {
                                    hints.insert("suppress-sound".into(), true.into());
                                }
                                SoundRequest::Play
                                    if session.capabilities().iter().any(|cap| cap == "sound") =>
                                {
                                    let path = crate::notification_sound::native_sound_file()
                                        .map_err(|e| e.to_string())?;
                                    let path = path.to_str().ok_or("sound path is not UTF-8")?;
                                    hints.insert(
                                        "sound-file".into(),
                                        ashpd::zbus::zvariant::Str::from(path).into(),
                                    );
                                }
                                SoundRequest::Play => {}
                            }
                            let body_format = if session
                                .capabilities()
                                .iter()
                                .any(|cap| cap == "body-markup")
                            {
                                super::BodyFormat::Markup
                            } else {
                                super::BodyFormat::Plain
                            };
                            let body = super::notification_body(&body, body_format);
                            session
                                .show(
                                    &id,
                                    freedesktop::Notification {
                                        app_name: "ZapTide",
                                        replaces_id: 0,
                                        app_icon: "",
                                        summary: &title,
                                        body: &body,
                                        actions: vec!["default".into(), "Open".into()],
                                        hints,
                                        expire_timeout: -1,
                                    },
                                )
                                .await
                                .map_err(|error| match error {
                                    ashpd::zbus::Error::MethodError(ref name, _, _)
                                        if !matches!(
                                            name.as_str(),
                                            "org.freedesktop.DBus.Error.NoReply"
                                                | "org.freedesktop.DBus.Error.Timeout"
                                                | "org.freedesktop.DBus.Error.Disconnected"
                                        ) =>
                                    {
                                        error.to_string()
                                    }
                                    _ => portal::AMBIGUOUS.to_owned(),
                                })?;
                            Ok(())
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
                    })
                    .await
                })
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

pub(super) fn create(
    application: &gtk::Application,
    sandboxed: bool,
    closed: impl Fn(&str) + 'static,
) -> Rc<dyn Port> {
    let gio = Gio(application.clone());
    if backend_choice(sandboxed) == BackendChoice::Portal {
        return Rc::new(Portal {
            gio,
            support: portal::Support::default(),
            queue: portal::Queue::default(),
            modes: RefCell::new(HashMap::new()),
        });
    }
    let (events, receiver) = async_channel::unbounded();
    let queue = portal::Queue::default();
    let event_queue = queue.clone();
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
                    };
                    let request = || Request {
                        token: "one".into(),
                        title: "title".into(),
                        body: "body".into(),
                        icon: None,
                        sound: SoundRequest::Silent,
                        notification: gtk::gio::Notification::new("title"),
                    };
                    native.show(request());
                    assert_eq!(received.recv().await.unwrap(), ('n', 0));
                    native.show(request());
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
                    };
                    let request = |token: &str| Request {
                        token: token.into(),
                        title: "title".into(),
                        body: "body".into(),
                        icon: None,
                        sound: SoundRequest::Silent,
                        notification: gtk::gio::Notification::new("title"),
                    };
                    native.show(request("old"));
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
                    native.show(request("old"));
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
                    native.show(request("new"));
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
                    native.show(request("unsupported"));
                    while native.queue.has_pending("unsupported") {
                        gtk::glib::timeout_future(std::time::Duration::from_millis(5)).await;
                    }
                    assert_eq!(*fallback.borrow(), 1);
                    native.show(request("unsupported"));
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
                    };
                    let mut png = std::io::Cursor::new(Vec::new());
                    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                        2,
                        1,
                        image::Rgba([1, 2, 3, 255]),
                    ))
                    .write_to(&mut png, image::ImageFormat::Png)
                    .unwrap();
                    let request = |token: &str, sound| Request {
                        token: token.into(),
                        title: "title".into(),
                        body: "<hello> &".into(),
                        icon: Some(png.get_ref().clone()),
                        sound,
                        notification: gtk::gio::Notification::new("title"),
                    };
                    native.show(request("one", SoundRequest::Play));
                    let (body, actions, hints) = wire_rx.recv().await.unwrap();
                    assert_eq!(body, "&lt;hello&gt; &amp;");
                    assert_eq!(actions, ["default", "Open"]);
                    let structure =
                        <&zbus::zvariant::Structure<'_>>::try_from(&hints["image-data"]).unwrap();
                    let fields = structure.fields();
                    assert_eq!(i32::try_from(&fields[0]).unwrap(), 512);
                    assert_eq!(i32::try_from(&fields[1]).unwrap(), 256);
                    assert_eq!(i32::try_from(&fields[2]).unwrap(), 2048);
                    let pixels = Vec::<u8>::try_from(fields[6].try_clone().unwrap()).unwrap();
                    assert_eq!(pixels.len(), 512 * 256 * 4);
                    let (pixels, remainder) = pixels.as_chunks::<4>();
                    assert!(remainder.is_empty());
                    assert!(pixels.iter().all(|pixel| *pixel == [1, 2, 3, 255]));
                    let path = <&str>::try_from(&hints["sound-file"]).unwrap();
                    assert_eq!(
                        std::fs::read(path).unwrap(),
                        crate::notification_sound::SOUND
                    );
                    native.show(request("two", SoundRequest::Silent));
                    let (_, _, hints) = wire_rx.recv().await.unwrap();
                    assert!(bool::try_from(&hints["suppress-sound"]).unwrap());
                    assert!(!hints.contains_key("sound-file") && !hints.contains_key("sound-name"));
                    assert_eq!(*fallback.borrow(), 0);
                    let mut refused = request("refused", SoundRequest::Silent);
                    refused.title = "refuse".into();
                    native.show(refused);
                    wire_rx.recv().await.unwrap();
                    while native.queue.has_pending("refused") {
                        gtk::glib::timeout_future(std::time::Duration::from_millis(5)).await;
                    }
                    assert_eq!(*fallback.borrow(), 1);
                    let mut ambiguous = request("ambiguous", SoundRequest::Silent);
                    ambiguous.title = "no-reply".into();
                    native.show(ambiguous);
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
