# io.web stage 2.2: the transparent proxy, straight to Cloud's iframe (build design, 2026-11-19)

Base: `daily_onworking_coa` at `31126b57`, 0.1.149 live. This is the build design of
[io-web-v2.md](io-web-v2.md) §13, the operator's decisions of 2026-11-19. Every claim about existing
code carries `file:line`, read on 2026-11-19.

Paths are shortened:

| Short | Path |
| --- | --- |
| `admin` | `src/bin/sy_admin.rs` |
| `exposures` | `src/bin/sy_admin/exposures.rs` |
| `edge` | `src/bin/sy_edge.rs` |
| `web/web`, `web/exposures`, `web/main` | `nodes/io/io-web/src/{web,exposures,main}.rs` |
| `arch` | `src/bin/sy_architect.rs` |
| `sdk/` | `crates/fluxbee_sdk/src/` |

---

## 0. What this builds (io-web-v2 §13)

- **A transparent reverse proxy** (§13.1, decision 3). What Archi does on its port it does the
  same inside Cloud's iframe. Fluxbee adds only three things:
  - the entry: Cloud's link, redeemed for the cookie (2.1);
  - the end: everything open is cut when the access ends (§13.4);
  - the permission for Cloud's page to frame it (§13.7).
- **No time, size or rewrite of the proxy's own** (decision 3; A-142 decided).
- **Streams frame by frame** (§13.3).
- **A renewal interrupts nobody** (decision 7).
- **Cookies (A-351):** the system's own pass, Fluxbee's never reach an upstream.
- **The headers of §13.7's table, and `cloud_origins`** in `publish_cloud_endpoint`.
- **The `Origin` check** on what writes (§13.8).
- **The gate** of §13.9.

Archi does not change (D10). io.cloud and Cloud's six operations do not change. Cloud's side is its
iframe, whose `src` is the link's `url` (§6).

---

## 1. The path of one request

```text
browser, inside Cloud's page (https://cloud.fluxbee.ai)
  │  TLS to the door, host archi.hive-<id>.fluxbee.ai, cookie __Host-fluxbee_session
  ▼
SY.edge (ingress)    TLS; host route; every method but CONNECT and TRACE; the body streamed;
  │                  `Upgrade: websocket`; no wait of its own; all of the client's cookies
  │                  (§3)
  │  mutual TLS to the motherbee's 9200, one connection per request (as built)
  ▼
io.web (motherbee)   the reserved prefix; the session (401 from the headers); the Origin check
  │                  (403); the cookie's own pairs removed; the cut's registry (§4)
  │  plain HTTP/1.1, one connection per request
  ▼
the upstream         Archi on loopback, a worker of the registry, or an internal address
```

The answer comes back the same way, frame by frame. io.web sets the application's headers of
§13.7, adds the sliding cookie, and drops `Server` and `X-Powered-By`. The edge adds HSTS.

---

## 2. SY.admin: `cloud_origins`

- **The field.** `publish_cloud_endpoint` (admin:8728-) takes an optional `cloud_origins`, a list
  of strings.
  - Absent: the origins kept stay. A run that only rotates a lost token names none.
  - `[]` clears them: nobody frames.
- **Each origin** is `https://` and a DNS host, lowercase as given, with an optional port. It has
  no user, path, query, fragment or wildcard, and is not an IP literal. At most 4, no duplicate.
  - The check runs before anything is read or minted, as `edge_node`'s does (admin:8743-8765).
  - Anything else is `INVALID_REQUEST`, which names the origin and the rule.
  - An origin outside the hive's own domain is taken but would frame a page whose cookie does not
    travel (§13.1, decision 4). The answer says nothing of it, and the node page documents it.
- **Where it is kept.** In SY.admin's exposures registry, `RegistryFile` (exposures:131-137), as
  `cloud_origins` with `#[serde(default)]`; `schema_version` stays 1.
  - It is written with a new generation once the run's door is open, after step 2 of the run's
    order (admin:8706-8726).
  - A door that does not open changes none of it. A write that fails is in the answer
    (`cloud_origins_persisted: false`), and the next write of the registry retries it, as its other
    writes do.
  - The answer names the origins kept.
- **The pushes:**
  - `IO_WEB_EXPOSURES` carries them as `cloud_origins`, from R1's field (§8). They come from
    `io_web_payload` (exposures:362-373).
  - `EDGE_WEB_ROUTES` carries them as `frame_origins`, from `payload_for` (admin:1082-1094), and
    only when there is at least one. The edge before 2.2 refuses any unknown field
    (`#[serde(deny_unknown_fields)]`, edge:3559-3572), so it never sees one until the operator
    names an origin, after the deploy.
- **`GET /web/status`** shows `cloud_origins`.
- **The action's contract and summary** name the field (`admin_action_request_contract`,
  `admin_action_summary`). The executor runs this action and shows its token once
  (admin:4272-4276), so Archi's handbook names the field: the origins are the page of the Fluxbee
  Cloud that manages the hive.
- **Tests:**
  - `publish_cloud_endpoint_keeps_the_origins_it_is_given`: named, absent (kept) and `[]` (cleared);
  - `publish_cloud_endpoint_refuses_an_origin_that_is_not_https_host_only` (http, a path, a
    wildcard, an IP, five origins, a duplicate);
  - `a_door_that_does_not_open_changes_no_origin`;
  - `the_pushes_carry_the_origins_and_the_edge_gets_none_while_empty`.

---

## 3. SY.edge: the pass-through

### 3.1 What a web host takes

- **Methods.** `web_policy_refusal` (edge:4764-4800) refuses only `CONNECT` and `TRACE`, with 405
  and `Allow` naming the methods it serves.
- **Bodies** pass. The request io.web gets carries the client's body, the `DemandBody` the door
  already wraps it in (edge:3836-3838), in place of `Body::empty()` (`io_web_request`,
  edge:4815-4833). hyper rebuilds the framing, as the door does today: `Content-Length` when the
  body's size is known, chunked otherwise.
- **Upgrades.** `Upgrade: websocket` on a GET whose `Connection` names `upgrade` passes; any other
  upgrade is refused, as today (400 `UPGRADE_NOT_ALLOWED`).
  - The door serves with upgrades (`serve_connection(…).with_upgrades()`, edge:3846-3849), and so
    does the connection to io.web (`dial`, edge:4666-4686).
  - The request to io.web carries `Connection: upgrade` and `Upgrade: websocket` in place of
    `Connection: close` (edge:4886). The strip of hop-by-hop headers keeps them for this request
    only (edge:4354-4364).
  - On io.web's 101, which `send` takes for a failure today (`IoWebFailure::Switched`,
    edge:4661-4663), the edge answers the client 101. It then copies bytes both ways between the two
    upgraded streams (`hyper::upgrade::on`) until either side closes.
- **No wait of its own.** `IO_WEB_HEAD_TIMEOUT` and `IO_WEB_BODY_IDLE_TIMEOUT` (edge:4347, 4998)
  go, with the `stalled` timer of `IoWebBody` (edge:5004-5040). `IO_WEB_CONNECT_TIMEOUT`
  (edge:4344) stays: reaching io.web is the edge's own work, not the application's.
- **The pace** (A-86, edge:4057-4240) stays as built.
  - It cuts a client that stops reading what the edge writes, and one that does not send a body the
    edge waits for.
  - A read is paced only while a handler waits for body bytes (edge:4222-4236), so an idle stream,
    an idle WebSocket or a long wait for Archi's answer is never taken for a slow client.
- **The slots** (A-90): a stream or a WebSocket holds its request's slots until it ends. By default
  that is 16 per address and 128 in all (edge:144-147). Archi's page holds one stream.
- **Cookies** (A-351). `Cookie` passes with every well-formed pair the client sent, whatever its
  name. `host_prefixed_cookies` (edge:4903-4920) keeps its syntax check (`is_tchar`,
  `is_cookie_octet`) and drops the prefix condition. io.web removes Fluxbee's own pairs (§4.3).
- **Kept as built:** the method overrides and the URL-rewrite headers are dropped
  (`DROPPED_REQUEST_HEADERS`, edge:4370-4383), so no GET reaches an upstream as a write past io.web's
  Origin check. The edge's forwarding headers replace the client's (edge:4835-4891).

### 3.2 Headers

- **`Strict-Transport-Security: max-age=31536000; includeSubDomains`** on every answer of the door,
  on every host (`with_security_headers`, edge:3965-3977). The door serves TLS only.
- **The edge's own set** (edge:5929-5938) goes on the edge's own answers only.
  - An answer that comes from io.web is marked, as `OwnContentPolicy` marks one today (edge:5280).
    It gets HSTS and nothing else: io.web sets its own headers (§4.4, §4.5).
  - On the edge's own answers the set adds `Permissions-Policy`, every feature off.
  - Their `frame-ancestors` is the routes' `frame_origins` on a web host, so an edge error inside
    Cloud's frame shows the edge's page. With none, it stays `'none'`, as it does on the main host.
- **`WebRoutes`** (edge:3570-3572) takes `frame_origins`, optional and empty by default.
  `validate_web_routes` (edge:3726-) checks it with SY.admin's rule (§2), and `web_routes.json`
  keeps it.

### 3.3 Logs

- The proxied request's line (edge:4545-4555) is unchanged: it times the head.
- An upgraded connection logs one more line when it closes: its request id, its bytes each way and
  how long it lived.

### 3.4 Tests

- `a_web_host_takes_every_method_but_connect_and_trace`
- `a_request_body_reaches_io_web_as_it_comes` (a slow body, its frames in order, `Content-Length`
  kept)
- `an_answer_streams_to_the_client_frame_by_frame` (a delayed second frame arrives alone, with no
  wait cut)
- `a_websocket_is_tunnelled_both_ways_and_any_other_upgrade_is_refused`
- `the_client_cookies_reach_io_web_whole_but_malformed_pairs`
- `an_io_web_answer_gets_hsts_and_none_of_the_edge_s_set`
- `the_edge_s_own_answer_on_a_web_host_names_the_frame_origins`
- `an_idle_stream_is_not_cut_by_the_pace`
- the A-90 slot tests and the A-86 pace tests, kept green

---

## 4. io.web: the proxy

### 4.1 Serving

- `serve_http` (web/web:433-460) serves with upgrades. `respond` (web/web:550-591) becomes
  asynchronous. Its first steps are 2.1's: the reserved prefix, the unknown host (404) and the
  session, with the 401 coming from the headers before a byte of the body is read.
- **A valid session reaches the upstream**, whatever the method. The placeholder page (`PAGE`,
  web/web:467-479) goes.
- **The `Origin` check** (§13.8) runs before anything is forwarded, on POST, PUT, PATCH and DELETE,
  and on a GET with `Upgrade: websocket`. It is the algorithm of Go's `CrossOriginProtection`:
  - `Sec-Fetch-Site: same-origin` or `none` passes;
  - any other value answers 403;
  - without that header, an `Origin` equal to `https://<host>` passes and any other answers 403;
  - with neither header, the request passes;
  - the 403 is a fixed page of io.web's, like the 401's (web/web:499-513), logged with its
    exposure and the header that refused it.

### 4.2 The upstream

| `upstream.kind` | Dialled | Read from |
| --- | --- | --- |
| `archi` | `127.0.0.1:<port>` | hive.yaml's `architect.listen` (arch:235-237, 6450-6459), read at boot from io.web's `config_dir` (web/main:111, 146-148); the default `127.0.0.1:3000` (arch:92). A listen on `0.0.0.0` or `[::]` is dialled on loopback. Archi's `JSR_ARCHITECT_LISTEN` lives in Archi's unit, which io.web cannot read: the node page documents it |
| `hive` | `<address>:<port>` | the registry's `address` (sdk/mesh/registry.rs:134-136) at the request. A hive gone from the registry answers 502 |
| `address` | `<address>:<port>` | the exposure |

- **The transport** is plain HTTP/1.1, one connection per request, nothing pooled, as the edge's to
  io.web (edge:4588-4593). The scheme stays open for the first third-party exposure (§13.5).
- **The connect timeout** is 5 s, io.web's own reachability. There is no wait for the head and no
  cut for silence.
- **An upstream that refuses or fails** answers io.web's fixed 502 page, logged with the failure's
  kind.

### 4.3 The request to the upstream

- **The headers** are the edge's, as they came: `Host` is the public host, and `X-Forwarded-For`,
  `-Proto`, `-Host`, `Forwarded` and `X-Request-Id` are the edge's (edge:4835-4891). The
  hop-by-hop headers are stripped again; io.web sends `Connection: close`, or the upgrade's pair.
- **`Cookie`** loses every pair whose name begins with `__Host-fluxbee_`, and goes when none is
  left.
- **The body** streams as it comes.
- **An upgrade:** on the upstream's 101, io.web answers the edge 101 and copies bytes both ways.

### 4.4 The answer back (§13.7's table, the application's column)

- **The body** streams frame by frame (hyper's `Incoming` as the response body), never gathered and
  never compressed.
- **The headers:**
  - the hop-by-hop ones are stripped;
  - every `Set-Cookie` whose name begins with `__Host-fluxbee_` is removed;
  - `Server` and `X-Powered-By` are removed;
  - `X-Content-Type-Options: nosniff` is added when missing;
  - `Content-Security-Policy: frame-ancestors <origins>` is added as a header of its own, `'none'`
    without origins; the application's own CSP headers stay as they are;
  - the edge adds HSTS (§3.2).
- **The session's cookie** is appended when it slides (web/exposures:1008-1014) or moves (§4.6).

### 4.5 io.web's own answers (§13.7's table, Fluxbee's column)

- `SECURITY_HEADERS` (web/web:522-532) stay.
- `Permissions-Policy` is added, every feature off.
- The policy's `frame-ancestors` names the origins in place of `'none'`, so the 401 and the enter
  page show inside Cloud's frame. The enter page's refresh to `/` (web/web:481-497) then runs in
  the frame.

### 4.6 The renewal (§13.1, decision 7)

- `Exposures::session` (web/exposures:954-1015) checks a cookie as today.
- When the exposure's newest enablement is another one whose key io.web holds, a cookie that serves
  is minted again under it:
  - its `e` and its tag change; `sid` and `issued` stay; `active` is now;
  - its `Max-Age` runs to the newest enablement's end;
  - the answer carries it, as a slide.
- The previous enablement stays live in the set until its own end ("at most two",
  sdk/protocol.rs:787-790), so its key still verifies the cookie meanwhile.

### 4.7 The cut (§13.4)

- **What is open** is held per exposure: every forwarded request until its answer ends, every
  upgraded connection until it closes. Each has a cancellation signal.
- **When it fires.** An exposure is left without access when a push removes its last live
  enablement or the exposure itself, or when its last enablement's `until` passes. io.web holds a
  timer per exposure, set again at each push. Then it cuts all of that exposure's at once:
  - **waiting for the upstream's head:** io.web answers its 401 page and drops the upstream's
    connection;
  - **an answer streaming:** io.web ends its body with an error, which the edge passes on by
    closing the client's connection (edge:5016-5040);
  - **an upgraded connection:** both streams are dropped.
- **The bound** is the push. A disable's push was acknowledged in 0.57 s in 2.1's gate. The clock's
  end comes at the second.

### 4.8 Logs and status

- **One line at `info` per forwarded request, when it ends.** It carries:
  - the edge's request id, the exposure id and its host, the method, the path without its query,
    and the status;
  - the bytes each way, and the milliseconds to the upstream's head and in all;
  - how it ended: `complete`, `client_closed`, `upstream_closed`, `cut`, `refused_401`,
    `refused_403` or `upstream_failed:<kind>`.

  Never a cookie, a query or a body. A stream or a WebSocket also logs a line when it opens. Today's
  `debug` line (web/web:446-454) goes into it.
- **`extensions.web.exposures`** (web/exposures:1017-1040) adds, per exposure:
  - `open`: `waiting`, `streams` and `websockets`;
  - `proxied` by class (`2xx`, `3xx`, `4xx`, `5xx`);
  - `refused` (`401`, `403`), `upstream_failures`, `cut` and `sessions_moved`.

  It also adds the set's `cloud_origins`.

### 4.9 Tests

- forwarding:
  - `a_session_reaches_the_upstream_with_every_method`
  - `a_request_without_a_session_reaches_no_upstream_and_reads_no_body`
  - `the_upstream_gets_the_edge_s_forwarding_headers_and_no_fluxbee_cookie`
- streaming and upgrades:
  - `the_answer_streams_frame_by_frame`
  - `a_websocket_is_tunnelled_and_cut`
- headers:
  - `the_answer_s_headers_follow_the_table` (`Server` and `X-Powered-By` gone, `nosniff` when
    missing, `frame-ancestors` its own header, the application's CSP untouched, a
    `__Host-fluxbee_session` `Set-Cookie` dropped)
  - `io_web_s_own_answers_frame_only_for_cloud_s_origins`
- the Origin check:
  - `a_write_from_another_origin_is_refused_403` (each of the algorithm's rows, and a WebSocket's)
  - `a_write_from_the_exposure_s_page_passes`
- the renewal:
  - `a_renewal_moves_a_session_to_the_new_enablement`
  - `an_open_stream_lives_through_a_renewal`
- the cut:
  - `a_disable_cuts_what_is_open_at_once` (a wait, a stream, a WebSocket)
  - `the_enablement_s_end_cuts_what_is_open`
- the upstream's address:
  - `archi_is_dialled_where_hive_yaml_says_it_listens`
  - `an_upstream_that_refuses_answers_502`

---

## 5. ops.py and observability

- The `web` line of `ops.py health` (`web_health`, lab/ops.py:303-) adds what is open (`open=N`),
  the failures (`fail=N`) and the number of origins (`origins=N`).
- `ops timeline` joins the edge's and io.web's lines by their request id.
- The logs stay local, in the journal.

---

## 6. Docs

- **docs/io/io-web.md:**
  - the proxy;
  - the upstream's address, including that Archi's env override is not seen;
  - the Origin check, the cut, the renewal;
  - the headers, the logs and the status.
- **docs/nodes/sy-edge.md:** the pass-through, the waits gone, the cookies, HSTS, `frame_origins`.
- **docs/nodes/sy-admin.md:** `publish_cloud_endpoint`'s `cloud_origins`.
- **The handbook** (docs/archi/handbook_fluxbee.md): the field, and that Archi sees no change.
- **docs/io-cloud-api.md §5.8:**
  - the iframe's `src` is the link's `url`;
  - the hive frames only the origins its operator named in `publish_cloud_endpoint`;
  - one domain, or the cookie does not travel;
  - a renewal moves the sessions;
  - writes come from the exposure's own page.
- **docs/reference/operations.md:** the `web` line.
- **FINDINGS:** A-142 and A-351 are marked built at integration; the board's item 31 is updated.

---

## 7. The gate on 8.x (§13.9, after R2)

The tools live in the git-ignored `scratchpad/gate-tools/`, as in 2.1, and print no token, link or
cookie. Each step's time goes live in the script; afterwards `ops timeline` reads them. One model
call in all (step 7).

0. **Smoke:** launch a throwaway node and read a stopped one.
1. **The origins.** Cloud's team is told before the token rotates.
   - `publish_cloud_endpoint` runs with `cloud_origins: ["https://cloud.fluxbee.ai"]`.
   - The new token goes to a 0600 file, handed over by path.
   - `GET /web/status` shows the origins, and the edge and io.web acknowledge them.
2. **The test upstream.** A throwaway HTTP server on worker1:18080, started and stopped by the gate.
   It serves:
   - `/slow`, which answers after 90 s;
   - `/sse`, an event a second with its own send time;
   - `/upload`, which counts the bytes and hashes them;
   - `/ws`, a WebSocket echo;
   - `/echo`, which answers with the headers it got;
   - `/cookie`, which sets `app_session=…` and also tries `Set-Cookie: __Host-fluxbee_session=x`.

   Every answer carries `Server`, `X-Powered-By` and a CSP of its own. The gate publishes `gate23`
   to it, enables it and redeems a link into a cookie jar.
3. **Through Cloud's door:**
   - **Waits and streams:**
     - `/slow` answers 200 at 90 s (A-142's case);
     - `/sse`'s cadence is measured, from each event's send time to its arrival;
     - 100 MB to `/upload` arrive whole, with the same hash, and the throughput is measured;
     - `/ws` echoes.
   - **Forwarding and cookies:**
     - `/echo` shows the public host, the edge's forwarding headers and the jar's `app_session`,
       and no `__Host-fluxbee_session`;
     - `/cookie`'s `__Host-fluxbee_session` never reaches the jar.
   - **Headers:**
     - a proxied answer has HSTS, the test upstream's CSP, a separate `frame-ancestors
       https://cloud.fluxbee.ai`, and `nosniff`, with no `Server` and no `X-Powered-By`;
     - io.web's 401 has its strict set, HSTS, `Permissions-Policy` and `frame-ancestors` with
       Cloud's origin.
   - **The Origin check:**
     - a POST with `Sec-Fetch-Site: same-site` answers 403, as does one with a sibling's `Origin`;
     - `same-origin`, or neither header, passes;
     - a WebSocket from a sibling's `Origin` answers 403.
4. **The cut.** With `/sse`, `/slow` and `/ws` open, a disable through the door. Each socket
   closes; the gate measures the time from the disable's answer to each close. The stream's
   reconnect answers 401.
5. **The renewal.** Enable for 120 s and open `/sse`, then enable again. The stream stays open past
   the first end, the next request's cookie names the new enablement, and the session still serves
   after the first end.
6. **The clock's end.** Enable for 60 s with `/sse` open: the stream is cut at `until`, ±1 s.
7. **Archi:**
   - enable `archi` and redeem a link;
   - check `/` (Archi's own page), `/api/status` and `/api/sessions`;
   - check `/api/messages/stream`, whose ping comes every 5 s, measured;
   - one chat turn through `POST /api/chat`, timed. This is the one model call;
   - a POST to `/api/chat` with a sibling's `Origin` answers 403, and Archi records no turn for
     it.
8. **Restarts.**
   - io.web, with a stream open: the stream ends, and the session serves again after the restart.
   - The edge: its streams end, and its routes are back at once.
9. **The iframe**, once, with Cloud's team, when its iframe exists: Archi inside Cloud's page on
   `cloud.fluxbee.ai`, with its page, a chat, an upload and the live messages. A disable cuts it.
10. **Cleanup.** `gate23` is unpublished and the test upstream stopped. Archi's exposure is kept,
    closed.

**The timings reported:**
- the proxy's own share (the head through the door against the same request on worker1 itself);
- the stream's cadence;
- the cut;
- the upload's throughput;
- the renewal's gap, which should be none;
- io.web's and the edge's restarts.

**Left to the tests:** the algorithm's rows one by one; malformed cookies; the header table's
corner cases; the pace and the slots.

---

## 8. Releases and branches (METHOD §9: one branch per binary; shared code alone)

| Release | Branches | What | Why in this order |
| --- | --- | --- | --- |
| R1, the Rust SDK alone | `coa/web22-sdk` | `IoWebExposuresPayload.cloud_origins` (sdk/protocol.rs:769-776), `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, with its wire test | SY.admin and io.web compile against it; io.web before R2 ignores it |
| R2, the binaries | `coa/web22-admin`, `coa/web22-edge`, `coa/web22-io-web` | §2, §3 and §4, each with its docs and tests; ops.py at integration | each is green alone. The edge's pass-through reaches io.web, which refuses or forwards; io.web with no origins frames nothing |

- **Totals:** four branches, two releases.
- **The gates:**
  - R1's: the smoke, and io.web acknowledging SY.admin's set.
  - R2's: §7, whole.
- **Files.** ops.py, FINDINGS, the logbook and the ledger are written on `daily_onworking_coa` at
  integration. The handbook goes in R2's SY.admin branch.
