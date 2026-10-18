# SY.edge

SY.edge is the public door of a Fluxbee mesh. On the ingress hive it serves HTTPS and turns each
request on an externalized channel (`/e/<ich>`) into a mesh message for the IO node that owns the
channel, then returns that node's reply. It also serves the static files that `SY.admin` published
for producers (`/public/<key>`). It resolves no identity: it forwards by the owner name SY.admin
gave it, and it takes commands only from `SY.admin@motherbee`.

Source: `src/bin/sy_edge.rs`. The URL commands it receives are sent by `SY.admin`
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
  (`resolve_http_listen`). On any other role it serves no door, and a VAULT_SECRET_CHANGED for
  its TLS key is ignored (`LiveTlsMaterial::NotFromVault` in `tls_secret_change_action`).
- There is no SY.identity on the ingress hive. The edge computes its own ILK from its L2 name
  (`fluxbee_sdk::deterministic_system_ilk_id` in `main`), uses it to read its secrets from the
  vault, and stamps it as `meta.src_ilk` on the requests it forwards to a channel owner.

## How it starts

On a spoke the unit file is written by the orchestrator: `core_service_unit_contents` in
`src/bin/sy_orchestrator.rs` gives `sy-edge` and `sy-orchestrator` (and no other unit)
`After=network.target rt-gateway.service` and `Wants=rt-gateway.service`. The packaged unit on the
motherbee (`gen_unit sy-edge ...` in `packaging/build-deb.sh`) has the same ordering. Neither
orders `sy-vault`: the vault is on the motherbee, so `Restart=always` is the retry loop (comment
above `TlsFetchFailure`).

`main` in `src/bin/sy_edge.rs`, in order:

1. `Config::load` reads `/etc/fluxbee/hive.yaml` (`role` and the `edge:` section) and the
   environment. A missing or unparsable hive.yaml gives no `edge:` section, so no public listener
   (`load_hive_edge`).
2. Logging from `RUST_LOG` (default `info,sy_edge=debug,fluxbee_sdk=info`), reloadable at run
   time (see Config).
3. Connects to the router (`RouterDispatcher::connect_with_retry`, retry every 1 s). All
   `system` messages go to one command channel (`build_sy_edge_rpc_profile`).
4. Loads the endpoint table from `endpoints_path` (`load_registry`) and the public artifact table
   from `publications_path` (`load_public_registry`, which drops expired or invalid rows).
5. Fetches channel secrets from the vault for rows that carry only a `secret_ref` or
   `verify_token_ref` (`resolve_secrets`).
6. If a public listener is configured (ingress role, or `SY_EDGE_HTTP_LISTEN`): builds TLS (vault
   first, then disk files), then starts the HTTP frontend (`run_frontend`). If TLS was requested and could not be built, `main` returns an error
   and the process exits; it never binds the public port in plaintext in that case.
7. Starts a reaper that every 600 s drops expired public rows and, if it dropped any, rewrites
   the publications file.
8. Runs the system message loop until the router channel closes.

## HTTP surface

`run_frontend` serves four routes:

| Route | Methods | What it does |
| --- | --- | --- |
| `/e/:ich` | any | Forward to the channel owner (`invoke_root` → `invoke`) |
| `/e/:ich/*extra` | any | Same, with `extra` passed as the path (`invoke_extra`) |
| `/public/:key` | GET, HEAD (else 405 with `Allow: GET, HEAD`) | Serve a published blob (`serve_public_artifact`) |
| `/healthz` | any | Answers `ok` |

With TLS, each connection is accepted with `tokio_rustls` and a rustls `ServerConfig` with no
client authentication (`tls_config_from_pem`). Without TLS (dev only, see Config) it uses
`axum::serve`.

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

Handled in the loop at the end of `main`:

| Message | From | What happens |
| --- | --- | --- |
| `EDGE_OPEN_URL` | `SY.admin@motherbee` | Validate the row and upsert it by `ich`, persist the table, fetch its secret, reply `{status, ich, url}` (`apply_open_url`) |
| `EDGE_CLOSE_URL` | `SY.admin@motherbee` | Remove the row (absent is `ok`, `closed: false`), persist (`apply_close_url`) |
| `EDGE_LIST_URLS` | `SY.admin@motherbee` | Return the rows without secret values (`apply_list_urls`) |
| `EDGE_PUBLISH_BLOB` | `SY.admin@motherbee` | Validate, wait for the file, add the public row (`apply_publish_blob`) |
| `EDGE_UNPUBLISH_BLOB` | `SY.admin@motherbee` | Remove the public row by `publication_id` (`apply_unpublish_blob`) |
| `VAULT_SECRET_CHANGED` | `SY.vault@<vault_hive>` | Re-fetch channel secrets; decide on a TLS reload (below) |
| `CONFIG_GET` / `CONFIG_SET` | `SY.admin@motherbee` | The edge's own config (`apply_node_config`) |
| `NODE_STATUS_GET` | an orchestrator, or SY.admin / SY.vault of the same hive | Default status reply (`try_handle_default_node_status`) |

The five `EDGE_*` commands are checked twice. The router delivers a protected SYSTEM action only
if `policy/system.rego` allows its origin (`prepare_local_delivery` in `src/router/mod.rs`; rule
(1) for the `EDGE_*` commands, rule (2) for `CONFIG_GET`/`CONFIG_SET`, rules (3)–(5) for
`NODE_STATUS_GET`). The edge then accepts an `EDGE_*` command only when the router-stamped
`src_l2_name` equals `EDGE_CONTROL_AUTHORITY`, `"SY.admin@motherbee"`
(`authorize_edge_service_command`; constant in `src/router/system_policy.rs`). A
VAULT_SECRET_CHANGED from any origin other than its vault is logged and ignored.

Row checks on open (`validate_endpoint_row_for_open`): `ich` not empty; `inbound_family` not
empty and not `system`; a unicast row needs a fully-qualified `IO.*` `owner_l2_name` and, for
`shared-secret`, a `secret` or `secret_ref`; a fanout row needs an `IO.*` glob with `@` and a
`verify_token` or `verify_token_ref`.

Who may open what is decided in SY.admin before the edge is asked: an `IO.*` node only for a
channel it owns (`authorize_channel_command`), the operator for any channel, and fanout rows only
by the operator. See [SY.admin](sy-admin.md).

## TLS and certificate rotation

The certificate comes from SY.vault: the secret named by `edge.tls_vault_key`, value
`{"cert": "<PEM chain>", "key": "<PEM key>"}`, read with the edge's own ILK
(`fetch_tls_config_from_vault`, `VaultCallerOwned`). The secret's owner must be
`SY.edge@<edge hive>` (comment in `packaging/hive.yaml.example`). Disk files
(`edge.tls_cert`, `edge.tls_key`) are used only when no vault key is set.

At boot a chain whose PEM has more `BEGIN CERTIFICATE` markers than parsed certificates, or a
leaf with no intermediates, is logged but still served (`TlsChainDiagnosis`,
`log_tls_chain_diagnosis`).

On a `VAULT_SECRET_CHANGED` for the TLS key (`tls_secret_change_action`):

- **delete**: keep serving the loaded certificate and log an error.
- **assign**: ignored (the value did not change).
- **bootstrap re-announce** (SY.vault restarting) of the version already loaded: the edge
  re-fetches and compares a SHA-256 of the cert PEM; same material, nothing happens; different
  material, it restarts.
- **any other change**: the edge first fetches the new material (up to 3 tries, 5 s apart, on
  transport errors). If it loads and is not truncated, the process calls `exit(0)` after 250 ms
  and systemd starts it again 5 s later with the new certificate. If not, it keeps serving the
  old one and logs why (`tls_reload_verdict`).

Only `sy-edge` restarts; nothing else on the hive does. The public port is down for those
seconds. Swapping the certificate in place is open as PB-1 in `lab/logbook/PENDING-BUGS.md`
(comment in `main`).

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

## Failure behavior

- **No TLS material.** If TLS was requested and could not be loaded, the process exits with an
  error and systemd retries every 5 s. A vault that does not answer is logged as transient ("normal
  on cold boot"); a bad or missing secret as an error that needs the operator
  (`TlsFetchFailure::from_vault_error`).
- **Bad files.** An unreadable or invalid endpoints file starts an empty table (every `/e/` gets
  404). An invalid publications file starts an empty public table.
- **Secret not fetched.** The channel answers 401 until a later `resolve_secrets` succeeds: on
  the next `EDGE_OPEN_URL` or the next VAULT_SECRET_CHANGED from its vault.
- **Owner down or slow.** 502 or 504 per request; the edge keeps running.
- **Router channel closed.** The loop ends, `main` returns, and systemd restarts the unit.

## Not there yet

- No ACME or Let's Encrypt: the code has no certificate issuance. The operator puts the PEM
  chain and key in SY.vault.
- No in-place certificate swap (PB-1): a rotation restarts the process.
- The `shared-secret` mode is one static bearer per channel. There is no other auth mode.
- The owner node's reply is always sent as `200 application/json`.

## Health

- `curl -s https://<public host>/healthz` answers `ok` (the route is served on the public
  listener itself).
- `systemctl status sy-edge` on the ingress hive. The log line
  `sy-edge public HTTPS frontend ready (TLS)` means the door is up.
- The open channels: `GET /channels/externalized?edge_node=SY.edge@<ingress>` on SY.admin
  (`list_externalized` → `EDGE_LIST_URLS`).

## Links

- [SY.admin](sy-admin.md): `externalize`, `publish_cloud_endpoint`, `publish_artifact`.
- [SY.orchestrator](sy-orchestrator.md): `add_hive` for the ingress role, units, Syncthing.
- [SY.vault](sy-vault.md): where the TLS material and channel tokens live.
- [The router](rt-gateway.md): the system policy that admits the `EDGE_*` commands.
- [Edge ingress spec v6](../edge-ingress-spec-v6.md): the design (ICH model, invariants).
- [IO.blob spec](../io-blob-spec-v1.md): curation and the public folder.
- [Host posture spec](../host-posture-and-exposure-spec-v1.md): what is exposed on each role.
