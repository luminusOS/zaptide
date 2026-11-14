use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::net::TcpStream;
use whatsapp_rust::transport::{Connector, Transport, TransportEvent, TransportFactory};
use whatsapp_rust::wacore::net::{WHATSAPP_WEB_ORIGIN, WHATSAPP_WEB_WS_URL};

const DIAL_DELAY: Duration = Duration::from_millis(250);
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(4);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(18);
const MAX_IN_FLIGHT: usize = 4;

pub(super) struct WebSocketTransportFactory {
    uri: http::Uri,
    connector: Connector,
    #[cfg(test)]
    addresses: Option<Vec<SocketAddr>>,
}

impl WebSocketTransportFactory {
    pub(super) fn new() -> Self {
        Self {
            uri: WHATSAPP_WEB_WS_URL.parse().expect("valid WhatsApp URL"),
            connector: whatsapp_rust::transport::default_tls_connector(),
            #[cfg(test)]
            addresses: None,
        }
    }

    #[cfg(test)]
    fn for_test(uri: String, addresses: Vec<SocketAddr>) -> Self {
        Self {
            uri: uri.parse().unwrap(),
            connector: whatsapp_rust::transport::default_tls_connector(),
            addresses: Some(addresses),
        }
    }
}

fn interleave_addresses(addresses: Vec<SocketAddr>) -> VecDeque<SocketAddr> {
    let Some(first) = addresses.first() else {
        return VecDeque::new();
    };
    let first_is_v6 = first.is_ipv6();
    let (mut preferred, mut alternate): (VecDeque<_>, VecDeque<_>) = addresses
        .into_iter()
        .partition(|address| address.is_ipv6() == first_is_v6);
    let mut ordered = VecDeque::new();
    while !preferred.is_empty() || !alternate.is_empty() {
        if let Some(address) = preferred.pop_front() {
            ordered.push_back(address);
        }
        if let Some(address) = alternate.pop_front() {
            ordered.push_back(address);
        }
    }
    ordered
}

async fn dial_addresses<F, Fut, T>(addresses: Vec<SocketAddr>, dial: F) -> io::Result<T>
where
    F: Fn(SocketAddr) -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    let mut addresses = interleave_addresses(addresses);
    let mut pending = FuturesUnordered::new();
    let mut last_error = io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses found");
    let mut next = tokio::time::Instant::now();

    loop {
        if pending.is_empty() && addresses.is_empty() {
            return Err(last_error);
        }
        tokio::select! {
            _ = tokio::time::sleep_until(next), if !addresses.is_empty() && pending.len() < MAX_IN_FLIGHT => {
                let address = addresses.pop_front().expect("nonempty address queue");
                let future = dial(address);
                pending.push(async move {
                    tokio::time::timeout(ATTEMPT_TIMEOUT, future).await
                        .unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "connection attempt timed out")))
                });
                next = tokio::time::Instant::now() + DIAL_DELAY;
            }
            Some(result) = pending.next(), if !pending.is_empty() => match result {
                Ok(connection) => return Ok(connection),
                Err(error) => {
                    last_error = error;
                    next = tokio::time::Instant::now();
                }
            }
        }
    }
}

#[async_trait]
impl TransportFactory for WebSocketTransportFactory {
    async fn create_transport(
        &self,
    ) -> anyhow::Result<(Arc<dyn Transport>, async_channel::Receiver<TransportEvent>)> {
        let host = self
            .uri
            .host()
            .ok_or_else(|| anyhow::anyhow!("missing WebSocket host"))?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let scheme = self
            .uri
            .scheme_str()
            .ok_or_else(|| anyhow::anyhow!("missing WebSocket scheme"))?;
        if scheme != "wss" && !(cfg!(test) && scheme == "ws") {
            anyhow::bail!("unsupported WebSocket scheme");
        }
        let port = self
            .uri
            .port_u16()
            .unwrap_or(if scheme == "wss" { 443 } else { 80 });
        let connector = if scheme == "wss" {
            &self.connector
        } else {
            &Connector::Plain
        };
        let builder = tokio_websockets::ClientBuilder::from_uri(self.uri.clone()).add_header(
            http::header::ORIGIN,
            http::HeaderValue::from_static(WHATSAPP_WEB_ORIGIN),
        )?;
        let connect = async {
            #[cfg(test)]
            let addresses = match &self.addresses {
                Some(addresses) => addresses.clone(),
                None => tokio::net::lookup_host((host, port)).await?.collect(),
            };
            #[cfg(not(test))]
            let addresses = tokio::net::lookup_host((host, port)).await?.collect();
            dial_addresses(addresses, |address| {
                let builder = &builder;
                async move {
                    let stream = TcpStream::connect(address).await?;
                    let stream = connector
                        .wrap(host, stream)
                        .await
                        .map_err(io::Error::other)?;
                    let (websocket, _) =
                        builder.connect_on(stream).await.map_err(io::Error::other)?;
                    Ok(websocket)
                }
            })
            .await
        };
        let websocket = tokio::time::timeout(CONNECT_TIMEOUT, connect).await??;
        Ok(whatsapp_rust::transport::from_websocket(websocket))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::net::TcpListener;

    fn address(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }

    #[test]
    fn interleaves_families_without_reordering_each_family() {
        let input = vec![
            address("[::1]:1"),
            address("[::1]:2"),
            address("127.0.0.1:3"),
            address("127.0.0.1:4"),
        ];
        assert_eq!(
            interleave_addresses(input).into_iter().collect::<Vec<_>>(),
            vec![
                address("[::1]:1"),
                address("127.0.0.1:3"),
                address("[::1]:2"),
                address("127.0.0.1:4")
            ]
        );
        assert_eq!(
            interleave_addresses(vec![address("[::1]:1"), address("[::1]:2")]).len(),
            2
        );
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn stalled_ipv6_falls_back_and_cancels_the_loser() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let addresses = vec![address("[::1]:1"), address("127.0.0.1:2")];
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            dial_addresses(addresses, |addr| {
                let cancelled = cancelled.clone();
                async move {
                    if addr.is_ipv6() {
                        let _flag = DropFlag(cancelled);
                        std::future::pending::<io::Result<SocketAddr>>().await
                    } else {
                        Ok(addr)
                    }
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.is_ipv4());
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancelling_the_dial_drops_pending_attempts() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let dial = dial_addresses(vec![address("[::1]:1")], move |_| {
            let flag = flag.clone();
            async move {
                let _flag = DropFlag(flag);
                std::future::pending::<io::Result<()>>().await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(30), dial)
                .await
                .is_err()
        );
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn fast_failure_advances_without_waiting_for_stagger() {
        let addresses = vec![address("[::1]:1"), address("127.0.0.1:2")];
        let start = tokio::time::Instant::now();
        let result = dial_addresses(addresses, |addr| async move {
            if addr.is_ipv6() {
                Err(io::ErrorKind::ConnectionRefused.into())
            } else {
                Ok(addr)
            }
        })
        .await
        .unwrap();
        assert!(result.is_ipv4());
        assert!(start.elapsed() < DIAL_DELAY);
    }

    #[tokio::test]
    async fn all_failed_and_empty_address_sets_return_errors() {
        let empty = dial_addresses(Vec::new(), |_| async { Ok::<_, io::Error>(()) })
            .await
            .unwrap_err();
        assert_eq!(empty.kind(), io::ErrorKind::AddrNotAvailable);
        let result = dial_addresses(
            vec![address("[::1]:1"), address("127.0.0.1:2")],
            |_| async { Err::<(), _>(io::Error::from(io::ErrorKind::ConnectionRefused)) },
        )
        .await
        .unwrap_err();
        assert_eq!(result.kind(), io::ErrorKind::ConnectionRefused);
    }

    #[tokio::test]
    async fn ipv6_only_uses_each_address() {
        let result = dial_addresses(
            vec![address("[::1]:1"), address("[::1]:2")],
            |addr| async move {
                if addr.port() == 1 {
                    Err(io::ErrorKind::ConnectionRefused.into())
                } else {
                    Ok(addr)
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(result, address("[::1]:2"));
    }

    #[tokio::test]
    async fn factory_falls_back_to_ipv4_and_upgrades_the_real_transport() {
        use futures_util::SinkExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (request, mut socket) = tokio_websockets::ServerBuilder::new()
                .accept(stream)
                .await
                .unwrap();
            assert_eq!(request.uri().path(), "/ws/chat");
            assert_eq!(
                request.headers()[http::header::ORIGIN],
                "https://web.whatsapp.com"
            );
            socket
                .send(tokio_websockets::Message::binary(b"synthetic".to_vec()))
                .await
                .unwrap();
        });
        let factory = WebSocketTransportFactory::for_test(
            format!("ws://localhost:{}/ws/chat", address.port()),
            vec![
                format!("[::1]:{}", address.port()).parse().unwrap(),
                address,
            ],
        );
        let (transport, events) = factory.create_transport().await.unwrap();
        assert!(matches!(
            events.recv().await.unwrap(),
            TransportEvent::Connected
        ));
        assert!(
            matches!(events.recv().await.unwrap(), TransportEvent::DataReceived(data) if &data[..] == b"synthetic")
        );
        server.await.unwrap();
        transport.disconnect().await;
    }

    async fn factory_falls_back_after_stalled_handshake(stall_tls: bool) {
        use tokio::io::AsyncReadExt;

        drop(WebSocketTransportFactory::new());
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let cert_der = cert.der().clone();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key.into())
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
        let v6 = TcpListener::bind("[::1]:0").await.unwrap();
        let v4 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let v6_addr = v6.local_addr().unwrap();
        let v4_addr = v4.local_addr().unwrap();
        let v6_acceptor = acceptor.clone();
        let stalled = tokio::spawn(async move {
            let (stream, _) = v6.accept().await.unwrap();
            let mut data = Vec::new();
            if stall_tls {
                let mut stream = stream;
                stream.read_to_end(&mut data).await.unwrap();
            } else {
                let mut stream = v6_acceptor.accept(stream).await.unwrap();
                let _ = stream.read_to_end(&mut data).await;
            }
            assert!(!data.is_empty());
        });
        let success = tokio::spawn(async move {
            let (stream, _) = v4.accept().await.unwrap();
            let tls = acceptor.accept(stream).await.unwrap();
            assert_eq!(tls.get_ref().1.server_name(), Some("localhost"));
            let (request, _socket) = tokio_websockets::ServerBuilder::new()
                .accept(tls)
                .await
                .unwrap();
            assert_eq!(request.headers()[http::header::ORIGIN], WHATSAPP_WEB_ORIGIN);
        });
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert_der).unwrap();
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let factory = WebSocketTransportFactory {
            uri: format!("wss://localhost:{}/ws/chat", v4_addr.port())
                .parse()
                .unwrap(),
            connector: Connector::Rustls(tokio_rustls::TlsConnector::from(Arc::new(config))),
            addresses: Some(vec![v6_addr, v4_addr]),
        };
        let (transport, events) =
            tokio::time::timeout(Duration::from_secs(2), factory.create_transport())
                .await
                .expect("IPv4 should win without waiting for the stalled IPv6 handshake")
                .unwrap();
        assert!(matches!(
            events.recv().await.unwrap(),
            TransportEvent::Connected
        ));
        success.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), stalled)
            .await
            .unwrap()
            .unwrap();
        transport.disconnect().await;
    }

    #[tokio::test]
    async fn factory_falls_back_when_ipv6_stalls_during_tls() {
        factory_falls_back_after_stalled_handshake(true).await;
    }

    #[tokio::test]
    async fn factory_falls_back_when_ipv6_stalls_during_websocket_upgrade() {
        factory_falls_back_after_stalled_handshake(false).await;
    }

    #[tokio::test]
    async fn tls_factory_validates_hostname_and_sends_sni() {
        let factory_default = WebSocketTransportFactory::new();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let cert_der = cert.der().clone();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key.into())
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(stream).await.unwrap();
            assert_eq!(tls.get_ref().1.server_name(), Some("localhost"));
            let (request, _socket) = tokio_websockets::ServerBuilder::new()
                .accept(tls)
                .await
                .unwrap();
            assert_eq!(request.headers()[http::header::ORIGIN], WHATSAPP_WEB_ORIGIN);
        });
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert_der).unwrap();
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let factory = WebSocketTransportFactory {
            uri: format!("wss://localhost:{}/ws/chat", address.port())
                .parse()
                .unwrap(),
            connector: Connector::Rustls(tokio_rustls::TlsConnector::from(Arc::new(config))),
            addresses: Some(vec![address]),
        };
        let (transport, _) = factory.create_transport().await.unwrap();
        server.await.unwrap();
        transport.disconnect().await;
        drop(factory_default);
    }

    #[tokio::test]
    async fn default_tls_connector_rejects_untrusted_certificate() {
        let factory_default = WebSocketTransportFactory::new();
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.der().clone()], key.into())
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            assert!(acceptor.accept(stream).await.is_err());
        });
        let factory = WebSocketTransportFactory {
            uri: format!("wss://localhost:{}/ws/chat", address.port())
                .parse()
                .unwrap(),
            addresses: Some(vec![address]),
            ..factory_default
        };
        assert!(factory.create_transport().await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn tls_factory_rejects_trusted_certificate_for_another_hostname() {
        drop(WebSocketTransportFactory::new());
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["other.invalid".to_owned()]).unwrap();
        let cert_der = cert.der().clone();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key.into())
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            assert!(acceptor.accept(stream).await.is_err());
        });
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert_der).unwrap();
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let factory = WebSocketTransportFactory {
            uri: format!("wss://localhost:{}/ws/chat", address.port())
                .parse()
                .unwrap(),
            connector: Connector::Rustls(tokio_rustls::TlsConnector::from(Arc::new(config))),
            addresses: Some(vec![address]),
        };
        assert!(factory.create_transport().await.is_err());
        server.await.unwrap();
    }
}
