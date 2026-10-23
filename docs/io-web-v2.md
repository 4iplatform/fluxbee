# io.web v2: the edge as reverse proxy, Archi through Cloud, artifacts

**Status:**

- The direction was agreed with the operator on 2026-10-22 and 2026-10-23.
- Stage 1 is designed and reviewed (§11):
  - release 0.1.84, the door ready for a second host, is validated on 8.x (2026-10-23; ledger
    0.1.84);
  - next is release 0.1.85, the proxy and io.web (§11.2).

**What this document is.** The single place that records where this track stands, so the next
iteration starts from here and not from the conversation. It supersedes:

- the io.web beta spec ([history](history/io-web-spec-beta-v1.md)), of which it keeps the reverse
  proxy to a fixed internal upstream over mTLS;
- the Archi-through-Cloud path of [host-posture §4.3](host-posture-and-exposure-spec-v1.md) and
  its decisions O5 and O6 (§5 below);
- a draft of 2026-10-22, *edge-publishing-v1*, never committed, that went too far (§8).

---

## 1. Decisions and principles

**What is needed** (the operator, 2026-10-23):

- **Publish Archi as a URL,** together with the Fluxbee tools that Fluxbee Cloud shows.
- **Publish artifacts:** HTML pages with their JS, and files for download.
- **No upload path.** *"Todos los archivos que suban van a venir por sus propios canales: wapp,
  slack, api"*; *"el upload lo hace el html de archi"*. Cloud exposes no chat for now.

**One door, several managers** (*"particionar el problema de entrada en una puerta, varios gestores
de problemas"*):

- **The edge is the door.** TLS, routing by host and path, generic protections (limits, methods,
  headers, slow clients). It has three surfaces (§3).
- **Each manager is an IO node and handles its own security,** *"como fue pensado siempre"*.
  - io.cloud handles Cloud.
  - io.web handles Archi's sessions and the tools behind the proxy.
  - Each channel's IO node handles its channel.
- **Sensitive things get their own host.** Archi lives on `archi.<id>`, never on the host of
  published content (D13).

**The principles for files** (the operator, 2026-10-22):

1. **Files move between hives only through Syncthing.** Nodes carry references, commands and
   metadata.
2. **Asset management is IO.blob's.**
3. **Whoever generates a file writes it into the blob first,** then asks IO.blob or SY.admin, with
   a command that carries the reference, to expose it.
4. **Files enter Fluxbee through the IO node that faces their source.** The node fetches them by
   reference, as io.slack does with Slack's files.
5. **The edge publishes the URL that connects to the asset.** It manages no assets.

**How to proceed** (the operator, 2026-10-23):

- **Fluxbee has many legs; this is not a security track only.** A new piece is a first-class
  citizen of the system (§6).
- **Design and build from the outside in:** the skeleton first, then the organs (§9).

---

## 2. What exists (verified in the code, 2026-10-23)

**The edge** (`src/bin/sy_edge.rs`) serves four routes: `/e/:ich`, `/e/:ich/*extra`,
`/public/:key` and `/healthz`.

**`/e/<ich>`, the channels,** works as designed in [edge v6](edge-ingress-spec-v6.md):

- **The HTTP request becomes a Fluxbee message** to the IO node that owns the channel.
- **The answer is always `200 application/json`.** The node sets neither the status nor any header.
- **The whole message is capped at 64 KiB:**
  - the router closes a node's socket on a frame over 128 KiB;
  - the body travels inside the JSON, escaped as text or in base64 (+33 %);
  - so the edge keeps half the frame as a margin.
- **This is enough for the channels.** None sends a file in its webhook: they send a reference, and
  the node fetches the file.
  - **WhatsApp** sends a `media_id`. io.wapp has to fetch the media from Meta's API, which is
    planned in its code but not built; io.wapp is test-grade.
  - **Slack** does not use the edge. io.slack opens an outbound connection (Socket Mode) and
    downloads attachments by their private URL. Slack accepts Socket Mode only for apps installed
    in one's own workspaces: an app distributed publicly would receive its events over HTTP,
    through `/e/`, with small payloads.
  - **io.api** rejects attachments. An integrator that needs to send a file would send a link, and
    io.api would fetch it.

**`/public/<key>`, single-file artifacts** ([io-blob-spec-v1](io-blob-spec-v1.md)):

1. A node on the mesh calls `publish_artifact` on SY.admin.
2. IO.blob@motherbee copies the file to `public/<sha256>`.
3. The Syncthing folder `fluxbee-blob-public` carries it one way to the ingress.
4. The edge serves it from that local copy.

The key is a 256-bit capability, valid for at most 30 days.

- **HTML is one sandboxed page** that can load nothing external.
- **Other files** download named by their hash.
- **The only publisher** is ai.generic's `publish_html_page`.

**The real reverse proxy** (browser → edge → internal HTTP service) was the third surface in the
io.web beta spec. It was never built.

**The mesh socket cannot carry web traffic.** It holds 128 KiB per message and does not stream.

**Archi** listens only on `127.0.0.1:3000` of the motherbee, with no authentication (D4, D10).

- It streams its chat with server-sent events.
- It accepts uploads: software up to 128 MiB, attachments up to 8 × 10 MiB.
- Today it is reached on the motherbee or through an SSH tunnel.

**io.cloud** runs on the motherbee.

- Cloud presents a fixed bearer, and the edge compares it on the `/e/` row before forwarding.
- io.cloud handles the tenant and permissions, and relays Cloud's operations to SY.admin.

**io.web** exists only as a name: in the orchestrator's list of core IO runtimes, and in a vault
check.

**Defects found in what exists** (FINDINGS A-81..A-86):

- **A-81:** an agent on a worker cannot publish.
- **A-82:** the edge logs capability keys.
- **A-83:** an unbuildable response answers an empty 200 without security headers.
- **A-84:** `bundle_upload` follows symlinks and extracts with no caps.
- **A-85:** io.slack's file fetch has no timeout and buffers everything.
- **A-86:** stalled readers can block the edge's downloads.

---

## 3. The target architecture

| Surface of the edge | For | Behind it | State |
| --- | --- | --- | --- |
| `/e/<ich>` | the channels: HTTP to a Fluxbee message | the channel's IO node | built, kept |
| `/public/…` (later renamed) | artifacts: static files and pages | the edge's local copy of `public/`, brought by Syncthing | built, to improve (§7) |
| reverse proxy | applications: Archi, Fluxbee tools | io.web, over an internal HTTP channel | to build |

**The internal HTTP channel:**

- HTTP with mutual TLS from the ingress to io.web, on a new port of the motherbee.
- It accepts only the ingress's mesh certificate. The motherbee already accepts the ingress on its
  mesh ports; this is one more, with the same logic.
- It carries streaming in both directions: pages, Archi's event stream, uploads to Archi.
- **The edge connects only to that fixed upstream,** never to a host or port named in a request or
  a route (from the beta spec).

**Placement:**

| Piece | Where | Why |
| --- | --- | --- |
| SY.edge | ingress | the door |
| io.cloud | motherbee | it sends commands to SY.admin, which refuses every caller on an ingress (A-62); it reads identity, which the ingress does not hold |
| io.web | motherbee | next to Archi, which listens on loopback (D4); the load of a few operators |
| IO.blob | motherbee | as today |
| artifacts | served by the edge from its copy of `public/` | downloads never load the motherbee, which curates each file once |

**No manager runs on the ingress.** It would put the managers outside the door: a compromised
ingress would control Cloud and Archi, not only see traffic. **No new hive** (the operator).

---

## 4. Archi through Cloud

1. **The code.** The operator, logged into Cloud, asks for Archi. Cloud asks SY.admin, through
   io.cloud, for a one-time code: about 120 s, bound to the Archi host, single use, logged.
2. **The exchange.** Cloud opens `https://archi.<id>/<exchange path>` top-level, never embedded
   (D9). The edge routes it to io.web, which redeems the code and sets a session cookie for that
   host only.
3. **Every later request:** the edge proxies it to io.web, io.web checks the session and passes the
   request to Archi on loopback.

- **Archi does not change** (D10).
- **Uploads** made from Archi's page travel through the same proxy into Archi, which writes them
  into the blob.

**What it needs outside the code:**

- the DNS record `archi.<id>`, made by hand by the operator (D12);
- a certificate for that name, issued by SY.admin's ACME client (0.1.82), which now serves more
  than one name;
- the edge choosing the certificate by name.

---

## 5. What changes in the October decisions

| Decision | Was | Now |
| --- | --- | --- |
| O5 (trust anchor for Archi through Cloud) | (a) end to end: the edge passes `archi.<id>` through by SNI without opening TLS; Cloud signs the mint with a key pinned on the motherbee | The edge terminates TLS and proxies. io.web owns the sessions, as each IO node owns its security. Signing the mint is still open (§10). The residual is the one io.cloud already has: a compromised ingress sees live traffic, and could ride a live session |
| O6 (certificates) | each holder its own certificate; io.web's for `archi.<id>` through the passthrough | the edge holds the certificates of every host it terminates, all issued by SY.admin's ACME client |
| D7, D13, D14 | io.web v2 = session-gated Archi plus artifacts; Archi never shares a host with tenant content; io.web handles artifacts | kept. Artifacts are served by the edge from `public/`, and managed by IO.blob with SY.admin |

---

## 6. io.web and the proxy as first-class citizens

A new node is not a special case. Every leg of Fluxbee has to know it:

| Leg | What it means here |
| --- | --- |
| Packaging | the runtime in the `.deb` and `base-nodes.json`; core-update and rollback like any node |
| Orchestrator | placement, spawn, the watchdog, reconcile, removal; inventory and versions |
| Control plane | CONFIG_GET/SET through the managed control plane (`managed_control_plane.rs`), state UNCONFIGURED/CONFIGURED/FAILED_CONFIG, the config contract documented |
| Identity | its ILK in the root tenant, like the other system IO nodes |
| Vault | its secrets (for example the key that signs sessions) by `vault_ref` |
| SY.admin | the actions and HTTP routes that declare proxy routes and mint codes; `mesh_access` rows; the action catalog |
| Router and policy | its name and the messages it exchanges; whatever the system policy needs |
| The edge | the third surface: proxy routes pushed by SY.admin, rebuilt after a reimage, the certificates per host |
| Cloud | the operation that asks for the code, relayed by io.cloud (the Cloud tables and catalog) |
| Archi | its tools and handbook, and its write list if new actions mutate |
| Observability | logs on the router stream, `NODE_STATUS_GET`, health in `ops.py health` |
| Posture | the new motherbee port in the host rules; the ingress's outbound connection |
| Docs and tests | node page, references, unit tests, CI, the gate on 8.x |

The edge's new surface has the same list from the edge's side.

---

## 7. Artifacts: what stays to decide

To be designed after the proxy. What is known:

- **Served by the edge** from its local copy of `public/` (§3), with SY.admin as the authority and
  IO.blob as the manager.
- **The operator's earlier answers (2026-10-22):**
  - a secret link that keeps its URL across versions;
  - expiry when asked for;
  - static content only.
- **Open:**
  - is an artifact one self-contained HTML page, or a multi-file site? Pages like Claude's
    artifacts are one file that loads libraries from a CDN, which today's sandbox blocks;
  - who publishes besides the agents (Archi, the operator);
  - original names on download.
- **Lessons from the reviews of the withdrawn draft** (§8), to reuse:
  - The edge cannot message SY.admin. Any reconcile is SY.admin polling the edge.
  - The edge stores and logs only `SHA-256(key)`.
  - A release switch carries a generation that never goes back.
  - An HTML page needs a content policy that names its own path, not `'self'`.
  - Names go out with RFC 6266 `filename*`.
  - Ownership is bound to an ILK, never to a node name.
  - Every new action needs its `mesh_access` row, its policy class, Archi's write list, and the
    Cloud tables.

---

## 8. Withdrawn (do not propose again without a new reason)

From the draft of 2026-10-22:

- **Uploads through the edge,** and an inbox folder from the ingress to the core: files enter
  through their own channels.
- **The edge as a proxy for files instead of a local copy:** files move through Syncthing.
- **A common fetcher for Cloud-provided URLs** (`ingest_file`), multi-file site bundles, per-tenant
  quotas, SQLite ledgers. None is needed for the current goal. They may return with a concrete need.

---

## 9. Next: from the outside in

A proposal to discuss with the operator before any code:

1. **The skeleton.**
   - io.web boots as a managed node: config, status, logs, inventory.
   - SY.admin pushes the edge a proxy route for `archi.<id>`.
   - The edge reaches io.web through the internal channel.
   - The browser gets one page from io.web through the edge, with a valid certificate for
     `archi.<id>`.
   - Archi stays out of it until there are sessions.
2. **The organs, one at a time,** each with its own review and gate:
   1. sessions from Cloud's one-time code, and Archi behind them;
   2. the operator's own way in (without Cloud);
   3. artifacts (§7);
   4. other Fluxbee tools behind io.web.

---

## 10. Open questions

1. **The mint.** Does Cloud sign its request with a key pinned on the motherbee, so a compromised
   ingress cannot ask for codes (O5's remaining half)? Or is Cloud's bearer through io.cloud enough,
   as for every other Cloud operation?
2. **Who may enter Archi.** Archi has no notion of users: whoever enters is "the operator". Does
   Cloud decide who is an operator (D8)?
3. **The operator without Cloud:** the motherbee or an SSH tunnel, as today, or io.web's own login.
4. **Which other Fluxbee tools** go behind io.web.
5. **The routes declared to the edge:**
   - only by host (`archi.<id>` → io.web);
   - or also by path on the main host;
   - and who may declare them: SY.admin only, from configuration.

---

## 11. Stage 1: the skeleton (design, reviewed 2026-10-23)

**Goal.** From a browser outside, `https://archi.<id>/` answers a page served by io.web on the
motherbee, through the edge, with a valid certificate. Archi stays out until sessions exist
(stage 2). This answers §10.5 for now: routes by host only, declared by SY.admin from
configuration.

**One review round** (fit with the code, security) split it into two releases. The certificate
for two names goes first, alone with the door's own fixes, because it is the piece that can take
PROD's `/e/` down.

**DNS (done, 2026-10-23):** `archi.hive-k3m9x7q2.fluxbee.ai` is a CNAME of `hive-k3m9x7q2.fluxbee.ai`
(71.182.182.80). The domain has no CAA record.

### 11.1 Release 0.1.84: the door ready for a second host

**Validated on 8.x (2026-10-23).** A-81 ran without the model, through a mesh client carrying
a test agent's identity. See the ledger's 0.1.84 entry.

1. **The declaration.** The motherbee's `hive.yaml` gets `web: {archi: true}`.
   - It needs at least one key: a bare `web:` reads as absent.
   - The host is derived: `archi.` + the host of `admin.public_base_url`. The domain never comes
     from code.
   - SY.admin reads it at boot and logs the declared host.
2. **One certificate for both names** (SY.admin's ACME client).
   - **What changes in the client:**
     - the settings hold a list of names;
     - one order carries every identifier, and every authorization is armed, readied, polled and
       cleared (the edge already keeps challenges per name);
     - the CSR carries every name, and coverage and chain checks run per name;
     - the status lists the names.
   - **`acme.json` keeps parsing,** with its `name` fields read as a one-name list. This is tested
     against PROD's file, so an upgrade never starts from an empty state.
   - **The transition is explicit,** as in 0.1.82, since a served certificate that misses a name
     only raises an alert and the issuance cap holds automatic orders:
     1. add `web:`;
     2. restart SY.admin;
     3. run `POST /acme/test`;
     4. run `POST /acme/renew`, which is forced and bypasses the cap.

     Removing `web:` takes effect at the next renewal.
   - **The main name is never held hostage.** When a combined order fails only on the added name,
     or the renewal is overdue, the client orders the main name alone. It keeps
     `name_not_covered` open, so the operator sees it.
   - **The public page will not show io.web's version:** Certificate Transparency logs publish
     `archi.<id>` anyway.
3. **The edge serves by host.**
   - **One canonical host per request:**
     - taken from the absolute-form authority if there is one, otherwise from the single Host;
     - two Hosts, or a Host that disagrees with the authority, gets 400;
     - lowercase, one trailing dot and `:443` stripped; other ports never match a route.
   - **Dispatch by host comes before any path route:**
     - a declared web host never gets `/e/` or `/public/` (D13). In this release it answers 404,
       with the security headers;
     - any other host keeps today's routes. Unknown hosts may move to 404 (host-posture §4.4) once
       nothing calls the edge by IP.
   - **Routing is per request,** never by SNI. With one certificate for both names, a different
     SNI gains nothing.
4. **The web routes reach the edge, and survive it.**
   - `EDGE_WEB_ROUTES {routes: [{host, upstream: "io.web"}]}` carries the full set. The edge
     accepts no other upstream.
   - SY.admin pushes it from its own 60 s loop, not the ACME loop, which runs every 6 h.
   - The edge keeps the set on disk, as it keeps its endpoints, so a restart serves it at once.
   - It travels on the edge's `tls` command channel, since the system loop can be held 60 s by a
     publication.
   - It is admitted only from `SY.admin@motherbee`. That means the router's edge command and
     response lists, `PROTECTED_SYSTEM_ACTIONS` and the policy's edge service actions, with both
     wasm files rebuilt.
5. **The door's own fixes:**
   - **A-87:** a single byte must not hold a connection forever. Connections are served with
     hyper's HTTP/1 builder and its head-read timeout, which starts before the first byte. There
     is no protocol sniffing and no HTTP/2: today h2 is reachable only by prior knowledge, and
     nothing uses it.
   - **A-86:** a minimum rate and an idle deadline, in a wrapper around every connection's
     stream, covering `/public`, `/e/` bodies and later the proxy. The peer address is kept, for
     the logs and for X-Forwarded-For later.
   - **A-88:** an `/e/` error never returns internal detail to the internet.
   - **A-83:** a response that cannot be built is a 500 with the security headers.
   - **A-82:** the edge stores and logs `SHA-256(key)`. Its `publications.json` is converted
     once, and the conversion is reported.
6. **In the same release:**
   - `turns.ctx`/`seq` and their index are dropped (step 2 of 2);
   - **A-81:** before taking its publication lock, SY.admin asks the producer's hive to send its
     blob folder (`sync_hint`, without waiting for idle), and waits up to 30 s for the file in the
     motherbee's `active/`. Any curate error releases what was curated.

**Gate:**

- **The certificate:**
  - `POST /acme/test`, then `/acme/renew`;
  - the served certificate names both hosts, and `/acme/status` lists both.
- **Hosts:**
  - `https://archi.<id>/` and `/e/…` on it answer 404, with a valid certificate;
  - on the main host, `/e/` (IO.cloud) and `/public` work as before.
- **Connections:**
  - a connection sending one byte, and one sending an h2 preface, are closed at the header
    timeout;
  - a restarted edge still answers 404 for the Archi host.
- **The rest:**
  - the columns are gone;
  - an agent on worker1 publishes a page.

### 11.2 Release 0.1.85: the proxy and io.web

1. **One shared module in the SDK** (behind a feature): the mesh TLS material and one reader of
   the hive registry. The router, SY.admin, the edge and io.web use it; today it is spread between
   the router library and the SY.admin binary.
2. **The edge proxies a web host to io.web.**
   - **The upstream:**
     - the host the ingress's router dials, on the port of one SDK constant shared with io.web
       (never 8443, which the handbook documents as the edge's old port);
     - its certificate is verified inside the handshake, with rustls' WebPki verifier over the
       mesh CA and the server name `motherbee`.
   - **Per-route policy:** in this stage, GET and HEAD only, with no body.
   - **Framing and headers:**
     - the framing is rebuilt: `Content-Length`, `Transfer-Encoding` and `Expect` are never
       copied, and any TE other than `chunked` is refused;
     - hop-by-hop headers, and the ones `Connection` lists, are stripped first;
     - the edge then sets its own `X-Forwarded-*`, `Forwarded` and `X-Request-Id`, replacing any
       the client sent (with `X-Real-IP`, `True-Client-IP`, `X-Original-URL`, `X-Rewrite-URL`);
     - Upgrade, CONNECT and TRACE are refused;
     - hop-by-hop headers are stripped from responses too.
   - **Errors and limits:**
     - an upstream failure is a fixed 502 with the request id;
     - a cap on in-flight proxied requests answers a quick 503;
     - the upstream connect and response-head have timeouts.
3. **io.web:** a new managed IO runtime, a first-class citizen (§6).
   - **Where it runs:** crate `nodes/io/io-web`, the motherbee only (a fail-closed self-check),
     the root tenant.
   - **Its configuration:** the common IO control plane, CONFIG_GET/SET. The port comes from the
     shared constant.
   - **How it starts.** `base-nodes.json` lists it with `boot: false`, so it runs only where the
     operator starts it: `run_node IO.web@motherbee` in the root tenant, one explicit step that
     the ledger records, as for any new runtime on an existing hive.
   - **TLS:**
     - its server certificate is the motherbee's mesh leaf;
     - a client verifier admits only a certificate whose hive the registry lists, live, with the
       role `ingress`;
     - connections from a hive that leaves the registry are cut, as the router does.
   - **What it serves:**
     - `GET /`: a minimal page, with no version and nothing internal, carrying
       `frame-ancestors 'none'`, `nosniff` and a strict CSP;
     - `GET /healthz`;
     - everything else is 404.
   - **Like any node:** NODE_STATUS_GET, logs on the router stream, inventory, upgrades with the
     core.
4. **The operator:**
   - `GET /web/status` (`web_status`) shows the host, the certificate's names, the route the edge
     acknowledged, and io.web's state;
   - `ops.py health` prints a `web` line.

**Gate:**

- the page is served from outside;
- worker1 is refused at io.web's handshake;
- a POST is refused at the edge;
- the restart and route checks of 0.1.84 still pass.

### 11.3 Before stage 2 (sessions)

These come from the security review:

- a `__Host-` session cookie;
- CSRF by exact Origin against `https://archi.<id>`, never against the Host header;
- the edge passes only `__Host-` cookies on the proxy, and none on the main host;
- proxy logs carry paths without their query, so the exchange code never reaches a journal;
- io.web answers 401 from the headers before reading any body;
- large bodies only on upload paths, counted on the bytes streamed.
