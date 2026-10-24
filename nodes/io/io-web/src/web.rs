//! io.web's port (docs/io-web-v2.md §11.2 item 3, docs/io/io-web.md): HTTPS on
//! `0.0.0.0:IO_WEB_PORT` with the motherbee's mesh leaf, for the edge on a connected ingress only,
//! and nothing over plaintext, ever.
//!
//! - **Who:** the client verifier (`server_config_for_roles`) admits a certificate from the mesh CA
//!   whose hive the registry lists as a connected ingress, read at each handshake: every one is a
//!   full handshake, since io.web keeps no TLS session to resume. Any other client, one with no
//!   certificate included, fails inside the handshake, so none of its requests is ever read. The
//!   registry is read once more after the handshake, before the connection is served.
//! - **For how long:** each connection checks the registry every `registry_recheck`, and is cut
//!   once its hive stops being a connected ingress, as the router cuts a WAN link.
//! - **What:** HTTP/1.1 only, with a head-read timeout; a request body is never read (`respond`).
//! - **When it cannot:** without its material, its registry or its port it serves nobody, says why
//!   in its status (`WebState::Unavailable`) and its log, and tries again.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use fluxbee_sdk::mesh::{
    install_crypto_provider, peer_hive_from_cert, HiveRegistry, HiveRole, MeshTlsError,
    MeshTlsMaterial, IO_WEB_PORT, MESH_TLS_DIR,
};
use fluxbee_sdk::protocol::PRIMARY_HIVE_ID;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tokio_rustls::TlsAcceptor;

/// Why io.web serves nobody: NODE_STATUS_GET's `last_error.code`. The motherbee's mesh material
/// (`ca.crt`, `cert.crt`, `cert.key`) cannot be read or used.
pub const MESH_TLS_UNAVAILABLE: &str = "mesh_tls_unavailable";
/// The hive registry cannot be read, so every handshake would fail.
pub const HIVE_REGISTRY_UNAVAILABLE: &str = "hive_registry_unavailable";
/// The port cannot be bound.
pub const LISTEN_FAILED: &str = "listen_failed";

/// Read and write buffer per connection: twice the edge door's 64 KiB, so a head the door admitted
/// still fits with the headers the edge adds. A longer head is refused (431).
const MAX_BUF_SIZE: usize = 128 * 1024;
/// A failed accept (out of descriptors, say) waits this long before the next one.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);
/// Connections closed at the cap are logged at most once per interval, with their count.
const REFUSED_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// What a connection may hold. io.web's only client is the edge, which keeps caps of its own
/// (docs/io-web-v2.md §11.2 item 2); these bound what any host of the internal network can hold.
#[derive(Debug, Clone)]
pub struct WebLimits {
    /// A TLS handshake must finish within this.
    pub handshake_timeout: Duration,
    /// Every request head must arrive within this, timed from the handshake or from the previous
    /// answer: one byte, or an h2 preface, never holds a connection longer, and a keep-alive
    /// connection idle that long is closed (hyper's head timer, as on the edge's door, A-87).
    pub header_read_timeout: Duration,
    /// How often each connection checks that its hive is still a connected ingress, and the port
    /// that the registry can be read.
    pub registry_recheck: Duration,
    /// The most connections held at once, handshakes included; one more is closed at accept.
    pub max_connections: usize,
    /// How long io.web waits before trying its material and its port again.
    pub retry: Duration,
}

/// io.web's limits. The registry is checked as often as the router's WAN checks it.
pub const LIMITS: WebLimits = WebLimits {
    handshake_timeout: Duration::from_secs(10),
    header_read_timeout: Duration::from_secs(30),
    registry_recheck: Duration::from_secs(10),
    max_connections: 256,
    retry: Duration::from_secs(30),
};

/// Where io.web serves from, and with what.
#[derive(Debug, Clone)]
pub struct WebSetup {
    pub listen: SocketAddr,
    /// The motherbee's mesh material: `ca.crt`, `cert.crt` and `cert.key`.
    pub tls_dir: PathBuf,
    pub registry: HiveRegistry,
    pub limits: WebLimits,
}

impl WebSetup {
    /// The motherbee's: `0.0.0.0:IO_WEB_PORT`, as the router listens on `0.0.0.0:9000` (the
    /// motherbee has only its internal leg); the leaf SY.orchestrator writes for it under
    /// `MESH_TLS_DIR`; its hive registry.
    pub fn motherbee() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], IO_WEB_PORT)),
            tls_dir: Path::new(MESH_TLS_DIR).join(PRIMARY_HIVE_ID),
            registry: HiveRegistry::motherbee(),
            limits: LIMITS,
        }
    }

    /// The port as NODE_STATUS_GET's `extensions.web` and CONFIG_GET's `runtime.web` show it.
    pub fn view(&self, state: &WebState) -> Value {
        let mut view = json!({
            "state": state.name(),
            "listen": self.listen.to_string(),
            "admits": ["ingress"],
        });
        if let WebState::Unavailable { code, message } = state {
            view["code"] = json!(code);
            view["message"] = json!(message);
        }
        view
    }
}

/// What io.web's port is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebState {
    /// Not serving yet: its config is not in use, or its first attempt is running.
    Starting,
    /// Admitting a connected ingress.
    Serving,
    /// Serving nobody, and why.
    Unavailable { code: &'static str, message: String },
}

impl WebState {
    fn unavailable(code: &'static str, message: String) -> Self {
        Self::Unavailable { code, message }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Serving => "serving",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

/// The port's state, written here and read by the control loop.
#[derive(Debug)]
pub struct WebStatus(Mutex<WebState>);

impl WebStatus {
    pub fn new() -> Self {
        Self(Mutex::new(WebState::Starting))
    }

    pub fn get(&self) -> WebState {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records `state`, and logs it when it changed, so each change is logged once.
    fn report(&self, state: WebState, listen: SocketAddr) {
        let mut current = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if *current == state {
            return;
        }
        match &state {
            WebState::Serving => tracing::info!(
                listen = %listen,
                "IO.web serving: it admits only a connected ingress, over the mesh's mutual TLS"
            ),
            WebState::Unavailable { code, message } => tracing::error!(
                listen = %listen,
                code = %code,
                detail = %message,
                "IO.web serves nobody, and nothing over plaintext"
            ),
            WebState::Starting => {}
        }
        *current = state;
    }
}

/// io.web's port for the life of the process. It opens once the config is in use (`configured`;
/// a config cannot fall out of use: a refused CONFIG_SET keeps the one in use). Until the material
/// and the port are there, it reports why and tries again every `retry`.
pub async fn run(setup: WebSetup, status: Arc<WebStatus>, mut configured: watch::Receiver<bool>) {
    if configured.wait_for(|configured| *configured).await.is_err() {
        // The control loop is gone, and the process with it.
        return;
    }
    install_crypto_provider();
    let (listener, acceptor) = loop {
        match open(&setup).await {
            Ok(opened) => break opened,
            Err(state) => {
                status.report(state, setup.listen);
                tokio::time::sleep(setup.limits.retry).await;
            }
        }
    };
    serve(listener, acceptor, &setup, &status).await;
}

/// The material, then the port: an acceptor that admits only a connected ingress, and the
/// listener.
async fn open(setup: &WebSetup) -> Result<(TcpListener, TlsAcceptor), WebState> {
    let tls_unavailable = |err: MeshTlsError| {
        WebState::unavailable(
            MESH_TLS_UNAVAILABLE,
            format!("{}: {err}", setup.tls_dir.display()),
        )
    };
    let material = MeshTlsMaterial::load_from_dir(&setup.tls_dir).map_err(tls_unavailable)?;
    let acceptor = acceptor(&material, &setup.registry).map_err(tls_unavailable)?;
    let listener = TcpListener::bind(setup.listen)
        .await
        .map_err(|err| WebState::unavailable(LISTEN_FAILED, format!("{}: {err}", setup.listen)))?;
    Ok((listener, acceptor))
}

/// A TLS acceptor that presents `material`'s leaf and admits a client certificate from the mesh CA
/// only when `registry` lists its hive as a connected ingress, at each handshake.
fn acceptor(
    material: &MeshTlsMaterial,
    registry: &HiveRegistry,
) -> Result<TlsAcceptor, MeshTlsError> {
    let config = material.server_config_for_roles(registry.clone(), &[HiveRole::Ingress])?;
    Ok(TlsAcceptor::from(config))
}

/// The registry, as the port's state: serving while it can be read. Otherwise the verifier finds
/// no hive in it and every handshake fails, so the port serves nobody.
fn registry_state(registry: &HiveRegistry) -> WebState {
    match registry.entries() {
        Ok(_) => WebState::Serving,
        Err(err) => WebState::unavailable(
            HIVE_REGISTRY_UNAVAILABLE,
            format!("{}: {err}", registry.root().display()),
        ),
    }
}

/// The accept loop: one task per connection, at most `max_connections` at once, and the
/// registry checked every `registry_recheck` for the state (the first time at once).
async fn serve(listener: TcpListener, acceptor: TlsAcceptor, setup: &WebSetup, status: &WebStatus) {
    let connections = Arc::new(Semaphore::new(setup.limits.max_connections));
    let mut registry_check = tokio::time::interval(setup.limits.registry_recheck);
    let mut refused = Refusals::default();
    loop {
        tokio::select! {
            _ = registry_check.tick() => {
                status.report(registry_state(&setup.registry), setup.listen);
            }
            accepted = listener.accept() => {
                let (tcp, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        tracing::warn!(error = %err, "IO.web accept failed");
                        tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                        continue;
                    }
                };
                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                    drop(tcp);
                    if let Some(count) = refused.note() {
                        tracing::warn!(
                            peer = %peer,
                            refused = count,
                            max_connections = setup.limits.max_connections,
                            "IO.web at its connection cap; closing new connections"
                        );
                    }
                    continue;
                };
                tokio::spawn(connection(
                    tcp,
                    peer,
                    acceptor.clone(),
                    setup.registry.clone(),
                    setup.limits.clone(),
                    permit,
                ));
            }
        }
    }
}

/// Connections closed at the cap since the last warning.
#[derive(Default)]
struct Refusals {
    count: u64,
    logged_at: Option<Instant>,
}

impl Refusals {
    /// Counts one; the count to log when a warning is due.
    fn note(&mut self) -> Option<u64> {
        self.count += 1;
        if self
            .logged_at
            .is_some_and(|at| at.elapsed() < REFUSED_LOG_INTERVAL)
        {
            return None;
        }
        self.logged_at = Some(Instant::now());
        Some(std::mem::take(&mut self.count))
    }
}

/// One connection, holding its permit to the end: the handshake under its timeout, the registry
/// once more, then HTTP, until the peer or the head timer ends it, or until its hive is no longer a
/// connected ingress.
async fn connection(
    tcp: TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
    registry: HiveRegistry,
    limits: WebLimits,
    _permit: OwnedSemaphorePermit,
) {
    let tls = match tokio::time::timeout(limits.handshake_timeout, acceptor.accept(tcp)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(err)) => {
            // A refused hive is logged by the verifier itself (fluxbee_sdk::mesh), with its role.
            tracing::debug!(peer = %peer, error = %err, "IO.web TLS handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!(peer = %peer, "IO.web TLS handshake timed out");
            return;
        }
    };
    // The verifier admitted the certificate's hive; it names the connection from here on.
    let Some(hive) = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .and_then(peer_hive_from_cert)
    else {
        return;
    };
    // Defence in depth, for one small file read: the verifier read the registry inside the
    // handshake, which is always a full one (`server_config_for_roles` keeps no session to
    // resume), and nothing is served unless the registry still says so after it.
    if registry.role(&hive) != Some(HiveRole::Ingress) {
        tracing::warn!(
            peer = %peer,
            hive = %hive,
            "IO.web closed a connection after its handshake: its hive is not a connected ingress"
        );
        return;
    }
    tracing::debug!(peer = %peer, hive = %hive, "IO.web connection admitted");
    let demoted = registry.until_not(&hive, limits.registry_recheck, |registry, hive| {
        registry.role(hive) == Some(HiveRole::Ingress)
    });
    tokio::select! {
        served = serve_http(tls, hive.clone(), limits.header_read_timeout) => {
            if let Err(err) = served {
                tracing::debug!(peer = %peer, hive = %hive, error = %err, "IO.web connection ended");
            }
        }
        () = demoted => {
            tracing::info!(
                peer = %peer,
                hive = %hive,
                "IO.web cut a connection: its hive is no longer a connected ingress"
            );
        }
    }
}

/// One connection's HTTP: HTTP/1 only, with no protocol sniffing, no HTTP/2 and no upgrade. Its
/// head timer starts before the first byte (`WebLimits::header_read_timeout`).
async fn serve_http<IO>(
    io: IO,
    hive: String,
    header_read_timeout: Duration,
) -> Result<(), hyper::Error>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |req: Request<Incoming>| {
        let response = respond(req.method(), req.uri().path());
        // The path without its query, so no query ever reaches the journal (docs/io-web-v2.md
        // §11.3); the request id is the edge's.
        tracing::debug!(
            hive = %hive,
            method = %req.method(),
            path = req.uri().path(),
            status = response.status().as_u16(),
            request_id = req
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default(),
            "IO.web answered"
        );
        std::future::ready(Ok::<_, Infallible>(response))
    });
    let mut http = http1::Builder::new();
    http.timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout)
        .max_buf_size(MAX_BUF_SIZE);
    http.serve_connection(TokioIo::new(io), service).await
}

/// The page at `/`: text only, so it renders under its own policy, which loads nothing; and
/// nothing that names a version or anything internal.
const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Fluxbee</title>
</head>
<body>
<h1>Fluxbee</h1>
<p>This address is served by Fluxbee.</p>
</body>
</html>
"#;

/// Every answer's policy: it loads nothing, its own origin included; no page may frame it; no
/// `<base>` and no form target. `base-uri`, `form-action` and `frame-ancestors` do not fall back to
/// `default-src`, so each is named.
const CONTENT_SECURITY_POLICY: &str =
    "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Every answer's headers: the edge door's set, with io.web's policy.
const SECURITY_HEADERS: [(&str, &str); 5] = [
    ("content-security-policy", CONTENT_SECURITY_POLICY),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("cache-control", "no-store"),
    ("x-robots-tag", "noindex, nofollow, noarchive"),
];

const TEXT: &str = "text/plain; charset=utf-8";

/// io.web's answer to one request, by its method and path alone: its body is never read. `GET /`
/// is the page and `GET /healthz` 200; HEAD gets GET's answer, whose body hyper does not send.
/// Anything else is 404. Every answer carries `SECURITY_HEADERS`.
fn respond(method: &Method, path: &str) -> Response<Full<Bytes>> {
    let readable = *method == Method::GET || *method == Method::HEAD;
    let (status, content_type, body) = match path {
        "/" if readable => (StatusCode::OK, "text/html; charset=utf-8", PAGE),
        "/healthz" if readable => (StatusCode::OK, TEXT, "ok\n"),
        _ => (StatusCode::NOT_FOUND, TEXT, "not found\n"),
    };
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    for (name, value) in SECURITY_HEADERS {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_sdk::mesh::{MeshCa, IO_WEB_SERVER_NAME};
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::timeout;
    use tokio_rustls::client::TlsStream;
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
    use tokio_rustls::rustls::{self, AlertDescription, ClientConfig, RootCertStore};
    use tokio_rustls::TlsConnector;

    /// A fresh directory of its own under the system's temp dir.
    fn temp_dir(label: &str) -> PathBuf {
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
    fn info(hive: &str, role: &str) -> String {
        format!("hive_id: {hive}\nstatus: connected\nrole: {role}\njoin:\n  phase: done\n")
    }

    /// A registry holding `ingress1`, a connected ingress, and `worker1`, a connected worker.
    fn registry() -> HiveRegistry {
        let root = temp_dir("registry");
        for (hive, role) in [("ingress1", "ingress"), ("worker1", "worker")] {
            std::fs::create_dir(root.join(hive)).expect("registry entry");
            std::fs::write(root.join(hive).join("info.yaml"), info(hive, role)).expect("info.yaml");
        }
        HiveRegistry::new(root)
    }

    /// `hive`'s material: a leaf `ca` issues, with `ca`'s certificate as the roots.
    fn material(ca: &MeshCa, hive: &str) -> MeshTlsMaterial {
        let leaf = ca.issue_leaf(hive).unwrap();
        MeshTlsMaterial::from_pem(&leaf.cert_pem, &leaf.key_pem, ca.ca_cert_pem()).unwrap()
    }

    /// The edge's client: `hive`'s leaf, verifying io.web under its name.
    fn client(ca: &MeshCa, hive: &str) -> Arc<ClientConfig> {
        material(ca, hive).client_config_by_name().unwrap()
    }

    /// io.web's limits, with the registry checked every 20 ms.
    fn limits() -> WebLimits {
        WebLimits {
            registry_recheck: Duration::from_millis(20),
            ..LIMITS
        }
    }

    /// io.web's accept loop on a port of its own, over the motherbee leaf `ca` issues.
    async fn start(
        ca: &MeshCa,
        registry: &HiveRegistry,
        limits: WebLimits,
    ) -> (SocketAddr, Arc<WebStatus>) {
        install_crypto_provider();
        let acceptor = acceptor(&material(ca, PRIMARY_HIVE_ID), registry).unwrap();
        start_with(acceptor, registry, limits).await
    }

    /// io.web's accept loop on a port of its own, over `acceptor`.
    async fn start_with(
        acceptor: TlsAcceptor,
        registry: &HiveRegistry,
        limits: WebLimits,
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
        tokio::spawn(async move { serve(listener, acceptor, &setup, &served).await });
        (listen, status)
    }

    /// A local address nothing listens on, for `run` to bind.
    async fn free_port() -> SocketAddr {
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        probe.local_addr().unwrap()
    }

    /// Waits up to 5 s for `status` to leave `Starting`.
    async fn settled(status: &WebStatus) -> WebState {
        for _ in 0..500 {
            let state = status.get();
            if state != WebState::Starting {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the port never left Starting");
    }

    /// A connection to io.web under its name, as the edge dials it.
    async fn dial(
        listen: SocketAddr,
        client: Arc<ClientConfig>,
    ) -> std::io::Result<TlsStream<TcpStream>> {
        let tcp = TcpStream::connect(listen).await?;
        let name = ServerName::try_from(IO_WEB_SERVER_NAME).unwrap();
        TlsConnector::from(client).connect(name, tcp).await
    }

    /// One request and its answer: the status, the headers (lowercase names) and the body.
    async fn exchange(
        stream: &mut TlsStream<TcpStream>,
        method: &str,
        path: &str,
    ) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
        let request = format!("{method} {path} HTTP/1.1\r\nHost: archi.example.com\r\n\r\n");
        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;
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
        let headers: Vec<(String, String)> = lines
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (name, value) = line.split_once(':').unwrap();
                (name.trim().to_ascii_lowercase(), value.trim().to_string())
            })
            .collect();
        // A HEAD answer announces the length of the body it does not carry.
        let length = match method {
            "HEAD" => 0,
            _ => header(&headers, "content-length").parse().unwrap(),
        };
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).await?;
        Ok((status.parse().unwrap(), headers, body))
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
        headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or_default()
    }

    /// What a client the handshake refuses gets. In TLS 1.3 its own handshake ends before the
    /// server checks its certificate, so the refusal arrives as the alert on its first read.
    async fn refused(listen: SocketAddr, client: Arc<ClientConfig>) -> std::io::Error {
        match dial(listen, client).await {
            Err(err) => err,
            Ok(mut stream) => exchange(&mut stream, "GET", "/healthz")
                .await
                .expect_err("refused in the handshake"),
        }
    }

    /// The rustls error a TLS stream's I/O error carries.
    fn rustls_error(err: &std::io::Error) -> Option<&rustls::Error> {
        err.get_ref()?.downcast_ref::<rustls::Error>()
    }

    /// Whether the server ends `stream` within `within`: EOF or an error, after whatever it sent.
    async fn ended(stream: &mut TlsStream<TcpStream>, within: Duration) -> bool {
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

    /// The handshake admits a connected ingress, and refuses a worker (access_denied) and a client
    /// with no certificate (certificate_required): no request of theirs is answered.
    #[tokio::test]
    async fn the_handshake_admits_only_a_connected_ingress() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let (listen, status) = start(&ca, &registry, limits()).await;
        assert_eq!(settled(&status).await, WebState::Serving);

        let mut ingress = dial(listen, client(&ca, "ingress1"))
            .await
            .expect("ingress1's handshake");
        let (code, _, body) = exchange(&mut ingress, "GET", "/healthz")
            .await
            .expect("ingress1 is served");
        assert_eq!((code, body.as_slice()), (200, b"ok\n".as_slice()));

        let err = refused(listen, client(&ca, "worker1")).await;
        assert_eq!(
            rustls_error(&err),
            Some(&rustls::Error::AlertReceived(
                AlertDescription::AccessDenied
            )),
            "{err}"
        );

        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(ca.ca_cert_pem().as_bytes()).unwrap())
            .unwrap();
        let anonymous = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let err = refused(listen, Arc::new(anonymous)).await;
        assert_eq!(
            rustls_error(&err),
            Some(&rustls::Error::AlertReceived(
                AlertDescription::CertificateRequired
            )),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// The registry is read once more after the handshake: a hive the handshake let through, here
    /// over an acceptor that checks the mesh CA alone (the WAN's), is closed before any request of
    /// its is answered unless the registry calls it a connected ingress.
    #[tokio::test]
    async fn after_its_handshake_only_a_connected_ingress_is_served() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        install_crypto_provider();
        let ca_only = TlsAcceptor::from(material(&ca, PRIMARY_HIVE_ID).server_config().unwrap());
        let (listen, _) = start_with(ca_only, &registry, limits()).await;

        let mut ingress = dial(listen, client(&ca, "ingress1"))
            .await
            .expect("ingress1's handshake");
        let (code, _, _) = exchange(&mut ingress, "GET", "/healthz")
            .await
            .expect("ingress1 is served");
        assert_eq!(code, 200);

        let mut worker = dial(listen, client(&ca, "worker1"))
            .await
            .expect("the mesh CA alone admits worker1's handshake");
        exchange(&mut worker, "GET", "/healthz")
            .await
            .expect_err("closed before any answer");
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// An admitted connection is cut once the registry stops calling its hive a connected
    /// ingress, and not before.
    #[tokio::test]
    async fn a_connection_is_cut_once_its_hive_is_no_longer_a_connected_ingress() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let (listen, _) = start(&ca, &registry, limits()).await;
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();
        let (code, _, _) = exchange(&mut ingress, "GET", "/healthz").await.unwrap();
        assert_eq!(code, 200);
        assert!(
            !ended(&mut ingress, Duration::from_millis(200)).await,
            "still an ingress: the connection stays open"
        );

        std::fs::write(
            registry.root().join("ingress1").join("info.yaml"),
            info("ingress1", "worker"),
        )
        .unwrap();
        assert!(
            ended(&mut ingress, Duration::from_secs(2)).await,
            "cut within a few rechecks"
        );
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// A-87 on io.web's port: a connection that sends one byte and no head is closed at the head
    /// timeout, with no answer.
    #[tokio::test]
    async fn a_connection_without_a_head_is_closed_at_the_head_timeout() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let limits = WebLimits {
            header_read_timeout: Duration::from_millis(200),
            ..limits()
        };
        let (listen, _) = start(&ca, &registry, limits).await;
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();
        ingress.write_all(b"G").await.unwrap();
        ingress.flush().await.unwrap();
        assert!(ended(&mut ingress, Duration::from_secs(3)).await);
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// Past its cap a new connection is closed at accept, before any handshake.
    #[tokio::test]
    async fn past_its_cap_a_new_connection_is_closed_at_once() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let limits = WebLimits {
            max_connections: 1,
            ..limits()
        };
        let (listen, _) = start(&ca, &registry, limits).await;
        // A connection that never starts its handshake holds the only slot.
        let _held = TcpStream::connect(listen).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut second = TcpStream::connect(listen).await.unwrap();
        let mut byte = [0u8; 1];
        let read = timeout(Duration::from_secs(2), second.read(&mut byte))
            .await
            .expect("closed at accept");
        assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// Without its material io.web binds nothing, so nothing is served, over TLS or plaintext,
    /// and its state says why.
    #[tokio::test]
    async fn without_its_material_it_binds_nothing_and_says_why() {
        let registry = registry();
        let listen = free_port().await;
        let scratch = temp_dir("no-material");
        let missing = scratch.join(PRIMARY_HIVE_ID);
        let setup = WebSetup {
            listen,
            tls_dir: missing.clone(),
            registry: registry.clone(),
            limits: limits(),
        };
        let status = Arc::new(WebStatus::new());
        let (_configured, configured_rx) = watch::channel(true);
        let task = tokio::spawn(run(setup, Arc::clone(&status), configured_rx));
        match settled(&status).await {
            WebState::Unavailable { code, message } => {
                assert_eq!(code, MESH_TLS_UNAVAILABLE);
                assert!(
                    message.contains(&missing.display().to_string()),
                    "{message}"
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(
            TcpStream::connect(listen).await.is_err(),
            "nothing listens on io.web's port"
        );
        task.abort();
        let _ = std::fs::remove_dir_all(registry.root());
        let _ = std::fs::remove_dir_all(scratch);
    }

    /// An unreadable registry admits nobody, since the verifier finds no hive in it, and the
    /// state says why.
    #[tokio::test]
    async fn an_unreadable_registry_admits_nobody_and_says_why() {
        let ca = MeshCa::generate().unwrap();
        let scratch = temp_dir("no-registry");
        let registry = HiveRegistry::new(scratch.join("hives"));
        let (listen, status) = start(&ca, &registry, limits()).await;
        match settled(&status).await {
            WebState::Unavailable { code, .. } => assert_eq!(code, HIVE_REGISTRY_UNAVAILABLE),
            other => panic!("{other:?}"),
        }
        let err = refused(listen, client(&ca, "ingress1")).await;
        assert_eq!(
            rustls_error(&err),
            Some(&rustls::Error::AlertReceived(
                AlertDescription::AccessDenied
            )),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(scratch);
    }

    /// The port opens only once the config is in use, from the material on disk.
    #[tokio::test]
    async fn the_port_opens_once_the_config_is_in_use() {
        let ca = MeshCa::generate().unwrap();
        let tls_dir = temp_dir("material");
        let leaf = ca.issue_leaf(PRIMARY_HIVE_ID).unwrap();
        std::fs::write(tls_dir.join("ca.crt"), ca.ca_cert_pem()).unwrap();
        std::fs::write(tls_dir.join("cert.crt"), &leaf.cert_pem).unwrap();
        std::fs::write(tls_dir.join("cert.key"), &leaf.key_pem).unwrap();
        let registry = registry();
        let listen = free_port().await;
        let setup = WebSetup {
            listen,
            tls_dir: tls_dir.clone(),
            registry: registry.clone(),
            limits: limits(),
        };
        let status = Arc::new(WebStatus::new());
        let (configured, configured_rx) = watch::channel(false);
        let task = tokio::spawn(run(setup, Arc::clone(&status), configured_rx));

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(status.get(), WebState::Starting);
        assert!(TcpStream::connect(listen).await.is_err(), "not open yet");

        configured.send_replace(true);
        assert_eq!(settled(&status).await, WebState::Serving);
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();
        let (code, _, _) = exchange(&mut ingress, "GET", "/healthz").await.unwrap();
        assert_eq!(code, 200);
        task.abort();
        let _ = std::fs::remove_dir_all(registry.root());
        let _ = std::fs::remove_dir_all(tls_dir);
    }

    /// Every route by method and path, each answer with the security headers.
    #[tokio::test]
    async fn each_route_answers_by_method_and_path_with_the_security_headers() {
        let html = "text/html; charset=utf-8";
        for (method, path, code, content_type, body) in [
            (Method::GET, "/", 200, html, PAGE),
            (Method::HEAD, "/", 200, html, PAGE),
            (Method::GET, "/healthz", 200, TEXT, "ok\n"),
            (Method::HEAD, "/healthz", 200, TEXT, "ok\n"),
            (Method::POST, "/", 404, TEXT, "not found\n"),
            (Method::PUT, "/healthz", 404, TEXT, "not found\n"),
            (Method::DELETE, "/", 404, TEXT, "not found\n"),
            (Method::OPTIONS, "*", 404, TEXT, "not found\n"),
            (Method::GET, "/index.html", 404, TEXT, "not found\n"),
            (Method::GET, "/healthz/", 404, TEXT, "not found\n"),
            (Method::GET, "/e/ich:1", 404, TEXT, "not found\n"),
            (Method::GET, "/public/key", 404, TEXT, "not found\n"),
        ] {
            let response = respond(&method, path);
            let case = format!("{method} {path}");
            assert_eq!(response.status().as_u16(), code, "{case}");
            assert_eq!(response.headers()[CONTENT_TYPE], content_type, "{case}");
            for (name, value) in SECURITY_HEADERS {
                assert_eq!(response.headers()[name], value, "{case}: {name}");
            }
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(bytes, body.as_bytes(), "{case}");
        }
    }

    /// The page renders under its own policy, which loads nothing, and names no version and
    /// nothing internal.
    #[test]
    fn the_page_loads_nothing_and_names_nothing_internal() {
        let page = PAGE.to_ascii_lowercase();
        for load in [
            "<style", "style=", "<script", "<link", "<img", "<iframe", "<object", "<embed",
            "<form", "<base", "src=", "href=", "url(", "@import", "http:", "https:",
        ] {
            assert!(!page.contains(load), "{load}");
        }
        for internal in [
            env!("CARGO_PKG_VERSION"),
            "0.1.",
            "version",
            "motherbee",
            "ingress",
            "io.web",
            "9200",
        ] {
            assert!(!page.contains(internal), "{internal}");
        }
        for directive in [
            "default-src 'none'",
            "base-uri 'none'",
            "form-action 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(CONTENT_SECURITY_POLICY.contains(directive), "{directive}");
        }
    }

    /// Over the wire: the page with its headers; HEAD sends no body, so the connection stays in
    /// step for the next request.
    #[tokio::test]
    async fn the_page_goes_out_with_its_headers_and_head_sends_no_body() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let (listen, _) = start(&ca, &registry, limits()).await;
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();

        let (code, headers, body) = exchange(&mut ingress, "GET", "/").await.unwrap();
        assert_eq!((code, body.as_slice()), (200, PAGE.as_bytes()));
        assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");
        for (name, value) in SECURITY_HEADERS {
            assert_eq!(header(&headers, name), value, "{name}");
        }

        let (code, headers, _) = exchange(&mut ingress, "HEAD", "/").await.unwrap();
        assert_eq!(code, 200);
        assert_eq!(
            header(&headers, "content-security-policy"),
            CONTENT_SECURITY_POLICY
        );
        let (code, _, body) = exchange(&mut ingress, "GET", "/healthz").await.unwrap();
        assert_eq!((code, body.as_slice()), (200, b"ok\n".as_slice()));
        let (code, _, _) = exchange(&mut ingress, "POST", "/").await.unwrap();
        assert_eq!(code, 404);
        let _ = std::fs::remove_dir_all(registry.root());
    }

    #[test]
    fn the_motherbee_serves_the_mesh_port_with_its_own_leaf() {
        let setup = WebSetup::motherbee();
        assert_eq!(setup.listen.to_string(), "0.0.0.0:9200");
        assert_eq!(setup.tls_dir, Path::new("/var/lib/fluxbee/tls/motherbee"));
        assert_eq!(setup.registry.root(), Path::new("/var/lib/fluxbee/hives"));
        assert_eq!(
            setup.view(&WebState::unavailable(LISTEN_FAILED, "taken".to_string())),
            json!({
                "state": "unavailable",
                "listen": "0.0.0.0:9200",
                "admits": ["ingress"],
                "code": "listen_failed",
                "message": "taken",
            })
        );
    }
}
