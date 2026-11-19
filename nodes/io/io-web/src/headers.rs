//! The headers io.web sets and strips (docs/io-web-v2.md §13.7; docs/io-web-v2-stage-2-2.md
//! §4.3-§4.5): on its own answers, on the request it forwards to an upstream, and on the upstream's
//! answer.
//!
//! - **Its own answers** (the 401, 403 and 502 pages, the link's page, `/.fluxbee/*`): the edge
//!   door's strict set, `Permissions-Policy` with every feature off, and a policy whose
//!   `frame-ancestors` names Cloud's origins, `'none'` without one.
//! - **The request to an upstream:** the edge's headers as they come, the hop-by-hop ones stripped
//!   again, `Connection: close` or a WebSocket's pair, and `Cookie` without Fluxbee's own pairs.
//! - **The upstream's answer:** its headers untouched but the hop-by-hop ones, Fluxbee's own
//!   cookies, `Server` and `X-Powered-By`; `nosniff` when missing, and a `frame-ancestors` policy of
//!   io.web's own beside the application's.
//!
//! An origin is written into a header only once it passes `is_frame_origin`, so nothing Cloud's set
//! carries is ever injected into one.

use hyper::header::{
    HeaderMap, HeaderName, HeaderValue, CONNECTION, CONTENT_LENGTH, CONTENT_SECURITY_POLICY,
    COOKIE, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_EXTENSIONS, SEC_WEBSOCKET_PROTOCOL, SERVER,
    SET_COOKIE, TRANSFER_ENCODING, UPGRADE, X_CONTENT_TYPE_OPTIONS,
};
use hyper::Method;

/// io.web's own answers' policy before its `frame-ancestors`: it loads nothing, its own origin
/// included; no `<base>` and no form target. `base-uri`, `form-action` and `frame-ancestors` do not
/// fall back to `default-src`, so each is named.
pub const OWN_POLICY: &str = "default-src 'none'; base-uri 'none'; form-action 'none'";

/// Every feature a page can ask for, off (io-web-v2 §13.7): io.web's own pages are text and use
/// none. Never on an application's answer, where it could turn off something the application uses.
pub const PERMISSIONS_POLICY: &str = "accelerometer=(), ambient-light-sensor=(), autoplay=(), \
     bluetooth=(), camera=(), display-capture=(), encrypted-media=(), fullscreen=(), gamepad=(), \
     geolocation=(), gyroscope=(), hid=(), idle-detection=(), local-fonts=(), magnetometer=(), \
     microphone=(), midi=(), payment=(), picture-in-picture=(), publickey-credentials-create=(), \
     publickey-credentials-get=(), screen-wake-lock=(), serial=(), storage-access=(), usb=(), \
     web-share=(), window-management=(), xr-spatial-tracking=()";

/// Every own answer's headers but its policy (`Framing::own_policy`): the edge door's set, and
/// `Permissions-Policy`.
pub const SECURITY_HEADERS: [(&str, &str); 5] = [
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("cache-control", "no-store"),
    ("x-robots-tag", "noindex, nofollow, noarchive"),
    ("permissions-policy", PERMISSIONS_POLICY),
];

/// The hop-by-hop headers (RFC 9110 §7.6.1, and the older `Keep-Alive` and `Proxy-Connection`), as
/// the edge strips them: they describe one connection, so they never cross io.web in either
/// direction, and neither does any header a `Connection` header names (`strip_hop_by_hop`).
const HOP_BY_HOP_HEADERS: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "proxy-authorization",
    "proxy-authenticate",
];

/// The start of Fluxbee's own cookie names, lowercase as compared: matched in any case, since a
/// browser that compares the `__Host-` prefix case-sensitively keeps a `__HOST-` cookie from any
/// host (§4.3, §4.4).
const FLUXBEE_COOKIE_PREFIX: &[u8] = b"__host-fluxbee_";

const X_POWERED_BY: &str = "x-powered-by";

/// What Cloud's origins give io.web's headers (§4.4, §4.5), built once per set.
#[derive(Debug, Clone)]
pub struct Framing {
    /// The origins written in the headers: Cloud's, those that pass `is_frame_origin`.
    pub origins: Vec<String>,
    /// io.web's own answers' policy: `OWN_POLICY`, then `frame-ancestors` naming the origins, or
    /// `'none'` without one, so a 401 shows inside Cloud's frame and nowhere else.
    pub own_policy: HeaderValue,
    /// The application's answers' header of io.web's own: `frame-ancestors 'self'` and the
    /// origins. `'self'` keeps an application able to frame its own pages; the browser enforces
    /// every policy header, so the application's own stay as they are.
    pub application: HeaderValue,
}

impl Framing {
    /// From Cloud's origins as the set holds them. Each is checked before it is written; the ones
    /// that fail are returned, to log, and none of them reaches a header.
    pub fn new(cloud_origins: &[String]) -> (Self, Vec<String>) {
        let (origins, skipped): (Vec<String>, Vec<String>) = cloud_origins
            .iter()
            .cloned()
            .partition(|origin| is_frame_origin(origin));
        let named = origins.join(" ");
        let own = if named.is_empty() {
            format!("{OWN_POLICY}; frame-ancestors 'none'")
        } else {
            format!("{OWN_POLICY}; frame-ancestors {named}")
        };
        let application = if named.is_empty() {
            "frame-ancestors 'self'".to_string()
        } else {
            format!("frame-ancestors 'self' {named}")
        };
        // Checked origins are ASCII letters, digits, `-`, `.`, `:` and `/`: always a valid value.
        let value = |text: String, fallback: &'static str| {
            HeaderValue::try_from(text).unwrap_or_else(|_| HeaderValue::from_static(fallback))
        };
        let framing = Self {
            own_policy: value(
                own,
                "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            ),
            application: value(application, "frame-ancestors 'self'"),
            origins,
        };
        (framing, skipped)
    }
}

impl Default for Framing {
    fn default() -> Self {
        Self::new(&[]).0
    }
}

/// Whether `origin` may be written into a header (§4.4): `https://`, then a lowercase host of LDH
/// labels and dots, then an optional `:port`. SY.admin checks Cloud's origins too; io.web checks
/// again what it writes.
pub fn is_frame_origin(origin: &str) -> bool {
    let Some(authority) = origin.strip_prefix("https://") else {
        return false;
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    let port_ok = port.is_none_or(|port| {
        !port.is_empty()
            && port.len() <= 5
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && port.parse::<u16>().is_ok_and(|port| port != 0)
    });
    port_ok && is_ldh_host(host)
}

/// One or more LDH labels (lowercase letters, digits and inner hyphens, 1 to 63 each) joined by
/// dots, 253 characters at most.
fn is_ldh_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// io.web's own answer's headers: the policy for `framing`'s origins, then `SECURITY_HEADERS`.
pub fn set_own_headers(headers: &mut HeaderMap, framing: &Framing) {
    headers.insert(CONTENT_SECURITY_POLICY, framing.own_policy.clone());
    for (name, value) in SECURITY_HEADERS {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
}

/// A WebSocket handshake that io.web tunnels (§4.3): a GET whose `Connection` names `upgrade` and
/// whose `Upgrade` names `websocket`, as the edge passes it. Any other upgrade goes on as a plain
/// request, its `Upgrade` stripped as a hop-by-hop header.
pub fn is_websocket_handshake(method: &Method, headers: &HeaderMap) -> bool {
    *method == Method::GET
        && names_token(headers, &CONNECTION, "upgrade")
        && names_token(headers, &UPGRADE, "websocket")
}

/// Whether one of `name`'s values lists `token`, in any case (a comma-separated list).
pub fn names_token(headers: &HeaderMap, name: &HeaderName, token: &str) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|item| item.trim().eq_ignore_ascii_case(token))
}

/// Remove the hop-by-hop headers (`HOP_BY_HOP_HEADERS`) and every header a `Connection` header
/// names, from a request or a response.
pub fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in HOP_BY_HOP_HEADERS {
        headers.remove(name);
    }
}

/// The headers a request goes to its upstream with (§4.3): the edge's as they came (`Host`, the
/// public host, and the edge's forwarding headers among them), the hop-by-hop ones stripped
/// again, then `Connection: close`, or `Connection: upgrade` and `Upgrade: websocket` for a
/// WebSocket's handshake; `Cookie` without Fluxbee's own pairs. `Content-Length` goes, and hyper
/// frames the body by its size, as the edge does, except an empty body's `Content-Length: 0`:
/// hyper sends none for an empty body, and some servers answer 411 to a POST without it.
pub fn upstream_request_headers(inbound: &HeaderMap, websocket: bool) -> HeaderMap {
    let mut headers = inbound.clone();
    let zero = content_length_is_zero(&headers);
    strip_hop_by_hop(&mut headers);
    headers.remove(CONTENT_LENGTH);
    if zero {
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
    }
    strip_fluxbee_cookies(&mut headers);
    if websocket {
        headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    } else {
        headers.insert(CONNECTION, HeaderValue::from_static("close"));
    }
    headers
}

/// Every `Content-Length` the request carries says 0, and there is one.
fn content_length_is_zero(headers: &HeaderMap) -> bool {
    let mut lengths = headers.get_all(CONTENT_LENGTH).iter().peekable();
    lengths.peek().is_some()
        && lengths.all(|value| {
            value
                .to_str()
                .ok()
                .and_then(|length| length.trim().parse::<u64>().ok())
                == Some(0)
        })
}

/// `Cookie` without Fluxbee's own pairs (§4.3): every pair whose name begins with
/// `__Host-fluxbee_`, in any case, goes, and the header goes when no pair is left. The others keep
/// their order and their bytes, in one header joined by `; `, as the edge sends them.
pub fn strip_fluxbee_cookies(headers: &mut HeaderMap) {
    if !headers.contains_key(COOKIE) {
        return;
    }
    let values: Vec<HeaderValue> = headers.get_all(COOKIE).iter().cloned().collect();
    let kept: Vec<&[u8]> = values
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b';'))
        .map(<[u8]>::trim_ascii)
        .filter(|pair| !pair.is_empty() && !is_fluxbee_cookie_name(cookie_name(pair)))
        .collect();
    headers.remove(COOKIE);
    if kept.is_empty() {
        return;
    }
    // The pairs' own bytes, from valid header values, and the separators: a valid value.
    if let Ok(value) = HeaderValue::from_bytes(&kept.join(&b"; "[..])) {
        headers.insert(COOKIE, value);
    }
}

/// A cookie pair's name: what comes before its first `=`, or the whole pair without one (a
/// nameless cookie, which a browser sends as its value alone).
fn cookie_name(pair: &[u8]) -> &[u8] {
    pair.iter()
        .position(|byte| *byte == b'=')
        .map_or(pair, |equals| &pair[..equals])
        .trim_ascii()
}

fn is_fluxbee_cookie_name(name: &[u8]) -> bool {
    name.len() >= FLUXBEE_COOKIE_PREFIX.len()
        && name[..FLUXBEE_COOKIE_PREFIX.len()].eq_ignore_ascii_case(FLUXBEE_COOKIE_PREFIX)
}

/// Whether an upstream's `Set-Cookie` would set one of Fluxbee's own cookies (§4.4): a name that
/// begins with `__Host-fluxbee_`, in any case; or no name and a value that begins so
/// (`Set-Cookie: =__Host-fluxbee_…`, or a pair without `=`), which a browser keeps as a nameless
/// cookie and sends back as `__Host-fluxbee_…` (RFC 6265bis §5.6).
fn sets_fluxbee_cookie(set_cookie: &[u8]) -> bool {
    let pair = set_cookie
        .split(|byte| *byte == b';')
        .next()
        .unwrap_or_default()
        .trim_ascii();
    match pair.iter().position(|byte| *byte == b'=') {
        Some(equals) => {
            let name = pair[..equals].trim_ascii();
            let value = pair[equals + 1..].trim_ascii();
            is_fluxbee_cookie_name(name) || (name.is_empty() && is_fluxbee_cookie_name(value))
        }
        None => is_fluxbee_cookie_name(pair),
    }
}

/// The upstream's answer's headers as io.web sends them on (§4.4, §13.7's table, the
/// application's column):
///
/// - a `Content-Length` that a `Transfer-Encoding` overrides goes (RFC 9112 §6.3): hyper's client
///   decodes by `Transfer-Encoding` but keeps the `Content-Length` beside it, which io.web's
///   server would take for the length; then the hop-by-hop headers go, and hyper frames the body
///   again, as the edge does;
/// - every `Set-Cookie` of one of Fluxbee's own cookies goes (`sets_fluxbee_cookie`);
/// - `Server` and `X-Powered-By` go;
/// - `X-Content-Type-Options: nosniff` comes when the answer has none;
/// - `framing`'s `frame-ancestors` comes as a policy header of its own, beside the application's.
///
/// The rest stays as the application sent it. The edge adds HSTS.
pub fn set_application_headers(headers: &mut HeaderMap, framing: &Framing) {
    if headers.contains_key(TRANSFER_ENCODING) {
        headers.remove(CONTENT_LENGTH);
    }
    strip_hop_by_hop(headers);
    if headers.contains_key(SET_COOKIE) {
        let kept: Vec<HeaderValue> = headers
            .get_all(SET_COOKIE)
            .iter()
            .filter(|value| !sets_fluxbee_cookie(value.as_bytes()))
            .cloned()
            .collect();
        headers.remove(SET_COOKIE);
        for value in kept {
            headers.append(SET_COOKIE, value);
        }
    }
    headers.remove(SERVER);
    headers.remove(X_POWERED_BY);
    if !headers.contains_key(X_CONTENT_TYPE_OPTIONS) {
        headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    }
    headers.append(CONTENT_SECURITY_POLICY, framing.application.clone());
}

/// Whether the upstream's 101 switches to a WebSocket, the one upgrade io.web asks for.
pub fn upgrades_to_websocket(headers: &HeaderMap) -> bool {
    names_token(headers, &UPGRADE, "websocket")
}

/// The headers of io.web's 101 to a WebSocket's handshake (§4.3, §3.1): `Upgrade: websocket` and
/// `Connection: upgrade`, which hyper adds to no 101, and the upstream's `Sec-WebSocket-Accept`,
/// `-Protocol` and `-Extensions`, as it sent them. Nothing else of a 101 crosses the edge.
pub fn websocket_answer_headers(upstream: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
    for name in [
        SEC_WEBSOCKET_ACCEPT,
        SEC_WEBSOCKET_PROTOCOL,
        SEC_WEBSOCKET_EXTENSIONS,
    ] {
        for value in upstream.get_all(&name) {
            headers.append(name.clone(), value.clone());
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    fn values<'a>(headers: &'a HeaderMap, name: &str) -> Vec<&'a str> {
        headers
            .get_all(name)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect()
    }

    /// An origin is written only as `https://`, a lowercase LDH host and an optional port;
    /// anything else, which could carry a separator or a quote into a header, is skipped and
    /// returned to log.
    #[test]
    fn only_a_well_formed_origin_reaches_a_header() {
        for good in [
            "https://cloud.fluxbee.ai",
            "https://fluxbee.ai:8443",
            "https://a-b.c1.example",
            "https://localhost",
        ] {
            assert!(is_frame_origin(good), "{good}");
        }
        for bad in [
            "",
            "http://cloud.fluxbee.ai",
            "https://",
            "https://Cloud.fluxbee.ai",
            "https://cloud.fluxbee.ai/",
            "https://cloud.fluxbee.ai/path",
            "https://cloud.fluxbee.ai:",
            "https://cloud.fluxbee.ai:0",
            "https://cloud.fluxbee.ai:65536",
            "https://cloud.fluxbee.ai:443:1",
            "https://*.fluxbee.ai",
            "https://-cloud.fluxbee.ai",
            "https://cloud..fluxbee.ai",
            "https://cloud.fluxbee.ai.",
            "https://user@cloud.fluxbee.ai",
            "https://cloud.fluxbee.ai 'unsafe-inline'",
            "https://cloud.fluxbee.ai;script-src *",
            "https://[::1]",
            "https://cloud.fluxbee.ai\r\nx: y",
        ] {
            assert!(!is_frame_origin(bad), "{bad:?}");
        }
        let origins = vec![
            "https://cloud.fluxbee.ai".to_string(),
            "https://x;y".to_string(),
            "https://fluxbee.ai:8443".to_string(),
        ];
        let (framing, skipped) = Framing::new(&origins);
        assert_eq!(skipped, ["https://x;y"]);
        assert_eq!(
            framing.own_policy,
            "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors \
             https://cloud.fluxbee.ai https://fluxbee.ai:8443"
        );
        assert_eq!(
            framing.application,
            "frame-ancestors 'self' https://cloud.fluxbee.ai https://fluxbee.ai:8443"
        );
        let (none, skipped) = Framing::new(&[]);
        assert!(skipped.is_empty());
        assert_eq!(
            none.own_policy,
            "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        );
        assert_eq!(none.application, "frame-ancestors 'self'");
    }

    /// The request's headers to the upstream: the edge's as they came, the hop-by-hop ones and
    /// those `Connection` names gone, `Connection: close` or the upgrade's pair, and no Fluxbee
    /// cookie, in any case, its name's or as a nameless pair.
    #[test]
    fn the_upstream_gets_the_edge_s_headers_without_fluxbee_s_cookies() {
        let inbound = map(&[
            ("host", "shop.hive-k3m9x7q2.fluxbee.ai"),
            ("x-forwarded-for", "203.0.113.7"),
            ("x-forwarded-proto", "https"),
            ("x-forwarded-host", "shop.hive-k3m9x7q2.fluxbee.ai"),
            (
                "forwarded",
                "for=203.0.113.7;proto=https;host=shop.hive-k3m9x7q2.fluxbee.ai",
            ),
            ("x-request-id", "0b5a3c1e-7d2f-4a6b-9c8d-1e2f3a4b5c6d"),
            ("connection", "close, x-hop"),
            ("x-hop", "1"),
            ("keep-alive", "timeout=5"),
            ("te", "trailers"),
            ("transfer-encoding", "chunked"),
            ("content-length", "12"),
            (
                "cookie",
                "app_session=abc; __Host-fluxbee_session=S1.x; theme=\"dark mode\"",
            ),
            (
                "cookie",
                "__HOST-FLUXBEE_other=1;lang=en ;  __host-fluxbee_session; =v",
            ),
            ("accept", "text/html"),
        ]);
        let headers = upstream_request_headers(&inbound, false);
        for kept in [
            "host",
            "x-forwarded-for",
            "x-forwarded-proto",
            "x-forwarded-host",
            "forwarded",
            "x-request-id",
            "accept",
        ] {
            assert_eq!(values(&headers, kept), values(&inbound, kept), "{kept}");
        }
        for gone in [
            "x-hop",
            "keep-alive",
            "te",
            "transfer-encoding",
            "content-length",
            "upgrade",
        ] {
            assert!(!headers.contains_key(gone), "{gone}");
        }
        assert_eq!(values(&headers, "connection"), ["close"]);
        assert_eq!(
            values(&headers, "cookie"),
            ["app_session=abc; theme=\"dark mode\"; lang=en; =v"]
        );

        let only_ours = map(&[("cookie", "__Host-fluxbee_session=S1.x")]);
        assert!(!upstream_request_headers(&only_ours, false).contains_key("cookie"));

        let handshake = map(&[
            ("connection", "Upgrade"),
            ("upgrade", "WebSocket"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ("sec-websocket-version", "13"),
        ]);
        assert!(is_websocket_handshake(&Method::GET, &handshake));
        assert!(!is_websocket_handshake(&Method::POST, &handshake));
        let headers = upstream_request_headers(&handshake, true);
        assert_eq!(values(&headers, "connection"), ["upgrade"]);
        assert_eq!(values(&headers, "upgrade"), ["websocket"]);
        assert_eq!(
            values(&headers, "sec-websocket-key"),
            ["dGhlIHNhbXBsZSBub25jZQ=="]
        );
        // Another upgrade goes on as a plain request.
        let h2c = map(&[("connection", "upgrade"), ("upgrade", "h2c")]);
        assert!(!is_websocket_handshake(&Method::GET, &h2c));
        let headers = upstream_request_headers(&h2c, false);
        assert!(!headers.contains_key("upgrade"));
        assert_eq!(values(&headers, "connection"), ["close"]);
    }

    /// An empty body's `Content-Length: 0` goes on as it came; any other length goes, and hyper
    /// frames the body.
    #[test]
    fn only_an_empty_body_keeps_its_content_length() {
        let zero = map(&[("content-length", "0")]);
        assert_eq!(
            values(&upstream_request_headers(&zero, false), "content-length"),
            ["0"]
        );
        for length in ["12", "", "abc"] {
            let headers = map(&[("content-length", length)]);
            assert!(
                !upstream_request_headers(&headers, false).contains_key("content-length"),
                "{length:?}"
            );
        }
    }

    /// §13.7's table for an application's answer: the hop-by-hop headers and a `Content-Length`
    /// a `Transfer-Encoding` overrides go; Fluxbee's own cookies, `Server` and `X-Powered-By` go;
    /// `nosniff` comes only when missing; `frame-ancestors` is a header of its own, beside the
    /// application's policy, untouched.
    #[test]
    fn the_application_s_answer_follows_the_table() {
        let (framing, _) = Framing::new(&["https://cloud.fluxbee.ai".to_string()]);
        let mut headers = map(&[
            ("content-type", "text/html"),
            ("transfer-encoding", "chunked"),
            ("content-length", "5"),
            ("connection", "keep-alive, x-hop"),
            ("x-hop", "1"),
            ("server", "nginx/1.25"),
            ("x-powered-by", "Express"),
            ("content-security-policy", "default-src 'self'"),
            ("set-cookie", "app_session=abc; Path=/; HttpOnly"),
            (
                "set-cookie",
                "__Host-fluxbee_session=planted; Path=/; Secure",
            ),
            ("set-cookie", "__HOST-FLUXBEE_other=1; Path=/"),
            ("set-cookie", "=__Host-fluxbee_session=nameless; Path=/"),
            ("set-cookie", "__host-fluxbee_session"),
            ("set-cookie", "theme=dark"),
            ("cache-control", "max-age=60"),
        ]);
        set_application_headers(&mut headers, &framing);
        for gone in [
            "transfer-encoding",
            "content-length",
            "connection",
            "x-hop",
            "server",
            "x-powered-by",
        ] {
            assert!(!headers.contains_key(gone), "{gone}");
        }
        assert_eq!(
            values(&headers, "set-cookie"),
            ["app_session=abc; Path=/; HttpOnly", "theme=dark"]
        );
        assert_eq!(values(&headers, "x-content-type-options"), ["nosniff"]);
        assert_eq!(
            values(&headers, "content-security-policy"),
            [
                "default-src 'self'",
                "frame-ancestors 'self' https://cloud.fluxbee.ai"
            ]
        );
        assert_eq!(values(&headers, "cache-control"), ["max-age=60"]);
        assert_eq!(values(&headers, "content-type"), ["text/html"]);

        // A length the answer announces alone stays; the application's own `nosniff` is not
        // doubled.
        let mut sized = map(&[
            ("content-length", "5"),
            ("x-content-type-options", "nosniff"),
        ]);
        set_application_headers(&mut sized, &Framing::default());
        assert_eq!(values(&sized, "content-length"), ["5"]);
        assert_eq!(values(&sized, "x-content-type-options"), ["nosniff"]);
        assert_eq!(
            values(&sized, "content-security-policy"),
            ["frame-ancestors 'self'"]
        );
    }

    /// The 101's headers: the upgrade's pair, and the upstream's `Sec-WebSocket-*` answer, nothing
    /// else.
    #[test]
    fn a_websocket_s_101_keeps_its_handshake_headers() {
        let upstream = map(&[
            ("upgrade", "websocket"),
            ("connection", "Upgrade"),
            ("sec-websocket-accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            ("sec-websocket-protocol", "chat"),
            ("sec-websocket-extensions", "permessage-deflate"),
            ("server", "ws/1"),
            ("set-cookie", "__Host-fluxbee_session=planted"),
        ]);
        assert!(upgrades_to_websocket(&upstream));
        assert!(!upgrades_to_websocket(&map(&[("upgrade", "h2c")])));
        let headers = websocket_answer_headers(&upstream);
        assert_eq!(headers.len(), 5);
        assert_eq!(values(&headers, "upgrade"), ["websocket"]);
        assert_eq!(values(&headers, "connection"), ["upgrade"]);
        assert_eq!(
            values(&headers, "sec-websocket-accept"),
            ["s3pPLMBiTxaQ9kYGzzhZRbK+xOo="]
        );
        assert_eq!(values(&headers, "sec-websocket-protocol"), ["chat"]);
        assert_eq!(
            values(&headers, "sec-websocket-extensions"),
            ["permessage-deflate"]
        );
    }

    /// io.web's own answers carry the strict set, `Permissions-Policy` and the policy for the
    /// set's origins.
    #[test]
    fn own_answers_carry_the_strict_set_and_the_origins() {
        let (framing, _) = Framing::new(&["https://cloud.fluxbee.ai".to_string()]);
        let mut headers = HeaderMap::new();
        set_own_headers(&mut headers, &framing);
        assert_eq!(
            values(&headers, "content-security-policy"),
            [
                "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors \
                 https://cloud.fluxbee.ai"
            ]
        );
        for (name, value) in SECURITY_HEADERS {
            assert_eq!(values(&headers, name), [value], "{name}");
        }
        assert!(PERMISSIONS_POLICY
            .split(", ")
            .all(|feature| feature.ends_with("=()")));
    }
}
