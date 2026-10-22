# SY.edge

SY.edge is the public door of a Fluxbee mesh. On the ingress hive it serves HTTPS and turns each
request on an externalized channel (`/e/<ich>`) into a mesh message for the IO node that owns the
channel, then returns that node's reply. It also serves the static files that `SY.admin` published
for producers (`/public/<key>`). It resolves no identity: it forwards by the owner name SY.admin
gave it, and it takes commands only from `SY.admin@motherbee`. Its certificate comes from SY.vault;
it swaps a new one in place and answers the ACME TLS-ALPN-01 challenge SY.admin arms when it
orders one ([ACME design](../acme-edge-certificates-v1.md) §4).

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
| State | `/etc/fluxbee/edge.endpoints.json`, `/var/lib/fluxbee/state/sy-edge/publications.json`, reads `/var/lib/fluxbee/blob/public` |

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

1. `Config::load` reads `/etc/fluxbee/hive.yaml` (`role` and the `edge:` section) and the
   environment. A missing or unparsable hive.yaml gives no `edge:` section, so no public listener
   (`load_hive_edge`).
2. Logging from `RUST_LOG` (default `info,sy_edge=debug,fluxbee_sdk=info`), reloadable at run
   time (see Config).
3. Connects to the router (`RouterDispatcher::connect_with_retry`, retry every 1 s). The three
   TLS commands go to their own command channel, every other `system` message to one shared
   channel (`build_sy_edge_rpc_profile`).
4. Loads the endpoint table from `endpoints_path` (`load_registry`) and the public artifact table
   from `publications_path` (`load_public_registry`, which drops expired or invalid rows).
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
7. Starts the TLS command task (`run_tls_commands`), beside the system loop.
8. Starts a reaper that every 600 s drops expired public rows and, if it dropped any, rewrites
   the publications file.
9. Runs the system message loop until the router channel closes.

## HTTP surface

`run_frontend` serves four routes:

| Route | Methods | What it does |
| --- | --- | --- |
| `/e/:ich` | any | Forward to the channel owner (`invoke_root` → `invoke`) |
| `/e/:ich/*extra` | any | Same, with `extra` passed as the path (`invoke_extra`) |
| `/public/:key` | GET, HEAD (else 405 with `Allow: GET, HEAD`) | Serve a published blob (`serve_public_artifact`) |
| `/healthz` | any | Answers `ok` |

With TLS, each connection goes through the acceptor of the TLS section below (`accept_tls`), then
hyper (`serve_tls_connection`); the main `ServerConfig` has no client authentication and an empty
ALPN list. Without TLS (the `plaintext` mode, see Config) it uses `axum::serve`.

Limits on the TLS door (`serve_tls_listener`, `DoorLimits`):

- **Connections:** at most `MAX_TLS_CONNECTIONS` (4096) at once, handshakes included. One more is
  closed at accept, and the refusals are logged at most once per 10 s with their count.
- **Silent clients:** the TLS handshake has its own 10 s timeout (TLS section). After it, the first
  request byte must arrive within `HTTP_HEADER_READ_TIMEOUT` (30 s). hyper-util's HTTP/1-or-2
  detection waits with no timeout of its own, so the edge waits for that byte itself. hyper then
  applies the same 30 s, through a Tokio timer, to every HTTP/1 request head, an idle keep-alive
  connection included.
- **A failed accept** (out of file descriptors, say) waits 100 ms before the next one, instead of
  spinning.

Error bodies are JSON: `{"ok": false, "error_code": ..., "message": ...}` (`http_error`).

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
   `413 BODY_TOO_LARGE`. A non-UTF-8 body is base64-encoded.
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
   `413 REQ_TOO_LARGE`.
9. **Reply.** Waits for any message with the same `trace_id` (`edge_reply_matcher`) up to
   `HANDLER_TIMEOUT_MS` (default 30 000). The reply payload becomes the body of a
   `200 application/json` response; the node cannot choose the HTTP status. Router
   `UNREACHABLE`/`TTL_EXCEEDED`: `502 HANDLER_UNREACHABLE`. Timeout: `504 HANDLER_TIMEOUT`
   (`rpc_error_to_http_status`, `rpc_error_code`).

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

- `key` must be 64 lowercase hex characters and present in the public table, with
  `expires_at` in the future; otherwise `404 PUBLIC_ARTIFACT_NOT_FOUND`.
- `ETag` is the SHA-256 in quotes; a matching `If-None-Match` gets `304`.
- At most `public_max_inflight` (default 128) downloads at once; else `503 PUBLIC_BUSY`.
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

## Mesh messages

Handled one at a time in the loop at the end of `main`, except the three TLS commands: the RPC
profile sends those to their own channel (`RPC_CH_TLS`, `build_sy_edge_rpc_profile`), and
`run_tls_commands` answers each in its own task over the shared TLS state, so they never wait
behind `EDGE_PUBLISH_BLOB` (up to 60 s) or a secret resolution.

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
| `VAULT_SECRET_CHANGED` | `SY.vault@<vault_hive>` | Re-fetch channel secrets; reload the certificate when the notice is about its key (below) |
| `CONFIG_GET` / `CONFIG_SET` | `SY.admin@motherbee` | The edge's own config (`apply_node_config`) |
| `NODE_STATUS_GET` | an orchestrator, or SY.admin / SY.vault of the same hive | Default status reply (`try_handle_default_node_status`) |

The eight `EDGE_*` commands are checked twice. The router delivers a protected SYSTEM action only
if `policy/system.rego` allows its origin (`prepare_local_delivery` in `src/router/mod.rs`; rule
(1) for the `EDGE_*` commands, rule (2) for `CONFIG_GET`/`CONFIG_SET`, rules (3)–(5) for
`NODE_STATUS_GET`). The edge then accepts an `EDGE_*` command only when the router-stamped
`src_l2_name` equals `EDGE_CONTROL_AUTHORITY`, `"SY.admin@motherbee"`
(`authorize_edge_service_command`; constant in `src/router/system_policy.rs`); a TLS command
from anyone else changes nothing and gets `{status: "error"}`. A VAULT_SECRET_CHANGED from any
origin other than its vault is logged and ignored. SY.vault addresses the notices about the edge's
own secrets to it as a unicast; the router keeps SY.vault's broadcast out of the DMZ (A-78,
[SY.vault](sy-vault.md)).

Row checks on open (`validate_endpoint_row_for_open`): `ich` not empty; `inbound_family` not
empty and not `system`; a unicast row needs a fully-qualified `IO.*` `owner_l2_name` and, for
`shared-secret`, a `secret` or `secret_ref`; a fanout row needs an `IO.*` glob with `@` and a
`verify_token` or `verify_token_ref`.

Who may open what is decided in SY.admin before the edge is asked: an `IO.*` node only for a
channel it owns (`authorize_channel_command`), the operator for any channel, and fanout rows only
by the operator. See [SY.admin](sy-admin.md).

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
| `blob_public_root` | `SY_EDGE_BLOB_PUBLIC_ROOT` | `/var/lib/fluxbee/blob/public` |
| `public_max_inflight` | `SY_EDGE_PUBLIC_MAX_INFLIGHT` | 128 |
| `public_ready_timeout_ms` | `SY_EDGE_PUBLIC_READY_TIMEOUT_MS` | 60 000 |
| `tls_vault_key` | `SY_EDGE_TLS_VAULT_KEY` | none |
| `vault_hive` | `SY_EDGE_VAULT_HIVE` | `motherbee` |
| `tls_cert`, `tls_key` | `SY_EDGE_TLS_CERT`, `SY_EDGE_TLS_KEY` | none |

Environment only: `CONFIG_DIR` (default `/etc/fluxbee`), `NODE_NAME`, `NODE_VERSION`,
`ROUTER_SOCKET` (default `/var/run/fluxbee/routers`), `UUID_PERSISTENCE_DIR` (default
`/var/lib/fluxbee/state/nodes`), `TTL` (16), `HANDLER_TIMEOUT_MS` (30 000), `MAX_INFLIGHT`
(1024), `RUST_LOG`.

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
  synced, then renamed (`persist_public_registry`).
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
  404). An invalid publications file starts an empty public table.
- **Secret not fetched.** The channel answers 401 until a later `resolve_secrets` succeeds: on
  the next `EDGE_OPEN_URL` or the next VAULT_SECRET_CHANGED from its vault.
- **Owner down or slow.** 502 or 504 per request; the edge keeps running.
- **Router channel closed.** The loop ends, `main` returns, and systemd restarts the unit.

## Not there yet

- The edge issues nothing itself: SY.admin orders the certificate and stores it in SY.vault
  (design §6), or the operator puts a PEM chain and key there by hand. The edge answers the
  challenge and serves what the vault holds.
- One certificate for every name: SNI passthrough for `cloud.<id>` and `archi.<id>` is out of
  scope for v1 (O5a, design).
- The `shared-secret` mode is one static bearer per channel. There is no other auth mode.
- The owner node's reply is always sent as `200 application/json`.

## Health

- `curl -s https://<public host>/healthz` answers `ok` (the route is served on the public
  listener itself).
- `systemctl status sy-edge` on the ingress hive. The log line
  `sy-edge public HTTPS frontend ready (TLS)` means the port is bound; `serving=false` on it means
  the edge is AWAITING a certificate.
- `EDGE_TLS_STATUS` (sent by SY.admin) says what the door serves, or why it awaits.
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
- [IO.blob spec](../io-blob-spec-v1.md): curation and the public folder.
- [Host posture spec](../host-posture-and-exposure-spec-v1.md): what is exposed on each role.
