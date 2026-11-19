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
//!   once its hive stops being a connected ingress, as the router cuts a WAN link. A WebSocket's
//!   tunnel runs inside its connection's scope, so it is cut the same way.
//! - **What:** HTTP/1.1 only, with a head-read timeout and upgrades. Each request is answered by its
//!   host, its session cookie and its path (`respond`, docs/io-web-v2-stage-2-1.md §5.5): a
//!   request whose session serves goes to its exposure's upstream (`proxy`,
//!   docs/io-web-v2-stage-2-2.md §4); any other gets one of io.web's own answers, from its headers
//!   alone, before a byte of its body is read.
//! - **When:** once its config is in use and its exposures are loaded, the file's set and its keys
//!   (`run`).
//! - **When it cannot:** without its material, its registry or its port it serves nobody, says why
//!   in its status (`WebState::Unavailable`) and its log, and tries again.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use fluxbee_sdk::mesh::{
    install_crypto_provider, peer_hive_from_cert, HiveRegistry, HiveRole, MeshTlsError,
    MeshTlsMaterial, IO_WEB_PORT, MESH_TLS_DIR,
};
use fluxbee_sdk::peer_slots::{PeerSlot, PeerSlots};
use fluxbee_sdk::protocol::PRIMARY_HIVE_ID;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::header::{
    HeaderMap, HeaderValue, ALLOW, CONTENT_TYPE, COOKIE, HOST, ORIGIN, SET_COOKIE, UPGRADE,
};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tokio_rustls::TlsAcceptor;

use crate::access::{session_clear_cookie, SESSION_COOKIE};
use crate::exposures::{unix_now, RequestTrace, SessionRefusal, ENTER_PATH};
use crate::headers::{self, Framing};
use crate::proxy::{self, BoxError, Exchange, Proxy, RequestBody, Tunnels, UpstreamBody};
use crate::upstream;

/// Why io.web serves nobody: NODE_STATUS_GET's `last_error.code`. The motherbee's mesh material
/// (`ca.crt`, `cert.crt`, `cert.key`) cannot be read or used.
pub const MESH_TLS_UNAVAILABLE: &str = "mesh_tls_unavailable";
/// The hive registry cannot be read, so every handshake would fail.
pub const HIVE_REGISTRY_UNAVAILABLE: &str = "hive_registry_unavailable";
/// The port cannot be bound.
pub const LISTEN_FAILED: &str = "listen_failed";

/// Read and write buffer per connection: twice the edge door's 64 KiB, so a head the door admitted
/// still fits with the headers the edge adds. A longer head is refused (431). The same buffer on
/// each connection to an upstream.
pub const MAX_BUF_SIZE: usize = 128 * 1024;
/// A failed accept (out of descriptors, say) waits this long before the next one.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);
/// Connections closed at the cap are logged at most once per interval, with their count.
const REFUSED_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// What a connection may hold. io.web's only client is the edge, which keeps caps of its own
/// (docs/io-web-v2.md §11.2 item 2); these bound what any host of the internal network can hold.
#[derive(Debug, Clone)]
pub struct WebLimits {
    /// A TLS handshake must finish within this: one on the internal network takes milliseconds.
    pub handshake_timeout: Duration,
    /// Every request head must arrive within this, timed from the handshake or from the previous
    /// answer: one byte, or an h2 preface, never holds a connection longer, and a keep-alive
    /// connection idle that long is closed (hyper's head timer, as on the edge's door, A-87). It
    /// times the head only: a request then waits for its upstream, and an answer streams, as long
    /// as the application takes.
    pub header_read_timeout: Duration,
    /// How often each connection checks that its hive is still a connected ingress, and the port
    /// that the registry can be read.
    pub registry_recheck: Duration,
    /// The most connections held at once past their handshake; one more is closed once its
    /// handshake ends. A connection still in its handshake holds none (FINDINGS A-89).
    pub max_connections: usize,
    /// The most handshakes under way at once, in all and from one source address; one more is
    /// closed at accept. A host that opens connections and stays silent fills these, and only for
    /// `handshake_timeout`, never the connections served (FINDINGS A-89).
    pub max_handshakes: usize,
    pub max_handshakes_per_source: usize,
    /// How long io.web waits before trying its material and its port again.
    pub retry: Duration,
}

/// io.web's limits. The registry is checked as often as the router's WAN checks it.
pub const LIMITS: WebLimits = WebLimits {
    handshake_timeout: Duration::from_secs(3),
    header_read_timeout: Duration::from_secs(30),
    registry_recheck: Duration::from_secs(10),
    max_connections: 256,
    max_handshakes: 128,
    max_handshakes_per_source: 32,
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
/// a config cannot fall out of use: a refused CONFIG_SET keeps the one in use) and the exposures
/// are loaded, its file's set and their keys (`loaded`, docs/io-web-v2-stage-2-1.md §5.5): opened
/// before, it would refuse the sessions they hold. Until the material and the port are there, it
/// reports why and tries again every `retry`.
pub async fn run(
    setup: WebSetup,
    status: Arc<WebStatus>,
    mut configured: watch::Receiver<bool>,
    mut loaded: watch::Receiver<bool>,
    proxy: Arc<Proxy>,
) {
    // Either sender gone means the process is going.
    if configured.wait_for(|configured| *configured).await.is_err()
        || loaded.wait_for(|loaded| *loaded).await.is_err()
    {
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
    serve(listener, acceptor, &setup, &status, proxy).await;
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
pub(crate) fn acceptor(
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

/// The accept loop: one task per connection, at most `max_handshakes` of them in their handshake
/// (`max_handshakes_per_source` from one address) and `max_connections` past it, and the registry
/// checked every `registry_recheck` for the state (the first time at once).
pub(crate) async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    setup: &WebSetup,
    status: &WebStatus,
    proxy: Arc<Proxy>,
) {
    let connections = Arc::new(Semaphore::new(setup.limits.max_connections));
    let handshakes = Arc::new(Semaphore::new(setup.limits.max_handshakes));
    let handshakes_per_source = PeerSlots::new(setup.limits.max_handshakes_per_source);
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
                let (Ok(handshake), Some(source)) = (
                    Arc::clone(&handshakes).try_acquire_owned(),
                    handshakes_per_source.try_take(peer.ip()),
                ) else {
                    drop(tcp);
                    if let Some(count) = refused.note() {
                        tracing::warn!(
                            peer = %peer,
                            refused = count,
                            max_handshakes = setup.limits.max_handshakes,
                            max_handshakes_per_source = setup.limits.max_handshakes_per_source,
                            "IO.web at its handshake cap; closing new connections"
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
                    (handshake, source),
                    Arc::clone(&connections),
                    Arc::clone(&proxy),
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

/// One connection: TCP keepalive on its socket; the handshake under its timeout, holding its
/// handshake slots; a connection slot from then on, to the end; the registry once more; then HTTP,
/// a WebSocket's tunnel included, until the peer or the head timer ends it, or until its hive is no
/// longer a connected ingress.
async fn connection(
    tcp: TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
    registry: HiveRegistry,
    limits: WebLimits,
    handshake: (OwnedSemaphorePermit, PeerSlot),
    connections: Arc<Semaphore>,
    proxy: Arc<Proxy>,
) {
    // With no wait of io.web's own on a request, an edge gone without a FIN would otherwise hold
    // this connection, and its upstream's, for ever (stage 2.2 §3.1).
    upstream::keep_alive(&tcp);
    let handshaken = tokio::time::timeout(limits.handshake_timeout, acceptor.accept(tcp)).await;
    drop(handshake);
    let tls = match handshaken {
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
    let Ok(_permit) = connections.try_acquire_owned() else {
        tracing::warn!(
            peer = %peer,
            max_connections = limits.max_connections,
            "IO.web at its connection cap; closing a connection past its handshake"
        );
        return;
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
        served = serve_http(tls, limits.header_read_timeout, proxy) => {
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

/// One connection's HTTP: HTTP/1 only, with no protocol sniffing and no HTTP/2. Its head timer
/// starts before the first byte (`WebLimits::header_read_timeout`). It takes upgrades: once hyper
/// hands an upgraded connection over, its WebSocket's tunnel runs here, inside the connection's
/// scope, where it keeps the connection's permit and a demoted ingress still cuts it (stage 2.2
/// §4.1); hyper's own future ends the moment it hands the connection over.
async fn serve_http<IO>(
    io: IO,
    header_read_timeout: Duration,
    proxy: Arc<Proxy>,
) -> Result<(), hyper::Error>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let tunnels = Arc::new(Tunnels::default());
    let service = {
        let tunnels = Arc::clone(&tunnels);
        service_fn(move |request: Request<Incoming>| {
            let proxy = Arc::clone(&proxy);
            let tunnels = Arc::clone(&tunnels);
            async move {
                let request = request.map(|body| body.map_err(BoxError::from).boxed_unsync());
                Ok::<_, Infallible>(respond(&proxy, request, unix_now(), &tunnels).await)
            }
        })
    };
    let mut http = http1::Builder::new();
    http.timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout)
        .max_buf_size(MAX_BUF_SIZE);
    let served = http
        .serve_connection(TokioIo::new(io), service)
        .with_upgrades()
        .await;
    let tunnel = tunnels.take();
    if let Some(tunnel) = tunnel {
        tunnel.await;
    }
    served
}

/// The page a redeemed link answers with, beside the session's cookie (§3.2). It moves on to `/`
/// at once with a refresh, not a 303: a `SameSite=Strict` cookie is withheld from a redirect inside
/// a navigation that began on another site (Cloud's), and is sent on a navigation this page starts.
/// Inside Cloud's frame the refresh runs in the frame.
pub(crate) const ENTER_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta http-equiv="refresh" content="0; url=/">
<title>Fluxbee</title>
</head>
<body>
<p>Opening this address.</p>
</body>
</html>
"#;

/// The fixed 401 page: every refusal of a link or a session answers it, whatever the reason, which
/// the log alone holds (§3.2, §3.3), and so does a request still waiting when its exposure is cut.
pub(crate) const UNAUTHORIZED_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Fluxbee</title>
</head>
<body>
<h1>Fluxbee</h1>
<p>This address opens only through a valid access link.</p>
</body>
</html>
"#;

/// The fixed 403 page of the Origin check (stage 2.2 §4.1): a write or a WebSocket that does not
/// come from the exposure's own page. Which header refused it, the log alone holds.
pub(crate) const FORBIDDEN_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Fluxbee</title>
</head>
<body>
<h1>Fluxbee</h1>
<p>This address takes this request only from its own page.</p>
</body>
</html>
"#;

/// The fixed 502 page (stage 2.2 §4.2): the exposure's upstream was not reached, or gave no answer
/// io.web can pass. What failed, the log alone holds.
pub(crate) const BAD_GATEWAY_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Fluxbee</title>
</head>
<body>
<h1>Fluxbee</h1>
<p>This address cannot be reached right now. Try again shortly.</p>
</body>
</html>
"#;

/// The paths io.web keeps for itself on every host (§5.5): no session needed, and never an
/// upstream's.
const RESERVED_PREFIX: &str = "/.fluxbee/";
/// io.web's health check, under the reserved prefix so that it never shadows an upstream's own.
const HEALTHZ_PATH: &str = "/.fluxbee/healthz";

/// What a 405 names: what a web host serves, every method an application uses but `CONNECT` and
/// `TRACE`, which the edge refuses first.
const ALLOWED_METHODS: &str = "GET, HEAD, OPTIONS, POST, PUT, PATCH, DELETE";

const TEXT: &str = "text/plain; charset=utf-8";
const HTML: &str = "text/html; charset=utf-8";

/// An answer's body: one of io.web's own, whole, or an upstream's, streamed
/// (`proxy::UpstreamBody`).
pub enum WebBody {
    Own(Full<Bytes>),
    Upstream(UpstreamBody),
}

impl WebBody {
    pub fn empty() -> Self {
        Self::Own(Full::new(Bytes::new()))
    }
}

impl Body for WebBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        match self.get_mut() {
            Self::Own(body) => Pin::new(body)
                .poll_frame(cx)
                .map(|frame| frame.map(|frame| frame.map_err(|never| match never {}))),
            Self::Upstream(body) => Pin::new(body).poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Own(body) => body.is_end_stream(),
            Self::Upstream(body) => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Own(body) => body.size_hint(),
            Self::Upstream(body) => body.size_hint(),
        }
    }
}

/// io.web's answer to one request at `now`, from its host, its session cookie and its path
/// (docs/io-web-v2-stage-2-1.md §3, §5.5; docs/io-web-v2-stage-2-2.md §4.1). A refusal comes from
/// the headers alone, before a byte of the body is read.
///
/// - `/.fluxbee/` is reserved on every host and never reaches an upstream: `GET /.fluxbee/enter`
///   redeems a link, any other method there answers 401 and never redeems; `GET` and `HEAD` of
///   `/.fluxbee/healthz` are 200; any other path there is 404.
/// - A host that is no exposure's is 404; `CONNECT` and `TRACE` are 405 (the edge refuses both
///   first).
/// - Any other request needs a valid session: without one, the fixed 401 page, which clears a
///   session cookie that is not valid (more than one is not valid; one whose enablement is live
///   but whose key io.web has not read yet is kept, §3.4).
/// - Then the Origin check (`origin_refusal`): a write or a WebSocket from another page answers
///   the fixed 403 page.
/// - Then the request goes to its exposure's upstream, whatever its method (`proxy::forward`).
///
/// io.web's own answers carry `headers::set_own_headers`; the upstream's, §13.7's table.
pub async fn respond(
    proxy: &Proxy,
    request: Request<RequestBody>,
    now: u64,
    tunnels: &Tunnels,
) -> Response<WebBody> {
    let exposures = &proxy.exposures;
    let framing = exposures.framing();
    let method = request.method().clone();
    let readable = method == Method::GET || method == Method::HEAD;
    let host = request_host(request.headers()).unwrap_or_default();
    let path = request.uri().path();
    match path {
        HEALTHZ_PATH if readable => return answer(StatusCode::OK, TEXT, "ok\n", None, &framing),
        ENTER_PATH if method == Method::GET => {
            let headers = request.headers();
            let trace = RequestTrace {
                request_id: header_text(headers, "x-request-id"),
                forwarded_for: header_text(headers, "x-forwarded-for"),
            };
            return match exposures.redeem(&host, link_code(request.uri().query()), now, trace) {
                Some(set_cookie) => {
                    answer(StatusCode::OK, HTML, ENTER_PAGE, Some(set_cookie), &framing)
                }
                None => unauthorized(None, &framing),
            };
        }
        // A HEAD never redeems, nor any other method.
        ENTER_PATH => return unauthorized(None, &framing),
        path if path.starts_with(RESERVED_PREFIX) => return not_found(&framing),
        _ => {}
    }
    let Some((exposure_id, gate)) = exposures.gate_of(&host) else {
        return not_found(&framing);
    };
    if method == Method::CONNECT || method == Method::TRACE {
        let mut refused = answer(
            StatusCode::METHOD_NOT_ALLOWED,
            TEXT,
            "method not allowed\n",
            None,
            &framing,
        );
        refused
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static(ALLOWED_METHODS));
        return refused;
    }
    let request_id = header_text(request.headers(), "x-request-id");
    let mut exchange = Exchange::new(request_id, &host, &method, path, exposure_id, gate);
    let admitted = match session_cookie(request.headers()) {
        SessionCookie::Absent => {
            exchange.refuse(401, "no session cookie".to_string(), None);
            return unauthorized(None, &framing);
        }
        SessionCookie::Several => {
            exchange.refuse(401, "more than one session cookie".to_string(), None);
            return unauthorized(Some(session_clear_cookie()), &framing);
        }
        SessionCookie::One(value) => match exposures.admit(&host, value, now) {
            Ok(admitted) => admitted,
            Err(refusal) => return refused_session(exchange, refusal, &framing),
        },
    };
    if let Some(why) = origin_refusal(&method, request.headers(), &host) {
        // Nothing is forwarded, and nothing stays registered.
        drop(admitted);
        exchange.refuse(403, why, None);
        return answer(StatusCode::FORBIDDEN, HTML, FORBIDDEN_PAGE, None, &framing);
    }
    proxy::forward(proxy, admitted, request, exchange, tunnels).await
}

/// The Origin check (stage 2.2 §4.1; io-web-v2 §13.8): a request that writes, every method but
/// GET, HEAD and OPTIONS as Go's `CrossOriginProtection` takes it, or a GET with
/// `Upgrade: websocket`, passes only from the exposure's own page. Every `*.fluxbee.ai` host is one
/// site, so `SameSite=Strict` stops no sibling's page; this does.
///
/// - `Sec-Fetch-Site: same-origin` or `none` passes; any other value is refused.
/// - Without it, `Origin: https://<host>` passes; any other origin is refused, `null` included.
/// - With neither header, it passes: a browser's cross-origin request carries one of them, and a
///   browser always sends `Origin` with a WebSocket's handshake (RFC 6455 §4.1).
///
/// Why it is refused, the header and its value, for the log; `None` when it passes.
fn origin_refusal(method: &Method, headers: &HeaderMap, host: &str) -> Option<String> {
    let writes = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let websocket = *method == Method::GET && headers::names_token(headers, &UPGRADE, "websocket");
    if !writes && !websocket {
        return None;
    }
    let sites: Vec<&HeaderValue> = headers.get_all("sec-fetch-site").iter().collect();
    if !sites.is_empty() {
        return sites
            .into_iter()
            .find(|site| !matches!(site.as_bytes(), b"same-origin" | b"none"))
            .map(|site| {
                format!(
                    "Sec-Fetch-Site {:?} is not same-origin",
                    String::from_utf8_lossy(site.as_bytes())
                )
            });
    }
    let own = format!("https://{host}");
    headers
        .get_all(ORIGIN)
        .iter()
        .find(|origin| origin.as_bytes() != own.as_bytes())
        .map(|origin| {
            format!(
                "Origin {:?} is not the exposure's own",
                String::from_utf8_lossy(origin.as_bytes())
            )
        })
}

/// The fixed 401 page for a session cookie that does not serve, its reason in the request's line
/// (stage 2.1 §5.10). It clears the cookie, unless the cookie may serve once io.web reads its
/// enablement's key (`SessionRefusal::kept`, §3.4).
fn refused_session(
    mut exchange: Exchange,
    refusal: SessionRefusal,
    framing: &Framing,
) -> Response<WebBody> {
    exchange.refuse(401, refusal.reason.to_string(), refusal.enablement_id);
    unauthorized((!refusal.kept).then(session_clear_cookie), framing)
}

/// One of io.web's own answers: `headers::set_own_headers` for the set's origins, and
/// `set_cookie` when there is one.
fn answer(
    status: StatusCode,
    content_type: &'static str,
    body: &'static str,
    set_cookie: Option<String>,
    framing: &Framing,
) -> Response<WebBody> {
    let mut response = Response::new(WebBody::Own(Full::new(Bytes::from_static(body.as_bytes()))));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers::set_own_headers(headers, framing);
    // A cookie's value is hex, digits and dots, and its attributes ASCII: always a valid value.
    if let Some(cookie) = set_cookie.and_then(|cookie| HeaderValue::try_from(cookie).ok()) {
        headers.insert(SET_COOKIE, cookie);
    }
    response
}

fn not_found(framing: &Framing) -> Response<WebBody> {
    answer(StatusCode::NOT_FOUND, TEXT, "not found\n", None, framing)
}

/// The fixed 401 page, with `set_cookie` when there is one.
pub(crate) fn unauthorized(set_cookie: Option<String>, framing: &Framing) -> Response<WebBody> {
    answer(
        StatusCode::UNAUTHORIZED,
        HTML,
        UNAUTHORIZED_PAGE,
        set_cookie,
        framing,
    )
}

/// The fixed 502 page.
pub(crate) fn bad_gateway(framing: &Framing) -> Response<WebBody> {
    answer(
        StatusCode::BAD_GATEWAY,
        HTML,
        BAD_GATEWAY_PAGE,
        None,
        framing,
    )
}

/// The request's host, as the edge forwards it: its one `Host` header, lowercase, without a
/// trailing dot or port 443 (the edge's own canonical form). Two or none give no host.
fn request_host(headers: &HeaderMap) -> Option<String> {
    let mut hosts = headers.get_all(HOST).iter();
    let (Some(value), None) = (hosts.next(), hosts.next()) else {
        return None;
    };
    let host = value.to_str().ok()?.trim().to_ascii_lowercase();
    let host = host.strip_suffix(":443").unwrap_or(&host);
    let host = host.strip_suffix('.').unwrap_or(host);
    (!host.is_empty()).then(|| host.to_string())
}

/// A header's text, empty when it has none.
fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
}

/// The code of `/.fluxbee/enter?code=…`: one `code`, or none.
fn link_code(query: Option<&str>) -> Option<&str> {
    let mut codes = query?
        .split('&')
        .filter_map(|pair| pair.strip_prefix("code="));
    let (Some(code), None) = (codes.next(), codes.next()) else {
        return None;
    };
    Some(code)
}

/// The session cookies a request's `Cookie` headers carry (§3.3).
#[derive(Debug, PartialEq, Eq)]
enum SessionCookie<'a> {
    Absent,
    One(&'a str),
    /// More than one, which is refused.
    Several,
}

fn session_cookie(headers: &HeaderMap) -> SessionCookie<'_> {
    let mut values = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value);
    match (values.next(), values.next()) {
        (None, _) => SessionCookie::Absent,
        (Some(value), None) => SessionCookie::One(value),
        (Some(_), Some(_)) => SessionCookie::Several,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{EnablementKeys, LinkCode};
    use crate::exposures::testing::*;
    use crate::exposures::{boot_second, Exposures};
    use crate::headers::{OWN_POLICY, PERMISSIONS_POLICY, SECURITY_HEADERS};
    use crate::testing::{
        self, answer_to, body_of, client, closed_port, cookie_of, dial, enabled, enabled_with,
        enter, exchange_as, header, info, material, page, proxy, registry, request, rustls_error,
        set_cookie, start_serving, start_with, temp_dir, token_of, without_body, Site, BOOT, CLOUD,
        UNTIL, UPSTREAM_PAGE,
    };
    use fluxbee_sdk::mesh::MeshCa;
    use fluxbee_sdk::protocol::{
        IoWebAccessLinkRequest, IoWebAccessLinkResponse, IoWebExposuresResponse,
        WebExposureKeySlot, IO_WEB_ERROR_KEY_UNAVAILABLE,
    };
    use fluxbee_sdk::vault::web_exposure_vault_key;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::timeout;
    use tokio_rustls::client::TlsStream;
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::CertificateDer;
    use tokio_rustls::rustls::{self, AlertDescription, ClientConfig, RootCertStore};

    const A: WebExposureKeySlot = WebExposureKeySlot::A;

    /// io.web holding no exposure, which never writes its file, and no upstream it could reach.
    fn idle() -> Arc<Proxy> {
        let exposures = Arc::new(Exposures::new(
            PathBuf::from("never-written/exposures.json"),
            MapKeys::new(),
            0,
        ));
        proxy(exposures, closed_port())
    }

    async fn body(response: Response<WebBody>) -> Bytes {
        body_of(response).await
    }

    /// 401 with the fixed page, and a cookie cleared when `clears`.
    async fn assert_unauthorized(response: Response<WebBody>, clears: bool, case: &str) {
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{case}");
        assert_eq!(
            set_cookie(&response),
            clears.then(session_clear_cookie),
            "{case}"
        );
        assert_eq!(body(response).await, UNAUTHORIZED_PAGE.as_bytes(), "{case}");
    }

    /// The upstream's own page, as a valid session gets it.
    async fn assert_upstream_page(response: Response<WebBody>, case: &str) {
        assert_eq!(response.status(), StatusCode::OK, "{case}");
        assert_eq!(body(response).await, UPSTREAM_PAGE.as_bytes(), "{case}");
    }

    /// io.web's accept loop on a port of its own, over the motherbee leaf `ca` issues, holding no
    /// exposure.
    async fn start(
        ca: &MeshCa,
        registry: &HiveRegistry,
        limits: WebLimits,
    ) -> (SocketAddr, Arc<WebStatus>) {
        start_serving(ca, registry, limits, idle()).await
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

    /// One request on `archi.example.com` and its answer.
    async fn exchange(
        stream: &mut TlsStream<TcpStream>,
        method: &str,
        path: &str,
    ) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
        exchange_as(stream, method, path, "archi.example.com", None).await
    }

    /// What a client the handshake refuses gets. In TLS 1.3 its own handshake ends before the
    /// server checks its certificate, so the refusal arrives as the alert on its first read.
    async fn refused(listen: SocketAddr, client: Arc<ClientConfig>) -> std::io::Error {
        match dial(listen, client).await {
            Err(err) => err,
            Ok(mut stream) => exchange(&mut stream, "GET", HEALTHZ_PATH)
                .await
                .expect_err("refused in the handshake"),
        }
    }

    /// The handshake admits a connected ingress, and refuses a worker (access_denied) and a client
    /// with no certificate (certificate_required): no request of theirs is answered.
    #[tokio::test]
    async fn the_handshake_admits_only_a_connected_ingress() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let (listen, status) = start(&ca, &registry, testing::limits()).await;
        assert_eq!(settled(&status).await, WebState::Serving);

        let mut ingress = dial(listen, client(&ca, "ingress1"))
            .await
            .expect("ingress1's handshake");
        let (code, _, body) = exchange(&mut ingress, "GET", HEALTHZ_PATH)
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
        let (listen, _) = start_with(ca_only, &registry, testing::limits(), idle()).await;

        let mut ingress = dial(listen, client(&ca, "ingress1"))
            .await
            .expect("ingress1's handshake");
        let (code, _, _) = exchange(&mut ingress, "GET", HEALTHZ_PATH)
            .await
            .expect("ingress1 is served");
        assert_eq!(code, 200);

        let mut worker = dial(listen, client(&ca, "worker1"))
            .await
            .expect("the mesh CA alone admits worker1's handshake");
        exchange(&mut worker, "GET", HEALTHZ_PATH)
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
        let (listen, _) = start(&ca, &registry, testing::limits()).await;
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();
        let (code, _, _) = exchange(&mut ingress, "GET", HEALTHZ_PATH).await.unwrap();
        assert_eq!(code, 200);
        assert!(
            !testing::ended(&mut ingress, Duration::from_millis(200)).await,
            "still an ingress: the connection stays open"
        );

        std::fs::write(
            registry.root().join("ingress1").join("info.yaml"),
            info("ingress1", "worker"),
        )
        .unwrap();
        assert!(
            testing::ended(&mut ingress, Duration::from_secs(2)).await,
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
            ..testing::limits()
        };
        let (listen, _) = start(&ca, &registry, limits).await;
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();
        ingress.write_all(b"G").await.unwrap();
        ingress.flush().await.unwrap();
        assert!(testing::ended(&mut ingress, Duration::from_secs(3)).await);
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// A-89: past the handshakes one address may hold, its next connection is closed at accept,
    /// before any handshake; a silent connection holds its slot only for the handshake timeout.
    #[tokio::test]
    async fn past_its_handshake_cap_a_new_connection_is_closed_at_once() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let limits = WebLimits {
            max_handshakes_per_source: 1,
            handshake_timeout: Duration::from_millis(500),
            ..testing::limits()
        };
        let (listen, _) = start(&ca, &registry, limits).await;
        // A connection that never starts its handshake holds the only slot.
        let mut held = TcpStream::connect(listen).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut second = TcpStream::connect(listen).await.unwrap();
        let mut byte = [0u8; 1];
        let read = timeout(Duration::from_secs(2), second.read(&mut byte))
            .await
            .expect("closed at accept");
        assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
        // The silent one is closed at its handshake timeout, and the slot comes back.
        let read = timeout(Duration::from_secs(2), held.read(&mut byte))
            .await
            .expect("closed at its handshake timeout");
        assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();
        let (code, _, _) = exchange(&mut ingress, "GET", HEALTHZ_PATH).await.unwrap();
        assert_eq!(code, 200);
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// A-89: connections in their handshake do not hold the connections served: with every one of
    /// those held, a silent connection still gets its handshake slot, and is closed only once its
    /// handshake ends.
    #[tokio::test]
    async fn silent_connections_hold_no_served_slot() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let limits = WebLimits {
            max_connections: 1,
            ..testing::limits()
        };
        let (listen, _) = start(&ca, &registry, limits).await;
        let _silent = TcpStream::connect(listen).await.unwrap();
        let _also_silent = TcpStream::connect(listen).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Two silent connections, and the edge is still served.
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();
        let (code, _, _) = exchange(&mut ingress, "GET", HEALTHZ_PATH).await.unwrap();
        assert_eq!(code, 200);
        // With the only served slot held, the next handshake ends and its connection is closed.
        let mut next = dial(listen, client(&ca, "ingress1")).await.unwrap();
        assert!(
            exchange(&mut next, "GET", HEALTHZ_PATH).await.is_err(),
            "past the served cap the connection is closed after its handshake"
        );
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
            limits: testing::limits(),
        };
        let status = Arc::new(WebStatus::new());
        let (_configured, configured_rx) = watch::channel(true);
        let (_loaded, loaded_rx) = watch::channel(true);
        let task = tokio::spawn(run(
            setup,
            Arc::clone(&status),
            configured_rx,
            loaded_rx,
            idle(),
        ));
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
        let (listen, status) = start(&ca, &registry, testing::limits()).await;
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

    /// The port opens only once the config is in use and the exposures are loaded, from the
    /// material on disk.
    #[tokio::test]
    async fn the_port_opens_once_the_config_is_in_use_and_the_exposures_are_loaded() {
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
            limits: testing::limits(),
        };
        let status = Arc::new(WebStatus::new());
        let (configured, configured_rx) = watch::channel(false);
        let (loaded, loaded_rx) = watch::channel(false);
        let task = tokio::spawn(run(
            setup,
            Arc::clone(&status),
            configured_rx,
            loaded_rx,
            idle(),
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(status.get(), WebState::Starting);
        assert!(TcpStream::connect(listen).await.is_err(), "not open yet");

        configured.send_replace(true);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(status.get(), WebState::Starting);
        assert!(
            TcpStream::connect(listen).await.is_err(),
            "not open before the exposures are loaded"
        );

        loaded.send_replace(true);
        assert_eq!(settled(&status).await, WebState::Serving);
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();
        let (code, _, _) = exchange(&mut ingress, "GET", HEALTHZ_PATH).await.unwrap();
        assert_eq!(code, 200);
        task.abort();
        let _ = std::fs::remove_dir_all(registry.root());
        let _ = std::fs::remove_dir_all(tls_dir);
    }

    /// The uuid of an `exp:` or `enb:` id in 32 hex, as codes and cookies carry it.
    fn hex_id(id: &str) -> String {
        uuid::Uuid::parse_str(id.split_once(':').unwrap().1)
            .unwrap()
            .simple()
            .to_string()
    }

    /// Every route by host, cookie and path. io.web's own answers carry the strict set; a valid
    /// session reaches the upstream, whose answer carries the application's column of §13.7's
    /// table and none of the strict set (stage 2.2 §4.4).
    #[tokio::test]
    async fn each_route_answers_with_the_security_headers() {
        let site = enabled("routes", BOOT, UNTIL).await;
        let session = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let with = Some(session.as_str());
        let unknown = "nowhere.hive-k3m9x7q2.fluxbee.ai";
        let not_found = "not found\n";
        let own_policy = format!("{OWN_POLICY}; frame-ancestors 'none'");
        for (method, path, host, cookie, code, content_type, body_text) in [
            (Method::GET, "/", SHOP, None, 401, HTML, UNAUTHORIZED_PAGE),
            (
                Method::DELETE,
                "/",
                SHOP,
                None,
                401,
                HTML,
                UNAUTHORIZED_PAGE,
            ),
            (
                Method::OPTIONS,
                "*",
                SHOP,
                None,
                401,
                HTML,
                UNAUTHORIZED_PAGE,
            ),
            (
                Method::GET,
                "/e/ich:1",
                SHOP,
                None,
                401,
                HTML,
                UNAUTHORIZED_PAGE,
            ),
            (Method::GET, "/", unknown, with, 404, TEXT, not_found),
            (
                Method::GET,
                "/public/key",
                unknown,
                None,
                404,
                TEXT,
                not_found,
            ),
            (Method::GET, HEALTHZ_PATH, SHOP, None, 200, TEXT, "ok\n"),
            (Method::HEAD, HEALTHZ_PATH, unknown, None, 200, TEXT, "ok\n"),
            (Method::PUT, HEALTHZ_PATH, SHOP, with, 404, TEXT, not_found),
            (Method::GET, "/.fluxbee/", SHOP, with, 404, TEXT, not_found),
            (
                Method::GET,
                "/.fluxbee/other",
                SHOP,
                with,
                404,
                TEXT,
                not_found,
            ),
            (
                Method::GET,
                ENTER_PATH,
                SHOP,
                None,
                401,
                HTML,
                UNAUTHORIZED_PAGE,
            ),
        ] {
            let case = format!("{method} {path} on {host}");
            let response = site
                .respond(request(method, path, host, cookie), BOOT + 20)
                .await;
            assert_eq!(response.status().as_u16(), code, "{case}");
            assert_eq!(response.headers()[CONTENT_TYPE], content_type, "{case}");
            assert_eq!(
                response.headers()["content-security-policy"],
                own_policy.as_str(),
                "{case}"
            );
            for (name, value) in SECURITY_HEADERS {
                assert_eq!(response.headers()[name], value, "{case}: {name}");
            }
            assert_eq!(body(response).await, body_text.as_bytes(), "{case}");
        }

        // A valid session reaches the upstream, on any path and with any method (a write with no
        // `Origin` and no `Sec-Fetch-Site` passes the Origin check): the upstream's answer, with
        // the application's headers.
        let reached = site.upstream.requests().len();
        for (method, path) in [
            (Method::GET, "/"),
            (Method::HEAD, "/"),
            (Method::GET, "/index.html"),
            (Method::POST, "/"),
        ] {
            let case = format!("{method} {path} with a session");
            let head = method == Method::HEAD;
            let response = site
                .respond(request(method, path, SHOP, with), BOOT + 20)
                .await;
            assert_eq!(response.status(), StatusCode::OK, "{case}");
            let headers = response.headers();
            assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8", "{case}");
            assert_eq!(headers["x-content-type-options"], "nosniff", "{case}");
            assert_eq!(
                headers["content-security-policy"], "frame-ancestors 'self'",
                "{case}"
            );
            for strict in [
                "referrer-policy",
                "cache-control",
                "x-robots-tag",
                "permissions-policy",
            ] {
                assert!(!headers.contains_key(strict), "{case}: {strict}");
            }
            let expected: &[u8] = if head { b"" } else { UPSTREAM_PAGE.as_bytes() };
            assert_eq!(body(response).await, expected, "{case}");
        }
        let seen = site.upstream.requests();
        assert_eq!(seen.len(), reached + 4, "each one reached the upstream");
        assert_eq!(
            seen[reached..]
                .iter()
                .map(|seen| (seen.method.clone(), seen.path.as_str()))
                .collect::<Vec<_>>(),
            [
                (Method::GET, "/"),
                (Method::HEAD, "/"),
                (Method::GET, "/index.html"),
                (Method::POST, "/"),
            ]
        );
        remove(site.exposures());
    }

    /// The request's host is read as the edge forwards it, in its canonical form; two hosts or
    /// none give no host.
    #[test]
    fn the_host_is_the_one_the_edge_forwards() {
        let host_of = |values: &[&str]| {
            let mut headers = HeaderMap::new();
            for value in values {
                headers.append(HOST, HeaderValue::from_str(value).unwrap());
            }
            request_host(&headers)
        };
        for value in [
            SHOP,
            "SHOP.hive-k3m9x7q2.fluxbee.ai",
            "shop.hive-k3m9x7q2.fluxbee.ai.:443",
        ] {
            assert_eq!(host_of(&[value]).as_deref(), Some(SHOP), "{value}");
        }
        assert_eq!(
            host_of(&["shop.hive-k3m9x7q2.fluxbee.ai:8443"]).as_deref(),
            Some("shop.hive-k3m9x7q2.fluxbee.ai:8443"),
            "another port is another host, which no exposure has"
        );
        assert_eq!(host_of(&[SHOP, ERP]), None);
        assert_eq!(host_of(&[]), None);
        assert_eq!(host_of(&[""]), None);
    }

    /// io.web's own pages render under their own policy, which loads nothing, and name no version
    /// and nothing internal.
    #[test]
    fn the_pages_load_nothing_and_name_nothing_internal() {
        for page in [
            ENTER_PAGE,
            UNAUTHORIZED_PAGE,
            FORBIDDEN_PAGE,
            BAD_GATEWAY_PAGE,
        ] {
            let page = page.to_ascii_lowercase();
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
                "upstream",
                "9200",
                "exp:",
                "enb:",
            ] {
                assert!(!page.contains(internal), "{internal}");
            }
        }
        assert!(ENTER_PAGE.contains(r#"<meta http-equiv="refresh" content="0; url=/">"#));
        let policy = Framing::default().own_policy;
        let policy = policy.to_str().unwrap();
        for directive in [
            "default-src 'none'",
            "base-uri 'none'",
            "form-action 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(policy.contains(directive), "{directive}");
        }
    }

    /// Over the wire: a link redeemed, then the upstream's page with the session's cookie, with
    /// the application's headers; HEAD reaches the upstream too and sends no body, so the
    /// connection stays in step for the next request.
    #[tokio::test]
    async fn the_page_goes_out_with_its_headers_and_head_sends_no_body() {
        let now = unix_now();
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        let shop = exposure(SHOP_ID, SHOP, vec![enablement(SHOP_ENB, now + 3600, A, 4)]);
        let site = Site::new(holding("wire", keys, now - 10, set(1, vec![shop])).await).await;
        let code = link(site.exposures(), SHOP_ID, SHOP_ENB, now);
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let (listen, _) =
            start_serving(&ca, &registry, testing::limits(), Arc::clone(&site.proxy)).await;
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();

        let target = format!("{ENTER_PATH}?code={code}");
        let (status, headers, body) = exchange_as(&mut ingress, "GET", &target, SHOP, None)
            .await
            .unwrap();
        assert_eq!((status, body.as_slice()), (200, ENTER_PAGE.as_bytes()));
        for (name, value) in SECURITY_HEADERS {
            assert_eq!(header(&headers, name), value, "{name}");
        }
        let cookie = cookie_of(header(&headers, "set-cookie"));

        let (status, headers, body) = exchange_as(&mut ingress, "GET", "/", SHOP, Some(&cookie))
            .await
            .unwrap();
        assert_eq!((status, body.as_slice()), (200, UPSTREAM_PAGE.as_bytes()));
        assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");
        assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
        assert_eq!(
            header(&headers, "content-security-policy"),
            "frame-ancestors 'self'"
        );
        assert_eq!(header(&headers, "cache-control"), "", "no strict set");
        let (status, headers, _) = exchange_as(&mut ingress, "HEAD", "/", SHOP, Some(&cookie))
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(
            header(&headers, "content-length"),
            UPSTREAM_PAGE.len().to_string(),
            "the upstream's HEAD announces its GET's length"
        );
        let (status, _, body) = exchange_as(&mut ingress, "GET", HEALTHZ_PATH, SHOP, None)
            .await
            .unwrap();
        assert_eq!((status, body.as_slice()), (200, b"ok\n".as_slice()));
        let (status, _, body) = exchange_as(&mut ingress, "GET", "/", SHOP, None)
            .await
            .unwrap();
        assert_eq!(
            (status, body.as_slice()),
            (401, UNAUTHORIZED_PAGE.as_bytes())
        );
        let methods: Vec<Method> = site
            .upstream
            .requests()
            .into_iter()
            .map(|seen| seen.method)
            .collect();
        assert_eq!(methods, [Method::GET, Method::HEAD]);
        let _ = std::fs::remove_dir_all(registry.root());
        remove(site.exposures());
    }

    /// A link redeems once, on its host, into a session cookie with every attribute of §3.3 and a
    /// page that moves on to `/`; the cookie then opens the exposure.
    #[tokio::test]
    async fn a_link_redeems_once_and_sets_the_host_cookie() {
        let site = enabled("redeem", BOOT, UNTIL).await;
        let code = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 10);
        let entered = site.respond(enter(&code, SHOP), BOOT + 15).await;
        assert_eq!(entered.status(), StatusCode::OK);
        let session = set_cookie(&entered).expect("a session");
        let attributes: Vec<&str> = session.split("; ").collect();
        assert_eq!(
            attributes[1..],
            [
                "Path=/",
                "Secure",
                "HttpOnly",
                "SameSite=Strict",
                "Max-Age=1800"
            ]
        );
        assert!(
            !session.to_ascii_lowercase().contains("domain"),
            "host-only, as __Host- requires: {session}"
        );
        let token = token_of(attributes[0]);
        assert_eq!(
            (token.x, token.e, token.issued, token.active),
            (
                hex_id(SHOP_ID).as_str(),
                hex_id(SHOP_ENB).as_str(),
                BOOT + 15,
                BOOT + 15
            )
        );
        assert_eq!(entered.headers()["cache-control"], "no-store");
        assert_eq!(entered.headers()["referrer-policy"], "no-referrer");
        assert_eq!(entered.headers()[CONTENT_TYPE], HTML);
        assert_eq!(body(entered).await, ENTER_PAGE.as_bytes());

        let served = site.respond(page(SHOP, attributes[0]), BOOT + 16).await;
        assert_upstream_page(served, "the cookie opens the exposure").await;

        assert_unauthorized(
            site.respond(enter(&code, SHOP), BOOT + 20).await,
            false,
            "twice",
        )
        .await;
        let view = site.exposures().view(BOOT + 20);
        assert_eq!(view["links_issued"], 1);
        assert_eq!(view["links_redeemed"], 1);
        assert_eq!(view["links_refused"], 1);
        assert_eq!(view["sessions_minted"], 1);
        remove(site.exposures());
    }

    /// The exposure is in the code and the host in its MAC: a link fails on another exposure's
    /// host, on a host that is no exposure's, and as a code minted for another host; it stays good
    /// on its own host.
    #[tokio::test]
    async fn a_link_is_refused_on_another_host() {
        let site = enabled("host", BOOT, UNTIL).await;
        let code = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 10);
        for host in [
            ERP,
            "nowhere.hive-k3m9x7q2.fluxbee.ai",
            "hive-k3m9x7q2.fluxbee.ai",
        ] {
            assert_unauthorized(
                site.respond(enter(&code, host), BOOT + 11).await,
                false,
                host,
            )
            .await;
        }
        // ERP's own code, signed with ERP's key but for another host, fails on ERP's host.
        let erp_keys = EnablementKeys::derive(&[2; 32]);
        let (x, e) = (hex_id(ERP_ID), hex_id(ERP_ENB));
        let elsewhere = LinkCode::mint(&erp_keys, SHOP, &x, &e, &"ab".repeat(16), BOOT + 10);
        assert_unauthorized(
            site.respond(enter(&elsewhere, ERP), BOOT + 11).await,
            false,
            "minted for another host",
        )
        .await;
        let its_own = LinkCode::mint(&erp_keys, ERP, &x, &e, &"cd".repeat(16), BOOT + 10);
        assert_eq!(
            site.respond(enter(&its_own, ERP), BOOT + 11).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            site.respond(enter(&code, SHOP), BOOT + 12).await.status(),
            StatusCode::OK,
            "the refusals spent nothing"
        );
        remove(site.exposures());
    }

    /// A link redeems up to 120 s after it was minted, and not a second later (§3.2).
    #[tokio::test]
    async fn a_link_is_refused_after_120_s() {
        let site = enabled("late", BOOT, UNTIL).await;
        let late = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 10);
        assert_unauthorized(
            site.respond(enter(&late, SHOP), BOOT + 10 + 121).await,
            false,
            "121 s",
        )
        .await;
        let in_time = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 10);
        assert_eq!(
            site.respond(enter(&in_time, SHOP), BOOT + 10 + 120)
                .await
                .status(),
            StatusCode::OK
        );
        remove(site.exposures());
    }

    /// A code changed anywhere, signed with another key, or malformed fails, as do two codes or
    /// none; the genuine one still redeems.
    #[tokio::test]
    async fn a_link_is_refused_tampered() {
        let site = enabled("tampered", BOOT, UNTIL).await;
        let code = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 10);
        let fields: Vec<&str> = code.split('.').collect();
        let with = |index: usize, value: &str| {
            let mut changed = fields.clone();
            changed[index] = value;
            changed.join(".")
        };
        let flipped = |text: &str| {
            let mut bytes = text.as_bytes().to_vec();
            let last = bytes.last_mut().unwrap();
            *last = if *last == b'0' { b'1' } else { b'0' };
            String::from_utf8(bytes).unwrap()
        };
        let iat: u64 = fields[4].parse().unwrap();
        let other_key = EnablementKeys::derive(&[9; 32]);
        for (case, tampered) in [
            ("tag", with(5, &flipped(fields[5]))),
            ("jti", with(3, &flipped(fields[3]))),
            ("iat", with(4, &(iat + 1).to_string())),
            ("enablement", with(2, &hex_id(ERP_ENB))),
            ("exposure", with(1, &hex_id(ERP_ID))),
            (
                "another key",
                LinkCode::mint(&other_key, SHOP, fields[1], fields[2], fields[3], iat),
            ),
            ("an upper-case tag", with(5, &fields[5].to_uppercase())),
            ("no tag", fields[..5].join(".")),
            ("a session's format", with(0, "S1")),
            ("empty", String::new()),
        ] {
            assert_unauthorized(
                site.respond(enter(&tampered, SHOP), BOOT + 11).await,
                false,
                case,
            )
            .await;
        }
        let twice = format!("{ENTER_PATH}?code={code}&code={code}");
        for (case, target) in [("two codes", twice.as_str()), ("no code", ENTER_PATH)] {
            let response = site
                .respond(request(Method::GET, target, SHOP, None), BOOT + 11)
                .await;
            assert_unauthorized(response, false, case).await;
        }
        assert_eq!(
            site.respond(enter(&code, SHOP), BOOT + 12).await.status(),
            StatusCode::OK
        );
        remove(site.exposures());
    }

    /// A link minted before io.web's boot is refused after it, one minted in the very second
    /// io.web started again included; one it mints since redeems (§3.2, §3.4). In the second it
    /// started io.web mints none: dated at its boot, such a link, once spent, would redeem again at
    /// a successor started within that same second, whose boot is the same.
    #[tokio::test]
    async fn a_link_is_refused_from_before_the_boot() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        let shop = exposure(SHOP_ID, SHOP, vec![enablement(SHOP_ENB, UNTIL, A, 4)]);
        let before = holding("boot", keys.clone(), BOOT, set(1, vec![shop])).await;
        let earlier = link(&before, SHOP_ID, SHOP_ENB, BOOT + 10);
        let last_second = link(&before, SHOP_ID, SHOP_ENB, BOOT + 20);

        // io.web starts again within BOOT + 20, and loads its set and its keys.
        let started = BOOT + 20;
        let file = before.file().to_path_buf();
        let after = Arc::new(Exposures::new(
            file.clone(),
            keys.clone(),
            boot_second(started),
        ));
        after.load_at_boot(started).await;
        let after = Site::new(after).await;
        for (case, code) in [("earlier", &earlier), ("in its last second", &last_second)] {
            let response = after.respond(enter(code, SHOP), started + 1).await;
            assert_unauthorized(response, false, case).await;
        }
        let request = IoWebAccessLinkRequest {
            exposure_id: SHOP_ID.to_string(),
            enablement_id: SHOP_ENB.to_string(),
            generation: 1,
        };
        assert!(
            matches!(
                after.exposures().issue_link(&request, started),
                IoWebAccessLinkResponse::Error { error_code, .. }
                    if error_code == IO_WEB_ERROR_KEY_UNAVAILABLE
            ),
            "no link in the second it started"
        );
        let fresh = link(after.exposures(), SHOP_ID, SHOP_ENB, started + 1);
        assert_eq!(
            after
                .respond(enter(&fresh, SHOP), started + 1)
                .await
                .status(),
            StatusCode::OK
        );

        // It stops within the second it minted that link, and its successor, started in it too,
        // refuses the code it spent.
        let successor = Arc::new(Exposures::new(file, keys, boot_second(started + 1)));
        successor.load_at_boot(started + 1).await;
        let successor = Site::new(successor).await;
        let again = successor.respond(enter(&fresh, SHOP), started + 1).await;
        assert_unauthorized(again, false, "spent before a restart within its second").await;
        remove(&before);
    }

    /// A link of an enablement that has ended is refused, within its 120 s too.
    #[tokio::test]
    async fn a_link_is_refused_of_an_ended_enablement() {
        let site = enabled("ended", BOOT, BOOT + 100).await;
        let code = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 90);
        assert_unauthorized(
            site.respond(enter(&code, SHOP), BOOT + 100).await,
            false,
            "ended",
        )
        .await;
        let other = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 90);
        assert_eq!(
            site.respond(enter(&other, SHOP), BOOT + 99).await.status(),
            StatusCode::OK
        );
        remove(site.exposures());
    }

    /// A HEAD never redeems, so a link's preview never spends it, nor does another method: the
    /// GET after them still redeems.
    #[tokio::test]
    async fn head_never_redeems() {
        let site = enabled("head", BOOT, UNTIL).await;
        let code = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 10);
        let target = format!("{ENTER_PATH}?code={code}");
        for method in [Method::HEAD, Method::POST] {
            let case = method.to_string();
            let response = site
                .respond(request(method, &target, SHOP, None), BOOT + 11)
                .await;
            assert_unauthorized(response, false, &case).await;
        }
        assert_eq!(site.exposures().view(BOOT + 11)["links_refused"], 0);
        assert_eq!(
            site.respond(enter(&code, SHOP), BOOT + 12).await.status(),
            StatusCode::OK
        );
        assert!(
            site.upstream.requests().is_empty(),
            "nothing under /.fluxbee/ reaches an upstream"
        );
        remove(site.exposures());
    }

    /// A session reaches the upstream on any path, its query passed on; once its last activity is
    /// 60 s old, the upstream's answer carries the cookie again with `active = now`, the same
    /// session, whose 30 min count from there.
    #[tokio::test]
    async fn a_session_serves_the_page_and_slides_after_60_s() {
        let site = enabled("slide", BOOT, UNTIL).await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let early = site
            .respond(
                request(Method::GET, "/any/path?x=1", SHOP, Some(&cookie)),
                BOOT + 69,
            )
            .await;
        assert_eq!(early.status(), StatusCode::OK);
        assert_eq!(set_cookie(&early), None, "59 s: no slide");
        assert_eq!(body(early).await, UPSTREAM_PAGE.as_bytes());
        let seen = site.upstream.requests();
        assert_eq!(
            (seen[0].path.as_str(), seen[0].query.as_deref()),
            ("/any/path", Some("x=1"))
        );

        let slid = site.respond(page(SHOP, &cookie), BOOT + 70).await;
        assert_eq!(slid.status(), StatusCode::OK);
        let renewed = set_cookie(&slid).expect("60 s: the cookie again");
        assert!(
            renewed.ends_with("; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=1800"),
            "{renewed}"
        );
        assert_eq!(body(slid).await, UPSTREAM_PAGE.as_bytes());
        let renewed = cookie_of(&renewed);
        let (old, new) = (token_of(&cookie), token_of(&renewed));
        assert_eq!(
            (new.sid, new.issued, new.active),
            (old.sid, old.issued, BOOT + 70)
        );

        assert_eq!(
            site.respond(page(SHOP, &renewed), BOOT + 70 + 1800)
                .await
                .status(),
            StatusCode::OK
        );
        assert_unauthorized(
            site.respond(page(SHOP, &cookie), BOOT + 10 + 1801).await,
            true,
            "the cookie before the slide",
        )
        .await;
        let head = request(Method::HEAD, "/", SHOP, Some(&renewed));
        assert_eq!(site.respond(head, BOOT + 80).await.status(), StatusCode::OK);
        remove(site.exposures());
    }

    /// 30 min without a request end a session: at 1800 s it still serves, at 1801 s it is refused
    /// and its cookie cleared.
    #[tokio::test]
    async fn a_session_ends_after_30_min_idle() {
        let site = enabled("idle", BOOT, UNTIL).await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        assert_eq!(
            site.respond(page(SHOP, &cookie), BOOT + 10 + 1800)
                .await
                .status(),
            StatusCode::OK
        );
        assert_unauthorized(
            site.respond(page(SHOP, &cookie), BOOT + 10 + 1801).await,
            true,
            "idle",
        )
        .await;
        remove(site.exposures());
    }

    /// A session ends with its enablement: its cookie's `Max-Age` stops there, and the first
    /// request at the end is refused.
    #[tokio::test]
    async fn a_session_ends_at_the_enablement_s_end() {
        let end = BOOT + 1000;
        let site = enabled("end", BOOT, end).await;
        let code = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 10);
        let entered = site.respond(enter(&code, SHOP), BOOT + 10).await;
        let session = set_cookie(&entered).unwrap();
        assert!(session.ends_with("; Max-Age=990"), "{session}");
        let cookie = cookie_of(&session);
        let last = site.respond(page(SHOP, &cookie), end - 1).await;
        assert_eq!(last.status(), StatusCode::OK);
        let slid = set_cookie(&last).unwrap();
        assert!(slid.ends_with("; Max-Age=1"), "{slid}");
        assert_unauthorized(
            site.respond(page(SHOP, &cookie), end).await,
            true,
            "the end",
        )
        .await;
        remove(site.exposures());
    }

    /// A session waits for its key (§3.4): io.web restarted and could not read the key yet, as
    /// with SY.vault or identity not ready, so the session gets the 401 page with its cookie kept,
    /// not cleared; once the retry reads the key, the same cookie reaches the upstream again.
    #[tokio::test]
    async fn a_session_waits_for_its_key_after_a_restart() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        let shop = exposure(SHOP_ID, SHOP, vec![enablement(SHOP_ENB, UNTIL, A, 4)]);
        let before =
            Site::new(holding("waits", keys.clone(), BOOT, set(1, vec![shop])).await).await;
        let cookie = before.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;

        let vault_key = web_exposure_vault_key(SHOP_ID, A).unwrap();
        keys.answer(&vault_key, Err("SY.vault is not ready".to_string()));
        let after = Arc::new(Exposures::new(
            before.exposures().file().to_path_buf(),
            keys.clone(),
            boot_second(BOOT + 100),
        ));
        after.load_at_boot(BOOT + 100).await;
        let after = Site::new(after).await;
        assert_eq!(after.exposures().view(BOOT + 100)["keys_unavailable"], 1);
        assert_unauthorized(
            after.respond(page(SHOP, &cookie), BOOT + 101).await,
            false,
            "its key not read yet",
        )
        .await;
        assert!(after.upstream.requests().is_empty());

        keys.put(SHOP_ID, A, 4, 1);
        after.exposures().retry(BOOT + 130).await;
        let served = after.respond(page(SHOP, &cookie), BOOT + 131).await;
        assert_upstream_page(served, "its key read").await;
        remove(before.exposures());
    }

    /// A session is its exposure's alone: on another exposure's host it is refused and cleared
    /// there, and still serves on its own.
    #[tokio::test]
    async fn a_session_of_one_exposure_fails_on_another() {
        let site = enabled("another", BOOT, UNTIL).await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        assert_unauthorized(
            site.respond(page(ERP, &cookie), BOOT + 11).await,
            true,
            "erp",
        )
        .await;
        assert_eq!(
            site.respond(page(SHOP, &cookie), BOOT + 12).await.status(),
            StatusCode::OK
        );
        remove(site.exposures());
    }

    /// A push that drops an exposure's enablement ends each of its sessions at their next
    /// request, and its pending link; the other exposure's session goes on.
    #[tokio::test]
    async fn a_disable_push_ends_every_session_at_once() {
        let site = enabled("disable", BOOT, UNTIL).await;
        let first = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let second = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 11).await;
        let other = site.sign_in(ERP_ID, ERP_ENB, ERP, BOOT + 12).await;
        let pending = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 13);

        let disabled = set(
            2,
            vec![
                exposure(SHOP_ID, SHOP, Vec::new()),
                exposure(ERP_ID, ERP, vec![enablement(ERP_ENB, UNTIL, A, 2)]),
            ],
        );
        let answer = site.exposures().apply_push(disabled, BOOT + 14).await;
        assert!(
            matches!(
                answer,
                IoWebExposuresResponse::Ok {
                    generation: 2,
                    changed: true,
                    ..
                }
            ),
            "{answer:?}"
        );
        for (case, cookie) in [("first", &first), ("second", &second)] {
            assert_unauthorized(
                site.respond(page(SHOP, cookie), BOOT + 14).await,
                true,
                case,
            )
            .await;
        }
        assert_unauthorized(
            site.respond(enter(&pending, SHOP), BOOT + 14).await,
            false,
            "the pending link",
        )
        .await;
        assert_eq!(
            site.respond(page(ERP, &other), BOOT + 14).await.status(),
            StatusCode::OK
        );
        remove(site.exposures());
    }

    /// More than one session cookie is refused and cleared, a valid one among them included;
    /// other cookies beside one are no matter.
    #[tokio::test]
    async fn more_than_one_session_cookie_is_refused() {
        let site = enabled("two", BOOT, UNTIL).await;
        let one = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let two = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 11).await;
        let both = format!("theme=dark; {one}; {two}");
        assert_unauthorized(
            site.respond(page(SHOP, &both), BOOT + 12).await,
            true,
            "two",
        )
        .await;
        let split = Request::builder()
            .uri("/")
            .header(HOST, SHOP)
            .header(COOKIE, one.as_str())
            .header(COOKIE, two.as_str())
            .body(())
            .unwrap();
        assert_unauthorized(site.respond(split, BOOT + 12).await, true, "two headers").await;
        let beside = format!("theme=dark; {one}; lang=en");
        assert_eq!(
            site.respond(page(SHOP, &beside), BOOT + 12).await.status(),
            StatusCode::OK
        );
        remove(site.exposures());
    }

    /// io.web's health check moved under the reserved prefix, on every host, whose other paths are
    /// 404; `/healthz` is now a path like any other, which a session guards.
    #[tokio::test]
    async fn healthz_moved_under_the_reserved_prefix() {
        let site = enabled("healthz", BOOT, UNTIL).await;
        let unknown = "nowhere.hive-k3m9x7q2.fluxbee.ai";
        for host in [SHOP, unknown, ""] {
            for method in [Method::GET, Method::HEAD] {
                let case = format!("{method} on '{host}'");
                let response = site
                    .respond(request(method, HEALTHZ_PATH, host, None), BOOT + 1)
                    .await;
                assert_eq!(response.status(), StatusCode::OK, "{case}");
                assert_eq!(body(response).await, b"ok\n".as_slice(), "{case}");
            }
        }
        let old = request(Method::GET, "/healthz", SHOP, None);
        assert_unauthorized(site.respond(old, BOOT + 1).await, false, "/healthz").await;
        let old = request(Method::GET, "/healthz", unknown, None);
        assert_eq!(
            site.respond(old, BOOT + 1).await.status(),
            StatusCode::NOT_FOUND
        );
        for path in ["/.fluxbee/", "/.fluxbee/healthz/", "/.fluxbee/admin"] {
            let response = site
                .respond(request(Method::GET, path, SHOP, None), BOOT + 1)
                .await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        remove(site.exposures());
    }

    /// §13.7's table, Fluxbee's column (stage 2.2 §4.5): every one of io.web's own answers, the
    /// 401, the 403, the 502, the link's page, a 404, carries the strict set, `Permissions-Policy`
    /// and a `frame-ancestors` naming Cloud's origins, the ones that pass io.web's check; `'none'`
    /// without one.
    #[tokio::test]
    async fn io_web_s_own_answers_frame_only_for_cloud_s_origins() {
        let site = enabled_with(
            "framed",
            BOOT,
            UNTIL,
            &[CLOUD, "https://fluxbee.ai:8443", "https://x'y"],
        )
        .await;
        let framed = format!(
            "{OWN_POLICY}; frame-ancestors https://cloud.fluxbee.ai https://fluxbee.ai:8443"
        );
        let session = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let code = link(site.exposures(), SHOP_ID, SHOP_ENB, BOOT + 10);
        let mut sibling = request(Method::POST, "/", SHOP, Some(&session));
        sibling.headers_mut().insert(
            ORIGIN,
            HeaderValue::from_static("https://other.hive-k3m9x7q2.fluxbee.ai"),
        );
        let cases = [
            ("401", request(Method::GET, "/", SHOP, None), 401),
            ("403", sibling, 403),
            ("404", request(Method::GET, "/.fluxbee/x", SHOP, None), 404),
            ("enter", enter(&code, SHOP), 200),
            ("healthz", request(Method::GET, HEALTHZ_PATH, "", None), 200),
        ];
        for (case, request, status) in cases {
            let response = site.respond(request, BOOT + 11).await;
            assert_eq!(response.status().as_u16(), status, "{case}");
            assert_eq!(
                response.headers()["content-security-policy"],
                framed.as_str(),
                "{case}"
            );
            assert_eq!(
                response.headers()["permissions-policy"],
                PERMISSIONS_POLICY,
                "{case}"
            );
            for (name, value) in SECURITY_HEADERS {
                assert_eq!(response.headers()[name], value, "{case}: {name}");
            }
        }
        // The 502: an upstream that is not there.
        let gone = proxy(Arc::clone(&site.proxy.exposures), closed_port());
        let failed = answer_to(&gone, without_body(page(SHOP, &session)), BOOT + 11).await;
        assert_eq!(failed.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(failed.headers()["content-security-policy"], framed.as_str());
        assert_eq!(body(failed).await, BAD_GATEWAY_PAGE.as_bytes());
        // The application's own answer frames for the same origins, and its own pages.
        let proxied = site.respond(page(SHOP, &session), BOOT + 11).await;
        assert_eq!(
            proxied.headers()["content-security-policy"],
            "frame-ancestors 'self' https://cloud.fluxbee.ai https://fluxbee.ai:8443"
        );
        assert!(!proxied.headers().contains_key("permissions-policy"));
        assert_eq!(
            site.exposures().view(BOOT + 11)["cloud_origins"],
            json!([CLOUD, "https://fluxbee.ai:8443", "https://x'y"]),
            "the status shows the origins as SY.admin pushed them"
        );
        remove(site.exposures());
    }

    /// A write with a valid session, as the method and its fetch metadata come.
    fn write(method: Method, session: &str, headers: &[(&str, &str)]) -> Request<()> {
        let mut request = request(method, "/", SHOP, Some(session));
        for (name, value) in headers {
            request.headers_mut().append(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        request
    }

    /// The Origin check (stage 2.2 §4.1), each row of its algorithm refused: a write or a
    /// WebSocket's handshake whose `Sec-Fetch-Site` is not same-origin or none; without it, whose
    /// `Origin` is not the exposure's own, `null` included. The fixed 403 page, nothing forwarded,
    /// and the refusal counted.
    #[tokio::test]
    async fn a_write_from_another_origin_is_refused_403() {
        let site = enabled("origin-refused", BOOT, UNTIL).await;
        let session = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let sibling = "https://other.hive-k3m9x7q2.fluxbee.ai";
        let websocket = [("connection", "Upgrade"), ("upgrade", "websocket")];
        let mut refused = Vec::new();
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            for metadata in [
                vec![("sec-fetch-site", "same-site")],
                vec![("sec-fetch-site", "cross-site")],
                vec![("sec-fetch-site", "same-site"), ("origin", SHOP_ORIGIN)],
                vec![
                    ("sec-fetch-site", "same-origin"),
                    ("sec-fetch-site", "same-site"),
                ],
                vec![("origin", sibling)],
                vec![("origin", "null")],
                vec![("origin", "http://shop.hive-k3m9x7q2.fluxbee.ai")],
                vec![("origin", "https://shop.hive-k3m9x7q2.fluxbee.ai:8443")],
            ] {
                refused.push((method.clone(), metadata));
            }
        }
        refused.push((
            Method::GET,
            [&websocket[..], &[("origin", sibling)]].concat(),
        ));
        refused.push((
            Method::GET,
            [&websocket[..], &[("sec-fetch-site", "same-site")]].concat(),
        ));
        refused.push((
            Method::from_bytes(b"PROPFIND").unwrap(),
            vec![("origin", sibling)],
        ));
        let count = refused.len();
        for (method, metadata) in refused {
            let case = format!("{method} {metadata:?}");
            let response = site
                .respond(write(method, &session, &metadata), BOOT + 11)
                .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{case}");
            assert_eq!(set_cookie(&response), None, "{case}");
            assert_eq!(body(response).await, FORBIDDEN_PAGE.as_bytes(), "{case}");
        }
        assert!(
            site.upstream.requests().is_empty(),
            "nothing refused reaches the upstream"
        );
        let view = site.exposures().view(BOOT + 11);
        let shop = &view["per_exposure"][0];
        assert_eq!(shop["exposure_id"], SHOP_ID);
        assert_eq!(shop["refused"]["403"], count);
        assert_eq!(
            shop["open"],
            json!({"waiting": 0, "streams": 0, "websockets": 0}),
            "a refused request stays registered nowhere"
        );
        remove(site.exposures());
    }

    const SHOP_ORIGIN: &str = "https://shop.hive-k3m9x7q2.fluxbee.ai";

    /// What the Origin check lets through: a write from the exposure's own page (`Sec-Fetch-Site`
    /// same-origin or none, or without it its own `Origin`), a write with neither header, and every
    /// read, GET, HEAD and OPTIONS, whatever page it comes from.
    #[tokio::test]
    async fn a_write_from_the_exposure_s_page_passes() {
        let site = enabled("origin-passes", BOOT, UNTIL).await;
        let session = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let sibling = "https://other.hive-k3m9x7q2.fluxbee.ai";
        let cases: Vec<(Method, Vec<(&str, &str)>)> = vec![
            (Method::POST, vec![("sec-fetch-site", "same-origin")]),
            (Method::POST, vec![("sec-fetch-site", "none")]),
            (
                Method::POST,
                vec![("sec-fetch-site", "same-origin"), ("origin", sibling)],
            ),
            (Method::PUT, vec![("origin", SHOP_ORIGIN)]),
            (Method::DELETE, vec![]),
            (Method::PATCH, vec![("sec-fetch-site", "same-origin")]),
            (
                Method::GET,
                vec![("sec-fetch-site", "cross-site"), ("origin", sibling)],
            ),
            (Method::HEAD, vec![("origin", sibling)]),
            (
                Method::OPTIONS,
                vec![("origin", sibling), ("sec-fetch-site", "same-site")],
            ),
            (
                Method::GET,
                vec![
                    ("connection", "Upgrade"),
                    ("upgrade", "websocket"),
                    ("sec-fetch-site", "same-origin"),
                ],
            ),
            (
                Method::GET,
                vec![
                    ("connection", "Upgrade"),
                    ("upgrade", "websocket"),
                    ("origin", SHOP_ORIGIN),
                ],
            ),
        ];
        let count = cases.len();
        for (method, metadata) in cases {
            let case = format!("{method} {metadata:?}");
            let response = site
                .respond(write(method, &session, &metadata), BOOT + 11)
                .await;
            assert_eq!(response.status(), StatusCode::OK, "{case}");
        }
        assert_eq!(site.upstream.requests().len(), count);
        assert_eq!(
            site.exposures().view(BOOT + 11)["per_exposure"][0]["refused"]["403"],
            0
        );
        remove(site.exposures());
    }

    /// `CONNECT` and `TRACE` reach no upstream, a session or not: 405, as the edge answers them
    /// first.
    #[tokio::test]
    async fn connect_and_trace_reach_no_upstream() {
        let site = enabled("tunnel-methods", BOOT, UNTIL).await;
        let session = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        for method in [Method::CONNECT, Method::TRACE] {
            let target = if method == Method::CONNECT {
                "shop.hive-k3m9x7q2.fluxbee.ai:443"
            } else {
                "/"
            };
            let response = site
                .respond(
                    request(method.clone(), target, SHOP, Some(&session)),
                    BOOT + 11,
                )
                .await;
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method}"
            );
            assert_eq!(response.headers()[ALLOW], ALLOWED_METHODS, "{method}");
        }
        assert!(site.upstream.requests().is_empty());
        remove(site.exposures());
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
