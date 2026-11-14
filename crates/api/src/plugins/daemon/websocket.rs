//! `interface websocket`, host side: the outbound `wss` client daemons get.
//!
//! Every socket is attributed to one binding. It counts against that
//! binding's socket cap, and the host closes it when the binding is removed
//! and when the daemon shuts down, whether or not the daemon still holds it.
//! A driver task owns each connection, so closing never waits on the daemon
//! letting go of its handle.
//!
//! Nothing comes from the process's ambient configuration: there is no proxy
//! and no OS trust store, and the TLS provider is named. Only hosts in the
//! plugin's manifest are dialled, over `wss` only.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use plugin::daemon::{
    BindingHandle, Close, FRAME_CAP, Frame, SOCKET_CAP, Socket, SocketError, Websocket,
};
use plugin::manifest::{WebsocketCapability, wss_host};
use rustls_pki_types::ServerName;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

/// The operator's socket limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub socket_cap: usize,
    pub frame_cap: usize,
    /// DNS, TCP, TLS and the WebSocket handshake together.
    pub connect_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            socket_cap: SOCKET_CAP,
            frame_cap: FRAME_CAP,
            connect_timeout: Duration::from_secs(15),
        }
    }
}

/// Where a host name is dialled. The host's is DNS; tests pin names to a
/// local server.
#[async_trait]
pub trait Resolve: Send + Sync + 'static {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>>;
}

pub struct Dns;

#[async_trait]
impl Resolve for Dns {
    async fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        Ok(tokio::net::lookup_host((host, port)).await?.collect())
    }
}

/// The TLS client every daemon socket uses: webpki's roots, the `ring`
/// provider, nothing read from the environment.
pub fn client_tls() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth(),
    )
}

/// One connection, as the driver owns it.
type Stream =
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// Each open socket's id and the host's way to close it, by binding id.
type Open = HashMap<String, Vec<(u64, watch::Sender<Option<Close>>)>>;

/// One plugin's sockets: what its daemon is handed as [`Websocket`].
pub struct PluginSockets {
    capability: Option<WebsocketCapability>,
    limits: Limits,
    tls: Arc<rustls::ClientConfig>,
    resolve: Arc<dyn Resolve>,
    /// Open sockets by binding id: each one's way to be closed by the host.
    open: Mutex<Open>,
    next: std::sync::atomic::AtomicU64,
    /// Handed to each socket's driver, so it can drop its registry entry.
    me: std::sync::Weak<PluginSockets>,
}

impl PluginSockets {
    /// `capability` is the plugin's manifest's; `None` refuses every connect.
    pub fn new(capability: Option<WebsocketCapability>, limits: Limits) -> Arc<Self> {
        PluginSockets::with(capability, limits, client_tls(), Arc::new(Dns))
    }

    pub fn with(
        capability: Option<WebsocketCapability>,
        limits: Limits,
        tls: Arc<rustls::ClientConfig>,
        resolve: Arc<dyn Resolve>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| PluginSockets {
            capability,
            limits,
            tls,
            resolve,
            open: Mutex::new(HashMap::new()),
            next: std::sync::atomic::AtomicU64::new(0),
            me: me.clone(),
        })
    }

    /// Sockets `binding` holds open.
    pub fn count(&self, binding: &str) -> usize {
        self.open
            .lock()
            .expect("socket registry poisoned")
            .get(binding)
            .map_or(0, Vec::len)
    }

    /// Closes every socket of `binding`: it was removed.
    pub fn close_binding(&self, binding: &str) {
        let closing = self
            .open
            .lock()
            .expect("socket registry poisoned")
            .remove(binding)
            .unwrap_or_default();
        for (_, close) in closing {
            let _ = close.send(Some(Close::new(1001, "the binding was removed")));
        }
    }

    /// Closes every socket: the daemon is shutting down or was dropped.
    pub fn close_all(&self) {
        let closing: Vec<_> = self
            .open
            .lock()
            .expect("socket registry poisoned")
            .drain()
            .flat_map(|(_, sockets)| sockets)
            .collect();
        for (_, close) in closing {
            let _ = close.send(Some(Close::new(1001, "the daemon is shutting down")));
        }
    }

    fn forget(&self, binding: &str, id: u64) {
        let mut open = self.open.lock().expect("socket registry poisoned");
        if let Some(sockets) = open.get_mut(binding) {
            sockets.retain(|(socket, _)| *socket != id);
            if sockets.is_empty() {
                open.remove(binding);
            }
        }
    }

    /// Takes a slot for `binding`, or refuses when it holds the cap.
    fn claim(&self, binding: &str) -> Result<(u64, watch::Receiver<Option<Close>>), SocketError> {
        let mut open = self.open.lock().expect("socket registry poisoned");
        let sockets = open.entry(binding.to_owned()).or_default();
        if sockets.len() >= self.limits.socket_cap {
            return Err(SocketError::NotAllowed(format!(
                "this binding already holds {} sockets",
                self.limits.socket_cap
            )));
        }
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (close, closed) = watch::channel(None);
        sockets.push((id, close));
        Ok((id, closed))
    }

    async fn dial(&self, url: &str, host: &str) -> Result<Stream, SocketError> {
        let failed = |stage: &str, error: &dyn std::fmt::Display| {
            SocketError::ConnectFailed(format!("{stage}: {error}"))
        };
        let port = port_of(url).unwrap_or(443);
        let addrs = self
            .resolve
            .resolve(host, port)
            .await
            .map_err(|error| failed("resolve", &error))?;
        let mut last = None;
        let mut tcp = None;
        for addr in addrs {
            match tokio::net::TcpStream::connect(addr).await {
                Ok(stream) => {
                    tcp = Some(stream);
                    break;
                }
                Err(error) => last = Some(error),
            }
        }
        let tcp = tcp.ok_or_else(|| match last {
            Some(error) => failed("connect", &error),
            None => SocketError::ConnectFailed(format!("`{host}` resolves to nothing")),
        })?;
        let name = ServerName::try_from(host.to_owned()).map_err(|error| failed("name", &error))?;
        let tls = tokio_rustls::TlsConnector::from(Arc::clone(&self.tls))
            .connect(name, tcp)
            .await
            .map_err(|error| failed("tls", &error))?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(self.limits.frame_cap))
            .max_frame_size(Some(self.limits.frame_cap));
        let (stream, _) = tokio_tungstenite::client_async_with_config(url, tls, Some(config))
            .await
            .map_err(|error| failed("handshake", &error))?;
        Ok(stream)
    }
}

/// The port a `wss` URL names, when it names one.
fn port_of(url: &str) -> Option<u16> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    authority.rsplit_once(':')?.1.parse().ok()
}

#[async_trait]
impl Websocket for PluginSockets {
    async fn connect(
        &self,
        binding: &dyn BindingHandle,
        url: &str,
    ) -> Result<Box<dyn Socket>, SocketError> {
        let capability = self.capability.as_ref().ok_or_else(|| {
            SocketError::NotAllowed("this plugin does not request `websocket`".into())
        })?;
        capability
            .allows_url(url)
            .map_err(SocketError::NotAllowed)?;
        let host = wss_host(url).map_err(SocketError::NotAllowed)?;
        let binding = binding.id().to_owned();
        let (id, closed) = self.claim(&binding)?;

        let dialled = tokio::time::timeout(self.limits.connect_timeout, self.dial(url, &host))
            .await
            .unwrap_or_else(|_| Err(SocketError::ConnectFailed("timed out".into())));
        let stream = match dialled {
            Ok(stream) => stream,
            Err(error) => {
                self.forget(&binding, id);
                return Err(error);
            }
        };
        // The binding may have been removed while this one was dialling.
        if closed.borrow().is_some() {
            return Err(SocketError::Closed(closed.borrow().clone().unwrap()));
        }

        let (commands, inbox) = mpsc::channel(8);
        let (deliver, frames) = mpsc::channel(1);
        tokio::spawn(drive(
            stream,
            inbox,
            deliver,
            closed,
            self.me.clone(),
            binding,
            id,
        ));
        Ok(Box::new(HostSocket {
            commands,
            frames: tokio::sync::Mutex::new(frames),
            frame_cap: self.limits.frame_cap,
            ended: Mutex::new(None),
        }))
    }
}

enum Command {
    Send(Frame, oneshot::Sender<Result<(), SocketError>>),
    Close(Close),
}

struct HostSocket {
    commands: mpsc::Sender<Command>,
    frames: tokio::sync::Mutex<mpsc::Receiver<Result<Frame, SocketError>>>,
    frame_cap: usize,
    /// How the connection ended, once it has: every later call answers it.
    ended: Mutex<Option<SocketError>>,
}

impl HostSocket {
    fn ended(&self) -> Option<SocketError> {
        self.ended.lock().expect("socket state poisoned").clone()
    }

    fn end(&self, error: SocketError) -> SocketError {
        self.ended
            .lock()
            .expect("socket state poisoned")
            .get_or_insert(error)
            .clone()
    }
}

fn gone() -> SocketError {
    SocketError::Closed(Close::new(1006, "the connection ended"))
}

#[async_trait]
impl Socket for HostSocket {
    async fn send(&self, frame: Frame) -> Result<(), SocketError> {
        if let Some(ended) = self.ended() {
            return Err(ended);
        }
        if frame.len() > self.frame_cap {
            return Err(SocketError::TooLarge);
        }
        let (done, result) = oneshot::channel();
        if self
            .commands
            .send(Command::Send(frame, done))
            .await
            .is_err()
        {
            return Err(self.end(gone()));
        }
        match result.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(self.end(error)),
            Err(_) => Err(self.end(gone())),
        }
    }

    async fn receive(&self) -> Result<Frame, SocketError> {
        if let Some(ended) = self.ended() {
            return Err(ended);
        }
        match self.frames.lock().await.recv().await {
            Some(Ok(frame)) => Ok(frame),
            Some(Err(error)) => Err(self.end(error)),
            None => Err(self.end(gone())),
        }
    }

    fn close(&self, close: Close) {
        let _ = self.commands.try_send(Command::Close(close));
    }
}

/// Owns one connection until it ends: the daemon's sends go out, frames come
/// in one at a time (the daemon reading is the only buffer), and the host's
/// close or the handle's drop ends it.
async fn drive(
    mut stream: Stream,
    mut commands: mpsc::Receiver<Command>,
    deliver: mpsc::Sender<Result<Frame, SocketError>>,
    mut closed: watch::Receiver<Option<Close>>,
    sockets: std::sync::Weak<PluginSockets>,
    binding: String,
    id: u64,
) {
    let closing = |close: &Close| WsMessage::Close(Some(close_frame(close)));
    loop {
        tokio::select! {
            changed = closed.changed() => {
                let close = match changed {
                    Ok(()) => closed.borrow().clone(),
                    Err(_) => None,
                }
                .unwrap_or_else(|| Close::new(1001, "the host closed the socket"));
                let _ = stream.send(closing(&close)).await;
                let _ = deliver.try_send(Err(SocketError::Closed(close)));
                break;
            }
            command = commands.recv() => match command {
                Some(Command::Send(frame, done)) => {
                    let message = match frame {
                        Frame::Text(text) => WsMessage::Text(text.into()),
                        Frame::Binary(bytes) => WsMessage::Binary(bytes.into()),
                    };
                    let sent = stream
                        .send(message)
                        .await
                        .map_err(|error| SocketError::Closed(Close::new(1006, error.to_string())));
                    let failed = sent.is_err();
                    let _ = done.send(sent);
                    if failed {
                        break;
                    }
                }
                Some(Command::Close(close)) => {
                    let _ = stream.send(closing(&close)).await;
                    let _ = deliver.try_send(Err(SocketError::Closed(close)));
                    break;
                }
                // The handle was dropped.
                None => {
                    let _ = stream.send(closing(&Close::normal())).await;
                    break;
                }
            },
            incoming = stream.next() => {
                let frame = match incoming {
                    Some(Ok(WsMessage::Text(text))) => Ok(Frame::Text(text.to_string())),
                    Some(Ok(WsMessage::Binary(bytes))) => Ok(Frame::Binary(bytes.to_vec())),
                    Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_))) => continue,
                    Some(Ok(WsMessage::Close(frame))) => Err(SocketError::Closed(match frame {
                        Some(frame) => Close::new(frame.code.into(), frame.reason.to_string()),
                        None => Close::new(1005, ""),
                    })),
                    Some(Err(tokio_tungstenite::tungstenite::Error::Capacity(_))) => {
                        let _ = stream.send(closing(&Close::new(1009, "frame too large"))).await;
                        Err(SocketError::TooLarge)
                    }
                    Some(Err(error)) => Err(SocketError::Closed(Close::new(1006, error.to_string()))),
                    None => Err(gone()),
                };
                let last = frame.is_err();
                // Waits for the daemon to read: the socket's only buffer.
                tokio::select! {
                    sent = deliver.send(frame) => if sent.is_err() { break },
                    _ = closed.changed() => {
                        let close = closed.borrow().clone()
                            .unwrap_or_else(|| Close::new(1001, "the host closed the socket"));
                        let _ = stream.send(closing(&close)).await;
                        break;
                    }
                }
                if last {
                    break;
                }
            }
        }
    }
    if let Some(sockets) = sockets.upgrade() {
        sockets.forget(&binding, id);
    }
}

fn close_frame(close: &Close) -> CloseFrame {
    CloseFrame {
        code: CloseCode::from(close.code),
        reason: close.reason.clone().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin::daemon::{MemoryStore, SendError, SessionError, SessionRef, SharedStore};
    use tokio::net::TcpListener;

    /// A test CA, and a `gateway.test` certificate it signed (P-256, valid
    /// until 2126, `CA:FALSE`; webpki refuses a CA as an end entity). The
    /// CA's key was not kept.
    const CA: &str = include_str!("testdata/ca.crt");
    const CERT: &str = include_str!("testdata/gateway.test.crt");
    const KEY: &str = include_str!("testdata/gateway.test.key");

    struct Pinned(SocketAddr);

    #[async_trait]
    impl Resolve for Pinned {
        async fn resolve(&self, _: &str, _: u16) -> std::io::Result<Vec<SocketAddr>> {
            Ok(vec![self.0])
        }
    }

    struct Handle(String);

    #[async_trait]
    impl BindingHandle for Handle {
        fn id(&self) -> &str {
            &self.0
        }
        fn config(&self) -> &plugin::Json {
            &serde_json::Value::Null
        }
        async fn open(&self, _: &str, _: Option<&str>) -> Result<SessionRef, SessionError> {
            Err(SessionError::Gone)
        }
        async fn send(
            &self,
            _: &str,
            _: plugin::daemon::Envelope,
            _: Option<&str>,
        ) -> Result<(), SendError> {
            Err(SendError::Gone)
        }
        async fn archive(&self, _: &str) -> Result<(), SessionError> {
            Err(SessionError::Gone)
        }
        fn storage(&self) -> SharedStore {
            MemoryStore::new(0)
        }
    }

    /// A `wss` echo server for `gateway.test`; `big` frames get a reply
    /// over the cap.
    async fn server() -> (SocketAddr, Arc<rustls::ClientConfig>) {
        use rustls_pki_types::pem::PemObject;
        let ca = rustls_pki_types::CertificateDer::from_pem_slice(CA.as_bytes()).unwrap();
        let cert = rustls_pki_types::CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap();
        let key = rustls_pki_types::PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
                        return;
                    };
                    while let Some(Ok(message)) = ws.next().await {
                        let reply = match message {
                            WsMessage::Text(text) if text.as_str() == "big" => {
                                WsMessage::Binary(vec![0; 2048].into())
                            }
                            WsMessage::Text(_) | WsMessage::Binary(_) => message,
                            WsMessage::Close(_) => break,
                            _ => continue,
                        };
                        if ws.send(reply).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        (addr, Arc::new(client))
    }

    async fn sockets(socket_cap: usize) -> Arc<PluginSockets> {
        let (addr, tls) = server().await;
        PluginSockets::with(
            Some(WebsocketCapability {
                hosts: vec!["gateway.test".into()],
            }),
            Limits {
                socket_cap,
                frame_cap: 1024,
                connect_timeout: Duration::from_secs(5),
            },
            tls,
            Arc::new(Pinned(addr)),
        )
    }

    /// Waits for the registry to catch up with a driver ending.
    async fn settles(sockets: &PluginSockets, binding: &str, count: usize) {
        for _ in 0..100 {
            if sockets.count(binding) == count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(sockets.count(binding), count);
    }

    #[tokio::test]
    async fn frames_go_both_ways_over_wss() {
        let sockets = sockets(4).await;
        let binding = Handle("p/discord".into());
        let socket = sockets
            .connect(&binding, "wss://gateway.test/?v=10")
            .await
            .unwrap();
        socket.send(Frame::Text("hello".into())).await.unwrap();
        assert_eq!(socket.receive().await.unwrap(), Frame::Text("hello".into()));
        socket.send(Frame::Binary(vec![1, 2, 3])).await.unwrap();
        assert_eq!(
            socket.receive().await.unwrap(),
            Frame::Binary(vec![1, 2, 3])
        );
        assert_eq!(sockets.count("p/discord"), 1);
        drop(socket);
        settles(&sockets, "p/discord", 0).await;
    }

    #[tokio::test]
    async fn a_host_not_listed_ws_and_the_socket_cap_are_not_allowed() {
        let sockets = sockets(2).await;
        let binding = Handle("p/discord".into());
        for url in [
            "wss://evil.test/",
            "ws://gateway.test/",
            "https://gateway.test/",
        ] {
            assert!(
                matches!(
                    sockets.connect(&binding, url).await,
                    Err(SocketError::NotAllowed(_))
                ),
                "{url}"
            );
        }
        let _one = sockets
            .connect(&binding, "wss://gateway.test/")
            .await
            .unwrap();
        let _two = sockets
            .connect(&binding, "wss://gateway.test/")
            .await
            .unwrap();
        assert!(matches!(
            sockets.connect(&binding, "wss://gateway.test/").await,
            Err(SocketError::NotAllowed(_))
        ));
        // The cap is per binding.
        let other = Handle("q/discord".into());
        sockets
            .connect(&other, "wss://gateway.test/")
            .await
            .unwrap();

        let none = PluginSockets::new(None, Limits::default());
        assert!(matches!(
            none.connect(&binding, "wss://gateway.test/").await,
            Err(SocketError::NotAllowed(_))
        ));
    }

    #[tokio::test]
    async fn an_oversized_frame_is_too_large_either_way() {
        let sockets = sockets(4).await;
        let binding = Handle("p/discord".into());
        let socket = sockets
            .connect(&binding, "wss://gateway.test/")
            .await
            .unwrap();
        assert_eq!(
            socket.send(Frame::Binary(vec![0; 1025])).await,
            Err(SocketError::TooLarge)
        );
        // Nothing was sent: the socket still works.
        socket.send(Frame::Text("ok".into())).await.unwrap();
        assert_eq!(socket.receive().await.unwrap(), Frame::Text("ok".into()));
        // The server answers `big` with 2 KiB, over the 1 KiB cap.
        socket.send(Frame::Text("big".into())).await.unwrap();
        assert_eq!(socket.receive().await, Err(SocketError::TooLarge));
        assert_eq!(socket.receive().await, Err(SocketError::TooLarge));
        settles(&sockets, "p/discord", 0).await;
    }

    #[tokio::test]
    async fn removing_a_binding_closes_its_sockets_and_shutdown_closes_all() {
        let sockets = sockets(4).await;
        let a = Handle("p/discord".into());
        let b = Handle("q/discord".into());
        let a1 = sockets.connect(&a, "wss://gateway.test/").await.unwrap();
        let b1 = sockets.connect(&b, "wss://gateway.test/").await.unwrap();

        sockets.close_binding("p/discord");
        assert!(matches!(
            a1.receive().await,
            Err(SocketError::Closed(Close { code: 1001, .. }))
        ));
        assert!(matches!(
            a1.send(Frame::Text("x".into())).await,
            Err(SocketError::Closed(_))
        ));
        settles(&sockets, "p/discord", 0).await;
        // The other binding's socket is untouched.
        b1.send(Frame::Text("still".into())).await.unwrap();
        assert_eq!(b1.receive().await.unwrap(), Frame::Text("still".into()));

        sockets.close_all();
        assert!(matches!(b1.receive().await, Err(SocketError::Closed(_))));
        settles(&sockets, "q/discord", 0).await;
    }

    #[test]
    fn the_host_tls_config_names_its_provider_and_roots() {
        // Both rustls providers are compiled in, so relying on a process
        // default would panic here.
        let tls = client_tls();
        assert!(tls.alpn_protocols.is_empty());
    }

    #[test]
    fn the_port_comes_from_the_url() {
        assert_eq!(port_of("wss://gateway.discord.gg/?v=10"), None);
        assert_eq!(port_of("wss://gateway.discord.gg:8443/x"), Some(8443));
    }
}
