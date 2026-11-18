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
//! - **What:** HTTP/1.1 only, with a head-read timeout. Each request is answered by its host, its
//!   session cookie and its path (`respond`, docs/io-web-v2-stage-2-1.md §5.5); its body is never
//!   read.
//! - **When:** once its config is in use and its exposures are loaded, the file's set and its keys
//!   (`run`).
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
use fluxbee_sdk::peer_slots::{PeerSlot, PeerSlots};
use fluxbee_sdk::protocol::PRIMARY_HIVE_ID;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE, COOKIE, HOST, SET_COOKIE};
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
use crate::exposures::{unix_now, Exposures, RequestTrace, SessionRefusal, ENTER_PATH};

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
    /// A TLS handshake must finish within this: one on the internal network takes milliseconds.
    pub handshake_timeout: Duration,
    /// Every request head must arrive within this, timed from the handshake or from the previous
    /// answer: one byte, or an h2 preface, never holds a connection longer, and a keep-alive
    /// connection idle that long is closed (hyper's head timer, as on the edge's door, A-87).
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
    exposures: Arc<Exposures>,
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
    serve(listener, acceptor, &setup, &status, exposures).await;
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

/// The accept loop: one task per connection, at most `max_handshakes` of them in their handshake
/// (`max_handshakes_per_source` from one address) and `max_connections` past it, and the registry
/// checked every `registry_recheck` for the state (the first time at once).
async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    setup: &WebSetup,
    status: &WebStatus,
    exposures: Arc<Exposures>,
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
                    Arc::clone(&exposures),
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

/// One connection: the handshake under its timeout, holding its handshake slots; a connection slot
/// from then on, to the end; the registry once more; then HTTP, until the peer or the head timer
/// ends it, or until its hive is no longer a connected ingress.
async fn connection(
    tcp: TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
    registry: HiveRegistry,
    limits: WebLimits,
    handshake: (OwnedSemaphorePermit, PeerSlot),
    connections: Arc<Semaphore>,
    exposures: Arc<Exposures>,
) {
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
        served = serve_http(tls, hive.clone(), limits.header_read_timeout, exposures) => {
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
    exposures: Arc<Exposures>,
) -> Result<(), hyper::Error>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |req: Request<Incoming>| {
        let response = respond(&exposures, &req, unix_now());
        // The path without its query, so neither a link's code nor any query reaches the journal
        // (docs/io-web-v2.md §11.3); never a cookie. The request id is the edge's.
        tracing::debug!(
            hive = %hive,
            method = %req.method(),
            host = request_host(req.headers()).unwrap_or_default(),
            path = req.uri().path(),
            status = response.status().as_u16(),
            request_id = header_text(req.headers(), "x-request-id"),
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

/// The placeholder page a valid session gets, on any path, where stage 2.2 reaches the exposure's
/// upstream (docs/io-web-v2-stage-2-1.md §3.3): text only, so it renders under its own policy,
/// which loads nothing; and nothing that names a version or anything internal.
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

/// The page a redeemed link answers with, beside the session's cookie (§3.2). It moves on to `/`
/// at once with a refresh, not a 303: a `SameSite=Strict` cookie is withheld from a redirect inside
/// a navigation that began on another site (Cloud's), and is sent on a navigation this page starts.
const ENTER_PAGE: &str = r#"<!doctype html>
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
/// the log alone holds (§3.2, §3.3).
const UNAUTHORIZED_PAGE: &str = r#"<!doctype html>
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

/// The paths io.web keeps for itself on every host (§5.5): no session needed, and never an
/// upstream's in stage 2.2.
const RESERVED_PREFIX: &str = "/.fluxbee/";
/// io.web's health check, under the reserved prefix so that it never shadows an upstream's own.
const HEALTHZ_PATH: &str = "/.fluxbee/healthz";

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
const HTML: &str = "text/html; charset=utf-8";

/// io.web's answer to one request at `now`, from its host, its session cookie and its path
/// (docs/io-web-v2-stage-2-1.md §3, §5.5). Its body is never read: a 401 comes from the headers
/// alone. HEAD gets GET's answer, whose body hyper does not send.
///
/// - `/.fluxbee/` is reserved on every host: `GET /.fluxbee/enter` redeems a link, any other method
///   there answers 401 and never redeems; `/.fluxbee/healthz` is 200; any other is 404.
/// - A host that is no exposure's is 404.
/// - Any other path needs a valid session: then GET and HEAD get the placeholder page, with the
///   cookie again once its last activity is 60 s old, and another method 404. Without one, the
///   fixed 401 page, which clears a session cookie that is not valid; more than one is not valid.
///   A cookie whose enablement is live but whose key io.web has not read yet is kept (§3.4).
///
/// Every answer carries `SECURITY_HEADERS`.
fn respond<B>(exposures: &Exposures, request: &Request<B>, now: u64) -> Response<Full<Bytes>> {
    let method = request.method();
    let readable = *method == Method::GET || *method == Method::HEAD;
    let headers = request.headers();
    let host = request_host(headers).unwrap_or_default();
    match request.uri().path() {
        HEALTHZ_PATH if readable => answer(StatusCode::OK, TEXT, "ok\n", None),
        ENTER_PATH if *method == Method::GET => {
            let trace = RequestTrace {
                request_id: header_text(headers, "x-request-id"),
                forwarded_for: header_text(headers, "x-forwarded-for"),
            };
            match exposures.redeem(&host, link_code(request.uri().query()), now, trace) {
                Some(set_cookie) => answer(StatusCode::OK, HTML, ENTER_PAGE, Some(set_cookie)),
                None => unauthorized(None),
            }
        }
        // A HEAD never redeems, nor any other method.
        ENTER_PATH => unauthorized(None),
        path if path.starts_with(RESERVED_PREFIX) => not_found(),
        _ if !exposures.knows_host(&host) => not_found(),
        _ => match session_cookie(headers) {
            SessionCookie::Absent => unauthorized(None),
            SessionCookie::Several => refused_session(
                &host,
                &SessionRefusal {
                    reason: "more than one session cookie",
                    exposure_id: exposures.exposure_of(&host),
                    ..SessionRefusal::default()
                },
            ),
            SessionCookie::One(value) => match exposures.session(&host, value, now) {
                Ok(slid) if readable => answer(StatusCode::OK, HTML, PAGE, slid),
                Ok(_) => not_found(),
                Err(refusal) => refused_session(&host, &refusal),
            },
        },
    }
}

/// The fixed 401 page for a session cookie that does not serve, logged with its exposure, its
/// host and why (stage 2.1 §5.10). It clears the cookie, unless the cookie may serve once io.web
/// reads its enablement's key (`SessionRefusal::kept`, §3.4).
fn refused_session(host: &str, refusal: &SessionRefusal) -> Response<Full<Bytes>> {
    tracing::debug!(
        exposure_id = refusal.exposure_id.as_deref().unwrap_or_default(),
        host = %host,
        enablement_id = refusal.enablement_id.as_deref().unwrap_or_default(),
        reason = refusal.reason,
        "IO.web refused a session"
    );
    unauthorized((!refusal.kept).then(session_clear_cookie))
}

/// An answer with `SECURITY_HEADERS`, and `set_cookie` when there is one.
fn answer(
    status: StatusCode,
    content_type: &'static str,
    body: &'static str,
    set_cookie: Option<String>,
) -> Response<Full<Bytes>> {
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
    // A cookie's value is hex, digits and dots, and its attributes ASCII: always a valid value.
    if let Some(cookie) = set_cookie.and_then(|cookie| HeaderValue::try_from(cookie).ok()) {
        headers.insert(SET_COOKIE, cookie);
    }
    response
}

fn not_found() -> Response<Full<Bytes>> {
    answer(StatusCode::NOT_FOUND, TEXT, "not found\n", None)
}

fn unauthorized(set_cookie: Option<String>) -> Response<Full<Bytes>> {
    answer(
        StatusCode::UNAUTHORIZED,
        HTML,
        UNAUTHORIZED_PAGE,
        set_cookie,
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
    use crate::access::{EnablementKeys, LinkCode, SessionToken};
    use crate::exposures::testing::*;
    use crate::exposures::{boot_second, Exposures};
    use fluxbee_sdk::mesh::{MeshCa, IO_WEB_SERVER_NAME};
    use fluxbee_sdk::protocol::{
        IoWebAccessLinkRequest, IoWebAccessLinkResponse, IoWebExposuresResponse,
        WebExposureKeySlot, IO_WEB_ERROR_KEY_UNAVAILABLE,
    };
    use fluxbee_sdk::vault::web_exposure_vault_key;
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::timeout;
    use tokio_rustls::client::TlsStream;
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
    use tokio_rustls::rustls::{self, AlertDescription, ClientConfig, RootCertStore};
    use tokio_rustls::TlsConnector;

    const BOOT: u64 = 1_763_121_000;
    const UNTIL: u64 = BOOT + 8 * 3600;
    const A: WebExposureKeySlot = WebExposureKeySlot::A;

    /// io.web holding no exposure, which never writes its file.
    fn no_exposures() -> Arc<Exposures> {
        Arc::new(Exposures::new(
            PathBuf::from("never-written/exposures.json"),
            MapKeys::new(),
            0,
        ))
    }

    /// io.web booted at `boot`, holding `shop` and `erp`, each enabled until `UNTIL` (or `shop`
    /// until `shop_until`) with its key.
    async fn enabled(label: &str, boot: u64, shop_until: u64) -> Arc<Exposures> {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        keys.put(ERP_ID, A, 2, 2);
        holding(
            label,
            keys,
            boot,
            set(
                1,
                vec![
                    exposure(SHOP_ID, SHOP, vec![enablement(SHOP_ENB, shop_until, A, 4)]),
                    exposure(ERP_ID, ERP, vec![enablement(ERP_ENB, UNTIL, A, 2)]),
                ],
            ),
        )
        .await
    }

    /// A request as the edge forwards it: its host, its request id and the client's address.
    fn request(method: Method, target: &str, host: &str, cookie: Option<&str>) -> Request<()> {
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

    fn enter(code: &str, host: &str) -> Request<()> {
        request(
            Method::GET,
            &format!("{ENTER_PATH}?code={code}"),
            host,
            None,
        )
    }

    fn page(host: &str, cookie: &str) -> Request<()> {
        request(Method::GET, "/", host, Some(cookie))
    }

    fn set_cookie(response: &Response<Full<Bytes>>) -> Option<String> {
        response
            .headers()
            .get(SET_COOKIE)
            .map(|value| value.to_str().unwrap().to_string())
    }

    /// The `Cookie` a browser sends back for a `Set-Cookie`.
    fn cookie_of(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap().to_string()
    }

    /// The session a `Cookie` carries.
    fn token_of(cookie: &str) -> SessionToken<'_> {
        SessionToken::parse(cookie.strip_prefix("__Host-fluxbee_session=").unwrap()).unwrap()
    }

    async fn body(response: Response<Full<Bytes>>) -> Bytes {
        response.into_body().collect().await.unwrap().to_bytes()
    }

    /// A session on `host`, through a link minted and redeemed at `now`: its `Cookie`.
    fn sign_in(
        exposures: &Exposures,
        exposure_id: &str,
        enablement_id: &str,
        host: &str,
        now: u64,
    ) -> String {
        let code = link(exposures, exposure_id, enablement_id, now);
        let entered = respond(exposures, &enter(&code, host), now);
        assert_eq!(entered.status(), StatusCode::OK);
        cookie_of(&set_cookie(&entered).expect("a session"))
    }

    /// 401 with the fixed page, and a cookie cleared when `clears`.
    async fn assert_unauthorized(response: Response<Full<Bytes>>, clears: bool, case: &str) {
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{case}");
        assert_eq!(
            set_cookie(&response),
            clears.then(session_clear_cookie),
            "{case}"
        );
        assert_eq!(body(response).await, UNAUTHORIZED_PAGE.as_bytes(), "{case}");
    }

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

    /// io.web's accept loop on a port of its own, over the motherbee leaf `ca` issues, holding no
    /// exposure.
    async fn start(
        ca: &MeshCa,
        registry: &HiveRegistry,
        limits: WebLimits,
    ) -> (SocketAddr, Arc<WebStatus>) {
        start_serving(ca, registry, limits, no_exposures()).await
    }

    /// The same, holding `exposures`.
    async fn start_serving(
        ca: &MeshCa,
        registry: &HiveRegistry,
        limits: WebLimits,
        exposures: Arc<Exposures>,
    ) -> (SocketAddr, Arc<WebStatus>) {
        install_crypto_provider();
        let acceptor = acceptor(&material(ca, PRIMARY_HIVE_ID), registry).unwrap();
        start_with(acceptor, registry, limits, exposures).await
    }

    /// io.web's accept loop on a port of its own, over `acceptor`.
    async fn start_with(
        acceptor: TlsAcceptor,
        registry: &HiveRegistry,
        limits: WebLimits,
        exposures: Arc<Exposures>,
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
        tokio::spawn(async move { serve(listener, acceptor, &setup, &served, exposures).await });
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

    /// One request on `archi.example.com` and its answer.
    async fn exchange(
        stream: &mut TlsStream<TcpStream>,
        method: &str,
        path: &str,
    ) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
        exchange_as(stream, method, path, "archi.example.com", None).await
    }

    /// One request on `host`, with `cookie` when there is one, and its answer: the status, the
    /// headers (lowercase names) and the body.
    async fn exchange_as(
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
            Ok(mut stream) => exchange(&mut stream, "GET", HEALTHZ_PATH)
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
        let (listen, _) = start_with(ca_only, &registry, limits(), no_exposures()).await;

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
        let (listen, _) = start(&ca, &registry, limits()).await;
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();
        let (code, _, _) = exchange(&mut ingress, "GET", HEALTHZ_PATH).await.unwrap();
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

    /// A-89: past the handshakes one address may hold, its next connection is closed at accept,
    /// before any handshake; a silent connection holds its slot only for the handshake timeout.
    #[tokio::test]
    async fn past_its_handshake_cap_a_new_connection_is_closed_at_once() {
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let limits = WebLimits {
            max_handshakes_per_source: 1,
            handshake_timeout: Duration::from_millis(500),
            ..limits()
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
            ..limits()
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
            limits: limits(),
        };
        let status = Arc::new(WebStatus::new());
        let (_configured, configured_rx) = watch::channel(true);
        let (_loaded, loaded_rx) = watch::channel(true);
        let task = tokio::spawn(run(
            setup,
            Arc::clone(&status),
            configured_rx,
            loaded_rx,
            no_exposures(),
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
            limits: limits(),
        };
        let status = Arc::new(WebStatus::new());
        let (configured, configured_rx) = watch::channel(false);
        let (loaded, loaded_rx) = watch::channel(false);
        let task = tokio::spawn(run(
            setup,
            Arc::clone(&status),
            configured_rx,
            loaded_rx,
            no_exposures(),
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

    /// Every route by host, cookie and path, each answer with the security headers.
    #[tokio::test]
    async fn each_route_answers_with_the_security_headers() {
        let exposures = enabled("routes", BOOT, UNTIL).await;
        let session = sign_in(&exposures, SHOP_ID, SHOP_ENB, SHOP, BOOT + 10);
        let with = Some(session.as_str());
        let unknown = "nowhere.hive-k3m9x7q2.fluxbee.ai";
        let not_found = "not found\n";
        for (method, path, host, cookie, code, content_type, body_text) in [
            (Method::GET, "/", SHOP, with, 200, HTML, PAGE),
            (Method::HEAD, "/", SHOP, with, 200, HTML, PAGE),
            (Method::GET, "/index.html", SHOP, with, 200, HTML, PAGE),
            (Method::POST, "/", SHOP, with, 404, TEXT, not_found),
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
            let response = respond(&exposures, &request(method, path, host, cookie), BOOT + 20);
            assert_eq!(response.status().as_u16(), code, "{case}");
            assert_eq!(response.headers()[CONTENT_TYPE], content_type, "{case}");
            for (name, value) in SECURITY_HEADERS {
                assert_eq!(response.headers()[name], value, "{case}: {name}");
            }
            assert_eq!(body(response).await, body_text.as_bytes(), "{case}");
        }
        remove(&exposures);
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

    /// The pages render under their own policy, which loads nothing, and name no version and
    /// nothing internal.
    #[test]
    fn the_pages_load_nothing_and_name_nothing_internal() {
        for page in [PAGE, ENTER_PAGE, UNAUTHORIZED_PAGE] {
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
                "9200",
                "exp:",
                "enb:",
            ] {
                assert!(!page.contains(internal), "{internal}");
            }
        }
        assert!(ENTER_PAGE.contains(r#"<meta http-equiv="refresh" content="0; url=/">"#));
        for directive in [
            "default-src 'none'",
            "base-uri 'none'",
            "form-action 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(CONTENT_SECURITY_POLICY.contains(directive), "{directive}");
        }
    }

    /// Over the wire: a link redeemed, then the page with the session's cookie, with its
    /// headers; HEAD sends no body, so the connection stays in step for the next request.
    #[tokio::test]
    async fn the_page_goes_out_with_its_headers_and_head_sends_no_body() {
        let now = unix_now();
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        let shop = exposure(SHOP_ID, SHOP, vec![enablement(SHOP_ENB, now + 3600, A, 4)]);
        let exposures = holding("wire", keys, now - 10, set(1, vec![shop])).await;
        let code = link(&exposures, SHOP_ID, SHOP_ENB, now);
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let (listen, _) = start_serving(&ca, &registry, limits(), Arc::clone(&exposures)).await;
        let mut ingress = dial(listen, client(&ca, "ingress1")).await.unwrap();

        let target = format!("{ENTER_PATH}?code={code}");
        let (status, headers, body) = exchange_as(&mut ingress, "GET", &target, SHOP, None)
            .await
            .unwrap();
        assert_eq!((status, body.as_slice()), (200, ENTER_PAGE.as_bytes()));
        let cookie = cookie_of(header(&headers, "set-cookie"));

        let (status, headers, body) = exchange_as(&mut ingress, "GET", "/", SHOP, Some(&cookie))
            .await
            .unwrap();
        assert_eq!((status, body.as_slice()), (200, PAGE.as_bytes()));
        assert_eq!(header(&headers, "content-type"), HTML);
        for (name, value) in SECURITY_HEADERS {
            assert_eq!(header(&headers, name), value, "{name}");
        }
        let (status, headers, _) = exchange_as(&mut ingress, "HEAD", "/", SHOP, Some(&cookie))
            .await
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(
            header(&headers, "content-security-policy"),
            CONTENT_SECURITY_POLICY
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
        let _ = std::fs::remove_dir_all(registry.root());
        remove(&exposures);
    }

    /// A link redeems once, on its host, into a session cookie with every attribute of §3.3 and a
    /// page that moves on to `/`; the cookie then opens the exposure.
    #[tokio::test]
    async fn a_link_redeems_once_and_sets_the_host_cookie() {
        let exposures = enabled("redeem", BOOT, UNTIL).await;
        let code = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 10);
        let entered = respond(&exposures, &enter(&code, SHOP), BOOT + 15);
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

        let served = respond(&exposures, &page(SHOP, attributes[0]), BOOT + 16);
        assert_eq!(served.status(), StatusCode::OK);
        assert_eq!(body(served).await, PAGE.as_bytes());

        assert_unauthorized(
            respond(&exposures, &enter(&code, SHOP), BOOT + 20),
            false,
            "twice",
        )
        .await;
        let view = exposures.view(BOOT + 20);
        assert_eq!(view["links_issued"], 1);
        assert_eq!(view["links_redeemed"], 1);
        assert_eq!(view["links_refused"], 1);
        assert_eq!(view["sessions_minted"], 1);
        remove(&exposures);
    }

    /// The exposure is in the code and the host in its MAC: a link fails on another exposure's
    /// host, on a host that is no exposure's, and as a code minted for another host; it stays good
    /// on its own host.
    #[tokio::test]
    async fn a_link_is_refused_on_another_host() {
        let exposures = enabled("host", BOOT, UNTIL).await;
        let code = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 10);
        for host in [
            ERP,
            "nowhere.hive-k3m9x7q2.fluxbee.ai",
            "hive-k3m9x7q2.fluxbee.ai",
        ] {
            assert_unauthorized(
                respond(&exposures, &enter(&code, host), BOOT + 11),
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
            respond(&exposures, &enter(&elsewhere, ERP), BOOT + 11),
            false,
            "minted for another host",
        )
        .await;
        let its_own = LinkCode::mint(&erp_keys, ERP, &x, &e, &"cd".repeat(16), BOOT + 10);
        assert_eq!(
            respond(&exposures, &enter(&its_own, ERP), BOOT + 11).status(),
            StatusCode::OK
        );
        assert_eq!(
            respond(&exposures, &enter(&code, SHOP), BOOT + 12).status(),
            StatusCode::OK,
            "the refusals spent nothing"
        );
        remove(&exposures);
    }

    /// A link redeems up to 120 s after it was minted, and not a second later (§3.2).
    #[tokio::test]
    async fn a_link_is_refused_after_120_s() {
        let exposures = enabled("late", BOOT, UNTIL).await;
        let late = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 10);
        assert_unauthorized(
            respond(&exposures, &enter(&late, SHOP), BOOT + 10 + 121),
            false,
            "121 s",
        )
        .await;
        let in_time = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 10);
        assert_eq!(
            respond(&exposures, &enter(&in_time, SHOP), BOOT + 10 + 120).status(),
            StatusCode::OK
        );
        remove(&exposures);
    }

    /// A code changed anywhere, signed with another key, or malformed fails, as do two codes or
    /// none; the genuine one still redeems.
    #[tokio::test]
    async fn a_link_is_refused_tampered() {
        let exposures = enabled("tampered", BOOT, UNTIL).await;
        let code = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 10);
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
                respond(&exposures, &enter(&tampered, SHOP), BOOT + 11),
                false,
                case,
            )
            .await;
        }
        let twice = format!("{ENTER_PATH}?code={code}&code={code}");
        for (case, target) in [("two codes", twice.as_str()), ("no code", ENTER_PATH)] {
            let response = respond(
                &exposures,
                &request(Method::GET, target, SHOP, None),
                BOOT + 11,
            );
            assert_unauthorized(response, false, case).await;
        }
        assert_eq!(
            respond(&exposures, &enter(&code, SHOP), BOOT + 12).status(),
            StatusCode::OK
        );
        remove(&exposures);
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
        let after = Exposures::new(file.clone(), keys.clone(), boot_second(started));
        after.load_at_boot(started).await;
        for (case, code) in [("earlier", &earlier), ("in its last second", &last_second)] {
            let response = respond(&after, &enter(code, SHOP), started + 1);
            assert_unauthorized(response, false, case).await;
        }
        let request = IoWebAccessLinkRequest {
            exposure_id: SHOP_ID.to_string(),
            enablement_id: SHOP_ENB.to_string(),
            generation: 1,
        };
        assert!(
            matches!(
                after.issue_link(&request, started),
                IoWebAccessLinkResponse::Error { error_code, .. }
                    if error_code == IO_WEB_ERROR_KEY_UNAVAILABLE
            ),
            "no link in the second it started"
        );
        let fresh = link(&after, SHOP_ID, SHOP_ENB, started + 1);
        assert_eq!(
            respond(&after, &enter(&fresh, SHOP), started + 1).status(),
            StatusCode::OK
        );

        // It stops within the second it minted that link, and its successor, started in it too,
        // refuses the code it spent.
        let successor = Exposures::new(file, keys, boot_second(started + 1));
        successor.load_at_boot(started + 1).await;
        let again = respond(&successor, &enter(&fresh, SHOP), started + 1);
        assert_unauthorized(again, false, "spent before a restart within its second").await;
        remove(&before);
    }

    /// A link of an enablement that has ended is refused, within its 120 s too.
    #[tokio::test]
    async fn a_link_is_refused_of_an_ended_enablement() {
        let exposures = enabled("ended", BOOT, BOOT + 100).await;
        let code = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 90);
        assert_unauthorized(
            respond(&exposures, &enter(&code, SHOP), BOOT + 100),
            false,
            "ended",
        )
        .await;
        let other = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 90);
        assert_eq!(
            respond(&exposures, &enter(&other, SHOP), BOOT + 99).status(),
            StatusCode::OK
        );
        remove(&exposures);
    }

    /// A HEAD never redeems, so a link's preview never spends it, nor does another method: the
    /// GET after them still redeems.
    #[tokio::test]
    async fn head_never_redeems() {
        let exposures = enabled("head", BOOT, UNTIL).await;
        let code = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 10);
        let target = format!("{ENTER_PATH}?code={code}");
        for method in [Method::HEAD, Method::POST] {
            let case = method.to_string();
            let response = respond(&exposures, &request(method, &target, SHOP, None), BOOT + 11);
            assert_unauthorized(response, false, &case).await;
        }
        assert_eq!(exposures.view(BOOT + 11)["links_refused"], 0);
        assert_eq!(
            respond(&exposures, &enter(&code, SHOP), BOOT + 12).status(),
            StatusCode::OK
        );
        remove(&exposures);
    }

    /// A session serves the page on any path; once its last activity is 60 s old, the answer
    /// carries the cookie again with `active = now`, the same session, whose 30 min count from
    /// there.
    #[tokio::test]
    async fn a_session_serves_the_page_and_slides_after_60_s() {
        let exposures = enabled("slide", BOOT, UNTIL).await;
        let cookie = sign_in(&exposures, SHOP_ID, SHOP_ENB, SHOP, BOOT + 10);
        let early = respond(
            &exposures,
            &request(Method::GET, "/any/path?x=1", SHOP, Some(&cookie)),
            BOOT + 69,
        );
        assert_eq!(early.status(), StatusCode::OK);
        assert_eq!(set_cookie(&early), None, "59 s: no slide");
        assert_eq!(body(early).await, PAGE.as_bytes());

        let slid = respond(&exposures, &page(SHOP, &cookie), BOOT + 70);
        assert_eq!(slid.status(), StatusCode::OK);
        let renewed = set_cookie(&slid).expect("60 s: the cookie again");
        assert!(
            renewed.ends_with("; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=1800"),
            "{renewed}"
        );
        let renewed = cookie_of(&renewed);
        let (old, new) = (token_of(&cookie), token_of(&renewed));
        assert_eq!(
            (new.sid, new.issued, new.active),
            (old.sid, old.issued, BOOT + 70)
        );

        assert_eq!(
            respond(&exposures, &page(SHOP, &renewed), BOOT + 70 + 1800).status(),
            StatusCode::OK
        );
        assert_unauthorized(
            respond(&exposures, &page(SHOP, &cookie), BOOT + 10 + 1801),
            true,
            "the cookie before the slide",
        )
        .await;
        let head = request(Method::HEAD, "/", SHOP, Some(&renewed));
        assert_eq!(
            respond(&exposures, &head, BOOT + 80).status(),
            StatusCode::OK
        );
        remove(&exposures);
    }

    /// 30 min without a request end a session: at 1800 s it still serves, at 1801 s it is refused
    /// and its cookie cleared.
    #[tokio::test]
    async fn a_session_ends_after_30_min_idle() {
        let exposures = enabled("idle", BOOT, UNTIL).await;
        let cookie = sign_in(&exposures, SHOP_ID, SHOP_ENB, SHOP, BOOT + 10);
        assert_eq!(
            respond(&exposures, &page(SHOP, &cookie), BOOT + 10 + 1800).status(),
            StatusCode::OK
        );
        assert_unauthorized(
            respond(&exposures, &page(SHOP, &cookie), BOOT + 10 + 1801),
            true,
            "idle",
        )
        .await;
        remove(&exposures);
    }

    /// A session ends with its enablement: its cookie's `Max-Age` stops there, and the first
    /// request at the end is refused.
    #[tokio::test]
    async fn a_session_ends_at_the_enablement_s_end() {
        let end = BOOT + 1000;
        let exposures = enabled("end", BOOT, end).await;
        let code = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 10);
        let entered = respond(&exposures, &enter(&code, SHOP), BOOT + 10);
        let session = set_cookie(&entered).unwrap();
        assert!(session.ends_with("; Max-Age=990"), "{session}");
        let cookie = cookie_of(&session);
        let last = respond(&exposures, &page(SHOP, &cookie), end - 1);
        assert_eq!(last.status(), StatusCode::OK);
        let slid = set_cookie(&last).unwrap();
        assert!(slid.ends_with("; Max-Age=1"), "{slid}");
        assert_unauthorized(
            respond(&exposures, &page(SHOP, &cookie), end),
            true,
            "the end",
        )
        .await;
        remove(&exposures);
    }

    /// A session waits for its key (§3.4): io.web restarted and could not read the key yet, as
    /// with SY.vault or identity not ready, so the session gets the 401 page with its cookie kept,
    /// not cleared; once the retry reads the key, the same cookie serves the page again.
    #[tokio::test]
    async fn a_session_waits_for_its_key_after_a_restart() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        let shop = exposure(SHOP_ID, SHOP, vec![enablement(SHOP_ENB, UNTIL, A, 4)]);
        let before = holding("waits", keys.clone(), BOOT, set(1, vec![shop])).await;
        let cookie = sign_in(&before, SHOP_ID, SHOP_ENB, SHOP, BOOT + 10);

        let vault_key = web_exposure_vault_key(SHOP_ID, A).unwrap();
        keys.answer(&vault_key, Err("SY.vault is not ready".to_string()));
        let after = Exposures::new(
            before.file().to_path_buf(),
            keys.clone(),
            boot_second(BOOT + 100),
        );
        after.load_at_boot(BOOT + 100).await;
        assert_eq!(after.view(BOOT + 100)["keys_unavailable"], 1);
        assert_unauthorized(
            respond(&after, &page(SHOP, &cookie), BOOT + 101),
            false,
            "its key not read yet",
        )
        .await;

        keys.put(SHOP_ID, A, 4, 1);
        after.retry(BOOT + 130).await;
        let served = respond(&after, &page(SHOP, &cookie), BOOT + 131);
        assert_eq!(served.status(), StatusCode::OK);
        assert_eq!(body(served).await, PAGE.as_bytes());
        remove(&before);
    }

    /// A session is its exposure's alone: on another exposure's host it is refused and cleared
    /// there, and still serves on its own.
    #[tokio::test]
    async fn a_session_of_one_exposure_fails_on_another() {
        let exposures = enabled("another", BOOT, UNTIL).await;
        let cookie = sign_in(&exposures, SHOP_ID, SHOP_ENB, SHOP, BOOT + 10);
        assert_unauthorized(
            respond(&exposures, &page(ERP, &cookie), BOOT + 11),
            true,
            "erp",
        )
        .await;
        assert_eq!(
            respond(&exposures, &page(SHOP, &cookie), BOOT + 12).status(),
            StatusCode::OK
        );
        remove(&exposures);
    }

    /// A push that drops an exposure's enablement ends each of its sessions at their next
    /// request, and its pending link; the other exposure's session goes on.
    #[tokio::test]
    async fn a_disable_push_ends_every_session_at_once() {
        let exposures = enabled("disable", BOOT, UNTIL).await;
        let first = sign_in(&exposures, SHOP_ID, SHOP_ENB, SHOP, BOOT + 10);
        let second = sign_in(&exposures, SHOP_ID, SHOP_ENB, SHOP, BOOT + 11);
        let other = sign_in(&exposures, ERP_ID, ERP_ENB, ERP, BOOT + 12);
        let pending = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 13);

        let disabled = set(
            2,
            vec![
                exposure(SHOP_ID, SHOP, Vec::new()),
                exposure(ERP_ID, ERP, vec![enablement(ERP_ENB, UNTIL, A, 2)]),
            ],
        );
        let answer = exposures.apply_push(disabled, BOOT + 14).await;
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
                respond(&exposures, &page(SHOP, cookie), BOOT + 14),
                true,
                case,
            )
            .await;
        }
        assert_unauthorized(
            respond(&exposures, &enter(&pending, SHOP), BOOT + 14),
            false,
            "the pending link",
        )
        .await;
        assert_eq!(
            respond(&exposures, &page(ERP, &other), BOOT + 14).status(),
            StatusCode::OK
        );
        remove(&exposures);
    }

    /// More than one session cookie is refused and cleared, a valid one among them included;
    /// other cookies beside one are no matter.
    #[tokio::test]
    async fn more_than_one_session_cookie_is_refused() {
        let exposures = enabled("two", BOOT, UNTIL).await;
        let one = sign_in(&exposures, SHOP_ID, SHOP_ENB, SHOP, BOOT + 10);
        let two = sign_in(&exposures, SHOP_ID, SHOP_ENB, SHOP, BOOT + 11);
        let both = format!("theme=dark; {one}; {two}");
        assert_unauthorized(
            respond(&exposures, &page(SHOP, &both), BOOT + 12),
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
        assert_unauthorized(respond(&exposures, &split, BOOT + 12), true, "two headers").await;
        let beside = format!("theme=dark; {one}; lang=en");
        assert_eq!(
            respond(&exposures, &page(SHOP, &beside), BOOT + 12).status(),
            StatusCode::OK
        );
        remove(&exposures);
    }

    /// io.web's health check moved under the reserved prefix, on every host, whose other paths are
    /// 404; `/healthz` is now a path like any other, which a session guards.
    #[tokio::test]
    async fn healthz_moved_under_the_reserved_prefix() {
        let exposures = enabled("healthz", BOOT, UNTIL).await;
        let unknown = "nowhere.hive-k3m9x7q2.fluxbee.ai";
        for host in [SHOP, unknown, ""] {
            for method in [Method::GET, Method::HEAD] {
                let case = format!("{method} on '{host}'");
                let response = respond(
                    &exposures,
                    &request(method, HEALTHZ_PATH, host, None),
                    BOOT + 1,
                );
                assert_eq!(response.status(), StatusCode::OK, "{case}");
                assert_eq!(body(response).await, b"ok\n".as_slice(), "{case}");
            }
        }
        let old = request(Method::GET, "/healthz", SHOP, None);
        assert_unauthorized(respond(&exposures, &old, BOOT + 1), false, "/healthz").await;
        let old = request(Method::GET, "/healthz", unknown, None);
        assert_eq!(
            respond(&exposures, &old, BOOT + 1).status(),
            StatusCode::NOT_FOUND
        );
        for path in ["/.fluxbee/", "/.fluxbee/healthz/", "/.fluxbee/admin"] {
            let response = respond(
                &exposures,
                &request(Method::GET, path, SHOP, None),
                BOOT + 1,
            );
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        remove(&exposures);
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
