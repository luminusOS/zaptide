//! A client of `org.freedesktop.Notifications` that keeps the server's
//! notification IDs behind ZapTide's tokens.
//!
//! The IDs belong to one server process. When the bus name changes owner the
//! session stops answering as current, and its IDs are never reused.

use std::collections::{HashMap, HashSet};

use ashpd::zbus::{self, zvariant::OwnedValue};
use futures_util::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

const DESTINATION: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

#[derive(Default)]
struct ServerIds {
    owner: Option<String>,
    by_token: HashMap<String, u32>,
    by_id: HashMap<u32, String>,
    replaced: HashSet<u32>,
}

impl ServerIds {
    fn set_owner(&mut self, owner: Option<&str>) {
        if self.owner.as_deref() != owner {
            self.owner = owner.map(str::to_owned);
            self.by_token.clear();
            self.by_id.clear();
            self.replaced.clear();
        }
    }

    fn id(&self, owner: &str, token: &str) -> Option<u32> {
        (self.owner.as_deref() == Some(owner))
            .then(|| self.by_token.get(token).copied())
            .flatten()
    }

    fn bind(&mut self, owner: &str, token: &str, id: u32) -> bool {
        if self.owner.as_deref() != Some(owner) || id == 0 {
            return false;
        }
        if let Some(previous) = self.by_token.insert(token.to_owned(), id) {
            if previous == id {
                self.replaced.insert(id);
            } else {
                self.replaced.remove(&previous);
            }
            self.by_id.remove(&previous);
        }
        if let Some(previous) = self.by_id.insert(id, token.to_owned())
            && previous != token
        {
            self.by_token.remove(&previous);
        }
        true
    }

    fn token(&self, owner: &str, id: u32) -> Option<&str> {
        if self.owner.as_deref() != Some(owner) {
            return None;
        }
        self.by_id.get(&id).map(String::as_str)
    }

    fn remove_id(&mut self, owner: &str, id: u32) -> Option<String> {
        // A close signal cannot distinguish the old instance from its replacement.
        if self.replaced.contains(&id) {
            return None;
        }
        let token = self.token(owner, id)?.to_owned();
        self.by_id.remove(&id);
        self.by_token.remove(&token);
        Some(token)
    }

    fn remove_token(&mut self, owner: &str, token: &str) -> Option<u32> {
        let id = self.id(owner, token)?;
        self.replaced.remove(&id);
        self.by_token.remove(token);
        self.by_id.remove(&id);
        Some(id)
    }
}

pub(super) struct Notification<'a> {
    pub app_name: &'a str,
    pub replaces_id: u32,
    pub app_icon: &'a str,
    pub summary: &'a str,
    pub body: &'a str,
    pub actions: Vec<String>,
    pub hints: HashMap<String, OwnedValue>,
    pub expire_timeout: i32,
}

pub(super) struct Client {
    connection: zbus::Connection,
    destination: String,
}

impl Client {
    #[cfg(test)]
    pub fn new(connection: zbus::Connection) -> Self {
        Self {
            connection,
            destination: DESTINATION.to_owned(),
        }
    }

    async fn proxy(&self) -> zbus::Result<zbus::Proxy<'_>> {
        zbus::Proxy::new(
            &self.connection,
            self.destination.as_str(),
            PATH,
            DESTINATION,
        )
        .await
    }

    pub async fn capabilities(&self) -> zbus::Result<Vec<String>> {
        self.proxy().await?.call("GetCapabilities", &()).await
    }

    pub async fn notify(&self, notification: Notification<'_>) -> zbus::Result<u32> {
        self.proxy()
            .await?
            .call(
                "Notify",
                &(
                    notification.app_name,
                    notification.replaces_id,
                    notification.app_icon,
                    notification.summary,
                    notification.body,
                    notification.actions,
                    notification.hints,
                    notification.expire_timeout,
                ),
            )
            .await
    }

    pub async fn close(&self, id: u32) -> zbus::Result<()> {
        self.proxy().await?.call("CloseNotification", &(id,)).await
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Event {
    Activated(String),
    Closed(String),
}

pub(super) struct Session {
    client: Client,
    owner: String,
    ids: Arc<Mutex<ServerIds>>,
    capabilities: Vec<String>,
    peer: bool,
    listener: tokio::task::JoinHandle<()>,
    owner_listener: Option<tokio::task::JoinHandle<()>>,
    owner_changed: Arc<AtomicBool>,
}

impl Session {
    pub async fn new(
        connection: zbus::Connection,
    ) -> zbus::Result<(Self, async_channel::Receiver<Event>)> {
        let owner = zbus::fdo::DBusProxy::new(&connection)
            .await?
            .get_name_owner(DESTINATION.try_into()?)
            .await?
            .to_string();
        Self::with_owner(connection, owner, false).await
    }

    pub(super) async fn with_owner(
        connection: zbus::Connection,
        owner: String,
        peer: bool,
    ) -> zbus::Result<(Self, async_channel::Receiver<Event>)> {
        let client = Client {
            connection,
            destination: if peer {
                DESTINATION.to_owned()
            } else {
                owner.clone()
            },
        };
        let owner_changed = Arc::new(AtomicBool::new(false));
        let owner_listener = if peer {
            None
        } else {
            let bus = zbus::fdo::DBusProxy::new(&client.connection).await?;
            let mut changes = bus
                .receive_name_owner_changed_with_args(&[(0, DESTINATION)])
                .await?;
            let changed = owner_changed.clone();
            Some(tokio::spawn(async move {
                if changes.next().await.is_some() {
                    changed.store(true, Ordering::Release);
                }
            }))
        };
        let proxy = client.proxy().await?;
        let mut signals = proxy.receive_all_signals().await?;
        let capabilities = client.capabilities().await?;
        let mut initial = ServerIds::default();
        initial.set_owner(Some(&owner));
        let ids = Arc::new(Mutex::new(initial));
        let (sender, receiver) = async_channel::unbounded();
        let signal_ids = ids.clone();
        let signal_owner = owner.clone();
        let connection = client.connection.clone();
        let listener = tokio::spawn(async move {
            while let Some(message) = signals.next().await {
                let mut ids = signal_ids.lock().await;
                if !peer && !owner_is_current(&connection, &signal_owner).await {
                    ids.set_owner(None);
                    continue;
                }
                let header = message.header();
                let event = match header.member().map(|member| member.as_str()) {
                    Some("ActionInvoked") => message
                        .body()
                        .deserialize::<(u32, String)>()
                        .ok()
                        .and_then(|(id, action)| (action == "default").then_some(id))
                        .and_then(|id| {
                            ids.token(&signal_owner, id)
                                .map(|token| Event::Activated(token.to_owned()))
                        }),
                    Some("NotificationClosed") => message
                        .body()
                        .deserialize::<(u32, u32)>()
                        .ok()
                        .and_then(|(id, _)| ids.remove_id(&signal_owner, id))
                        .map(Event::Closed),
                    _ => None,
                };
                if let Some(event) = event {
                    let _ = sender.try_send(event);
                }
            }
        });
        Ok((
            Self {
                client,
                owner,
                ids,
                capabilities,
                peer,
                listener,
                owner_listener,
                owner_changed,
            },
            receiver,
        ))
    }

    /// Whether the server advertised `capability` when the session opened.
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|cap| cap == capability)
    }

    pub async fn is_current(&self) -> bool {
        let mut ids = self.ids.lock().await;
        self.current(&mut ids).await
    }

    async fn current(&self, ids: &mut ServerIds) -> bool {
        if !self.owner_changed.load(Ordering::Acquire)
            && (self.peer || owner_is_current(&self.client.connection, &self.owner).await)
        {
            ids.owner.as_deref() == Some(self.owner.as_str())
        } else {
            ids.set_owner(None);
            false
        }
    }

    pub async fn show(&self, token: &str, mut notification: Notification<'_>) -> zbus::Result<u32> {
        let mut ids = self.ids.lock().await;
        if !self.current(&mut ids).await {
            return Err(zbus::Error::Failure(
                "notification server owner changed".into(),
            ));
        }
        notification.replaces_id = ids.id(&self.owner, token).unwrap_or(0);
        let id = self.client.notify(notification).await?;
        ids.bind(&self.owner, token, id);
        Ok(id)
    }

    pub async fn withdraw(&self, token: &str) -> zbus::Result<()> {
        let mut ids = self.ids.lock().await;
        if !self.current(&mut ids).await {
            return Ok(());
        }
        if let Some(id) = ids.remove_token(&self.owner, token) {
            self.client.close(id).await?;
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.listener.abort();
        if let Some(listener) = &self.owner_listener {
            listener.abort();
        }
    }
}

async fn owner_is_current(connection: &zbus::Connection, owner: &str) -> bool {
    match zbus::fdo::DBusProxy::new(connection).await {
        Ok(proxy) => proxy
            .get_name_owner(DESTINATION.try_into().expect("valid bus name"))
            .await
            .is_ok_and(|current| current.as_str() == owner),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct State {
        received: Vec<(u32, HashMap<String, OwnedValue>)>,
        closed: Vec<u32>,
    }

    struct Fake(Arc<Mutex<State>>);

    struct PrivateBus(std::process::Child);

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn server_ids_are_scoped_to_owner_and_keep_tokens_separate() {
        let mut ids = ServerIds::default();
        ids.set_owner(Some(":1.10"));
        assert!(ids.bind(":1.10", "account-one", 42));
        assert!(ids.bind(":1.10", "account-two", 43));
        assert_eq!(ids.id(":1.10", "account-one"), Some(42));
        assert_eq!(ids.token(":1.10", 43), Some("account-two"));
        assert_eq!(ids.remove_id(":1.10", 42).as_deref(), Some("account-one"));
        assert_eq!(ids.id(":1.10", "account-two"), Some(43));
        ids.set_owner(Some(":1.11"));
        assert_eq!(ids.remove_token(":1.11", "account-two"), None);
        assert!(!ids.bind(":1.10", "stale", 43));
        assert!(ids.bind(":1.11", "new", 43));
        assert_eq!(ids.remove_id(":1.10", 43), None);
        assert_eq!(ids.token(":1.11", 43), Some("new"));
    }

    #[zbus::interface(name = "org.freedesktop.Notifications", crate = "ashpd::zbus")]
    impl Fake {
        fn get_capabilities(&self) -> Vec<String> {
            vec!["sound".into(), "actions".into()]
        }

        #[allow(
            clippy::too_many_arguments,
            reason = "D-Bus Notify requires eight separate wire arguments"
        )]
        fn notify(
            &self,
            app_name: String,
            replaces_id: u32,
            app_icon: String,
            summary: String,
            body: String,
            actions: Vec<String>,
            hints: HashMap<String, OwnedValue>,
            expire_timeout: i32,
        ) -> u32 {
            assert_eq!((app_name.as_str(), app_icon.as_str()), ("ZapTide", ""));
            assert_eq!((summary.as_str(), body.as_str()), ("Title", "Body"));
            assert_eq!(actions, ["default", "Open"]);
            assert_eq!(expire_timeout, -1);
            let mut state = self.0.lock().unwrap();
            let id = if replaces_id == 0 {
                42 + state.received.iter().filter(|(id, _)| *id == 0).count() as u32
            } else {
                replaces_id
            };
            state.received.push((replaces_id, hints));
            id
        }

        fn close_notification(&self, id: u32) {
            self.0.lock().unwrap().closed.push(id);
        }
    }

    #[tokio::test]
    async fn private_bus_replaces_owner_without_reusing_server_ids() {
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
        let _bus = PrivateBus(child);
        let address = address.trim();
        let state = Arc::new(Mutex::new(State::default()));
        let server = zbus::connection::Builder::address(address)
            .unwrap()
            .name(DESTINATION)
            .unwrap()
            .serve_at(PATH, Fake(state.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let connection = zbus::connection::Builder::address(address)
            .unwrap()
            .build()
            .await
            .unwrap();
        let (session, _events) = Session::new(connection.clone()).await.unwrap();
        let notification = || Notification {
            app_name: "ZapTide",
            replaces_id: 0,
            app_icon: "",
            summary: "Title",
            body: "Body",
            actions: vec!["default".into(), "Open".into()],
            hints: HashMap::new(),
            expire_timeout: -1,
        };
        assert_eq!(session.show("old", notification()).await.unwrap(), 42);
        session.owner_listener.as_ref().unwrap().abort();
        server.release_name(DESTINATION).await.unwrap();
        server.request_name(DESTINATION).await.unwrap();
        assert!(session.is_current().await);
        assert_eq!(session.show("old", notification()).await.unwrap(), 42);
        assert_eq!(state.lock().unwrap().received.last().unwrap().0, 42);
        let (session, _events) = Session::new(connection.clone()).await.unwrap();
        server.release_name(DESTINATION).await.unwrap();
        server.request_name(DESTINATION).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !session.owner_changed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!session.is_current().await);
        server.release_name(DESTINATION).await.unwrap();
        let new_state = Arc::new(Mutex::new(State::default()));
        let _new_server = zbus::connection::Builder::address(address)
            .unwrap()
            .name(DESTINATION)
            .unwrap()
            .serve_at(PATH, Fake(new_state.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        assert!(!session.is_current().await);
        let (new_session, _events) = Session::new(connection).await.unwrap();
        assert_eq!(new_session.show("new", notification()).await.unwrap(), 42);
        assert_eq!(new_state.lock().unwrap().received[0].0, 0);
    }

    #[tokio::test]
    async fn session_routes_peer_signals_and_withdraws_server_id() {
        let state = Arc::new(Mutex::new(State::default()));
        let (server_end, client_end) = tokio::net::UnixStream::pair().unwrap();
        let server = zbus::connection::Builder::unix_stream(server_end)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(PATH, Fake(state.clone()))
            .unwrap()
            .build();
        let client = zbus::connection::Builder::unix_stream(client_end)
            .p2p()
            .build();
        let (server, connection) = tokio::try_join!(server, client).unwrap();
        let (session, events) = Session::with_owner(connection, ":peer".into(), true)
            .await
            .unwrap();
        let notification = || Notification {
            app_name: "ZapTide",
            replaces_id: 0,
            app_icon: "",
            summary: "Title",
            body: "Body",
            actions: vec!["default".into(), "Open".into()],
            hints: HashMap::new(),
            expire_timeout: -1,
        };
        assert_eq!(
            session.show("account-one", notification()).await.unwrap(),
            42
        );
        assert_eq!(
            session.show("account-two", notification()).await.unwrap(),
            43
        );
        assert_eq!(
            session.show("account-one", notification()).await.unwrap(),
            42
        );
        server
            .emit_signal(
                None::<&str>,
                PATH,
                DESTINATION,
                "NotificationClosed",
                &(42_u32, 2_u32),
            )
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), events.recv())
                .await
                .is_err()
        );
        server
            .emit_signal(
                None::<&str>,
                PATH,
                DESTINATION,
                "ActionInvoked",
                &(43_u32, "default"),
            )
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap(),
            Event::Activated("account-two".into())
        );
        server
            .emit_signal(
                None::<&str>,
                PATH,
                DESTINATION,
                "NotificationClosed",
                &(43_u32, 2_u32),
            )
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap(),
            Event::Closed("account-two".into())
        );
        session.withdraw("account-two").await.unwrap();
        session.withdraw("account-one").await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(
            state.received.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [0, 0, 42]
        );
        assert_eq!(state.closed, [42]);
    }

    #[tokio::test]
    async fn private_peer_notify_and_close_preserve_wire_payload() {
        let state = Arc::new(Mutex::new(State::default()));
        let (server_end, client_end) = tokio::net::UnixStream::pair().unwrap();
        let server = zbus::connection::Builder::unix_stream(server_end)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(PATH, Fake(state.clone()))
            .unwrap()
            .build();
        let client = zbus::connection::Builder::unix_stream(client_end)
            .p2p()
            .build();
        let (_server, connection) = tokio::try_join!(server, client).unwrap();
        let client = Client::new(connection);
        assert_eq!(client.capabilities().await.unwrap(), ["sound", "actions"]);
        for replaces_id in [0, 42] {
            let mut hints = HashMap::new();
            hints.insert(
                "sound-file".into(),
                zbus::zvariant::Str::from("/synthetic/sound.wav").into(),
            );
            hints.insert("suppress-sound".into(), true.into());
            let id = client
                .notify(Notification {
                    app_name: "ZapTide",
                    replaces_id,
                    app_icon: "",
                    summary: "Title",
                    body: "Body",
                    actions: vec!["default".into(), "Open".into()],
                    hints,
                    expire_timeout: -1,
                })
                .await
                .unwrap();
            assert_eq!(id, 42);
        }
        client.close(42).await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.closed, [42]);
        assert_eq!(
            state.received.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [0, 42]
        );
        for (_, hints) in &state.received {
            assert_eq!(
                <&str>::try_from(&hints["sound-file"]).unwrap(),
                "/synthetic/sound.wav"
            );
            assert!(bool::try_from(&hints["suppress-sound"]).unwrap());
        }
    }
}
