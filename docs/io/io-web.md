# IO.web

IO.web is the motherbee's web node behind the edge. The edge on the ingress proxies each exposure's
host to it, over the mesh's own mutual TLS. An exposure is one host, `<label>.<main host>`, mapped
to one internal upstream; Fluxbee Cloud alone publishes, enables and closes one, through io.cloud
and SY.admin ([io.web v2](../io-web-v2.md) §12, §13).

- **Stage 2.1** ([its design](../io-web-v2-stage-2-1.md)): io.web holds the exposures SY.admin
  pushes, mints and redeems their one-time links, and keeps their sessions in signed cookies.
- **Stage 2.2** ([its design](../io-web-v2-stage-2-2.md)): io.web is a transparent reverse proxy.
  What an application does on its own port it does the same behind io.web, inside Fluxbee Cloud's
  iframe. io.web adds only three things: the entry (Cloud's link, redeemed for the cookie), the end
  (everything open is cut when the access ends) and the permission for Cloud's page to frame it. It
  adds no time, no size and no rewrite of its own.

Only a hive the motherbee's registry lists as a connected ingress may connect: any other client
fails inside the TLS handshake. It never serves plaintext.

Source, in `nodes/io/io-web/src/`:

- `main.rs`: the node (boot, self-check, control plane, status) and the exposures' two commands;
- `exposures.rs`: the set SY.admin pushes, its file, the keys, the links, the sessions, and the
  access clock;
- `access.rs`: the link's and the cookie's formats and MACs;
- `web.rs`: the port (TLS, connections, upgrades), what each request gets, and the Origin check;
- `proxy.rs`: the forwarding: the request, the answer streamed, the WebSocket's tunnel, the cut and
  the request's log line;
- `upstream.rs`: where each upstream is dialled, its checks, and the connection;
- `headers.rs`: the headers io.web sets and strips, on its own answers and on an application's;
- `open.rs`: what is open on each exposure, its cut and its counts.

Its config contract is `nodes/io/common/src/io_web_adapter_config.rs`. The commands' shapes are in
`fluxbee_sdk::protocol` (`IoWebExposuresPayload` and the rest). The mesh TLS and the hive registry
it reads are `fluxbee_sdk::mesh` (`crates/fluxbee_sdk/src/mesh/`, feature `mesh-tls`).

| | |
| --- | --- |
| Runtime | `io.web`, binary `io-web` (`packaging/base-nodes.json`, `boot: false`) |
| Node name | `IO.web@motherbee`, in the root tenant `tnt:00000000-0000-0000-0000-000000000001` |
| Unit | `fluxbee-node-IO.web-motherbee`, transient, `Restart=always`, as every managed node |
| Listens on | `0.0.0.0:9200` (`fluxbee_sdk::mesh::IO_WEB_PORT`), TLS only |
| Certificate | the motherbee's mesh leaf, `/var/lib/fluxbee/tls/motherbee/` (`ca.crt`, `cert.crt`, `cert.key`), which SY.orchestrator writes at its boot |
| Admits | a client certificate from the mesh CA whose hive `/var/lib/fluxbee/hives` lists as a connected ingress |
| Config | `/var/lib/fluxbee/nodes/IO/IO.web@motherbee/config.json`; no field |
| Exposures | `/var/lib/fluxbee/nodes/IO/IO.web@motherbee/exposures.json`, mode 0600: its copy of SY.admin's set, without secrets |
| Keys | in SY.vault, `web_exposure_key:<exposure uuid>:<a\|b>`, owned by IO.web; read with its own ILK |
| Upstreams | Archi where `/etc/fluxbee/hive.yaml`'s `architect.listen` says it listens; a worker at the registry's `address`; an internal address (§ The upstream's address) |

## Where it runs

- **As `IO.web@motherbee`, in the root tenant, and nowhere else.** The binary checks both before it
  does anything else (`ensure_canonical_instance`):
  - its spawn name (`FLUXBEE_NODE_NAME`, the name the router registers it under) must be exactly
    `IO.web@motherbee`, which also keeps it on the motherbee;
  - its tenant must be the root tenant: `FLUXBEE_NODE_TENANT_ID`, which the orchestrator injects,
    else `_system.tenant_id` or `tenant_id` of its `config.json`. A spawn that names no tenant is
    refused.

  Otherwise it logs `IO.web runs only as IO.web@motherbee in the root tenant; refusing to run` and
  exits, before it connects to a router or opens a port, so a second io.web (a tenant's, or a
  mistyped name) never takes 9200. A managed runtime has no systemd `ExecCondition`, so the check is
  in the binary, as io.blob and io.cloud keep to the motherbee.
- **The orchestrator refuses the same at launch.** `io.web` is one of its core IO runtimes
  (`CORE_IO_RUNTIMES` in `src/bin/sy_orchestrator.rs`), with io.cloud and io.blob. Any other IO node
  is refused in the root tenant (`TENANT_ROOT_NOT_ALLOWED`), and a core IO runtime runs only as its
  own node (`CORE_IO_NAME_MISMATCH`: `io.web` only as `IO.web@motherbee`, and that name only on
  `io.web`) and only in the root tenant (`TENANT_ROOT_REQUIRED`; `core_io_refusal`).
- **Next to Archi** (io-web-v2 §3), which listens on the motherbee's loopback. io.web reaches it
  there, plain HTTP on loopback, for Archi's exposure. Archi does not change: it does not know it
  is exposed.

## How it starts

`boot: false`: the `.deb` builds and publishes the runtime, and `fluxbee-firstboot` does not start
it. The operator starts it once, in the root tenant, with `runtime_version: current` so that it
follows each upgrade:

```bash
curl -sS -X POST http://127.0.0.1:8080/hives/motherbee/nodes -H 'content-type: application/json' \
  -d '{"node_name":"IO.web@motherbee","runtime":"io.web","runtime_version":"current","tenant_id":"tnt:00000000-0000-0000-0000-000000000001"}'
```

On an existing hive this is one explicit step, which the ledger records, as for any new runtime.

From then on it is a managed node like the others:

- the orchestrator relaunches it at boot from its `config.json`;
- a new `current` for io.web, from a `.deb` upgrade or a publish, moves it to that version within
  a minute (`reconcile_persisted_custom_nodes`);
- `DELETE /hives/motherbee/nodes/IO.web@motherbee` with `{"purge_instance": false}` stops it, and
  it stays stopped across boots; with `{"purge_instance": true}` it removes it.

`fluxbee-factory-reset` keeps only the base nodes that boot, so it treats `IO.web@motherbee` as a
node the operator created: it kills and purges it, and the operator starts it again afterwards.

A purge removes its `exposures.json` too, which SY.admin pushes back within a minute. The keys
written for its old ILK stay unreadable to the new one, so each enablement stays closed until Cloud
enables it again. Enabling an exposure needs io.web launched: SY.admin answers `IO_WEB_ABSENT`
until `run_node IO.web@motherbee` has run. The brake on every exposure at once is
`kill_node IO.web@motherbee`.

## What it serves

HTTP/1.1 only, with upgrades for a WebSocket. The answer depends on the request's host, its session
cookie and its path (`respond` in `web.rs`). The host is the one `Host` header the edge sets, in its
canonical form. Every refusal comes from the headers alone, before a byte of the body is read.

| Request | Answer |
| --- | --- |
| `GET`/`HEAD /.fluxbee/healthz`, on any host | 200, `text/plain`, `ok` |
| `GET /.fluxbee/enter?code=…` | a link redeemed: 200 with the session's `Set-Cookie` and a page that moves on to `/`; otherwise the 401 page |
| `/.fluxbee/enter` with any other method, `HEAD` included | the 401 page: only a GET redeems |
| any other path under `/.fluxbee/` | 404, `text/plain`, `not found` |
| any other path on a host that is no exposure's | 404, `text/plain`, `not found` |
| `CONNECT` or `TRACE` on an exposure's host | 405, with `Allow`: the edge refuses both first |
| any other request without a valid session | 401, `text/html`: the fixed 401 page; a session cookie that is not valid is cleared (`Max-Age=0`) |
| a write or a WebSocket from another page, with a valid session | 403, `text/html`: the fixed 403 page (§ The Origin check) |
| any other request with a valid session | forwarded to the exposure's upstream, whatever its method (§ The proxy) |
| a valid session whose upstream is not reached | 502, `text/html`: the fixed 502 page |

- **`/.fluxbee/` is reserved on every host,** and never reaches an upstream. `/.fluxbee/healthz`
  replaced `/healthz` in stage 2.1, since `/healthz` would shadow an upstream's own; `/healthz` is
  a path like any other.
- **Every refusal of a link or a session answers the same 401 page,** whatever the reason, which
  only the log holds.

**io.web's own answers** (the 401, 403, 404, 405 and 502, the link's page, the health check) carry
these headers (`headers::set_own_headers`), §13.7's table, Fluxbee's column:

| Header | Value |
| --- | --- |
| `Content-Security-Policy` | `default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors <Cloud's origins>`, `'none'` without any |
| `X-Content-Type-Options` | `nosniff` |
| `Referrer-Policy` | `no-referrer` |
| `Cache-Control` | `no-store` |
| `X-Robots-Tag` | `noindex, nofollow, noarchive` |
| `Permissions-Policy` | every feature off (`accelerometer=(), …, xr-spatial-tracking=()`), the edge's own answers' value |

- **The policy loads nothing,** not even from the page's own origin. `base-uri`, `form-action` and
  `frame-ancestors` do not fall back to `default-src`, so each is named. The pages are plain text in
  HTML: no style, no script, no image and no link. They therefore render under their own policy,
  which a test checks (`the_pages_load_nothing_and_name_nothing_internal`).
- **Only Cloud's page may frame them,** so a session that ended shows io.web's 401 page inside
  Cloud's frame, not the browser's error. The link's page moves on to `/` inside the frame too.
- **The edge adds `Strict-Transport-Security`** to an answer of io.web's that carries none, and
  nothing else: io.web sets every other header itself.

## The proxy

A request whose session serves goes to its exposure's upstream (`proxy::forward`), and the
upstream's answer comes back as the application sent it.

**The request to the upstream** (§4.3, `headers::upstream_request_headers`):

- the method and the target as they came, the query included, over HTTP/1.1;
- the edge's headers as they came: `Host` is the public host, and `X-Forwarded-For`, `-Proto`,
  `-Host`, `Forwarded` and `X-Request-Id` are the edge's;
- the hop-by-hop headers stripped again, and every header a `Connection` header names; then
  `Connection: close`, or `Connection: upgrade` and `Upgrade: websocket` for a WebSocket's
  handshake. Any other upgrade goes on as a plain request, its `Upgrade` stripped;
- `Cookie` without any pair whose name begins with `__Host-fluxbee_`, in any case: no Fluxbee
  cookie ever reaches an upstream. The header goes when no pair is left;
- the body streamed as it comes. `Content-Length` goes and hyper frames the body, by its size or
  chunked, except an empty body's `Content-Length: 0`, which goes on as it came: hyper sends none
  for an empty body, and some servers answer 411 to a POST without it.

**The answer back** (§4.4, `headers::set_application_headers`), §13.7's table, the application's
column:

| | The application's answer |
| --- | --- |
| its status and its headers | as it sent them, but what this table removes |
| the body | frame by frame as it arrives: never gathered, never compressed |
| hop-by-hop headers | removed; a `Content-Length` beside a `Transfer-Encoding` is removed first, since hyper's client decodes by `Transfer-Encoding` but keeps the length, which io.web's server would take for the body's |
| `Set-Cookie` of a `__Host-fluxbee_*` name, in any case, or a nameless one whose value begins so | removed: a browser would send it back as Fluxbee's own cookie |
| `Server`, `X-Powered-By` | removed |
| `X-Content-Type-Options: nosniff` | added when missing |
| `Content-Security-Policy: frame-ancestors 'self' <Cloud's origins>` | added as a header of its own, `'self'` alone without origins. The browser enforces every policy header, so the application's own stay as they are, and `'self'` keeps an application able to frame its own pages |
| the strict set, `Permissions-Policy` | never added: they could turn off something the application uses |
| the session's cookie | appended when it slides or moves (§ How access ends) |
| `Strict-Transport-Security` | added by the edge when the application sends none |

**No wait of its own.** Only reaching the upstream is bounded (5 s). Then a request waits for its
answer as long as the application takes (Archi's chat answers at its turn's end; FINDINGS A-142),
an answer streams as long as it lasts, and a WebSocket stays open until one side closes it or the
access ends. An SSE event reaches the browser when the application writes it.

**A WebSocket** (a GET with exactly one `Upgrade: websocket` and a `Connection` naming `upgrade`,
as the edge passes it): the upstream's 101 must carry exactly one `Upgrade: websocket` and one
`Sec-WebSocket-Accept`. io.web then answers the edge 101 with exactly one `Upgrade: websocket`, one
`Connection: upgrade` and the upstream's `Sec-WebSocket-Accept`, `-Protocol` and `-Extensions`, and
nothing else of the upstream's 101: the edge takes no other 101. It then copies bytes both ways.
The tunnel runs inside its connection's scope, so it keeps the connection's permit and a demoted
ingress still cuts it; it ends both sides at the first end of either. A 101 to anything io.web did
not ask for, or a handshake's 101 without those headers, is a 502 (`switched`).

**A client's body the edge could not finish** (an aborted upload, a client below the edge's pace)
reaches io.web as a body that breaks. The edge answers the client itself (408 or 400), so io.web
drops the request and its upstream's connection, logged `client_closed`, and counts no failure.

## The upstream's address

Where io.web dials each kind (`upstream.rs`, §4.2):

| `upstream.kind` | Dialled | Read from |
| --- | --- | --- |
| `archi` | where Archi listens | `/etc/fluxbee/hive.yaml`'s `architect.listen` (io.web's `config_dir`), read once at boot; Archi's own default `127.0.0.1:3000` without it. A listen on `0.0.0.0` or `[::]` is dialled on loopback, any other address as it is |
| `hive` | `<address>:<port>` | the hive registry's `address` for the hive, read at each request: only for a connected worker with its join done. A DNS name is resolved then |
| `address` | `<address>:<port>` | the exposure, as SY.admin took it |

- **Archi's env override is not seen.** Archi takes `JSR_ARCHITECT_LISTEN` before hive.yaml, and
  that variable lives in Archi's unit, which io.web cannot read. An Archi moved by it alone is
  dialled at hive.yaml's address, and its exposure answers 502: set `architect.listen` instead,
  then restart io.web, which reads hive.yaml at boot only.
- **Archi's listen must be an IP and a port** (`127.0.0.1:3000`), as the shipped hive.yaml has it.
  A name, a hive.yaml io.web cannot read or parse, or a port of Fluxbee's own leaves io.web without
  Archi's address: the boot logs why (`IO.web has no address for Archi: …`), and Archi's exposure
  answers 502 (`upstream_failed:archi_listen`).
- **Never a Fluxbee port,** whatever the kind: 22, 80, 443, 8080, 8384, 9000, 9100, 9200, 21027,
  22000, and 3000 but for Archi's own kind. io.web holds a copy of SY.admin's list
  (`REFUSED_UPSTREAM_PORTS`) until the next Rust SDK release moves it into the SDK (FINDINGS A-355).
- **A worker's or an internal address is checked again** as SY.admin checks an `address` upstream:
  10/8, 172.16/12, 192.168/16 or fc00::/7, and never loopback, link-local, unspecified, multicast,
  broadcast, or an address the motherbee holds. A name that resolves to several addresses is
  dialled at the first that passes; one that resolves to none that passes answers 502
  (`address_refused`). A hive gone from the registry, or no longer a connected worker, answers
  502 (`hive_unknown`).
- **The connection** is plain HTTP/1.1 over TCP, one per request, nothing pooled, closed once its
  request is over. Resolving the name and opening the connection take at most 5 s. The scheme stays
  open until the first third-party exposure (io-web-v2 §13.5).
- **TCP keepalive** on every socket io.web holds, the edge's connections and the upstreams': the
  kernel probes an idle connection after 60 s, every 10 s, and drops it after 6 probes unanswered.
  With no wait of its own, a peer gone without a FIN (power lost, a partition) would otherwise hold
  a stream, a WebSocket or a pending answer for ever. A live peer answers the probes whatever its
  application does, so this is reachability, not a wait put on the application.

## The Origin check

Every `*.fluxbee.ai` host is one site, so `SameSite=Strict` stops no sibling: a page on another
exposure's host could post into Archi while the operator has it open, and the browser would attach
the session cookie (io-web-v2 §13.8). io.web therefore answers 403, and forwards nothing, to a
request that writes or opens a WebSocket without coming from the exposure's own page
(`origin_refusal` in `web.rs`):

- it covers every method but GET, HEAD and OPTIONS, as Go's `CrossOriginProtection` does, and a GET
  with `Upgrade: websocket`;
- `Sec-Fetch-Site: same-origin` or `none` passes; any other value is refused;
- without that header, an `Origin` equal to `https://<the exposure's host>` passes, and any other
  is refused, `Origin: null` included;
- with neither header, the request passes: a browser's cross-origin request carries one of them,
  and a browser always sends `Origin` with a WebSocket's handshake.

It runs after the session check, so a request without a session gets the 401. The 403 is a fixed
page, logged with the header that refused it and its value. Archi's page calls its own host only,
inside Cloud's frame too, so nothing changes for Archi. A legitimate write from another origin (an
identity provider's or a payment's POST back) is refused: a question for the first third-party
exposure.

## Exposures

SY.admin owns the set of exposures and pushes it whole to io.web with `IO_WEB_EXPOSURES`: at each
change, then every 60 s ([stage 2.1](../io-web-v2-stage-2-1.md) §2.3, §5.5). Each exposure has its
id (`exp:<uuid>`), its host, its upstream, and its live enablements, newest last, each
`{enablement_id, until, slot, key_version}`. None of it is secret.

**Who may push.** The two commands arrive on a channel of their own (`exposures`), served by their
own task, so a key read never holds NODE_STATUS_GET or CONFIG. io.web takes them only:

- as system messages, by their exact names;
- from the router-stamped `SY.admin@motherbee`, a constant: never the config's `admin_target`;
- never as a tap copy (`via_tap`).

Anything else named after one of them is answered `UNAUTHORIZED`, on its own trace, and logged; it
changes nothing.

The set also carries `cloud_origins`, the origins of Fluxbee Cloud's page that may frame the
exposures, as the operator named them in `publish_cloud_endpoint` (stage 2.2 §2). io.web writes an
origin into a header only once it is `https://`, a lowercase host of LDH labels and dots, and an
optional `:port`; any other is skipped and logged, so nothing in the set is ever injected into a
header.

**A push** (`IO_WEB_EXPOSURES`, answered with `IO_WEB_EXPOSURES_RESPONSE`):

| Its generation | io.web |
| --- | --- |
| lower than the one held | refuses it, `STALE_GENERATION`, and keeps its set |
| equal | acknowledges it and reads nothing |
| higher | replaces its set in memory at once, writes its file, then reads the keys it does not hold |

- **Revocation is at once.** Whatever the new set no longer holds (a disabled exposure, an
  unpublished one, the oldest enablement of a renewal) fails at the next request, before any key
  is read. An exposure the set leaves without access is cut at once (§ How access ends).
- **The answer** is `{status: ok, generation, changed, enablements: [{enablement_id, key: ready |
  unavailable, detail}]}`, one entry per live enablement. SY.admin's enable waits for it: io.web
  answers once it has tried to read each key it lacks, `key: ready`, or `key: unavailable` with why
  in `detail` when the read failed or the key did not match. A payload it cannot read is
  `INVALID_REQUEST`, and changes nothing.

**The keys.** Each enablement's secret `K` is in SY.vault, in its exposure's key slot
`web_exposure_key:<uuid>:<slot>`, which SY.admin put at the enable and forgot (§3.1). io.web reads
it:

- as its owner, with its own ILK (`FLUXBEE_NODE_ILK_ID`), as io.slack reads its key, within 3 s,
  every missing key at once;
- and takes it only as SY.admin wrote it: resource type `web_session_key`, owned by IO.web, at the
  enablement's `key_version`, value `{secret: <64 hex>}`.

A key that cannot be read, or does not match, is `unavailable` with the reason: its links and
sessions fail closed, and io.web reads it again every 30 s. Once a key is read, io.web keeps only
the two keys derived from it, `K_link` and `K_sess` (HMAC-SHA256 of `K` with two fixed labels), so
that a link never verifies as a cookie.

**Its file.** `exposures.json` beside its `config.json`, mode 0600, written through a temporary
file: `{schema_version: 1, generation, exposures, cloud_origins}`, without secrets; `cloud_origins`
only when there is one, so an io.web before 2.2 still reads a file without origins. A restart
keeps the origins, and SY.admin's next push of the same generation is acknowledged without a
read. A write that fails takes the old file away, so that a restart never brings back what a newer
set dropped, and is tried again at the next push or retry.

**At boot** io.web loads its file, then reads its keys, and only then opens its port, so sessions
survive a restart. A session whose key io.web could not read at boot (SY.vault or identity not
ready yet) waits: it is answered the 401 page, with its cookie kept, until the retry reads the key,
and then serves again. A file it cannot read leaves it with no exposure, which fails closed, until
SY.admin's next push.

**A link** (`IO_WEB_ACCESS_LINK {exposure_id, enablement_id, generation}`) is minted only for the
exposure's current enablement, live, with its key held, and never in the second io.web started.
Otherwise the answer is `EXPOSURE_UNKNOWN`, `ENABLEMENT_UNKNOWN` (io.web's set may be older:
SY.admin pushes it and asks once more) or `KEY_UNAVAILABLE` (in that first second too: ask again).

## Links and sessions

**The link** (§3.2) is `https://<host>/.fluxbee/enter?code=L1.<x>.<e>.<jti>.<iat>.<tag>`:

- `x` and `e` are the exposure's and the enablement's uuids in 32 hex;
- `jti` is 16 random bytes in hex, `iat` when io.web minted it in Unix seconds;
- `tag` is the HMAC-SHA256, under `K_link`, of `L1|<host>|<x>|<e>|<jti>|<iat>`. The host is in the
  MAC and not in the code, so a code redeemed on another host fails.

Its answer gives `url`, `expires_at` (`iat` + 120) and `link_id`, the first 16 hex of the SHA-256
of `jti`. The code rides the query, so no journal ever holds it.

**Redeeming it.** A `GET /.fluxbee/enter` checks, in this order:

1. the host is an exposure's;
2. the code parses strictly;
3. `x` is that exposure;
4. `e` is a live enablement of it, whose key io.web holds;
5. the tag verifies, compared in constant time;
6. `iat` is no earlier than io.web's boot (the first whole second after it started), and the link
   is at most 120 s old;
7. `jti` was never redeemed; it is then kept until `iat` + 120.

Any failure answers the same 401 page. A link minted before io.web restarted is refused, and Cloud
asks for another. A HEAD never redeems, so a link's preview does not spend it.

**The session** (§3.3) is one cookie, set by the redeemed link:

```text
__Host-fluxbee_session=S1.<x>.<e>.<sid>.<issued>.<active>.<tag>; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=<min(1800, until - now)>
```

- `tag` is the HMAC-SHA256, under `K_sess`, of `S1|<host>|<x>|<e>|<sid>|<issued>|<active>`.
- **No `Domain`:** the cookie is the host's alone, as `__Host-` requires.
- **The link's answer is a page, not a redirect:** it sets the cookie and moves on to `/` with a
  meta refresh. A `SameSite=Strict` cookie is withheld from a redirect inside a navigation that
  began on Cloud's site, and is sent on the navigation the page starts.
- **It is valid while** its tag verifies over the request's host, its enablement is live with its
  key held, and its last activity (`active`) is at most 30 min old. More than one session cookie
  in a request is refused.
- **It slides:** once `active` is 60 s old, the answer carries the cookie again with `active = now`.
- **It moves at a renewal** (stage 2.2 §4.6): when the exposure's newest enablement is another one
  whose key io.web holds, a cookie that serves is minted again under it at once: its `e` and its
  tag change, `sid` and `issued` stay, `active` is now, and its `Max-Age` is the slide's under the
  newest enablement's `until`. The answer carries it, as a slide.
- **No table holds sessions:** the cookie is the session, so they survive a restart of io.web.

**What is open** on each exposure is registered (`open.rs`): every forwarded request until its
answer ends, every WebSocket until it closes. A request registers under the same lock as its
session check, and a cut runs under the writer's side of that lock, so a request is either
registered before a cut and cut with the rest, or checked after it and refused: none escapes.

**How access ends** (§3.4, stage 2.2 §4.7):

| Event | Effect |
| --- | --- |
| Cloud disables or unpublishes the exposure | SY.admin's push drops the enablements: every link and session fails at the next request, and everything open on the exposure is cut at once |
| The last enablement reaches its `until` | io.web's own clock ends its links and sessions, and cuts everything open, within the second |
| A renewal | a new enablement, key and slot. Nothing open is cut: each session moves to the new enablement at its next request. New links come from the new one only. The previous enablement stays until its own end, so its key still verifies a cookie not moved yet; a session that makes no request until then fails, and Cloud's link brings it back. Cloud's way to throw everyone out is a disable and a new enable |
| 30 min without a request | the session ends |
| io.web restarts | sessions survive (both slots are read again); one whose key is not read yet waits, answered 401 with its cookie kept, until the key is read; links minted before it are refused. What was open ends with the process |
| io.web purged and launched again | a new ILK cannot read the old keys: closed until Cloud enables again |

**The cut** (io-web-v2 §13.4): when an exposure is left without access, by a push or by the clock,
everything open on it ends at once:

- **a request still waiting for its upstream's head** (Archi's chat): io.web answers its 401 page,
  which clears the cookie, and drops the upstream's connection; the application gets what it gets
  when a browser tab closes mid-request;
- **an answer streaming:** io.web ends its body with an error and drops the upstream's connection;
  the edge passes it on by closing the client's connection, and a browser's `EventSource` tries
  again, gets 401 and stops;
- **a WebSocket:** both sides are dropped.

The bound is the push: a disable reaches io.web, acknowledged, within a second. The clock's end
comes at the second: one task sleeps until the earliest end among the exposures still open, is
woken by each set installed, and reads the wall clock at least every 60 s.

## TLS and who may call it

**The handshake decides.**

- **The server certificate** is the motherbee's mesh leaf, whose SAN is `motherbee`. The edge
  verifies it with rustls' WebPki verifier over the mesh CA, under the name
  `fluxbee_sdk::mesh::IO_WEB_SERVER_NAME` (io-web-v2 §11.2 item 2).
- **The client verifier** (`MeshTlsMaterial::server_config_for_roles` with `HiveRole::Ingress`):
  - the client certificate must chain to the mesh CA;
  - its hive (`peer_hive_from_cert`) must be one the registry gives the role `ingress`, read at that
    handshake with the strict reading: status `connected`, and no join phase but `done`.
- **Every other client fails inside the handshake,** so none of its requests is ever read:
  - a worker, an egress, or a hive the registry does not hold gets the alert `access_denied`;
  - a client with no certificate gets `certificate_required`;
  - a certificate from another CA fails on its chain.

  The verifier logs each refused hive with its role (`mesh client refused in the handshake`).
- **Every handshake is a full one.** A resumed TLS session would restore the client's certificate
  without the verifier, so io.web keeps no session cache and sends no ticket, and the edge never
  offers to resume (`client_config_by_name`).
- **After the handshake** io.web reads the registry once more, and closes a connection whose hive
  is not a connected ingress before it reads any request (defence in depth).

**Admission is live.**

- Each connection checks the registry every 10 s, as the router's WAN does, and is cut once its
  hive stops being a connected ingress (`HiveRegistry::until_not`).
- A removed or demoted ingress therefore loses its open connections within about 10 s, and its
  next handshake fails at once.

**The limits** (`LIMITS` in `web.rs`):

| Limit | Value | Why |
| --- | --- | --- |
| TLS handshake | 3 s | one on the internal network takes milliseconds; a client that stalls its handshake holds its handshake slot no longer (A-89) |
| Request head | 30 s, from the handshake or the previous answer | one byte, or an h2 preface, never holds a connection longer; it also closes a keep-alive connection idle that long (hyper's head timer, as on the edge's door, A-87). It times the head only: then a request waits for its upstream, and an answer streams, as long as the application takes |
| Head size | 128 KiB per connection buffer | twice the edge door's, so a head the door admitted fits with the headers the edge adds; a longer head gets 431 |
| Handshakes | 128 at once, 32 from one source address (its IPv6 /64 as one) | one more is closed at accept, with a warning at most every 10 s. A host that opens connections and stays silent fills these, for 3 s each, never the connections served (A-89) |
| Connections | 256 at once, past their handshake | one more is closed once its handshake ends, with a warning. A stream or a WebSocket holds its connection until it ends |
| Reaching an upstream | 5 s, its name resolved and its TCP connection open | io.web's own reachability: the only bound on a proxied request |
| TCP keepalive | 60 s idle, then every 10 s, 6 probes | on every socket io.web holds, the edge's and the upstreams': a peer gone without a FIN is dropped within about two minutes |

The edge's own caps on proxied requests come first (io-web-v2 §11.2 item 2). A client that keeps
idle keep-alive connections must close them before the 30 s head timer does, or it may write into a
connection io.web is closing.

## Configuration

The contract (`IoWebAdapterConfigContract`) has **no field**:

- **the port** is the shared constant `IO_WEB_PORT`, the one the edge dials;
- **the certificate** is the motherbee's mesh leaf, at its fixed path;
- **who may connect** is the registry's connected ingress, never a list in a config;
- **the exposures and their keys** come by SY.admin's push, never by config: a config is open to
  more than Cloud, and only Cloud asks for an exposure (io-web-v2 §12.2);
- **the upstreams' addresses** neither: Archi's is hive.yaml's `architect.listen`, a worker's the
  registry's (§ The upstream's address).

So the orchestrator's seed alone is io.web's whole config:

- **It boots CONFIGURED on its defaults.** The SDK reads a `config.json` without operator content
  as UNCONFIGURED (awaiting the operator), which for io.web would mean DEGRADED for good, so io.web
  applies its contract to nothing instead (`boot_control_plane`). `CONFIG_GET` answers
  `state: CONFIGURED`, `effective_config: {"io": {}}`.
- **Any `io.*` field is refused, never dropped,** with `invalid_config` naming it
  (`io.web has no configurable field in this stage; refused: io.port`). An operator who sets one
  learns at once that it does nothing. Keys outside `io` are the platform's (the orchestrator's
  `tenant_id`) and are not carried over, as in io.blob and io.cloud.
- **A refused config does not serve.** A `config.json` whose operator content the contract refuses
  (a `run_node` with such a `config`) boots FAILED_CONFIG, and the port does not open until a
  `CONFIG_SET` the contract accepts. A refused `CONFIG_SET` keeps the config in use.

CONFIG_GET and CONFIG_SET go through SY.admin (`node_control_config_get`, `node_control_config_set`);
io.web answers only `SY.admin@motherbee` and `SY.orchestrator@motherbee`:

```bash
curl -sS -X POST http://127.0.0.1:8080/hives/motherbee/nodes/IO.web@motherbee/control/config-get -d '{}'
```

Besides the common fields, the answer has `runtime`:

- `web`, the port's state and its `exposures` (below);
- `tls_dir` and `hive_registry`, where it reads its material and its registry;
- `control_plane_metrics`.

## Operating it

### Status

`GET /hives/motherbee/nodes/IO.web@motherbee/status` reports what io.web answers to
`NODE_STATUS_GET` (`health_source: NODE_REPORTED`):

- **`health_state`** is `HEALTHY` only while the config is in use and the port serves. Otherwise it
  is `DEGRADED`, never `ERROR`, as across the managed family.
- **`last_error`** gives the reason. It is the config's while the config is not in use, otherwise
  the port's:

  | `last_error.code` | Meaning | What to do |
  | --- | --- | --- |
  | `invalid_config` | the config carries a field the contract refuses | `CONFIG_SET` a config without it, for example `{}` |
  | `mesh_tls_unavailable` | `/var/lib/fluxbee/tls/motherbee/` cannot be read or used | SY.orchestrator writes it at its boot: restart `sy-orchestrator`, then wait for io.web's next try |
  | `hive_registry_unavailable` | `/var/lib/fluxbee/hives` cannot be read, so every handshake fails | the deb's postinst creates it; check that it exists and root can read it |
  | `listen_failed` | `0.0.0.0:9200` cannot be bound | `ss -ltnp 'sport = :9200'` shows what holds it |

- **`extensions.web`** is the port's state: `state` (`starting`, `serving` or `unavailable`),
  `listen`, `admits: ["ingress"]`, and `code` and `message` while unavailable.
- **`extensions.web.exposures`** is what io.web holds of the exposures:

  | Field | Meaning |
  | --- | --- |
  | `generation` | the generation of the set it holds; 0 before any |
  | `exposures` | the exposures in its set |
  | `enabled` | those with a live enablement |
  | `keys_unavailable` | live enablements whose key io.web does not hold: they fail closed |
  | `links_issued`, `links_redeemed`, `links_refused`, `sessions_minted` | since io.web started |
  | `cloud_origins` | Cloud's origins, as the set carries them |
  | `per_exposure` | one entry per exposure of the set, below |

  Each entry of `per_exposure` (stage 2.2 §4.8), counted since io.web started:

  | Field | Meaning |
  | --- | --- |
  | `exposure_id`, `host` | the exposure |
  | `open` | what is open now: `waiting` (for the upstream's head), `streams` (an answer's body on its way) and `websockets` |
  | `proxied` | the upstream's answers passed on, by class: `1xx` (a WebSocket's 101), `2xx`, `3xx`, `4xx`, `5xx` |
  | `refused` | io.web's refusals: `401` (no valid session) and `403` (the Origin check) |
  | `upstream_failures` | requests answered io.web's 502 |
  | `cut` | requests and WebSockets the cut ended |
  | `sessions_moved` | sessions moved to a renewal's enablement |

  SY.admin's `web_status` shows the same set from SY.admin's side, with io.web's last answer.

io.web tries its material and its port again every 30 s. It checks the registry every 10 s, so the
state follows the registry both ways.

### Logs

io.web logs to its unit's journal: `journalctl -u fluxbee-node-IO.web-motherbee`. The level is
`info`, with `io_web` at `debug`, unless `RUST_LOG` says otherwise.

| Line | Level | When |
| --- | --- | --- |
| `IO.web runs only as IO.web@motherbee in the root tenant; refusing to run` | error | at boot, with the name, the tenant and why, before it exits |
| `IO.web ready (managed control plane); its port opens once its config is in use and its exposures are loaded` | info | at boot, with the config state, the port, the TLS directory, the registry, the exposures' file and its ILK |
| `IO.web dials Archi's exposure where hive.yaml's architect.listen says Archi listens` | info | at boot, with the address |
| `IO.web has no address for Archi: …` | error | at boot, with why: Archi's exposure answers 502 until io.web restarts with a hive.yaml it can use |
| `IO.web loaded its exposures from its file` | info | at boot, with the path, the generation and the count |
| `IO.web has no exposures file yet: …` | info | at boot, without a file |
| `IO.web cannot read its exposures file: …` | error | at boot, with why: it holds none until SY.admin's next push |
| `IO.web holds an exposure` | info | an exposure new or changed in a set, with its enablements' ids and ends |
| `IO.web dropped an exposure: its links and sessions fail from now on` | info | an exposure a new set no longer holds |
| `IO.web applied its exposures` | info | a newer set taken, with its generation, whether it changed, the counts and Cloud's origins |
| `IO.web holds Cloud's origins: the pages that may frame the exposures` | info | the origins changed, with them and the ones written in headers |
| `IO.web writes none of Cloud's origins that is not https://<lowercase host>[:port] into a header: …` | warn | an origin skipped, escaped |
| `IO.web cut what was open on an exposure: it is left without access` | info | a cut, with what was open (`waiting`, `streams`, `websockets`) and why: a push left no live enablement, dropped the exposure, or the last enablement ended |
| `IO.web refused a set older than the one it holds` | warn | `STALE_GENERATION` |
| `IO.web key ready` | info | a key read and taken, with its slot and version |
| `IO.web key unavailable: …` | warn | a key not read or not matching, with why; a retry logs only a change |
| `IO.web could not write its exposures file: …` | error | with the path and why, and whether the old file is gone |
| `IO.web refused an exposure command: only SY.admin@motherbee sends them` | warn | with the caller, the name, the kind, `via_tap` and why |
| `IO.web link issued` | info | with `link_id` and `expires_at` |
| `IO.web issued no link` | warn | with the error code and why |
| `IO.web link redeemed` | info | with `link_id`, `expires_at`, the edge's `X-Request-Id` and `X-Forwarded-For` |
| `IO.web session minted` | info | with `sid` (its first 8 hex) and `max_age` |
| `IO.web link refused` | warn | with the reason, `link_id` and `expires_at` when the code parsed, `X-Request-Id` and `X-Forwarded-For` |
| `IO.web proxied a request` | info | each request on an exposure's host that reached the session's check and passed it, at its end (below) |
| `IO.web refused a request` | info | the same line for a 401 or a 403, at once, with why in `detail` |
| `IO.web stream opened` | info | an answer that announces no length (chunked or until close, as SSE comes), when its head passes, with its status and `head_ms` |
| `IO.web WebSocket opened` | info | a WebSocket's 101, with `head_ms` |
| `IO.web moved a session to its exposure's newest enablement` | info | with `sid` (its first 8 hex), the enablement it left and the one it took, `max_age` and the request id |
| `IO.web serving: it admits only a connected ingress, over the mesh's mutual TLS` | info | the port serves, at first and after a recovery |
| `IO.web serves nobody, and nothing over plaintext` | error | with `code` and `detail`, once per change |
| `mesh client refused in the handshake: ...` | warn | a refused hive, with its role (logged by `fluxbee_sdk::mesh`) |
| `IO.web closed a connection after its handshake: its hive is not a connected ingress` | warn | the registry, read once more after the handshake, no longer admits the hive |
| `IO.web cut a connection: its hive is no longer a connected ingress` | info | the registry demoted or removed the hive |
| `IO.web at its handshake cap; closing new connections` | warn | at most every 10 s, with the count |
| `IO.web at its connection cap; closing a connection past its handshake` | warn | each one, with the peer |

**The request's line** (stage 2.2 §4.8, `proxy::Exchange`), written once when the request ends,
joined to the edge's by its request id (`ops timeline`):

| Field | |
| --- | --- |
| `request_id` | the edge's `X-Request-Id` |
| `exposure_id`, `host`, `enablement_id` | the exposure, and the session's enablement (or the refused cookie's) when there is one |
| `method`, `path` | the path without its query |
| `status` | what the client got: the upstream's status, or io.web's 401, 403 or 502 |
| `upstream` | the address dialled, when one was |
| `bytes_in`, `bytes_out` | the body each way; a WebSocket's bytes each way |
| `head_ms`, `duration_ms` | to the upstream's head, and in all |
| `ending` | `complete`, `client_closed`, `upstream_closed`, `cut`, `refused_401`, `refused_403` or `upstream_failed:<kind>` (`archi_listen`, `fluxbee_port`, `hive_unknown`, `hive_address`, `resolve`, `address_refused`, `connect`, `connect_timeout`, `http_handshake`, `head`, `switched`) |
| `detail` | a refusal's reason (the session's, or the header the Origin check refused and its value), or what failed |

io.web's other answers (the health check, the reserved paths, a host that is no exposure's) have no
line of io.web's own: the edge's line has them, and a redeemed or refused link has its own lines.

Every line about an exposure carries its `exposure_id` and its `host`, and the enablement's
`enablement_id` when there is one ([stage 2.1](../io-web-v2-stage-2-1.md) §5.10).

**No line holds a key, a code, a URL with its query, a cookie or a body.** A link is named by its
`link_id`, a session by the first 8 hex of its `sid`, and a request's path is logged without its
query (io-web-v2 §11.3).

io.web exchanges only system messages on the mesh (CONFIG_*, NODE_STATUS_*, its exposures'
commands and its vault reads), which the router does not copy to its stream: the link's answer,
which holds the code, never reaches the router stream.

## Posture

- **Its port is 9200 on every interface of the motherbee,** which has only its internal leg, as for
  the router's 9000 and identity's 9100.
- **Its only expected client** is the edge on the ingress, dialing out over the internal network.
- **No host rule is written for it:**
  - no role writes ufw or firewalld rules since 0.1.83;
  - the full firewall of host-posture stage 4 is parked, and when it is built its motherbee row
    needs tcp 9200 from the ingress;
  - an active ufw or firewalld can close 9200 as it can close 9000 or 9100 (posture alert
    `host_firewall_active`).

## For third-party systems

An exposure of a third party's system (an Odoo on a worker, io-web-v2 §13.5) is forwarded the same
way, transparently. What to keep in mind:

- **Cookies across the site.** Every `*.fluxbee.ai` host is one site. An upstream can set a cookie
  with `Domain=fluxbee.ai`, which the browser then sends to every other host of the site: Cloud's
  page, the main host and every other exposure, so to their upstreams too. That is session fixation
  against another hive's system. io.web strips Fluxbee's own cookies only, and passes an
  application's `Set-Cookie` as it is. Whether io.web strips the `Domain` attribute of an upstream's
  `Set-Cookie`, making it host-only, is a rewrite like the framing one: a question for the first
  third-party exposure. Cloud is safe as long as its own credentials are host-only `__Host-`
  cookies, which an upstream cannot set or overwrite.
- **The frame.** An application that sends its own `X-Frame-Options` or `frame-ancestors 'self'`
  (Odoo's login page, forum reports say) does not show in Cloud's iframe: io.web adds its own
  policy beside the application's, and the browser enforces both.
- **Forwarding headers.** An application that builds its own URLs trusts the edge's
  `X-Forwarded-*` only when told to (Odoo's `proxy_mode`).
- **The Origin check** refuses a legitimate write from another origin, such as an identity
  provider's POST back.
- **The upstream's scheme** is plain HTTP; an internal HTTPS upstream is not dialled yet.

## Not there yet

- A logout, and a deny-list of sessions by `sid`: a stolen cookie lasts until 30 min idle or its
  enablement's end, and Cloud's remedy is a disable and a new enable.
- An exposure's second port, as Odoo's WebSocket on its own port needs: an exposure is one host and
  one port (io-web-v2 §13.5).
- The operator's own way in, other than the motherbee or an SSH tunnel.

## Links

- [io.web v2](../io-web-v2.md): the design, §11.2 for stage 1, §12 for exposures and §13 for the
  transparent proxy.
- [io.web stage 2.1](../io-web-v2-stage-2-1.md): exposures and access, the build design.
- [io.web stage 2.2](../io-web-v2-stage-2-2.md): the transparent proxy, the build design.
- [SY.edge](../nodes/sy-edge.md): the door, and the proxy that dials io.web.
- [SY.admin](../nodes/sy-admin.md): the exposures' actions, their pushes and `web_status`.
- [io-common](io-common.md): what every IO node shares.
