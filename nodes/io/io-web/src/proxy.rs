//! The forwarding (docs/io-web-v2.md §13.3, §13.4; docs/io-web-v2-stage-2-2.md §4.2-§4.4, §4.7,
//! §4.8): a request whose session serves goes to its exposure's upstream, whatever its method, and
//! the answer comes back as the application sent it.
//!
//! - **The request** keeps the edge's headers and its body, streamed as it comes (`headers`).
//! - **The answer** streams back frame by frame, never gathered and never compressed, with
//!   §13.7's headers and the session's cookie when it slides or moves.
//! - **A WebSocket** is tunnelled: io.web answers 101 and copies bytes both ways, inside its
//!   connection's scope (`Tunnels`, `web::serve_http`).
//! - **No wait of its own:** only reaching the upstream is bounded (`upstream`). A request waits
//!   for its upstream as long as the application takes, and an answer streams as long as it lasts.
//! - **The cut:** everything open on an exposure ends at once when the exposure is left without
//!   access (`open`): a request still waiting gets the 401 page, an answer streaming ends with an
//!   error, which the edge passes on by closing the client's connection, and a tunnel drops both of
//!   its sides.
//! - **Its line:** each request logs one line when it ends (`Exchange`), and a stream or a
//!   WebSocket one more when it opens; never a cookie, a query or a body.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::client::conn::http1::SendRequest;
use hyper::header::{HeaderMap, HeaderValue, SET_COOKIE};
use hyper::upgrade::OnUpgrade;
use hyper::{Method, Request, Response, StatusCode, Uri, Version};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

use crate::access::session_clear_cookie;
use crate::exposures::{Admitted, Exposures, Slide};
use crate::headers::{self, Framing};
use crate::open::{Class, Cut, Gate, Registration};
use crate::upstream::{Failure, Upstreams};
use crate::web::{self, WebBody};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A request's body as io.web forwards it: the edge's, streamed (hyper's `Incoming` on the port).
pub type RequestBody = UnsyncBoxBody<Bytes, BoxError>;

/// What a tunnel copies at a time, each way.
const TUNNEL_BUFFER: usize = 16 * 1024;

/// What io.web's port needs to answer a request: the exposures, and where each upstream is.
pub struct Proxy {
    pub exposures: Arc<Exposures>,
    pub upstreams: Upstreams,
}

type Tunnel = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The tunnel of one connection's WebSocket. `forward` leaves it here with the 101, and
/// `web::serve_http` runs it once hyper has handed the connection over: inside the connection's
/// scope, so it keeps the connection's permit and a demoted ingress still cuts it (§4.1).
#[derive(Default)]
pub struct Tunnels(Mutex<Option<Tunnel>>);

impl Tunnels {
    fn put(&self, tunnel: Tunnel) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(tunnel);
    }

    pub fn take(&self) -> Option<Tunnel> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// How a request on an exposure's host ended, as its line says (§4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The answer's body ended, or the answer had none.
    Complete,
    /// The edge went away first: the client closed, or a WebSocket's client side ended first.
    ClientClosed,
    /// The upstream's answer broke mid-body, or a WebSocket's upstream side ended first.
    UpstreamClosed,
    /// The exposure was left without access while it was open.
    Cut,
    Refused401,
    Refused403,
    /// The upstream was not reached, or gave no answer io.web can pass: its 502, with the kind.
    UpstreamFailed(&'static str),
}

impl std::fmt::Display for Ending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Complete => f.write_str("complete"),
            Self::ClientClosed => f.write_str("client_closed"),
            Self::UpstreamClosed => f.write_str("upstream_closed"),
            Self::Cut => f.write_str("cut"),
            Self::Refused401 => f.write_str("refused_401"),
            Self::Refused403 => f.write_str("refused_403"),
            Self::UpstreamFailed(kind) => write!(f, "upstream_failed:{kind}"),
        }
    }
}

/// One request on an exposure's host, from its head to its end (§4.8): what its line says, written
/// once when it is dropped; its end is counted on its exposure's gate as it comes. It holds the
/// request's registration (`open`) while what it forwarded is open. It never holds a cookie, a
/// query or a body.
pub struct Exchange {
    request_id: String,
    exposure_id: String,
    host: String,
    method: Method,
    /// Without its query.
    path: String,
    /// The session's enablement, or the refused cookie's, when there is one.
    enablement_id: Option<String>,
    started: Instant,
    /// The upstream's head, from `started`.
    head: Option<Duration>,
    status: Option<u16>,
    /// Where it was dialled.
    upstream: Option<String>,
    ended: Option<(Ending, Instant)>,
    /// A refusal's reason, or what failed.
    detail: Option<String>,
    /// The client's body to the upstream; a WebSocket's bytes from the client.
    bytes_in: Arc<AtomicU64>,
    /// The answer's body to the client; a WebSocket's bytes from the upstream.
    bytes_out: AtomicU64,
    /// The client's body broke on its way to the upstream: the edge went away.
    client_failed: Arc<AtomicBool>,
    gate: Arc<Gate>,
    open: Option<Registration>,
}

impl Exchange {
    /// A request on the host of exposure `exposure_id`, whose gate is `gate`, starting now.
    pub fn new(
        request_id: &str,
        host: &str,
        method: &Method,
        path: &str,
        exposure_id: String,
        gate: Arc<Gate>,
    ) -> Self {
        Self {
            request_id: request_id.to_string(),
            exposure_id,
            host: host.to_string(),
            method: method.clone(),
            path: path.to_string(),
            enablement_id: None,
            started: Instant::now(),
            head: None,
            status: None,
            upstream: None,
            ended: None,
            detail: None,
            bytes_in: Arc::new(AtomicU64::new(0)),
            bytes_out: AtomicU64::new(0),
            client_failed: Arc::new(AtomicBool::new(false)),
            gate,
            open: None,
        }
    }

    /// Ends it, unless it has ended already: the first end is the one it logs, and the one its
    /// exposure counts, at once.
    fn end(&mut self, ending: Ending) {
        if self.ended.is_some() {
            return;
        }
        self.ended = Some((ending, Instant::now()));
        match ending {
            Ending::Refused401 => self.gate.count_refused(401),
            Ending::Refused403 => self.gate.count_refused(403),
            Ending::UpstreamFailed(_) => self.gate.count_upstream_failure(),
            Ending::Cut => self.gate.count_cut(),
            Ending::Complete | Ending::ClientClosed | Ending::UpstreamClosed => {}
        }
    }

    /// io.web's refusal, `status` 401 or 403, with why; logged when dropped.
    pub fn refuse(&mut self, status: u16, reason: String, enablement_id: Option<String>) {
        self.status = Some(status);
        self.detail = Some(reason);
        self.enablement_id = enablement_id;
        self.end(if status == 403 {
            Ending::Refused403
        } else {
            Ending::Refused401
        });
    }

    /// The line of a stream or a WebSocket when it opens (§4.8).
    fn log_open(&self, what: &'static str) {
        tracing::info!(
            request_id = %self.request_id,
            exposure_id = %self.exposure_id,
            host = %self.host,
            method = %self.method,
            path = %self.path,
            status = self.status,
            upstream = self.upstream.as_deref().unwrap_or_default(),
            head_ms = self.head.map(ms),
            "{what}"
        );
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        let (ending, at) = self
            .ended
            .unwrap_or_else(|| (Ending::ClientClosed, Instant::now()));
        let message = match ending {
            Ending::Refused401 | Ending::Refused403 => "IO.web refused a request",
            _ => "IO.web proxied a request",
        };
        tracing::info!(
            request_id = %self.request_id,
            exposure_id = %self.exposure_id,
            host = %self.host,
            enablement_id = self.enablement_id.as_deref().unwrap_or_default(),
            method = %self.method,
            path = %self.path,
            status = self.status,
            upstream = self.upstream.as_deref().unwrap_or_default(),
            bytes_in = self.bytes_in.load(Ordering::Relaxed),
            bytes_out = self.bytes_out.load(Ordering::Relaxed),
            head_ms = self.head.map(ms),
            duration_ms = ms(at.saturating_duration_since(self.started)),
            ending = %ending,
            detail = self.detail.as_deref().unwrap_or_default(),
            "{message}"
        );
    }
}

/// A request whose session serves, `admitted`, to its upstream (§4.2-§4.4): its answer, streamed;
/// a WebSocket's 101, its tunnel left in `tunnels`; the 401 page when the exposure is cut while it
/// waits; io.web's 502 page when the upstream is not reached or answers what io.web cannot pass.
pub async fn forward(
    proxy: &Proxy,
    admitted: Admitted,
    mut request: Request<RequestBody>,
    mut exchange: Exchange,
    tunnels: &Tunnels,
) -> Response<WebBody> {
    let Admitted {
        enablement_id,
        upstream,
        framing,
        slide,
        open,
    } = admitted;
    exchange.enablement_id = Some(enablement_id);
    let mut cut = open.cut();
    exchange.open = Some(open);
    let websocket = headers::is_websocket_handshake(request.method(), request.headers());
    let downstream = websocket.then(|| hyper::upgrade::on(&mut request));
    let request = upstream_request(request, websocket, &exchange);

    // Reaching the upstream, and its head: as long as the application takes, unless a cut comes.
    let reached = tokio::select! {
        biased;
        () = &mut cut => return cut_while_waiting(exchange, &framing),
        reached = proxy.upstreams.connect(&upstream) => reached,
    };
    let (tcp, address) = match reached {
        Ok(reached) => reached,
        Err(failure) => return unreachable(exchange, failure, &framing),
    };
    exchange.upstream = Some(address.to_string());
    let (mut sender, driver) = match handshake(tcp).await {
        Ok(handshaken) => handshaken,
        Err(failure) => return unreachable(exchange, failure, &framing),
    };
    let answered = tokio::select! {
        biased;
        () = &mut cut => return cut_while_waiting(exchange, &framing),
        answered = sender.send_request(request) => answered,
    };
    let answer = match answered {
        Ok(answer) => answer,
        // The client's body broke first: the edge could not finish it (an aborted upload, a
        // client below its pace) and answered the client itself. The request is dropped.
        Err(_) if exchange.client_failed.load(Ordering::Relaxed) => {
            return client_gone(exchange, &framing);
        }
        Err(err) => {
            return unreachable(exchange, Failure::new("head", error_chain(&err)), &framing);
        }
    };
    exchange.head = Some(exchange.started.elapsed());
    exchange.status = Some(answer.status().as_u16());
    if answer.status() == StatusCode::SWITCHING_PROTOCOLS {
        return match downstream {
            Some(downstream) if headers::upgrades_to_websocket(answer.headers()) => {
                switch(answer, downstream, exchange, cut, driver, tunnels)
            }
            _ => unreachable(
                exchange,
                Failure::new(
                    "switched",
                    "the upstream switched protocols to what io.web did not ask for, or answered \
                     a WebSocket's handshake without one Upgrade: websocket and one \
                     Sec-WebSocket-Accept",
                ),
                &framing,
            ),
        };
    }
    pass_on(answer, exchange, &framing, slide, cut, driver)
}

/// The request as the upstream gets it (§4.3): the method, the target in origin form, HTTP/1.1,
/// `headers::upstream_request_headers`, and the client's body as it comes, counted.
fn upstream_request(
    request: Request<RequestBody>,
    websocket: bool,
    exchange: &Exchange,
) -> Request<CountedBody> {
    let (parts, body) = request.into_parts();
    let mut upstream = Request::new(CountedBody {
        inner: body,
        count: Arc::clone(&exchange.bytes_in),
        failed: Arc::clone(&exchange.client_failed),
    });
    *upstream.method_mut() = parts.method;
    *upstream.uri_mut() = parts
        .uri
        .path_and_query()
        .cloned()
        .map(Uri::from)
        .unwrap_or_else(|| Uri::from_static("/"));
    *upstream.version_mut() = Version::HTTP_11;
    *upstream.headers_mut() = headers::upstream_request_headers(&parts.headers, websocket);
    upstream
}

/// hyper's HTTP/1 client on `tcp`, its connection run by a task of its own until the request is
/// over (`Driver`), with upgrades, so a WebSocket's 101 hands its connection over.
async fn handshake(tcp: TcpStream) -> Result<(SendRequest<CountedBody>, Driver), Failure> {
    let (sender, connection) = hyper::client::conn::http1::Builder::new()
        .max_buf_size(web::MAX_BUF_SIZE)
        .handshake(TokioIo::new(tcp))
        .await
        .map_err(|err| Failure::new("http_handshake", error_chain(&err)))?;
    let driver = tokio::spawn(async move {
        if let Err(err) = connection.with_upgrades().await {
            tracing::debug!(
                error = %error_chain(&err),
                "IO.web: a connection to an upstream ended with an error"
            );
        }
    });
    Ok((sender, Driver(driver)))
}

/// The cut of a request still waiting for its upstream (§4.7): io.web's 401 page, which clears
/// the cookie of an access that has ended; the upstream's connection drops with the request.
fn cut_while_waiting(mut exchange: Exchange, framing: &Framing) -> Response<WebBody> {
    exchange.status = Some(StatusCode::UNAUTHORIZED.as_u16());
    exchange.end(Ending::Cut);
    web::unauthorized(Some(session_clear_cookie()), framing)
}

/// A request whose client's body broke on its way (`CountedBody`): the edge has answered the
/// client itself, so nobody reads io.web's answer; it is logged `client_closed`, with no status,
/// and counted nowhere.
fn client_gone(mut exchange: Exchange, framing: &Framing) -> Response<WebBody> {
    exchange.detail = Some("the client's body broke on its way to the upstream".to_string());
    exchange.end(Ending::ClientClosed);
    web::bad_gateway(framing)
}

/// io.web's fixed 502 page for an upstream not reached, or one whose answer io.web cannot pass,
/// logged with the failure's kind and detail.
fn unreachable(mut exchange: Exchange, failure: Failure, framing: &Framing) -> Response<WebBody> {
    exchange.status = Some(StatusCode::BAD_GATEWAY.as_u16());
    exchange.detail = Some(failure.detail);
    exchange.end(Ending::UpstreamFailed(failure.kind));
    web::bad_gateway(framing)
}

/// A WebSocket's 101 (§4.3): the handshake's headers for the edge, and the tunnel, left in
/// `tunnels` for the connection's scope to run.
fn switch(
    answer: Response<Incoming>,
    downstream: OnUpgrade,
    mut exchange: Exchange,
    cut: Cut,
    driver: Driver,
    tunnels: &Tunnels,
) -> Response<WebBody> {
    let mut switched = Response::new(WebBody::empty());
    *switched.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *switched.headers_mut() = headers::websocket_answer_headers(answer.headers());
    exchange
        .gate
        .count_answer(StatusCode::SWITCHING_PROTOCOLS.as_u16());
    if let Some(open) = exchange.open.as_mut() {
        open.now(Class::WebSocket);
    }
    exchange.log_open("IO.web WebSocket opened");
    let upstream = hyper::upgrade::on(answer);
    tunnels.put(Box::pin(tunnel(
        exchange, downstream, upstream, cut, driver,
    )));
    switched
}

/// The upstream's answer as it goes on (§4.4): its status, `headers::set_application_headers`,
/// the session's cookie when it slides or moves, and its body frame by frame.
fn pass_on(
    answer: Response<Incoming>,
    mut exchange: Exchange,
    framing: &Framing,
    slide: Option<Slide>,
    cut: Cut,
    driver: Driver,
) -> Response<WebBody> {
    let (mut parts, body) = answer.into_parts();
    exchange.gate.count_answer(parts.status.as_u16());
    headers::set_application_headers(&mut parts.headers, framing);
    if let Some(slide) = slide {
        attach_session(&mut parts.headers, slide, &exchange);
    }
    if let Some(open) = exchange.open.as_mut() {
        open.now(Class::Stream);
    }
    // An answer that announces no length (chunked, or until the connection closes) is a stream,
    // as SSE is: its end may be long in coming, so it says when it opens.
    if !body.is_end_stream() && body.size_hint().exact().is_none() {
        exchange.log_open("IO.web stream opened");
    }
    Response::from_parts(
        parts,
        WebBody::Upstream(UpstreamBody {
            upstream: Some((body, driver)),
            cut: Some(cut),
            exchange,
        }),
    )
}

/// The session's cookie on the answer, beside the application's own; a move is counted and logged.
fn attach_session(headers: &mut HeaderMap, slide: Slide, exchange: &Exchange) {
    // A cookie's value is hex, digits and dots, and its attributes ASCII: always a valid value.
    let Ok(value) = HeaderValue::try_from(slide.set_cookie) else {
        return;
    };
    headers.append(SET_COOKIE, value);
    if let Some(moved) = slide.moved {
        exchange.gate.count_session_moved();
        tracing::info!(
            sid = %moved.sid,
            exposure_id = %exchange.exposure_id,
            host = %exchange.host,
            from_enablement_id = %moved.from,
            enablement_id = %moved.to,
            max_age = moved.max_age,
            request_id = %exchange.request_id,
            "IO.web moved a session to its exposure's newest enablement"
        );
    }
}

/// The task that runs one connection to an upstream, aborted once its request is over: the answer
/// ended, the client went away, or a cut came. The connection never outlives its request.
struct Driver(JoinHandle<()>);

impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The client's body on its way to the upstream, as it comes, counted.
pub struct CountedBody {
    inner: RequestBody,
    count: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
}

impl Body for CountedBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.count.fetch_add(len(data), Ordering::Relaxed);
                }
            }
            Poll::Ready(Some(Err(_))) => this.failed.store(true, Ordering::Relaxed),
            Poll::Ready(None) | Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// An upstream's answer body on its way to the client, frame by frame as each arrives: never
/// gathered, never compressed. It holds the request's `Exchange`, and with it its registration,
/// until it is dropped; the upstream's answer and connection until its end, or its cut, which ends
/// it with an error and drops the upstream's connection at once.
pub struct UpstreamBody {
    /// The upstream's answer and the task that runs its connection; `None` once it ended.
    upstream: Option<(Incoming, Driver)>,
    /// `None` once it fired.
    cut: Option<Cut>,
    exchange: Exchange,
}

impl Body for UpstreamBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = self.get_mut();
        let Some((inner, _)) = this.upstream.as_mut() else {
            return Poll::Ready(None);
        };
        if let Some(cut) = this.cut.as_mut() {
            if cut.as_mut().poll(cx).is_ready() {
                this.cut = None;
                this.upstream = None;
                this.exchange.end(Ending::Cut);
                return Poll::Ready(Some(Err(
                    "io.web cut the answer: its exposure is left without access".into(),
                )));
            }
        }
        match Pin::new(inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.exchange
                        .bytes_out
                        .fetch_add(len(data), Ordering::Relaxed);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(err))) => {
                this.upstream = None;
                this.exchange.detail = Some(error_chain(&err));
                // A client's body that broke ends the upstream's connection too.
                let ending = if this.exchange.client_failed.load(Ordering::Relaxed) {
                    Ending::ClientClosed
                } else {
                    Ending::UpstreamClosed
                };
                this.exchange.end(ending);
                Poll::Ready(Some(Err(err.into())))
            }
            Poll::Ready(None) => {
                this.upstream = None;
                this.exchange.end(Ending::Complete);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.upstream
            .as_ref()
            .is_none_or(|(inner, _)| inner.is_end_stream())
    }

    fn size_hint(&self) -> SizeHint {
        self.upstream
            .as_ref()
            .map_or_else(|| SizeHint::with_exact(0), |(inner, _)| inner.size_hint())
    }
}

impl Drop for UpstreamBody {
    fn drop(&mut self) {
        // hyper polls no body it knows is empty (a HEAD's, a 204's, a 304's): it is complete.
        if self
            .upstream
            .as_ref()
            .is_some_and(|(inner, _)| inner.is_end_stream())
        {
            self.exchange.end(Ending::Complete);
        }
    }
}

/// Which side of a tunnel's copy ended it.
enum Side {
    /// What it reads from: EOF or an error.
    Reader,
    /// What it writes to.
    Writer,
}

/// A WebSocket's tunnel (§4.3, §4.7): bytes copied both ways, as they come, until the first end
/// of either side or the cut, which ends both. Both sides are dropped then: a side that never
/// closes keeps nothing open.
async fn tunnel(
    mut exchange: Exchange,
    downstream: OnUpgrade,
    upstream: OnUpgrade,
    mut cut: Cut,
    _driver: Driver,
) {
    let upstream = match upstream.await {
        Ok(upgraded) => upgraded,
        Err(err) => {
            exchange.detail = Some(error_chain(&err));
            exchange.end(Ending::UpstreamClosed);
            return;
        }
    };
    let downstream = match downstream.await {
        Ok(upgraded) => upgraded,
        Err(err) => {
            exchange.detail = Some(error_chain(&err));
            exchange.end(Ending::ClientClosed);
            return;
        }
    };
    let (mut client_read, mut client_write) = tokio::io::split(TokioIo::new(downstream));
    let (mut upstream_read, mut upstream_write) = tokio::io::split(TokioIo::new(upstream));
    let ending = tokio::select! {
        biased;
        () = &mut cut => Ending::Cut,
        side = pipe(&mut client_read, &mut upstream_write, &exchange.bytes_in) => match side {
            Side::Reader => Ending::ClientClosed,
            Side::Writer => Ending::UpstreamClosed,
        },
        side = pipe(&mut upstream_read, &mut client_write, &exchange.bytes_out) => match side {
            Side::Reader => Ending::UpstreamClosed,
            Side::Writer => Ending::ClientClosed,
        },
    };
    exchange.end(ending);
}

/// Copies `from` to `to` as bytes come, each read written and flushed at once, counted, until one
/// side ends.
async fn pipe<R, W>(from: &mut R, to: &mut W, count: &AtomicU64) -> Side
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; TUNNEL_BUFFER];
    loop {
        let read = match from.read(&mut buffer).await {
            Ok(0) | Err(_) => return Side::Reader,
            Ok(read) => read,
        };
        if to.write_all(&buffer[..read]).await.is_err() || to.flush().await.is_err() {
            return Side::Writer;
        }
        count.fetch_add(len(&buffer[..read]), Ordering::Relaxed);
    }
}

fn len(data: &[u8]) -> u64 {
    u64::try_from(data.len()).unwrap_or(u64::MAX)
}

fn ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// An error and its sources, outermost first, for a log line.
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut cause = err.source();
    while let Some(err) = cause {
        text.push_str(": ");
        text.push_str(&err.to_string());
        cause = err.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exposures::testing::*;
    use crate::exposures::{run_access_clock, unix_now};
    use crate::testing::{
        answer_to, body_of, client, closed_port, cookie_of, dial, edge_send, enabled, enabled_with,
        enter, eventually, header, page, proxy, read_head, registry, request, set_cookie,
        start_serving, token_of, with_body, without_body, Site, TestUpstream, BOOT, CLOUD, UNTIL,
        UPSTREAM_PAGE,
    };
    use fluxbee_sdk::mesh::{HiveRegistry, MeshCa};
    use fluxbee_sdk::protocol::{
        IoWebEnablement, IoWebExposuresPayload, IoWebExposuresResponse, WebExposureKeySlot,
        WebExposureUpstream,
    };
    use http_body_util::{BodyExt, Empty, Full};
    use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::VecDeque;
    use tokio::time::timeout;

    const A: WebExposureKeySlot = WebExposureKeySlot::A;
    const B: WebExposureKeySlot = WebExposureKeySlot::B;
    /// ERP's renewal, whose key io.web never reads.
    const ERP_RENEWED: &str = "enb:3d4e5f6a-7b8c-4d9e-8fa0-b1c2d3e4f5a6";
    const SHOP_ORIGIN: &str = "https://shop.hive-k3m9x7q2.fluxbee.ai";
    const WEBSOCKET_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

    /// The uuid of an `enb:` id in 32 hex, as cookies carry it.
    fn hex_id(id: &str) -> String {
        uuid::Uuid::parse_str(id.split_once(':').unwrap().1)
            .unwrap()
            .simple()
            .to_string()
    }

    /// What `/echo` says the upstream got.
    async fn echoed(response: Response<WebBody>) -> Value {
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&body_of(response).await).expect("/echo's JSON")
    }

    /// The exposure's part of the status.
    fn status_of(exposures: &Exposures, now: u64, index: usize) -> Value {
        exposures.view(now)["per_exposure"][index].clone()
    }

    /// The set of `site`, SHOP holding `enablements` and ERP as `enabled` made it.
    fn shop_set(generation: u64, enablements: Vec<IoWebEnablement>) -> IoWebExposuresPayload {
        set(
            generation,
            vec![
                exposure(SHOP_ID, SHOP, enablements),
                exposure(ERP_ID, ERP, vec![enablement(ERP_ENB, UNTIL, A, 2)]),
            ],
        )
    }

    /// io.web live on the real clock: SHOP and ERP enabled for an hour, booted 10 s ago, on a port
    /// of its own; a session on SHOP.
    struct Live {
        site: Site,
        listen: std::net::SocketAddr,
        ca: MeshCa,
        registry: HiveRegistry,
        cookie: String,
        now: u64,
    }

    impl Live {
        async fn start(label: &str) -> Self {
            let now = unix_now();
            let keys = MapKeys::new();
            keys.put(SHOP_ID, A, 4, 1);
            keys.put(ERP_ID, A, 2, 2);
            let set = set(
                1,
                vec![
                    exposure(SHOP_ID, SHOP, vec![enablement(SHOP_ENB, now + 3600, A, 4)]),
                    exposure(ERP_ID, ERP, vec![enablement(ERP_ENB, now + 3600, A, 2)]),
                ],
            );
            let site = Site::new(holding(label, keys, now - 10, set).await).await;
            let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, now).await;
            let ca = MeshCa::generate().unwrap();
            let registry = registry();
            let (listen, _) = start_serving(
                &ca,
                &registry,
                crate::testing::limits(),
                Arc::clone(&site.proxy),
            )
            .await;
            Self {
                site,
                listen,
                ca,
                registry,
                cookie,
                now,
            }
        }

        /// `path` on SHOP with the session, as the edge sends it.
        fn get(&self, path: &str) -> Request<Empty<Bytes>> {
            Request::builder()
                .uri(path)
                .header(HOST, SHOP)
                .header(COOKIE, self.cookie.as_str())
                .header("x-request-id", "5f0c2d1e-0000-4000-8000-00000000c0de")
                .header("connection", "close")
                .body(Empty::new())
                .unwrap()
        }

        async fn send<B>(&self, request: Request<B>) -> (Response<Incoming>, JoinHandle<()>)
        where
            B: Body<Data = Bytes> + Send + 'static,
            B::Error: Into<BoxError>,
        {
            let (response, driver) =
                edge_send(self.listen, client(&self.ca, "ingress1"), request).await;
            (response.expect("io.web answers"), driver)
        }

        /// A WebSocket's handshake on SHOP over a raw TLS connection, its 101 read.
        async fn websocket(
            &self,
        ) -> (
            tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
            Vec<(String, String)>,
        ) {
            let mut tls = dial(self.listen, client(&self.ca, "ingress1"))
                .await
                .unwrap();
            let handshake = format!(
                "GET /ws HTTP/1.1\r\nHost: {SHOP}\r\nCookie: {}\r\nConnection: Upgrade\r\n\
                 Upgrade: websocket\r\nSec-WebSocket-Key: {WEBSOCKET_KEY}\r\n\
                 Sec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: chat\r\n\
                 Origin: {SHOP_ORIGIN}\r\nX-Request-Id: 5f0c2d1e-0000-4000-8000-0000000000ws\r\n\r\n",
                self.cookie
            );
            tls.write_all(handshake.as_bytes()).await.unwrap();
            tls.flush().await.unwrap();
            let (status, headers) = read_head(&mut tls).await.unwrap();
            assert_eq!(status, 101, "{headers:?}");
            (tls, headers)
        }

        fn remove(&self) {
            let _ = std::fs::remove_dir_all(self.registry.root());
            remove(self.site.exposures());
        }
    }

    /// Reads `body` until it has delivered `wanted`, within `within`.
    async fn read_until<B>(body: &mut B, wanted: &str, within: Duration) -> String
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::fmt::Debug,
    {
        let mut text = String::new();
        timeout(within, async {
            while !text.contains(wanted) {
                let frame = body
                    .frame()
                    .await
                    .expect("the stream is open")
                    .expect("a frame");
                if let Some(data) = frame.data_ref() {
                    text.push_str(std::str::from_utf8(data).unwrap());
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{wanted:?} within {within:?}; got {text:?}"));
        text
    }

    /// A body of unknown size, as a client streams one chunked.
    struct Chunks(VecDeque<Bytes>);

    impl Body for Chunks {
        type Data = Bytes;
        type Error = BoxError;

        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
            Poll::Ready(
                self.get_mut()
                    .0
                    .pop_front()
                    .map(|data| Ok(Frame::data(data))),
            )
        }
    }

    /// A valid session reaches the upstream with every method an application uses, its body
    /// whole and its method as it came; HEAD gets the upstream's head and no body.
    #[tokio::test]
    async fn a_session_reaches_the_upstream_with_every_method() {
        let site = enabled("methods", BOOT, UNTIL).await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let methods = [
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
            Method::from_bytes(b"PROPFIND").unwrap(),
        ];
        for method in methods.clone() {
            let case = method.to_string();
            let sent: &'static [u8] = match method {
                Method::POST | Method::PUT | Method::PATCH => b"name=value&more=1",
                _ => b"",
            };
            let request = with_body(request(method.clone(), "/echo", SHOP, Some(&cookie)), sent);
            let response = answer_to(&site.proxy, request, BOOT + 11).await;
            if method == Method::HEAD {
                assert_eq!(response.status(), StatusCode::OK, "{case}");
                assert!(body_of(response).await.is_empty(), "{case}");
                continue;
            }
            let echo = echoed(response).await;
            assert_eq!(echo["method"], method.as_str(), "{case}");
            assert_eq!(echo["body_bytes"], sent.len(), "{case}");
            assert_eq!(
                echo["body_sha256"],
                crate::access::hex(&Sha256::digest(sent)),
                "{case}"
            );
        }
        let seen: Vec<Method> = site
            .upstream
            .requests()
            .into_iter()
            .map(|seen| seen.method)
            .collect();
        assert_eq!(seen, methods);
        assert_eq!(
            status_of(site.exposures(), BOOT + 11, 0)["proxied"]["2xx"],
            methods.len()
        );
        remove(site.exposures());
    }

    /// Without a session nothing reaches the upstream, and the 401 comes from the headers alone:
    /// it arrives while the body the head announced was never sent, and so does the 403 of a write
    /// from another page.
    #[tokio::test]
    async fn a_request_without_a_session_reaches_no_upstream_and_reads_no_body() {
        let live = Live::start("no-session").await;
        for (case, cookie, origin, status) in [
            ("no cookie", String::new(), "", 401),
            (
                "a cookie that does not verify",
                format!("{}0", live.cookie),
                "",
                401,
            ),
            (
                "another page's write",
                live.cookie.clone(),
                "https://other.hive-k3m9x7q2.fluxbee.ai",
                403,
            ),
        ] {
            let mut tls = dial(live.listen, client(&live.ca, "ingress1"))
                .await
                .unwrap();
            let cookie = if cookie.is_empty() {
                String::new()
            } else {
                format!("Cookie: {cookie}\r\n")
            };
            let origin = if origin.is_empty() {
                String::new()
            } else {
                format!("Origin: {origin}\r\n")
            };
            let head = format!(
                "POST /upload HTTP/1.1\r\nHost: {SHOP}\r\n{cookie}{origin}Content-Length: \
                 1000000\r\nConnection: close\r\n\r\n"
            );
            tls.write_all(head.as_bytes()).await.unwrap();
            tls.flush().await.unwrap();
            let (answered, _) = timeout(Duration::from_secs(2), read_head(&mut tls))
                .await
                .unwrap_or_else(|_| panic!("{case}: answered before any body byte"))
                .unwrap();
            assert_eq!(answered, status, "{case}");
        }
        assert_eq!(live.site.upstream.connections(), 0, "no upstream dialled");
        let status = status_of(live.site.exposures(), live.now, 0);
        assert_eq!(status["refused"], json!({"401": 2, "403": 1}));
        live.remove();
    }

    /// The upstream gets the edge's request as it came: the public host, the edge's forwarding
    /// headers, the query, the body, and every cookie but Fluxbee's own, whatever their case; and
    /// `Connection: close`.
    #[tokio::test]
    async fn the_upstream_gets_the_edge_s_forwarding_headers_and_no_fluxbee_cookie() {
        let site = enabled("forwarding", BOOT, UNTIL).await;
        let session = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let mut sent = request(Method::POST, "/echo?page=2&code=abc", SHOP, None);
        let forwarded = format!("for=203.0.113.7;proto=https;host={SHOP}");
        for (name, value) in [
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", SHOP),
            ("forwarded", forwarded.as_str()),
            ("connection", "close"),
            (
                "cookie",
                &format!("app_session=abc; {session}; __HOST-FLUXBEE_other=1; theme=\"dark mode\""),
            ),
            ("content-type", "application/x-www-form-urlencoded"),
        ] {
            sent.headers_mut().append(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        let echo = echoed(answer_to(&site.proxy, with_body(sent, b"a=1"), BOOT + 11).await).await;
        let headers = &echo["headers"];
        for (name, value) in [
            ("host", SHOP),
            ("x-forwarded-for", "203.0.113.7"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", SHOP),
            ("forwarded", forwarded.as_str()),
            ("x-request-id", "0b5a3c1e-7d2f-4a6b-9c8d-1e2f3a4b5c6d"),
            ("connection", "close"),
            ("cookie", "app_session=abc; theme=\"dark mode\""),
            ("content-type", "application/x-www-form-urlencoded"),
            ("content-length", "3"),
        ] {
            assert_eq!(headers[name], json!([value]), "{name}");
        }
        assert_eq!(echo["query"], "page=2&code=abc");
        assert_eq!(echo["body_bytes"], 3);
        assert!(
            !echo.to_string().to_ascii_lowercase().contains("fluxbee_"),
            "no Fluxbee cookie reaches the upstream: {echo}"
        );
        remove(site.exposures());
    }

    /// An answer streams frame by frame through io.web's port: the first event arrives alone, the
    /// next only once the upstream writes it, with no wait cutting the silence between them.
    #[tokio::test]
    async fn the_answer_streams_frame_by_frame() {
        let live = Live::start("stream").await;
        let (response, _driver) = live.send(live.get("/sse")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "text/event-stream");
        assert!(!response.headers().contains_key(CONTENT_LENGTH));
        let mut body = response.into_body();
        let first = read_until(&mut body, "data: 0\n\n", Duration::from_secs(2)).await;
        assert_eq!(first, "data: 0\n\n");
        assert!(
            timeout(Duration::from_millis(300), body.frame())
                .await
                .is_err(),
            "nothing more until the upstream writes it"
        );
        let open = status_of(live.site.exposures(), live.now, 0)["open"].clone();
        assert_eq!(open, json!({"waiting": 0, "streams": 1, "websockets": 0}));
        live.site.upstream.send_event("1");
        let second = read_until(&mut body, "data: 1\n\n", Duration::from_secs(2)).await;
        assert_eq!(second, "data: 1\n\n");
        live.remove();
    }

    /// A WebSocket's handshake gets its 101 with the handshake's headers and nothing else of the
    /// upstream's; bytes then cross both ways, and a disable drops both sides at once.
    #[tokio::test]
    async fn a_websocket_is_tunnelled_and_cut() {
        let live = Live::start("websocket").await;
        let (mut tls, headers) = live.websocket().await;
        // Exactly one of each, as the edge takes a 101 (stage 2.2 §3.1).
        for (name, value) in [
            ("upgrade", "websocket"),
            ("connection", "upgrade"),
            ("sec-websocket-accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            ("sec-websocket-protocol", "chat"),
        ] {
            let values: Vec<&str> = headers
                .iter()
                .filter(|(header, _)| header == name)
                .map(|(_, value)| value.as_str())
                .collect();
            assert_eq!(values, [value], "{name}");
        }
        assert_eq!(header(&headers, "server"), "");
        let handshake = &live.site.upstream.requests()[0];
        assert_eq!(handshake.headers["upgrade"], "websocket");
        assert_eq!(handshake.headers["connection"], "upgrade");
        assert_eq!(handshake.headers["sec-websocket-key"], WEBSOCKET_KEY);
        assert!(!handshake.headers.contains_key(COOKIE), "no Fluxbee cookie");

        for message in [&b"hello"[..], &b"and again"[..]] {
            tls.write_all(message).await.unwrap();
            tls.flush().await.unwrap();
            let mut echoed = vec![0u8; message.len()];
            timeout(Duration::from_secs(2), tls.read_exact(&mut echoed))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(echoed, message);
        }
        let exposures = live.site.exposures();
        assert_eq!(
            status_of(exposures, live.now, 0)["open"],
            json!({"waiting": 0, "streams": 0, "websockets": 1})
        );

        let answer = exposures
            .apply_push(shop_set(2, Vec::new()), unix_now())
            .await;
        assert!(matches!(answer, IoWebExposuresResponse::Ok { .. }));
        assert!(
            crate::testing::ended(&mut tls, Duration::from_secs(2)).await,
            "the client's side is dropped"
        );
        assert!(
            eventually(Duration::from_secs(2), || live
                .site
                .upstream
                .websockets_closed()
                == 1)
            .await,
            "the upstream's side is dropped"
        );
        let status = status_of(exposures, live.now, 0);
        assert_eq!(status["cut"], 1);
        assert_eq!(status["proxied"]["1xx"], 1);
        assert_eq!(
            status["open"],
            json!({"waiting": 0, "streams": 0, "websockets": 0})
        );
        live.remove();
    }

    /// §13.7's table on an application's answer: `Server` and `X-Powered-By` gone, `nosniff`
    /// added, `frame-ancestors` a header of its own naming Cloud's origin, the application's own
    /// policy untouched, Fluxbee's cookies dropped and the application's kept; the session's
    /// cookie beside them once it slides.
    #[tokio::test]
    async fn the_answer_s_headers_follow_the_table() {
        let site = enabled_with("table", BOOT, UNTIL, &[CLOUD]).await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let values = |response: &Response<WebBody>, name: &str| -> Vec<String> {
            response
                .headers()
                .get_all(name)
                .iter()
                .map(|value| value.to_str().unwrap().to_string())
                .collect()
        };
        let answer = site
            .respond(
                request(Method::GET, "/headers", SHOP, Some(&cookie)),
                BOOT + 11,
            )
            .await;
        assert_eq!(answer.status(), StatusCode::OK);
        assert!(!answer.headers().contains_key("server"));
        assert!(!answer.headers().contains_key("x-powered-by"));
        assert_eq!(values(&answer, "x-content-type-options"), ["nosniff"]);
        assert_eq!(
            values(&answer, "content-security-policy"),
            [
                "default-src 'self'",
                "frame-ancestors 'self' https://cloud.fluxbee.ai"
            ]
        );
        assert_eq!(
            values(&answer, "set-cookie"),
            ["app_session=abc; Path=/; HttpOnly"]
        );
        assert_eq!(
            values(&answer, "content-type"),
            ["text/html; charset=utf-8"]
        );
        assert_eq!(body_of(answer).await, UPSTREAM_PAGE.as_bytes());

        let slid = site
            .respond(
                request(Method::GET, "/headers", SHOP, Some(&cookie)),
                BOOT + 70,
            )
            .await;
        let cookies = values(&slid, "set-cookie");
        assert_eq!(cookies.len(), 2, "{cookies:?}");
        assert_eq!(cookies[0], "app_session=abc; Path=/; HttpOnly");
        assert!(
            cookies[1].starts_with("__Host-fluxbee_session=S1.")
                && cookies[1].ends_with("; Max-Age=1800"),
            "{cookies:?}"
        );
        remove(site.exposures());
    }

    /// A renewal moves a session at its next request (stage 2.2 §4.6): the cookie is minted again
    /// under the newest enablement at once, its `sid` and `issued` kept and `active` now, its
    /// `Max-Age` the slide's under the new end. The old cookie still serves while its enablement
    /// lives, the moved one after it. A newest enablement whose key io.web does not hold moves
    /// nobody.
    #[tokio::test]
    async fn a_renewal_moves_a_session_to_the_new_enablement() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        keys.put(SHOP_ID, B, 1, 3);
        keys.put(ERP_ID, A, 2, 2);
        let first_end = BOOT + 600;
        let site = Site::new(
            holding(
                "renewal",
                keys.clone(),
                BOOT,
                shop_set(1, vec![enablement(SHOP_ENB, first_end, A, 4)]),
            )
            .await,
        )
        .await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let erp = site.sign_in(ERP_ID, ERP_ENB, ERP, BOOT + 10).await;
        let renewed = set(
            2,
            vec![
                exposure(
                    SHOP_ID,
                    SHOP,
                    vec![
                        enablement(SHOP_ENB, first_end, A, 4),
                        enablement(SHOP_RENEWED, UNTIL, B, 1),
                    ],
                ),
                exposure(
                    ERP_ID,
                    ERP,
                    vec![
                        enablement(ERP_ENB, UNTIL, A, 2),
                        enablement(ERP_RENEWED, UNTIL + 60, B, 9),
                    ],
                ),
            ],
        );
        site.exposures().apply_push(renewed, BOOT + 20).await;

        let moved = site.respond(page(SHOP, &cookie), BOOT + 21).await;
        assert_eq!(moved.status(), StatusCode::OK);
        let set = set_cookie(&moved).expect("moved at once, 11 s after its last activity");
        assert!(set.ends_with("; Max-Age=1800"), "{set}");
        let moved = cookie_of(&set);
        let (old, new) = (token_of(&cookie), token_of(&moved));
        assert_eq!(
            (new.e, new.sid, new.issued, new.active),
            (
                hex_id(SHOP_RENEWED).as_str(),
                old.sid,
                old.issued,
                BOOT + 21
            )
        );
        let again = site.respond(page(SHOP, &moved), BOOT + 22).await;
        assert_eq!(again.status(), StatusCode::OK);
        assert_eq!(set_cookie(&again), None, "on the newest already");
        assert_eq!(
            site.respond(page(SHOP, &cookie), BOOT + 30).await.status(),
            StatusCode::OK,
            "the old cookie serves while its enablement lives"
        );
        let refused = site.respond(page(SHOP, &cookie), first_end).await;
        assert_eq!(
            refused.status(),
            StatusCode::UNAUTHORIZED,
            "its enablement ended"
        );
        let at_end = site.respond(page(SHOP, &moved), first_end).await;
        assert_eq!(at_end.status(), StatusCode::OK, "the moved one lives on");

        // ERP's newest key is not read: its session stays on its own enablement.
        let stays = site.respond(page(ERP, &erp), BOOT + 21).await;
        assert_eq!(stays.status(), StatusCode::OK);
        assert_eq!(set_cookie(&stays), None);
        let view = site.exposures().view(BOOT + 22);
        assert_eq!(view["per_exposure"][0]["sessions_moved"], 2);
        assert_eq!(view["per_exposure"][1]["sessions_moved"], 0);
        remove(site.exposures());
    }

    /// An open stream lives through a renewal and through the previous enablement's end, which
    /// leave the exposure with access; the disable after them cuts it.
    #[tokio::test]
    async fn an_open_stream_lives_through_a_renewal() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        keys.put(SHOP_ID, B, 1, 3);
        keys.put(ERP_ID, A, 2, 2);
        let site = Site::new(
            holding(
                "renewal-stream",
                keys,
                BOOT,
                shop_set(1, vec![enablement(SHOP_ENB, UNTIL, A, 4)]),
            )
            .await,
        )
        .await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let response = site
            .respond(request(Method::GET, "/sse", SHOP, Some(&cookie)), BOOT + 11)
            .await;
        let mut body = response.into_body();
        read_until(&mut body, "data: 0\n\n", Duration::from_secs(2)).await;
        for (generation, enablements) in [
            (
                2,
                vec![
                    enablement(SHOP_ENB, UNTIL, A, 4),
                    enablement(SHOP_RENEWED, UNTIL + 3600, B, 1),
                ],
            ),
            (3, vec![enablement(SHOP_RENEWED, UNTIL + 3600, B, 1)]),
        ] {
            site.exposures()
                .apply_push(shop_set(generation, enablements), BOOT + 12)
                .await;
            let event = generation.to_string();
            site.upstream.send_event(&event);
            read_until(
                &mut body,
                &format!("data: {event}\n\n"),
                Duration::from_secs(2),
            )
            .await;
        }
        site.exposures()
            .apply_push(shop_set(4, Vec::new()), BOOT + 13)
            .await;
        let cut = timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("the cut ends the stream at once");
        assert!(matches!(cut, Some(Err(_))), "ended with an error");
        assert!(
            eventually(Duration::from_secs(2), || site.upstream.dropped() == 1).await,
            "the upstream's answer is dropped"
        );
        assert_eq!(status_of(site.exposures(), BOOT + 13, 0)["cut"], 1);
        remove(site.exposures());
    }

    /// A disable cuts at once everything open on the exposure, through io.web's port: a request
    /// waiting for its upstream gets the 401 page, a stream ends with an error, a WebSocket loses
    /// both sides; the upstream sees each one go. The other exposure is not touched.
    #[tokio::test]
    async fn a_disable_cuts_what_is_open_at_once() {
        let live = Live::start("disable-cut").await;
        let waiting = tokio::spawn({
            let request = live.get("/slow");
            let (listen, client) = (live.listen, client(&live.ca, "ingress1"));
            async move { edge_send(listen, client, request).await }
        });
        let (stream, _stream_driver) = live.send(live.get("/sse")).await;
        let mut stream = stream.into_body();
        read_until(&mut stream, "data: 0\n\n", Duration::from_secs(2)).await;
        let (mut websocket, _) = live.websocket().await;
        let exposures = live.site.exposures();
        assert!(
            eventually(Duration::from_secs(2), || status_of(exposures, live.now, 0)
                ["open"]
                == json!({"waiting": 1, "streams": 1, "websockets": 1}))
            .await,
            "{}",
            status_of(exposures, live.now, 0)
        );

        let disabled = std::time::Instant::now();
        exposures
            .apply_push(shop_set(2, Vec::new()), unix_now())
            .await;
        let (waited, _) = timeout(Duration::from_secs(2), waiting)
            .await
            .expect("the wait is answered at once")
            .unwrap();
        let waited = waited.unwrap();
        assert_eq!(waited.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            waited.headers()["set-cookie"],
            crate::access::session_clear_cookie()
        );
        let ended = timeout(Duration::from_secs(2), async {
            loop {
                match stream.frame().await {
                    Some(Ok(_)) => continue,
                    other => break other,
                }
            }
        })
        .await
        .expect("the stream ends at once");
        assert!(matches!(ended, Some(Err(_))), "{ended:?}");
        assert!(crate::testing::ended(&mut websocket, Duration::from_secs(2)).await);
        assert!(disabled.elapsed() < Duration::from_secs(2));
        let upstream = &live.site.upstream;
        assert!(
            eventually(Duration::from_secs(2), || upstream.dropped() == 2
                && upstream.websockets_closed() == 1)
            .await,
            "dropped {}, websockets closed {}",
            upstream.dropped(),
            upstream.websockets_closed()
        );
        let status = status_of(exposures, live.now, 0);
        assert_eq!(status["cut"], 3);
        assert_eq!(
            status["open"],
            json!({"waiting": 0, "streams": 0, "websockets": 0})
        );
        assert_eq!(status_of(exposures, live.now, 1)["cut"], 0);
        live.remove();
    }

    /// io.web's own clock cuts what is open at the last enablement's end, within a second of it;
    /// and a request whose session was checked with a time just before the end, once the clock has
    /// cut, is refused at its registration (§4.7's no race).
    #[tokio::test]
    async fn the_enablement_s_end_cuts_what_is_open() {
        let now = unix_now();
        let end = now + 2;
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        keys.put(ERP_ID, A, 2, 2);
        let exposures = holding(
            "clock",
            keys,
            now - 10,
            shop_set(1, vec![enablement(SHOP_ENB, end, A, 4)]),
        )
        .await;
        let clock = tokio::spawn(run_access_clock(Arc::clone(&exposures)));
        let site = Site::new(exposures).await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, now).await;
        let response = site
            .respond(request(Method::GET, "/sse", SHOP, Some(&cookie)), now)
            .await;
        let mut stream = response.into_body();
        read_until(&mut stream, "data: 0\n\n", Duration::from_secs(2)).await;
        let waiting = tokio::spawn({
            let proxy = Arc::clone(&site.proxy);
            let request = without_body(request(Method::GET, "/slow", SHOP, Some(&cookie)));
            async move { answer_to(&proxy, request, now).await }
        });

        let cut = timeout(Duration::from_secs(5), stream.frame())
            .await
            .expect("cut at the end");
        assert!(matches!(cut, Some(Err(_))));
        let cut_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let end = end as f64;
        assert!(
            (end..end + 1.5).contains(&cut_at),
            "cut at {cut_at}, the end {end}"
        );
        let waited = timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(waited.status(), StatusCode::UNAUTHORIZED);
        // Checked as of `now`, before the end, it would pass; the gate the clock closed refuses it.
        let late = site.respond(page(SHOP, &cookie), now).await;
        assert_eq!(late.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(status_of(site.exposures(), now, 0)["cut"], 2);
        clock.abort();
        remove(site.exposures());
    }

    /// An upstream not reached answers io.web's fixed 502 page, with io.web's own headers, and is
    /// counted: Archi's port closed, a hive gone from the registry, an address SY.admin would
    /// refuse, and an upstream that switches protocols unasked.
    #[tokio::test]
    async fn an_upstream_that_refuses_answers_502() {
        let keys = MapKeys::new();
        keys.put(SHOP_ID, A, 4, 1);
        keys.put(ERP_ID, A, 2, 2);
        let mut hive = exposure(ERP_ID, ERP, vec![enablement(ERP_ENB, UNTIL, A, 2)]);
        hive.upstream = WebExposureUpstream::Hive {
            hive: "worker9".to_string(),
            port: 8069,
        };
        let exposures = holding(
            "refused",
            keys,
            BOOT,
            set(
                1,
                vec![
                    exposure(SHOP_ID, SHOP, vec![enablement(SHOP_ENB, UNTIL, A, 4)]),
                    hive,
                ],
            ),
        )
        .await;
        let shut = proxy(Arc::clone(&exposures), closed_port());
        let code = link(&exposures, SHOP_ID, SHOP_ENB, BOOT + 10);
        let entered = answer_to(&shut, without_body(enter(&code, SHOP)), BOOT + 10).await;
        let shop = cookie_of(&set_cookie(&entered).unwrap());
        let code = link(&exposures, ERP_ID, ERP_ENB, BOOT + 10);
        let entered = answer_to(&shut, without_body(enter(&code, ERP)), BOOT + 10).await;
        let erp = cookie_of(&set_cookie(&entered).unwrap());
        for (case, host, cookie) in [
            ("Archi's port closed", SHOP, &shop),
            ("worker9 gone", ERP, &erp),
        ] {
            let failed = answer_to(&shut, without_body(page(host, cookie)), BOOT + 11).await;
            assert_eq!(failed.status(), StatusCode::BAD_GATEWAY, "{case}");
            assert_eq!(
                failed.headers()["cache-control"],
                "no-store",
                "{case}: io.web's own headers"
            );
            assert_eq!(
                body_of(failed).await,
                crate::web::BAD_GATEWAY_PAGE.as_bytes(),
                "{case}"
            );
        }
        // An upstream that answers 101 to a plain request, or a handshake's 101 without its
        // `Sec-WebSocket-Accept`: io.web passes no switch it did not ask for, nor a broken one.
        let upstream = TestUpstream::start().await;
        let switching = proxy(Arc::clone(&exposures), upstream.address);
        let unasked = answer_to(
            &switching,
            without_body(request(Method::GET, "/ws", SHOP, Some(&shop))),
            BOOT + 12,
        )
        .await;
        assert_eq!(unasked.status(), StatusCode::BAD_GATEWAY);
        let mut handshake = request(Method::GET, "/ws-no-accept", SHOP, Some(&shop));
        for (name, value) in [
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-key", WEBSOCKET_KEY),
            ("sec-websocket-version", "13"),
        ] {
            handshake
                .headers_mut()
                .insert(name, HeaderValue::from_static(value));
        }
        let broken = answer_to(&switching, without_body(handshake), BOOT + 12).await;
        assert_eq!(broken.status(), StatusCode::BAD_GATEWAY);
        let view = exposures.view(BOOT + 12);
        assert_eq!(view["per_exposure"][0]["upstream_failures"], 3);
        assert_eq!(view["per_exposure"][0]["proxied"]["1xx"], 0);
        assert_eq!(view["per_exposure"][1]["upstream_failures"], 1);
        remove(&exposures);
    }

    /// An upstream's chunked answer that also says `Content-Length` loses the length: hyper's
    /// client decodes the chunks but keeps the header, which io.web's server would take for the
    /// length. The edge gets the whole body, framed again.
    #[tokio::test]
    async fn an_upstream_s_chunked_answer_loses_its_content_length() {
        let raw = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = raw.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = raw.accept().await {
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        if tcp.read(&mut byte).await.unwrap_or(0) == 0 {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    let _ = tcp
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: \
                              chunked\r\nContent-Length: 3\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
                        )
                        .await;
                });
            }
        });
        let live = Live::start("framing").await;
        let framing = proxy(Arc::clone(&live.site.proxy.exposures), address);
        let answer = answer_to(
            &framing,
            without_body(request(Method::GET, "/", SHOP, Some(&live.cookie))),
            live.now,
        )
        .await;
        assert!(!answer.headers().contains_key(CONTENT_LENGTH));
        assert!(!answer.headers().contains_key("transfer-encoding"));
        assert_eq!(body_of(answer).await, "hello world");

        // Through the port, as the edge reads it.
        let ca = MeshCa::generate().unwrap();
        let registry = registry();
        let (listen, _) = start_serving(&ca, &registry, crate::testing::limits(), framing).await;
        let (response, _driver) = edge_send(listen, client(&ca, "ingress1"), live.get("/")).await;
        let response = response.unwrap();
        assert!(!response.headers().contains_key(CONTENT_LENGTH));
        assert_eq!(body_of(response).await, "hello world");
        let _ = std::fs::remove_dir_all(registry.root());
        live.remove();
    }

    /// An empty POST keeps the `Content-Length: 0` it came with, since hyper sends none for an
    /// empty body; any other body is framed by hyper by its size, chunked when the size is not
    /// known.
    #[tokio::test]
    async fn an_empty_post_keeps_its_content_length_0() {
        let site = enabled("framing-request", BOOT, UNTIL).await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let post = |length: Option<&str>| {
            let mut post = request(Method::POST, "/echo", SHOP, Some(&cookie));
            if let Some(length) = length {
                post.headers_mut()
                    .insert(CONTENT_LENGTH, HeaderValue::from_str(length).unwrap());
            }
            post
        };
        let zero =
            echoed(answer_to(&site.proxy, without_body(post(Some("0"))), BOOT + 11).await).await;
        assert_eq!(zero["headers"]["content-length"], json!(["0"]));
        let none = echoed(answer_to(&site.proxy, without_body(post(None)), BOOT + 11).await).await;
        assert!(none["headers"].get("content-length").is_none(), "{none}");
        let sized = echoed(
            answer_to(
                &site.proxy,
                post(Some("99"))
                    .map(|()| crate::testing::boxed(Full::new(Bytes::from_static(b"abc")))),
                BOOT + 11,
            )
            .await,
        )
        .await;
        assert_eq!(
            sized["headers"]["content-length"],
            json!(["3"]),
            "hyper's own length"
        );
        assert_eq!(sized["body_bytes"], 3);
        let chunks = Chunks(VecDeque::from([
            Bytes::from_static(b"one,"),
            Bytes::from_static(b"two"),
        ]));
        let streamed = echoed(
            answer_to(
                &site.proxy,
                post(None).map(|()| crate::testing::boxed(chunks)),
                BOOT + 11,
            )
            .await,
        )
        .await;
        assert_eq!(streamed["headers"]["transfer-encoding"], json!(["chunked"]));
        assert_eq!(streamed["body_bytes"], 7);
        remove(site.exposures());
    }

    /// A body that errors at its first frame's heels, as the edge passes one it could not finish.
    struct Broken(bool);

    impl Body for Broken {
        type Data = Bytes;
        type Error = BoxError;

        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
            let sent = std::mem::replace(&mut self.get_mut().0, true);
            Poll::Ready(Some(if sent {
                Err("the edge could not finish the body".into())
            } else {
                Ok(Frame::data(Bytes::from_static(b"partial")))
            }))
        }
    }

    /// A client's body the edge could not finish (an aborted upload, a client below its pace)
    /// reaches io.web as a body that errors: the edge answered the client itself, so io.web drops
    /// the request, `client_closed`, and counts no upstream failure.
    #[tokio::test]
    async fn a_client_body_that_breaks_drops_its_request() {
        let site = enabled("broken-body", BOOT, UNTIL).await;
        let cookie = site.sign_in(SHOP_ID, SHOP_ENB, SHOP, BOOT + 10).await;
        let upload = request(Method::POST, "/echo", SHOP, Some(&cookie))
            .map(|()| crate::testing::boxed(Broken(false)));
        let _ = answer_to(&site.proxy, upload, BOOT + 11).await;
        let status = status_of(site.exposures(), BOOT + 11, 0);
        assert_eq!(status["upstream_failures"], 0, "{status}");
        assert_eq!(status["proxied"]["2xx"], 0, "{status}");
        assert_eq!(
            status["open"],
            json!({"waiting": 0, "streams": 0, "websockets": 0})
        );
        remove(site.exposures());
    }

    #[test]
    fn the_endings_read_as_the_line_names_them() {
        for (ending, name) in [
            (Ending::Complete, "complete"),
            (Ending::ClientClosed, "client_closed"),
            (Ending::UpstreamClosed, "upstream_closed"),
            (Ending::Cut, "cut"),
            (Ending::Refused401, "refused_401"),
            (Ending::Refused403, "refused_403"),
            (Ending::UpstreamFailed("connect"), "upstream_failed:connect"),
        ] {
            assert_eq!(ending.to_string(), name);
        }
    }
}
