//! What io.web's tests share: the edge's side of the port (the mesh material, the registry, a TLS
//! client, raw and hyper requests), a test upstream, and the requests `respond` takes.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use fluxbee_sdk::mesh::IO_WEB_SERVER_NAME;
use fluxbee_sdk::mesh::{install_crypto_provider, HiveRegistry, MeshCa, MeshTlsMaterial};
use fluxbee_sdk::protocol::WebExposureKeySlot;
use fluxbee_sdk::protocol::PRIMARY_HIVE_ID;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{HeaderMap, HeaderValue, COOKIE, HOST, SET_COOKIE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{self, ClientConfig};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::access::SessionToken;
use crate::exposures::testing::{
    enablement, exposure, holding, link, set, MapKeys, ERP, ERP_ENB, ERP_ID, SHOP, SHOP_ENB,
    SHOP_ID,
};
use crate::exposures::{Exposures, ENTER_PATH};
use crate::proxy::{BoxError, Proxy, RequestBody, Tunnels};
use crate::upstream::Upstreams;
use crate::web::{self, WebBody, WebLimits, WebSetup, WebStatus, LIMITS};

/// The test upstream's page, on any path it has no route for.
pub const UPSTREAM_PAGE: &str = "<!doctype html>\n<title>upstream</title>\n<p>Its own page.</p>\n";

/// A fresh directory of its own under the system's temp dir.
pub fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "io-web-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A registry entry as SY.orchestrator writes it for a joined hive.
pub fn info(hive: &str, role: &str) -> String {
    format!("hive_id: {hive}\nstatus: connected\nrole: {role}\njoin:\n  phase: done\n")
}

/// A registry holding `ingress1`, a connected ingress, and `worker1`, a connected worker.
pub fn registry() -> HiveRegistry {
    let root = temp_dir("registry");
    for (hive, role) in [("ingress1", "ingress"), ("worker1", "worker")] {
        std::fs::create_dir(root.join(hive)).expect("registry entry");
        std::fs::write(root.join(hive).join("info.yaml"), info(hive, role)).expect("info.yaml");
    }
    HiveRegistry::new(root)
}

/// `hive`'s material: a leaf `ca` issues, with `ca`'s certificate as the roots.
pub fn material(ca: &MeshCa, hive: &str) -> MeshTlsMaterial {
    let leaf = ca.issue_leaf(hive).unwrap();
    MeshTlsMaterial::from_pem(&leaf.cert_pem, &leaf.key_pem, ca.ca_cert_pem()).unwrap()
}

/// The edge's client: `hive`'s leaf, verifying io.web under its name.
pub fn client(ca: &MeshCa, hive: &str) -> Arc<ClientConfig> {
    material(ca, hive).client_config_by_name().unwrap()
}

/// io.web's limits, with the registry checked every 20 ms.
pub fn limits() -> WebLimits {
    WebLimits {
        registry_recheck: Duration::from_millis(20),
        ..LIMITS
    }
}

/// A loopback address nothing listens on: bound, then closed.
pub fn closed_port() -> SocketAddr {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap()
}

/// io.web answering for `exposures`, with Archi at `archi` and no worker.
pub fn proxy(exposures: Arc<Exposures>, archi: SocketAddr) -> Arc<Proxy> {
    Arc::new(Proxy {
        exposures,
        upstreams: Upstreams::new(Ok(archi), HiveRegistry::new("/nowhere")),
    })
}

/// io.web's accept loop on a port of its own, over the motherbee leaf `ca` issues, answering with
/// `proxy`.
pub async fn start_serving(
    ca: &MeshCa,
    registry: &HiveRegistry,
    limits: WebLimits,
    proxy: Arc<Proxy>,
) -> (SocketAddr, Arc<WebStatus>) {
    install_crypto_provider();
    let acceptor = web::acceptor(&material(ca, PRIMARY_HIVE_ID), registry).unwrap();
    start_with(acceptor, registry, limits, proxy).await
}

/// io.web's accept loop on a port of its own, over `acceptor`.
pub async fn start_with(
    acceptor: TlsAcceptor,
    registry: &HiveRegistry,
    limits: WebLimits,
    proxy: Arc<Proxy>,
) -> (SocketAddr, Arc<WebStatus>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let setup = WebSetup {
        listen: listener.local_addr().unwrap(),
        tls_dir: PathBuf::new(),
        registry: registry.clone(),
        limits,
    };
    let status = Arc::new(WebStatus::new());
    let served = Arc::clone(&status);
    let listen = setup.listen;
    tokio::spawn(async move { web::serve(listener, acceptor, &setup, &served, proxy).await });
    (listen, status)
}

/// A connection to io.web under its name, as the edge dials it.
pub async fn dial(
    listen: SocketAddr,
    client: Arc<ClientConfig>,
) -> std::io::Result<TlsStream<TcpStream>> {
    let tcp = TcpStream::connect(listen).await?;
    let name = ServerName::try_from(IO_WEB_SERVER_NAME).unwrap();
    TlsConnector::from(client).connect(name, tcp).await
}

/// One request on `host`, with `cookie` when there is one, and its answer: the status, the
/// headers (lowercase names) and the body, whose length the answer announces.
pub async fn exchange_as(
    stream: &mut TlsStream<TcpStream>,
    method: &str,
    path: &str,
    host: &str,
    cookie: Option<&str>,
) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let cookie = cookie
        .map(|cookie| format!("Cookie: {cookie}\r\n"))
        .unwrap_or_default();
    let request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n{cookie}\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let (status, headers) = read_head(stream).await?;
    // A HEAD answer announces the length of the body it does not carry.
    let length = match method {
        "HEAD" => 0,
        _ => header(&headers, "content-length").parse().unwrap(),
    };
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await?;
    Ok((status, headers, body))
}

/// An answer's head, read byte by byte: its status and its headers (lowercase names).
pub async fn read_head(
    stream: &mut TlsStream<TcpStream>,
) -> std::io::Result<(u16, Vec<(String, String)>)> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).expect("an ASCII head");
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap();
    let headers = lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.trim().to_ascii_lowercase(), value.trim().to_string())
        })
        .collect();
    Ok((status.parse().unwrap(), headers))
}

pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

/// The rustls error a TLS stream's I/O error carries.
pub fn rustls_error(err: &std::io::Error) -> Option<&rustls::Error> {
    err.get_ref()?.downcast_ref::<rustls::Error>()
}

/// Whether the server ends `stream` within `within`: EOF or an error, after whatever it sent.
pub async fn ended(stream: &mut TlsStream<TcpStream>, within: Duration) -> bool {
    let mut buf = [0u8; 512];
    timeout(within, async {
        while let Ok(read) = stream.read(&mut buf).await {
            if read == 0 {
                break;
            }
        }
    })
    .await
    .is_ok()
}

/// The edge's side of one request, as the edge sends it: a connection of its own to io.web,
/// hyper's client on it with upgrades, and the answer's head; the connection's task beside.
pub async fn edge_send<B>(
    listen: SocketAddr,
    client: Arc<ClientConfig>,
    request: Request<B>,
) -> (hyper::Result<Response<Incoming>>, JoinHandle<()>)
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    let tls = dial(listen, client).await.expect("the edge's handshake");
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .expect("hyper's handshake");
    let driver = tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    (sender.send_request(request).await, driver)
}

/// A request's body as `respond` takes it.
pub fn boxed<B>(body: B) -> RequestBody
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    body.map_err(Into::into).boxed_unsync()
}

/// `request` with no body, as `respond` takes it.
pub fn without_body(request: Request<()>) -> Request<RequestBody> {
    request.map(|()| boxed(Empty::<Bytes>::new()))
}

/// `request` with `body`, as `respond` takes it.
pub fn with_body(request: Request<()>, body: &'static [u8]) -> Request<RequestBody> {
    request.map(|()| boxed(Full::new(Bytes::from_static(body))))
}

/// `respond`'s answer to `request`, with a connection's tunnel slot of its own.
pub async fn answer_to(
    proxy: &Proxy,
    request: Request<RequestBody>,
    now: u64,
) -> Response<WebBody> {
    web::respond(proxy, request, now, &Tunnels::default()).await
}

/// An answer's whole body.
pub async fn body_of<B>(response: Response<B>) -> Bytes
where
    B: Body<Data = Bytes>,
    B::Error: std::fmt::Debug,
{
    response.into_body().collect().await.unwrap().to_bytes()
}

/// Polls `condition` every 10 ms for up to `within`.
pub async fn eventually(within: Duration, condition: impl Fn() -> bool) -> bool {
    timeout(within, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

/// A time the tests' synthetic clocks start at, and an enablement's end 8 h later.
pub const BOOT: u64 = 1_763_121_000;
pub const UNTIL: u64 = BOOT + 8 * 3600;
/// Fluxbee Cloud's page, as `publish_cloud_endpoint` names it.
pub const CLOUD: &str = "https://cloud.fluxbee.ai";
const A: WebExposureKeySlot = WebExposureKeySlot::A;

/// io.web holding `exposures`, every exposure's upstream the site's own test upstream (Archi's
/// kind, at its address).
pub struct Site {
    pub upstream: TestUpstream,
    pub proxy: Arc<Proxy>,
}

impl Site {
    pub async fn new(exposures: Arc<Exposures>) -> Self {
        let upstream = TestUpstream::start().await;
        let proxy = proxy(exposures, upstream.address);
        Self { upstream, proxy }
    }

    pub fn exposures(&self) -> &Exposures {
        &self.proxy.exposures
    }

    pub async fn respond(&self, request: Request<()>, now: u64) -> Response<WebBody> {
        answer_to(&self.proxy, without_body(request), now).await
    }

    /// A session on `host`, through a link minted and redeemed at `now`: its `Cookie`.
    pub async fn sign_in(
        &self,
        exposure_id: &str,
        enablement_id: &str,
        host: &str,
        now: u64,
    ) -> String {
        let code = link(self.exposures(), exposure_id, enablement_id, now);
        let entered = self.respond(enter(&code, host), now).await;
        assert_eq!(entered.status(), StatusCode::OK);
        cookie_of(&set_cookie(&entered).expect("a session"))
    }
}

/// io.web booted at `boot`, holding `shop` and `erp`, each enabled until `UNTIL` (or `shop`
/// until `shop_until`) with its key, and Cloud's `origins`.
pub async fn enabled_with(label: &str, boot: u64, shop_until: u64, origins: &[&str]) -> Site {
    let keys = MapKeys::new();
    keys.put(SHOP_ID, A, 4, 1);
    keys.put(ERP_ID, A, 2, 2);
    let mut set = set(
        1,
        vec![
            exposure(SHOP_ID, SHOP, vec![enablement(SHOP_ENB, shop_until, A, 4)]),
            exposure(ERP_ID, ERP, vec![enablement(ERP_ENB, UNTIL, A, 2)]),
        ],
    );
    set.cloud_origins = origins.iter().map(ToString::to_string).collect();
    Site::new(holding(label, keys, boot, set).await).await
}

pub async fn enabled(label: &str, boot: u64, shop_until: u64) -> Site {
    enabled_with(label, boot, shop_until, &[]).await
}

/// A request as the edge forwards it: its host, its request id and the client's address.
pub fn request(method: Method, target: &str, host: &str, cookie: Option<&str>) -> Request<()> {
    let mut request = Request::builder()
        .method(method)
        .uri(target)
        .header(HOST, host)
        .header("x-request-id", "0b5a3c1e-7d2f-4a6b-9c8d-1e2f3a4b5c6d")
        .header("x-forwarded-for", "203.0.113.7");
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, cookie);
    }
    request.body(()).unwrap()
}

pub fn enter(code: &str, host: &str) -> Request<()> {
    request(
        Method::GET,
        &format!("{ENTER_PATH}?code={code}"),
        host,
        None,
    )
}

pub fn page(host: &str, cookie: &str) -> Request<()> {
    request(Method::GET, "/", host, Some(cookie))
}

pub fn set_cookie<B>(response: &Response<B>) -> Option<String> {
    response
        .headers()
        .get(SET_COOKIE)
        .map(|value| value.to_str().unwrap().to_string())
}

/// The `Cookie` a browser sends back for a `Set-Cookie`.
pub fn cookie_of(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_string()
}

/// The session a `Cookie` carries.
pub fn token_of(cookie: &str) -> SessionToken<'_> {
    SessionToken::parse(cookie.strip_prefix("__Host-fluxbee_session=").unwrap()).unwrap()
}

/// One request the test upstream got.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: hyper::Method,
    pub path: String,
    pub query: Option<String>,
    pub headers: HeaderMap,
    /// Read whole on the routes that read it (`/echo` and the page).
    pub body: Vec<u8>,
}

/// What the test upstream saw, and what drives its streams.
pub struct UpstreamState {
    pub connections: AtomicUsize,
    pub requests: Mutex<Vec<Seen>>,
    /// Each `/sse` answer's sender: `send_event` writes one frame to every stream open.
    events: Mutex<Vec<mpsc::UnboundedSender<Bytes>>>,
    /// Releases every `/slow` waiting.
    pub release: Notify,
    /// Answers dropped before their end: a `/slow` whose connection closed, an `/sse` body.
    pub dropped: AtomicUsize,
    /// WebSockets whose upstream side ended.
    pub websockets_closed: AtomicUsize,
}

/// A throwaway HTTP/1.1 upstream on loopback, with upgrades:
///
/// - `/echo` reads the body and answers its request as JSON: method, path, query, headers, the
///   body's length and SHA-256;
/// - `/sse` answers `text/event-stream`, one event at once and one per `send_event`;
/// - `/slow` answers only once released;
/// - `/ws` takes a WebSocket's handshake and echoes the bytes;
/// - `/headers` answers its page with `Server`, `X-Powered-By`, a policy and cookies of its own,
///   Fluxbee's among them;
/// - any other path reads the body and answers `UPSTREAM_PAGE`, with its length.
pub struct TestUpstream {
    pub address: SocketAddr,
    pub state: Arc<UpstreamState>,
    task: JoinHandle<()>,
}

impl Drop for TestUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestUpstream {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(UpstreamState {
            connections: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            release: Notify::new(),
            dropped: AtomicUsize::new(0),
            websockets_closed: AtomicUsize::new(0),
        });
        let served = Arc::clone(&state);
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                served.connections.fetch_add(1, Ordering::Relaxed);
                let state = Arc::clone(&served);
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let state = Arc::clone(&state);
                        async move { Ok::<_, Infallible>(route(&state, request).await) }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(tcp), service)
                        .with_upgrades()
                        .await;
                });
            }
        });
        Self {
            address,
            state,
            task,
        }
    }

    /// One more frame on every `/sse` stream open.
    pub fn send_event(&self, text: &str) {
        let frame = Bytes::from(format!("data: {text}\n\n"));
        self.state
            .events
            .lock()
            .unwrap()
            .retain(|stream| stream.send(frame.clone()).is_ok());
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.state.requests.lock().unwrap().clone()
    }

    pub fn connections(&self) -> usize {
        self.state.connections.load(Ordering::Relaxed)
    }

    pub fn dropped(&self) -> usize {
        self.state.dropped.load(Ordering::Relaxed)
    }

    pub fn websockets_closed(&self) -> usize {
        self.state.websockets_closed.load(Ordering::Relaxed)
    }
}

type UpstreamBody = UnsyncBoxBody<Bytes, Infallible>;

/// Counts an answer dropped before its end, unless it was disarmed at its end.
struct Dropped {
    state: Arc<UpstreamState>,
    armed: bool,
}

impl Dropped {
    fn new(state: &Arc<UpstreamState>) -> Self {
        Self {
            state: Arc::clone(state),
            armed: true,
        }
    }
}

impl Drop for Dropped {
    fn drop(&mut self) {
        if self.armed {
            self.state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// An `/sse` answer's body: each frame as `send_event` sends it.
struct EventBody {
    events: mpsc::UnboundedReceiver<Bytes>,
    _dropped: Dropped,
}

impl Body for EventBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.get_mut()
            .events
            .poll_recv(cx)
            .map(|frame| frame.map(|data| Ok(Frame::data(data))))
    }
}

fn full(text: impl Into<Bytes>) -> UpstreamBody {
    Full::new(text.into()).boxed_unsync()
}

async fn route(
    state: &Arc<UpstreamState>,
    mut request: Request<Incoming>,
) -> Response<UpstreamBody> {
    let mut seen = Seen {
        method: request.method().clone(),
        path: request.uri().path().to_string(),
        query: request.uri().query().map(str::to_string),
        headers: request.headers().clone(),
        body: Vec::new(),
    };
    match seen.path.as_str() {
        "/sse" => {
            state.requests.lock().unwrap().push(seen);
            let (sender, events) = mpsc::unbounded_channel();
            let _ = sender.send(Bytes::from_static(b"data: 0\n\n"));
            state.events.lock().unwrap().push(sender);
            let body = EventBody {
                events,
                _dropped: Dropped::new(state),
            };
            let mut response = Response::new(body.boxed_unsync());
            response.headers_mut().insert(
                "content-type",
                HeaderValue::from_static("text/event-stream"),
            );
            response
        }
        "/slow" => {
            state.requests.lock().unwrap().push(seen);
            let mut waiting = Dropped::new(state);
            state.release.notified().await;
            waiting.armed = false;
            Response::new(full("slow\n"))
        }
        "/ws" => {
            state.requests.lock().unwrap().push(seen);
            let upgrade = hyper::upgrade::on(&mut request);
            let state = Arc::clone(state);
            tokio::spawn(async move {
                if let Ok(upgraded) = upgrade.await {
                    let mut io = TokioIo::new(upgraded);
                    let mut buffer = [0u8; 1024];
                    loop {
                        match io.read(&mut buffer).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => {
                                if io.write_all(&buffer[..read]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
                state.websockets_closed.fetch_add(1, Ordering::Relaxed);
            });
            let mut response = Response::new(full(Bytes::new()));
            *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            let headers = response.headers_mut();
            for (name, value) in [
                ("upgrade", "websocket"),
                ("connection", "Upgrade"),
                ("sec-websocket-accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
                ("sec-websocket-protocol", "chat"),
                ("server", "upstream/1.0"),
            ] {
                headers.insert(name, HeaderValue::from_static(value));
            }
            response
        }
        path => {
            let path = path.to_string();
            let body = request
                .into_body()
                .collect()
                .await
                .map(|collected| collected.to_bytes().to_vec())
                .unwrap_or_default();
            seen.body = body.clone();
            let echoed = echo(&seen);
            state.requests.lock().unwrap().push(seen);
            match path.as_str() {
                "/echo" => {
                    let mut response = Response::new(full(echoed.to_string()));
                    response
                        .headers_mut()
                        .insert("content-type", HeaderValue::from_static("application/json"));
                    response
                }
                "/headers" => {
                    let mut response = Response::new(full(UPSTREAM_PAGE));
                    let headers = response.headers_mut();
                    for (name, value) in [
                        ("content-type", "text/html; charset=utf-8"),
                        ("server", "upstream/1.0"),
                        ("x-powered-by", "test"),
                        ("content-security-policy", "default-src 'self'"),
                        ("set-cookie", "app_session=abc; Path=/; HttpOnly"),
                        (
                            "set-cookie",
                            "__Host-fluxbee_session=planted; Path=/; Secure",
                        ),
                        ("set-cookie", "=__Host-fluxbee_session=nameless; Path=/"),
                    ] {
                        headers.append(name, HeaderValue::from_static(value));
                    }
                    response
                }
                _ => {
                    let mut response = Response::new(full(UPSTREAM_PAGE));
                    response.headers_mut().insert(
                        "content-type",
                        HeaderValue::from_static("text/html; charset=utf-8"),
                    );
                    response
                }
            }
        }
    }
}

/// `/echo`'s answer: the request as the upstream got it.
fn echo(seen: &Seen) -> Value {
    let mut headers = serde_json::Map::new();
    for name in seen.headers.keys() {
        let values: Vec<Value> = seen
            .headers
            .get_all(name)
            .iter()
            .map(|value| json!(String::from_utf8_lossy(value.as_bytes())))
            .collect();
        headers.insert(name.as_str().to_string(), Value::Array(values));
    }
    json!({
        "method": seen.method.as_str(),
        "path": seen.path,
        "query": seen.query,
        "headers": headers,
        "body_bytes": seen.body.len(),
        "body_sha256": crate::access::hex(&Sha256::digest(&seen.body)),
    })
}
