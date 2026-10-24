# SY.edge

SY.edge is the public door of a Fluxbee mesh. On the ingress hive it serves HTTPS and turns each
request on an externalized channel (`/e/<ich>`) into a mesh message for the IO node that owns the
channel, then returns that node's reply. It also serves the static files that `SY.admin` published
for producers (`/public/<key>`). It resolves no identity: it forwards by the owner name SY.admin
gave it, and it takes commands only from `SY.admin@motherbee`. Its certificate comes from SY.vault;
it swaps a new one in place and answers the ACME TLS-ALPN-01 challenge SY.admin arms when it
orders one ([ACME design](../acme-edge-certificates-v1.md) §4). It chooses each request's host
before any path: only its main name, an IP literal or no host reach `/e/` and `/public/`; the web
hosts SY.admin declares (`archi.<id>`) go to io.web on the motherbee, through a reverse proxy over
the mesh's mutual TLS, its third surface ([io.web v2](../io-web-v2.md) §11.2); any other host
answers 404.

Source: `src/bin/sy_edge.rs`; the certificate checks it shares with SY.admin are in
`src/tls_material.rs`. The URL commands it receives are sent by `SY.admin`
(`handle_externalize`, `handle_unexternalize`, `handle_list_externalized`,
`handle_publish_artifact` in `src/bin/sy_admin.rs`).

| | |
| --- | --- |
| Binary | `/usr/bin/sy-edge` (`packaging/build-deb.sh`, `RUST_BINS` entry `sy-edge:sy_edge`) |
| Node name | `SY.edge@<hive>` (`NODE_NAME`, default `SY.edge`, in `Config::load`) |
| Role | ingress (`system_nodes.ingress` in `packaging/hive.yaml.example`) |
| systemd unit | `sy-edge.service`: `After=network.target rt-gateway.service`, `Wants=rt-gateway.service`, `Restart=always`, `RestartSec=5`, `TimeoutStopSec=15` |
| Listens on | `edge.listen` from hive.yaml, ingress role only (`resolve_http_listen`); `add_hive` asks for it, e.g. `0.0.0.0:443` |
| Dials | io.web, for the web hosts only: the host of `wan.uplinks[0].address` on port 9200 (`IO_WEB_PORT`), over the mesh's mutual TLS (see The proxy to io.web) |
| State | `/etc/fluxbee/edge.endpoints.json`, `/var/lib/fluxbee/state/sy-edge/publications.json`, `/var/lib/fluxbee/state/sy-edge/web_routes.json`, reads `/var/lib/fluxbee/blob/public` |

## Where it runs

- The ingress hive's node list comes from the motherbee's hive.yaml template
  `system_nodes.ingress` (`SY.config.routes`, `SY.opa.rules`, `SY.edge` in
  `packaging/hive.yaml.example`). `add_hive` with `role=ingress` reads that template
  (`system_nodes_for_role(&local_hive, HiveRole::Ingress)` in `src/bin/sy_orchestrator.rs`) and
  renders it into the ingress hive.yaml.
- The binary itself does not refuse a role. It always connects to the router, but it binds the
  public listener only when `role` is `ingress`, or when `SY_EDGE_HTTP_LISTEN` is set
  (`resolve_http_listen`). On any other role it serves no door: a VAULT_SECRET_CHANGED for its
  TLS key is ignored (no `EdgeTls`, so `tls_secret_change_action` gets no key), and the TLS
  commands are answered with an error.
- There is no SY.identity on the ingress hive. The edge computes its own ILK from its L2 name
  (`fluxbee_sdk::deterministic_system_ilk_id` in `main`), uses it to read its secrets from the
  vault, and stamps it as `meta.src_ilk` on the requests it forwards to a channel owner.

## How it starts

On a spoke the unit file is written by the orchestrator: `core_service_unit_contents` in
`src/bin/sy_orchestrator.rs` gives `sy-edge` and `sy-orchestrator` (and no other unit)
`After=network.target rt-gateway.service` and `Wants=rt-gateway.service`. The packaged unit on the
motherbee (`gen_unit sy-edge ...` in `packaging/build-deb.sh`) has the same ordering. Neither
orders `sy-vault`: the vault is on the motherbee. A vault edge does not need it at boot anyway: when
the secret cannot be read it starts AWAITING and re-reads on its own (see TLS below).
`Restart=always` remains the retry for a crash and for a disk edge whose files do not load.

`main` in `src/bin/sy_edge.rs`, in order:

1. `Config::load` reads `/etc/fluxbee/hive.yaml` (`role`, the `edge:` section, and for the proxy
   `hive_id` and `wan.uplinks[0].address`) and the environment. A missing or unparsable hive.yaml
   gives no `edge:` section, so no public listener (`load_hive_edge`). `wan:` is the router's
   section and is read leniently (`first_uplink_address`): a shape the edge does not expect leaves
   the proxy without an upstream, never the door without its listener.
2. Logging from `RUST_LOG` (default `info,sy_edge=debug,fluxbee_sdk=info`), reloadable at run
   time (see Config).
3. Connects to the router (`RouterDispatcher::connect_with_retry`, retry every 1 s). The door's
   four commands (the three TLS ones and `EDGE_WEB_ROUTES`) go to their own command channel,
   every other `system` message to one shared channel (`build_sy_edge_rpc_profile`).
4. Loads the endpoint table from `endpoints_path` (`load_registry`), the public artifact table
   from `publications_path` (`load_public_registry`, which drops expired or invalid rows and
   converts a file that still holds capability keys, see State on disk), and the web hosts from
   `web_routes_path` (`WebRouteTable::load`).
5. Fetches channel secrets from the vault for rows that carry only a `secret_ref` or
   `verify_token_ref` (`resolve_secrets`).
6. If a public listener is configured (ingress role, or `SY_EDGE_HTTP_LISTEN`): resolves the TLS
   mode (`TlsMode::resolve`) and builds it, then starts the HTTP frontend (`run_frontend`).
   - `vault`: one read of the secret (`start_vault_tls`). If it cannot be served, the edge starts
     AWAITING instead of exiting, binds the port anyway, and re-reads in the background.
   - `disk`: the files must load and pass the checks, else `main` returns an error and the
     process exits.
   - An invalid TLS configuration (an empty `tls_vault_key`, only one of `tls_cert`/`tls_key`)
     also exits. The public port is never bound in plaintext unless the mode is `plaintext`.
   - The proxy's upstream is built once (`IoWebClient::for_hive`): the line
     `sy-edge web proxy ready: web hosts go to io.web` names it, or an ERROR says what is missing
     (the mesh TLS material, `hive_id`, `wan.uplinks`) and every web host answers 502 until it is
     fixed and the edge restarts.
7. Starts the door's command task (`run_tls_commands`), beside the system loop.
8. Starts a reaper that every 600 s drops expired public rows and, if it dropped any, rewrites
   the publications file.
9. Runs the system message loop until the router channel closes.

## HTTP surface

Every request is dispatched by its host first, before any path (`Door::handle`):

1. **The host** (`request_host`, `canonical_host`): the authority of an absolute-form target when
   there is one, otherwise the single `Host` header; the name lowercase with one trailing dot
   dropped, and its port, unless it is 443 (or empty). Two `Host` headers, a `Host` that disagrees
   with the target's authority, or one that does not parse: `400 BAD_HOST`.
2. **Who gets the door's own routes** (`host_route`, host-posture §4.4): a request with no host
   at all (HTTP/1.0, an empty `Host`), an IP literal, and the edge's main name on any port. The
   main name comes from the certificate served (`main_certified_name`, see Web hosts) and is never
   a web host.
3. **A web host** (Web hosts below) goes to io.web for every path, `/healthz` included (The proxy
   to io.web below): it never reaches `/e/` or `/public/` (io-web-v2 D13). Only on its own port:
   the same host with any port but 443 is an unknown host (io-web-v2 §11.1, "other ports never
   match a route"), and gets the 404 below, never the door's routes either.
4. **Any other host** answers `404 NOT_FOUND` while a certificate is served. With none
   (plaintext, or a vault edge still awaiting) the edge cannot tell its main name, and every host
   but a web host keeps the door's routes. When the certificate's names tell no main name
   (unrelated names), each of them that is not a web host is the door's.
5. **The door's own routes** (`main_routes`):

| Route | Methods | What it does |
| --- | --- | --- |
| `/e/:ich` | any | Forward to the channel owner (`invoke_root` → `invoke`) |
| `/e/:ich/*extra` | any | Same, with `extra` passed as the path (`invoke_extra`) |
| `/public/:key` | GET, HEAD (else 405 with `Allow: GET, HEAD`) | Serve a published blob (`serve_public_artifact`) |
| `/healthz` | any | Answers `ok` |

Routing is per request, never by SNI. Each connection goes through the acceptor of the TLS
section below (`accept_tls`) unless the mode is `plaintext`, then HTTP (`serve_http`): hyper's
HTTP/1 server only. There is no protocol sniffing, no HTTP/2 (an h2 preface is refused as a bad
HTTP/1 request) and no upgrade. The main `ServerConfig` has no client authentication and an empty
ALPN list. The plaintext mode runs the same server, with the same limits, without TLS.

Limits on the door (`serve_door`, `serve_http`, `DoorLimits`):

- **Connections:** at most `MAX_CONNECTIONS` (4096) at once, TLS handshakes included, and at
  most `max_connections_per_ip` (256 by default; 0 turns it off) from one peer address
  (`PeerSlots`; an IPv6 peer counts by its /64, an IPv4-mapped address as its IPv4). Both are
  counted at accept, before the handshake; one more is closed at once, and the refusals are
  logged at most once per 10 s, with their count and the peer that hit the cap. Behind a NAT that
  rewrites source addresses, every client is one peer: check what the edge sees before trusting
  the per-address caps.
- **Request heads (A-87):** the TLS handshake has its own 10 s timeout (TLS section). After it,
  every request head must arrive within `HTTP_HEADER_READ_TIMEOUT` (30 s), through hyper's timer,
  which starts before the first byte. A client that sends nothing, one byte or an h2 preface, and
  an idle keep-alive connection, are closed at the latest when it fires. hyper buffers at most
  64 KiB per connection (`max_buf_size`): a longer head gets `431`.
- **Pace (A-86):** every connection's stream is wrapped (`PacedStream`). While the edge waits on
  the peer (a write the peer does not take, or a read while a handler waits for request body
  bytes), the connection spends a credit of 30 s that every 1024 bytes moved refill by one second,
  never above 30 s (`DOOR_PACE`, `Pace`). A peer that moves nothing for 30 s, or keeps moving less
  than 1 KiB/s, gets an I/O error that ends the connection, its handler and the permits they hold
  (a download slot, an `/e/` slot, a proxied request's slot). The time the edge spends on its own
  work (a channel's handler, io.web) is never charged: hyper keeps reading to notice an end of file
  while a request is served, and only the request body sets the demand (`BodyDemand`,
  `DemandBody`). hyper reads a body's first bytes before the handler runs, so the read leaves its
  waker on the demand, and the handler's first wait wakes it: a body that never starts is cut too.
- **Delivery, not buffer room (A-86):** each accepted socket gets `TCP_NOTSENT_LOWAT` = 16 KiB
  (`track_delivery`, Linux), so it reports itself writable only once less than that is still
  unsent. Otherwise Linux wakes a writer when its send queue drains to two thirds of a buffer that
  grows with the congestion window, and one wait on a slow mobile link could outlast the credit.
- **The peer's address** from the accept loop reaches every handler as
  `ConnectInfo<SocketAddr>` (`Door::handle`), for the logs (`request_peer`), and the proxy, for
  `X-Forwarded-For` and `Forwarded`.
- **A failed accept** (out of file descriptors, say) waits 100 ms before the next one, instead of
  spinning.

Error bodies are JSON: `{"ok": false, "error_code": ..., "message": ...}` (`http_error`; the
proxy's 502 adds `request_id`). The message is fixed text per code, never an error's detail,
which goes to the edge's log only (A-88).

Every answer of the door carries the security headers (`SECURITY_HEADERS`, added by
`with_security_headers` in `Door::handle` where a response lacks them, so a handler's own header
stays): `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
`Referrer-Policy: no-referrer`, `X-Robots-Tag: noindex, nofollow, noarchive` and
`Content-Security-Policy: default-src 'none'; frame-ancestors 'none'`. That covers the `/e/`
relay, the fanout `hub.challenge` echo, axum's own 404 and 400, `/healthz`, every `/public`
answer and every answer through the proxy, io.web's own included: there a header io.web set (its
content policy, say) wins. A published artifact's `200`, `206` and `304` keep the content policy
the artifact chose (`OwnContentPolicy`): the sandboxed page its own CSP, a download or a static
file none, since a `default-src 'none'` can stop a browser's PDF viewer and artifacts may be
framed. A response the edge cannot build (an invalid header value, say) is a
`500 INTERNAL_ERROR` with them, never an empty 200 (`built`, A-83).

### `/e/<ich>`: forwarding into the mesh

`invoke`, step by step:

1. **Capacity.** One permit per request from a semaphore of `MAX_INFLIGHT` (default 1024). None
   free: `503 SERVICE_UNAVAILABLE`.
2. **Lookup.** The `ich` must be in the endpoint table: else `404 NOT_FOUND`.
3. **Fanout handshake.** For a fanout row, a `GET` with `hub.mode=subscribe` is answered here:
   `200` with `hub.challenge` as text if `hub.verify_token` matches (constant-time), else `403`.
4. **Methods.** If the row lists `methods`, any other method gets `405 METHOD_NOT_ALLOWED`.
5. **Auth.** See the next section. Failure: `401 UNAUTHORIZED`.
6. **Body.** Read up to `HTTP_ENVELOPE_INLINE_LIMIT_BYTES` (64 KiB); larger:
   `413 BODY_TOO_LARGE`. A body that comes slower than the door's pace, or is not whole within
   `INLINE_BODY_TIMEOUT` (30 s; a chunked body that keeps just above the pace would otherwise hold
   an `/e/` slot for minutes): `408 BODY_TIMEOUT`, and the connection ends. A non-UTF-8 body is
   base64-encoded.
7. **Fanout delivery.** For a fanout row the request is broadcast with `meta.target` set to the
   row's `fanout_family` glob and payload `{raw_body_base64, signature}` (the
   `x-hub-signature-256` header), and the caller gets `200` at once.
8. **Forward.** Otherwise one unicast message to `owner_l2_name`:
   - `meta.msg_type` = the row's `inbound_family`, `meta.ich` = the channel,
     `meta.src_ilk` = the edge's own ILK;
   - `meta.context` = `{method, path, query, headers}`, where `headers` keeps only
     `content-type`, `accept`, `accept-language`, `user-agent`, `x-request-id`
     (`ALLOWED_REQUEST_HEADERS`, `filter_request_headers`); `Authorization`, cookies and
     `X-Forwarded-*` never go in;
   - payload = the body parsed as JSON, else the text, else `{"body_base64": ...}`.
   The whole envelope must fit in 64 KiB (`ensure_http_envelope_within_limit`), else
   `413 REQ_TOO_LARGE` (the sizes go to the log).
9. **Reply.** Waits for any message with the same `trace_id` (`edge_reply_matcher`) up to
   `HANDLER_TIMEOUT_MS` (default 30 000). The reply payload becomes the body of a
   `200 application/json` response; the node cannot choose the HTTP status. Router
   `UNREACHABLE`/`TTL_EXCEEDED`: `502 HANDLER_UNREACHABLE`. Timeout: `504 HANDLER_TIMEOUT`
   (`rpc_error_to_http_status`, `rpc_error_code`). Each code answers a fixed message
   (`rpc_error_message`, A-88); the error's text, with its trace, target and message family, is
   logged with the channel and the peer, and never sent.

### Channel auth modes

`AuthMode` has two values (`public`, `shared-secret`):

- **public**: no check at the door.
- **shared-secret**: the request needs `Authorization: Bearer <token>`, compared in constant time
  (`bearer_matches`, `constant_time_eq`). An empty stored secret never matches. After a rotation
  the previous token stays valid for 10 minutes (`SHARED_SECRET_GRACE_MS`, set in
  `apply_open_url`, kept in RAM only).

Where the token comes from (`handle_externalize` in `src/bin/sy_admin.rs`): SY.admin mints one
(`mint_entry_token`, two UUIDv4 in hex) unless the caller sent `secret`, stores it in SY.vault as
`edge_channel_secret:<ich>` with `owner_node` = the edge, and sends the edge only the
`secret_ref`. The edge reads the value back with its own ILK (`resolve_secrets`). The caller gets
the token in the `externalize` reply (`token`). A fanout row is `public` at the door; its verify
token is stored as `edge_fanout_verify:<ich>` and fetched the same way.

### `/public/<key>`: published blobs

`serve_public_artifact`:

- `key` must be 64 lowercase hex characters, and its SHA-256 present in the public table, with
  `expires_at` in the future; otherwise `404 PUBLIC_ARTIFACT_NOT_FOUND`. The table holds only
  `SHA-256(key)` (`key_sha256`): neither the disk nor a log line of the DMZ holds a key, and the
  hash it holds opens nothing (A-82).
- `ETag` is the SHA-256 in quotes; a matching `If-None-Match` gets `304`.
- At most `public_max_inflight` (default 128) downloads at once; else `503 PUBLIC_BUSY`. One
  peer address holds at most `public_max_inflight_per_ip` of them (default 8; 0 turns it off);
  one more from it gets `429 PUBLIC_PEER_BUSY`, logged at most once per 10 s with the peer. Both
  slots are held until the last byte, or until the connection ends.
- The file is `<blob_public_root>/<public_name>`, opened with `O_NOFOLLOW`, checked to stay inside
  the root and to have the recorded size (`open_public_artifact_file`).
- One byte range is supported (`206`, or `416` for a bad range).
- Headers: `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
  `X-Robots-Tag: noindex, nofollow, noarchive`, `Referrer-Policy: no-referrer`,
  `Content-Disposition` inline or attachment, and for `sandboxed-html-v1` a sandboxing
  `Content-Security-Policy`.

Allowed content (`validate_public_artifact_row`): `sandboxed-html-v1` with
`text/html; charset=utf-8`; `static-v1` with plain text, JSON, PDF, PNG, JPEG, WebP or GIF
(`allowed_static_content_type`); `download-v1` with `application/octet-stream` as an attachment.

The bytes reach the ingress through Syncthing, not through the mesh. SY.admin's
`publish_artifact` asks `IO.blob@motherbee` to curate the file, then sends `EDGE_PUBLISH_BLOB`; the
edge acks only after the file shows up under the public root with the right size and SHA-256
(`wait_for_public_artifact_ready`, polling every 250 ms up to `public_ready_timeout_ms`, default
60 000).

### The proxy to io.web

The door's third surface ([io.web v2](../io-web-v2.md) §11.2): every request for a web host, on
any path, goes to io.web on the motherbee (`WebProxy::forward`). Nothing else is reached through
it, never a host or a port that a request or a route names.

**The upstream** (`IoWebClient`, built once at boot by `IoWebClient::for_hive`):

- **Where:** the host this hive's router dials, the host of `wan.uplinks[0].address` in hive.yaml
  (on PROD `10.10.10.10`), on `IO_WEB_PORT` (9200, `fluxbee_sdk::mesh`).
- **TLS, mutual, over the mesh:** the edge presents its hive's leaf from
  `/var/lib/fluxbee/tls/<hive_id>/` (`MESH_TLS_DIR`), and verifies io.web's certificate inside the
  handshake with rustls' WebPki verifier over the mesh CA, under the name `motherbee`
  (`IO_WEB_SERVER_NAME`, `MeshTlsMaterial::client_config_by_name`), never the address it dials. A
  leaf of another hive from the same CA fails the handshake. It never resumes a TLS session: io.web
  checks the registry only on a full handshake.
- **One connection per proxied request:** TCP, TLS, then hyper's HTTP/1 client, closed when the
  response ends. Nothing is pooled: on the LAN a handshake costs milliseconds, and no request can
  desync another on its way to io.web.
- **Without the material, a valid `hive_id` or an uplink,** the edge logs why at boot (ERROR) and
  every web host answers the 502 below. It never falls back to plaintext, or to the router's
  CA-only verifier. Its answer to `EDGE_WEB_ROUTES` says so (`upstream`, see Web hosts), so
  SY.admin's `GET /web/status` shows it.

**The route's policy** (`web_policy_refusal`), applied before anything reaches io.web. In this
stage every web route takes GET and HEAD with no body:

| Request | Answer |
| --- | --- |
| any method but GET and HEAD, CONNECT and TRACE included | `405 METHOD_NOT_ALLOWED`, `Allow: GET, HEAD` |
| an `Upgrade` header | `400 UPGRADE_NOT_ALLOWED` |
| a body: a `Content-Length` other than 0, or any `Transfer-Encoding` | `400 BODY_NOT_ALLOWED` |

**What io.web gets** (`io_web_request`, `io_web_request_headers`):

- the method, and the target in origin form (`/path?query`) whatever form the client sent;
- the client's headers, first without the hop-by-hop ones (`Connection`, `Keep-Alive`,
  `Proxy-Connection`, `TE`, `Trailer`, `Transfer-Encoding`, `Upgrade`, `Proxy-Authorization`,
  `Proxy-Authenticate`) and every header a `Connection` header names (`strip_hop_by_hop`), then
  without `Content-Length`, `Expect`, `Host`, `Forwarded`, `X-Request-Id`, every
  `X-Forwarded-*`, `X-Real-IP`, `True-Client-IP`, `X-Original-URL`, `X-Rewrite-URL` and the
  method overrides `X-HTTP-Method-Override`, `X-HTTP-Method` and `X-Method-Override`
  (`DROPPED_REQUEST_HEADERS`);
- then the edge's own, in place of any the client sent: `Host` (the web host, in canonical form),
  `X-Forwarded-For` (the peer address the accept loop saw; an IPv4-mapped one as its IPv4),
  `X-Forwarded-Proto` (`https`, or `http` on a plaintext door), `X-Forwarded-Host` (the web
  host), `Forwarded` (RFC 7239, `for=<peer>;proto=<proto>;host=<host>`, an IPv6 peer as
  `"[<address>]"`) and a fresh `X-Request-Id` (a UUID);
- no body: the framing is the edge's own. Cookies and `Authorization` pass as the client sent
  them; the session rules come with stage 2 (io-web-v2 §11.3).

**What the client gets** (`from_io_web`): io.web's status and headers, without the hop-by-hop ones
and those io.web's `Connection` names, and without a `Content-Length` that a `Transfer-Encoding`
overrode (`strip_io_web_response_headers`). The body streams as io.web sends it, through the
door's paced stream, and hyper frames it again. The security headers io.web did not set are
added (see above).

**Errors and limits:**

- **Any failure to get io.web's response head** answers `502 UPSTREAM_UNAVAILABLE`, with a fixed
  message and the request id, in the body (`request_id`) and in `X-Request-Id`
  (`io_web_unavailable`). That is: no upstream, a refused connection, a TLS handshake the edge
  refuses (io.web's certificate does not verify), io.web refusing the edge's certificate, a reset
  or a close before the head, a `101`, and either timeout. In TLS 1.3 io.web's refusal
  (`access_denied`) is the first thing the edge reads after it sends the request: the edge's
  connect succeeds first. The detail (`IoWebFailure`) goes to the edge's log only.
- **`IO_WEB_CONNECT_TIMEOUT`, 5 s:** the TCP connection and the TLS handshake, together.
- **`IO_WEB_HEAD_TIMEOUT`, 30 s:** io.web's response head, once the request is sent. The body
  has no deadline of its own, and the client keeps the door's pace.
- **`web_max_inflight`, 128 by default (0 turns it off):** the proxied requests in flight at
  once, each with its own connection to io.web, each held from the request to the response's last
  byte, or to the end of the client's connection. One more gets a quick `503 WEB_BUSY` and never
  reaches io.web; the refusals are logged at most once per 10 s, with their count.
- **hyper's buffer** on a connection to io.web is 64 KiB (`IO_WEB_MAX_BUF`), as on the door's
  own connections.

**Logs:** one INFO line per request io.web answers, `sy-edge proxied a request to io.web`. A
request io.web does not answer is counted, and the failures are logged at most once per 10 s, as
the refusals at the cap are: the WARN `sy-edge: io.web did not answer proxied requests (502): ...`
carries `failed` (how many since the last line) and the last one's `error`. Each line carries
`request_id`, `method`, `host`, `path` without its query (stage 2's exchange code rides the query,
§11.3), `status`, `duration_ms` (to the response head) and `peer`, the WARN those of the last
failure. A request the route's policy refuses is a DEBUG line.

## Mesh messages

Handled one at a time in the loop at the end of `main`, except the door's four commands (the
three TLS ones and `EDGE_WEB_ROUTES`): the RPC profile sends those to their own channel
(`RPC_CH_TLS`, `build_sy_edge_rpc_profile`), and `run_tls_commands` answers each in its own task
over the shared state, so they never wait behind `EDGE_PUBLISH_BLOB` (up to 60 s) or a secret
resolution.

| Message | From | What happens |
| --- | --- | --- |
| `EDGE_OPEN_URL` | `SY.admin@motherbee` | Validate the row and upsert it by `ich`, persist the table, fetch its secret, reply `{status, ich, url}` (`apply_open_url`) |
| `EDGE_CLOSE_URL` | `SY.admin@motherbee` | Remove the row (absent is `ok`, `closed: false`), persist (`apply_close_url`) |
| `EDGE_LIST_URLS` | `SY.admin@motherbee` | Return the rows without secret values (`apply_list_urls`) |
| `EDGE_PUBLISH_BLOB` | `SY.admin@motherbee` | Validate, wait for the file, add the public row (`apply_publish_blob`) |
| `EDGE_UNPUBLISH_BLOB` | `SY.admin@motherbee` | Remove the public row by `publication_id` (`apply_unpublish_blob`) |
| `EDGE_TLS_STATUS` | `SY.admin@motherbee` | What the listener serves: mode, state, the leaf (`tls_status_answer`; shape in the TLS section) |
| `EDGE_TLS_RELOAD` | `SY.admin@motherbee` | Re-read the certificate once and swap it in place if it passes (`tls_reload_answer`; vault edge only) |
| `EDGE_ACME_CHALLENGE` | `SY.admin@motherbee` | Arm or clear a TLS-ALPN-01 challenge (`acme_challenge_answer`; vault edge only) |
| `EDGE_WEB_ROUTES` | `SY.admin@motherbee` | Replace the web hosts with the full set, keep it on disk, answer the set applied and the proxy's upstream (`web_routes_answer`; see Web hosts) |
| `VAULT_SECRET_CHANGED` | `SY.vault@<vault_hive>` | Re-fetch channel secrets; reload the certificate when the notice is about its key (below) |
| `CONFIG_GET` / `CONFIG_SET` | `SY.admin@motherbee` | The edge's own config (`apply_node_config`) |
| `NODE_STATUS_GET` | an orchestrator, or SY.admin / SY.vault of the same hive | Default status reply (`try_handle_default_node_status`) |

The nine `EDGE_*` commands are checked twice. The router delivers a protected SYSTEM action only
if `policy/system.rego` allows its origin (`prepare_local_delivery` in `src/router/mod.rs`; rule
(1) for the `EDGE_*` commands, rule (2) for `CONFIG_GET`/`CONFIG_SET`, rules (3)–(5) for
`NODE_STATUS_GET`). The edge then accepts an `EDGE_*` command only when the router-stamped
`src_l2_name` equals `EDGE_CONTROL_AUTHORITY`, `"SY.admin@motherbee"`
(`authorize_edge_service_command`; constant in `src/router/system_policy.rs`); a TLS or
web-routes command from anyone else changes nothing and gets `{status: "error"}`. A
VAULT_SECRET_CHANGED from any origin other than its vault is logged and ignored. SY.vault
addresses the notices about the edge's own secrets to it as a unicast; the router keeps
SY.vault's broadcast out of the DMZ (A-78, [SY.vault](sy-vault.md)).

Row checks on open (`validate_endpoint_row_for_open`): `ich` not empty; `inbound_family` not
empty and not `system`; a unicast row needs a fully-qualified `IO.*` `owner_l2_name` and, for
`shared-secret`, a `secret` or `secret_ref`; a fanout row needs an `IO.*` glob with `@` and a
`verify_token` or `verify_token_ref`.

Who may open what is decided in SY.admin before the edge is asked: an `IO.*` node only for a
channel it owns (`authorize_channel_command`), the operator for any channel, and fanout rows only
by the operator. See [SY.admin](sy-admin.md).

## Web hosts

The hosts the door serves only through io.web ([io.web v2](../io-web-v2.md) §11.1): today
`archi.<id>`, which SY.admin derives from its `web:` declaration and pushes every minute. The edge
keeps them in `WebRouteTable`, read by every request (see HTTP surface).

`EDGE_WEB_ROUTES` carries the full set, `{routes: [{host, upstream}]}`, and replaces it whole
(`web_routes_answer`). The payload refuses unknown fields. A set is refused, and the one served
stays, when:

- a host is not a DNS name once in `canonical_host` form: a port other than 443, a wildcard, an
  IP literal (`validate_web_routes`, `is_dns_host_name`);
- a host appears twice;
- an `upstream` is anything but `"io.web"` (`WebUpstream` admits no other; the edge never
  connects to a host or a port a route names);
- a host is the edge's main name, which would take `/e/` and `/public/` off it. The edge tells
  its main name from the certificate it serves (`main_certified_name`): its only DNS name (PROD
  before the combined certificate), or the one every other name lies under (after it,
  `archi.<id>` lies under `<id>`). With unrelated names, or no certificate served (awaiting,
  plaintext), it cannot tell, and the check is skipped.

The answer is `{status: "ok", routes, changed, persisted, upstream}`: the set served from now on,
in canonical form and sorted, whether it differs from the one before, whether the file holds it,
and the proxy's way to io.web (`WebProxy::upstream_status`), which SY.admin shows as
`routes.upstream` in `GET /web/status`:

- `state`: `ready`, or `unavailable` when the edge found no upstream at its boot; then `reason`
  says what is missing (the mesh TLS material, `hive_id`, `wan.uplinks`), and every web host
  answers 502 until it is fixed and the edge restarts;
- `last_failure`: the last proxied request io.web did not answer, `{at, kind}` (Unix seconds, and
  `no_upstream`, `connect`, `handshake`, `connect_timeout`, `http_handshake`, `head_timeout`,
  `head` or `switched_protocols`, `IoWebFailure::kind`), or null; never anything of the request.
The set is written to `web_routes_path` through a synced temporary file and a rename, only when
the file does not already hold it; a failed write is logged at ERROR, the set is served anyway, and
the next push writes it again.

At boot the file is read before the door opens, so a restarted edge proxies a web host at once; no
file is no web host. It is read before any certificate: once the boot read of the
certificate serves one, a route that names the main name is dropped, logged at ERROR, and written
over at the next push (`WebRouteTable::forget_main_name`); a request for the main name gets the
door's routes in any case (`host_route`). A corrupt or unreadable file is an ERROR and starts with
no web host until SY.admin pushes the set again, within a minute; meanwhile the web host is an
unknown host, and gets the same 404.

## TLS and the certificate

The design is [ACME certificates for the public edge](../acme-edge-certificates-v1.md) §4. All of
it lives in `EdgeTls`, shared by the acceptor, the TLS command task, the system loop's vault
notices and the awaiting re-reads.

### Modes

`TlsMode::resolve`, from the `edge:` section (see Config):

| Mode | When | What it serves |
| --- | --- | --- |
| `vault` | `tls_vault_key` is set (it wins over disk files) | The secret under that key, `{"cert": "<PEM chain>", "key": "<PEM key>"}`, read with the edge's own ILK (`VaultSecretSource`, `VaultCallerOwned`). Its owner must be `SY.edge@<edge hive>` (comment in `packaging/hive.yaml.example`). The only mode that reloads and answers ACME. |
| `disk` | `tls_cert` and `tls_key` are both set | The two PEM files, read once at boot (`EdgeTls::from_disk`). |
| `plaintext` | none of them | No TLS. `add_hive` writes it only with `ingress.allow_plaintext: true`, never on port 443. |

An empty `tls_vault_key`, or only one of `tls_cert` and `tls_key`, is refused at boot.

### The checks

Material passes `tls_material::inspect`, the same function SY.admin runs before it stores a
certificate: the chain parses and is not truncated (more `BEGIN CERTIFICATE` markers than parsed
certificates, PB-2), the key matches the leaf, and the leaf is valid now (not expired, not before
its start) (`check_material`).

Two weak chains depend on what is served (`EdgeTls::apply_material`): a truncated chain, judged on
the certificates the parser kept, and a leaf with no intermediates, which strict clients (Go,
Python `ssl`, webhooks) refuse.

- **Something is served:** a reload that would replace it with a weak chain refuses it. Re-reading
  the chain already served is never a refusal.
- **Nothing is served** (at boot, while awaiting, a disk edge): the weak chain is served and logged
  at ERROR (`log_tls_chain_diagnosis`). An edge up on a weak chain beats an edge that is down.

### The acceptor

Every TLS connection is accepted with `tokio_rustls::LazyConfigAcceptor`, which reads the
ClientHello before a configuration is chosen (`accept_tls`, `EdgeTls::route_hello`):

| ClientHello | Configuration | Then |
| --- | --- | --- |
| ALPN exactly `["acme-tls/1"]`, and a challenge armed for its SNI (lowercase, no trailing dot) | that challenge's, with `alpn_protocols = ["acme-tls/1"]` | finish the handshake and close; HTTP is never reached |
| ALPN exactly `["acme-tls/1"]`, and nothing armed for its SNI, or no SNI | none | close |
| anything else, `acme-tls/1` mixed with other protocols included | the main one: the served certificate, empty ALPN list | the HTTP routes above |
| anything else, while awaiting | none | close |

One timeout, 10 s (`TLS_HANDSHAKE_TIMEOUT`), covers reading the ClientHello and finishing the
handshake, on every path. A client that sends nothing, or stops halfway, is dropped.

### The ACME challenge

SY.admin arms a challenge with `EDGE_ACME_CHALLENGE` while it orders a certificate (design §6).
The edge mints the TLS-ALPN-01 certificate itself, so no private key crosses the mesh
(`acme_challenge_certificate`):

- a fresh P-256 key and an empty subject;
- SAN = the name, which must be a DNS host name: no wildcard, no IP literal;
- the critical `acmeIdentifier` extension (RFC 8737) holding the digest. The digest must be 64 hex
  characters, checked before rcgen, which asserts its length.

The challenge's `ServerConfig` is built from a `CertifiedKey` directly
(`acme_challenge_server_config`). rustls' `with_single_cert` re-parses the leaf with webpki, which
refuses the unknown critical extension the certificate exists to carry.

An armed challenge lives in memory under its name until `ttl_secs` passes (600 s by default, 1800 s
at most) or a clear names the same digest. A newer arm replaces an older one, and a late clear
never removes it.

### Hot reload (PB-1, closed)

The main configuration sits behind a lock that each new connection reads. `EdgeTls::reload`:

1. reads the secret once (15 s, `VAULT_TLS_READ_TIMEOUT`; no retry loop on this path);
2. checks it;
3. swaps only when it passes. Connections already open finish on the configuration they started
   with. Identical material (the same chain) swaps nothing, and `loaded_at` stays.

Each read also records which vault write the edge serves: the secret's version and its metadata's
`updated_at`, both from the VAULT_GET answer. A vault rollback re-mints version numbers, so only
the pair names one write. A re-read of the chain already served moves the pair to the write just
read. (SY.vault answers a VAULT_GET with the record's metadata, the same a VAULT_GET_METADATA
returns, so the edge makes no second call; the router's edge gate admits only VAULT_GET anyway.)

A refusal keeps the current certificate and says why. When a serving edge gets no verdict from the
vault (`transient`) it logs a WARN, not the ERROR of a refused secret. Nothing exits, and reloads run
one at a time. Three things trigger one:

- `EDGE_TLS_RELOAD` from SY.admin.
- A `VAULT_SECRET_CHANGED` for the edge's own key (`tls_secret_change_action`; matched by key, not
  by `resource_type`). Put, rotate, rollback and SY.vault's boot re-announce all reload; the
  re-read tells a re-announce from a change, since a vault rollback re-mints version numbers. The
  reload runs in its own task, so the loop never waits on the vault for it. A delete keeps
  serving what is loaded; an assignment changes nothing.
- The awaiting re-reads (next).

### Awaiting

A vault edge whose secret cannot be served at boot starts with no main configuration instead of
exiting (`start_vault_tls`). A missing secret used to crash-loop the unit, so a new ingress could
never answer the challenge for its first certificate. While awaiting it:

- answers armed `acme-tls/1` handshakes, closes every other TLS handshake, and never serves
  plaintext;
- re-reads the secret on its own and starts serving when good material appears
  (`run_awaiting_rereads`, `AWAITING_SCHEDULE`). A reload or a vault notice can get there first.

| The verdict it awaits on | Re-reads |
| --- | --- |
| `transient` (a cold boot: the vault is not up yet) | every 3 s for the first 2 minutes, then as below |
| `not_found`, `forbidden`, `invalid_material` | after 30 s, then twice as late each time, up to 5 min |

The reason is kept typed (`TlsRefusal::from_vault_error`) and reported by `EDGE_TLS_STATUS`. It is
logged at ERROR at boot and whenever it changes; repeats are a WARN, or DEBUG for the fast
transient re-reads.

| `reason` | When |
| --- | --- |
| `not_found` | the vault has no secret under the key (`KEY_NOT_FOUND`) |
| `forbidden` | the vault refused the edge (`UNAUTHORIZED`: a wrong owner, an inactive tenant) |
| `invalid_material` | the secret itself is wrong: no `cert`/`key`, a chain the checks refuse, or a request or stored value the vault calls invalid (`INVALID_REQUEST`, `INVALID_VALUE`) |
| `transient` | no verdict on the secret: the vault did not answer (`vault_failure_is_transport`), its answer did not parse, or it could not do its job (`IDENTITY_UNAVAILABLE`, `STORAGE_ERROR`, `IO_ERROR`, `ENCRYPTION_ERROR`, `NODE_ERROR`, `MASTER_KEY_NOT_AVAILABLE`, and any code it may add) |

A refused reload uses the same reasons. SY.admin must not roll a certificate back on `transient`:
nothing was said about the secret.

### The fingerprint

Everywhere the edge reports one (`EDGE_TLS_STATUS`, the logs) it is the SHA-256 of the leaf's DER,
in hex (`tls_material::LeafInfo`), the same value SY.admin computes.

### The TLS commands

All three come from `SY.admin@motherbee` only, and answer `{status: "error", error}` when they
cannot run: another origin, an edge with no public listener, or a RELOAD or CHALLENGE on a
`disk` or `plaintext` edge.

| Command | Request | Answer |
| --- | --- | --- |
| `EDGE_TLS_STATUS` | `{}` | `status: "ok"`, `mode`, `tls_vault_key` (null unless `vault`), `state` (`serving` or `awaiting`), `reason` and `detail` (awaiting only), the served leaf (`leaf_pem`, `fingerprint`, `not_before`, `not_after`, `names` (its DNS SANs), `issuer`, `chain_len`, `loaded_at`), and the vault write it came from: `vault_version` (integer) and `vault_updated_at` (the secret's metadata `updated_at`, as SY.vault gives it). Times are Unix seconds. The two `vault_*` fields are null off the vault (`disk`, `plaintext`) and while awaiting; a `plaintext` edge is `serving` with null leaf fields. |
| `EDGE_TLS_RELOAD` | `{}` | One re-read and verdict: the STATUS fields after it (`vault_version` and `vault_updated_at` included), `changed`, and when refused `refused: {reason, detail}`. A refusal is still `status: "ok"`. |
| `EDGE_ACME_CHALLENGE` | `{name, digest, ttl_secs?}` to arm, `{name, digest, clear: true}` to clear; other fields are refused | `{status, name, armed, expires_at}`: what stays armed for the name after the command |

## Config

`hive.yaml`, section `edge:` (`EdgeSection`), each with an environment override
(`Config::load`):

| Key | Env | Default |
| --- | --- | --- |
| `listen` | `SY_EDGE_HTTP_LISTEN` (bypasses the role check) | none: no door |
| `endpoints_path` | `SY_EDGE_ENDPOINTS` | `/etc/fluxbee/edge.endpoints.json` |
| `publications_path` | `SY_EDGE_PUBLICATIONS` | `/var/lib/fluxbee/state/sy-edge/publications.json` |
| `web_routes_path` | `SY_EDGE_WEB_ROUTES` | `/var/lib/fluxbee/state/sy-edge/web_routes.json` |
| `blob_public_root` | `SY_EDGE_BLOB_PUBLIC_ROOT` | `/var/lib/fluxbee/blob/public` |
| `max_connections_per_ip` | `SY_EDGE_MAX_CONNECTIONS_PER_IP` | 256 (0: no cap) |
| `public_max_inflight` | `SY_EDGE_PUBLIC_MAX_INFLIGHT` | 128 |
| `public_max_inflight_per_ip` | `SY_EDGE_PUBLIC_MAX_INFLIGHT_PER_IP` | 8 (0: no cap) |
| `public_ready_timeout_ms` | `SY_EDGE_PUBLIC_READY_TIMEOUT_MS` | 60 000 |
| `web_max_inflight` | `SY_EDGE_WEB_MAX_INFLIGHT` | 128 (0: no cap) |
| `tls_vault_key` | `SY_EDGE_TLS_VAULT_KEY` | none |
| `vault_hive` | `SY_EDGE_VAULT_HIVE` | `motherbee` |
| `tls_cert`, `tls_key` | `SY_EDGE_TLS_CERT`, `SY_EDGE_TLS_KEY` | none |

Environment only: `CONFIG_DIR` (default `/etc/fluxbee`), `NODE_NAME`, `NODE_VERSION`,
`ROUTER_SOCKET` (default `/var/run/fluxbee/routers`), `UUID_PERSISTENCE_DIR` (default
`/var/lib/fluxbee/state/nodes`), `TTL` (16), `HANDLER_TIMEOUT_MS` (30 000), `MAX_INFLIGHT`
(1024), `RUST_LOG`.

hive.yaml only, outside `edge:`, for the proxy: `hive_id` (its mesh TLS material,
`/var/lib/fluxbee/tls/<hive_id>/`) and `wan.uplinks[0].address` (io.web's host), the ones
add_hive writes for the ingress. Neither has an environment override: the upstream is never
anything but the motherbee the router dials.

`add_hive` writes the ingress hive.yaml `edge:` block (`resolve_add_hive_ingress_section` and the
template in `src/bin/sy_orchestrator.rs`): `ingress.listen` is required; `ingress.tls_vault_key`
is required unless `ingress.allow_plaintext: true`, and plaintext is refused on port 443;
`ingress.vault_hive` defaults to `motherbee`; `ingress.endpoints_json` is refused ("publish URLs
with externalize").

Live config: `CONFIG_SET` applies `log_level` at once (reloads the log filter); every other key
is answered as `restart_required` and not applied. `CONFIG_GET` returns the current
`log_level` (`apply_node_config`).

## State on disk

- `endpoints_path`: the channel table, rewritten after each open or close through a `.json.tmp`
  file and a rename (`persist_registry`). It keeps `secret_ref` and `verify_token_ref`, never the
  secret values (`registry_to_persisted_rows`). Read at boot so the edge starts with its routes.
- `publications_path`: the public artifact table, written to a `.json.tmp` with mode 0640,
  synced, then renamed (`persist_public_registry`, `write_state_file`). Each row holds
  `key_sha256`, never the capability key (A-82). A file written before 0.1.84 holds `key`: the
  first load hashes every key, rewrites the file without them, and logs one line with how many
  rows carried one (`load_public_registry`, `hash_capability_key`). The conversion is one way: an
  older edge cannot read the new file and would start with an empty public table. A load drops
  invalid rows and logs how many. Every write (a publication, an unpublication, the expiry reaper,
  `reap_public_registry`) happens while it holds the table's write lock: one writer of the file at
  a time, and always the latest table.
- `web_routes_path`: the web hosts, `{routes: [{host, upstream}]}`, written the same way (see
  Web hosts).
- `blob_public_root`: read only. Filled by Syncthing from the motherbee.
- The node UUID under `UUID_PERSISTENCE_DIR`.
- Nothing about TLS: the certificate is read from SY.vault (or the disk files), and the armed ACME
  challenges live in memory only.

## Failure behavior

- **No usable certificate.** A vault edge starts AWAITING and re-reads on its own; the reason is
  logged at ERROR and reported by `EDGE_TLS_STATUS` (see TLS). A disk edge whose files do not load
  exits, and systemd retries every 5 s; so does an invalid TLS configuration. The port is never
  bound in plaintext in either case.
- **A bad new certificate.** A reload refuses it, keeps serving the current one and answers why;
  nothing restarts.
- **The certificate deleted from the vault.** The edge keeps serving what it loaded.
- **Bad files.** An unreadable or invalid endpoints file starts an empty table (every `/e/` gets
  404). An invalid publications file starts an empty public table. An unreadable or invalid web
  routes file starts with no web host, so a web host is an unknown host (404) until SY.admin's
  next push, within a minute.
- **io.web down, slow or refusing.** Each proxied request answers the fixed 502 with its request
  id, and the edge logs why, at most once per 10 s with the count; nothing restarts. No mesh TLS
  material, `hive_id` or uplink at boot: every web host answers 502 until it is fixed and the edge
  restarts (the boot ERROR says which, and so does `upstream` in the answer to `EDGE_WEB_ROUTES`).
  More than `web_max_inflight` requests at once: a quick 503.
- **Slow, stalled or greedy clients.** A client below the door's pace loses its connection, and
  the download or `/e/` slot it held; a head that does not arrive in 30 s, or an `/e/` body not
  whole in 30 s, ends it too. One address past its share of connections or downloads is refused
  (see HTTP surface).
- **Secret not fetched.** The channel answers 401 until a later `resolve_secrets` succeeds: on
  the next `EDGE_OPEN_URL` or the next VAULT_SECRET_CHANGED from its vault.
- **Owner down or slow.** 502 or 504 per request; the edge keeps running.
- **Router channel closed.** The loop ends, `main` returns, and systemd restarts the unit.

## Not there yet

- The edge issues nothing itself: SY.admin orders the certificate and stores it in SY.vault
  (design §6), or the operator puts a PEM chain and key there by hand. The edge answers the
  challenge and serves what the vault holds.
- One certificate for every name: the edge chooses no certificate by SNI. SY.admin's ACME client
  orders one that names the main host and every web host, and the edge terminates TLS for all of
  them (io-web-v2 §5, O6; §11.1).
- The proxy's stage 2 (io-web-v2 §11.3): the edge passes cookies and `Authorization` to io.web as
  the client sent them; filtering cookies to `__Host-` ones, and bodies on upload paths, come
  with sessions. Today every web route is GET and HEAD with no body.
- The proxy keeps no connection to io.web between requests, and io.web's response body has no
  deadline of its own: an io.web that stalls mid-body holds that request's slot until the client
  leaves.
- The `shared-secret` mode is one static bearer per channel. There is no other auth mode.
- The owner node's reply is always sent as `200 application/json`.

## Health

- `curl -s https://<public host>/healthz` answers `ok` (the route is served on the public
  listener itself).
- `systemctl status sy-edge` on the ingress hive. The log line
  `sy-edge public HTTPS frontend ready (TLS)` means the port is bound; `serving=false` on it means
  the edge is AWAITING a certificate.
- `EDGE_TLS_STATUS` (sent by SY.admin) says what the door serves, or why it awaits.
- The proxy: `sy-edge web proxy ready: web hosts go to io.web` at boot names the upstream, and
  each proxied request leaves its line (see The proxy to io.web). On the motherbee,
  `GET /web/status` shows its upstream and last failure (`routes.upstream`). Through the edge,
  `curl -s https://archi.<id>/healthz` asks io.web's own `/healthz`.
- The open channels: `GET /channels/externalized?edge_node=SY.edge@<ingress>` on SY.admin
  (`list_externalized` → `EDGE_LIST_URLS`).

## Links

- [SY.admin](sy-admin.md): `externalize`, `publish_cloud_endpoint`, `publish_artifact`.
- [SY.orchestrator](sy-orchestrator.md): `add_hive` for the ingress role, units, Syncthing.
- [SY.vault](sy-vault.md): where the TLS material and channel tokens live, and its notices to the
  edge (A-78).
- [ACME certificates for the public edge](../acme-edge-certificates-v1.md): the certificate, its
  checks and the three TLS commands.
- [The router](rt-gateway.md): the system policy that admits the `EDGE_*` commands.
- [Edge ingress spec v6](../edge-ingress-spec-v6.md): the design (ICH model, invariants).
- [io.web v2](../io-web-v2.md): the web hosts and the proxy to io.web (§11).
- [IO.blob spec](../io-blob-spec-v1.md): curation and the public folder.
- [Host posture spec](../host-posture-and-exposure-spec-v1.md): what is exposed on each role.
