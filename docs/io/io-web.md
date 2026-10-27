# IO.web

IO.web is the motherbee's web node behind the edge. The edge on the ingress proxies each web host
that SY.admin declares (`archi.<id>`) to it, over the mesh's own mutual TLS. In this stage (release
0.1.85, [io.web v2](../io-web-v2.md) §11.2) it serves one static page and a health check; the
sessions, and Archi behind them, come in stage 2 (§11.3). Only a hive the motherbee's registry
lists as a connected ingress may connect: any other client fails inside the TLS handshake. It
never serves plaintext.

Source: `nodes/io/io-web/src/main.rs` (the node: boot, self-check, control plane, status) and
`nodes/io/io-web/src/web.rs` (the port: TLS, connections, routes). Its config contract is
`nodes/io/common/src/io_web_adapter_config.rs`. The mesh TLS and the hive registry it reads are
`fluxbee_sdk::mesh` (`crates/fluxbee_sdk/src/mesh/`, feature `mesh-tls`).

| | |
| --- | --- |
| Runtime | `io.web`, binary `io-web` (`packaging/base-nodes.json`, `boot: false`) |
| Node name | `IO.web@motherbee`, in the root tenant `tnt:00000000-0000-0000-0000-000000000001` |
| Unit | `fluxbee-node-IO.web-motherbee`, transient, `Restart=always`, as every managed node |
| Listens on | `0.0.0.0:9200` (`fluxbee_sdk::mesh::IO_WEB_PORT`), TLS only |
| Certificate | the motherbee's mesh leaf, `/var/lib/fluxbee/tls/motherbee/` (`ca.crt`, `cert.crt`, `cert.key`), which SY.orchestrator writes at its boot |
| Admits | a client certificate from the mesh CA whose hive `/var/lib/fluxbee/hives` lists as a connected ingress |
| Config | `/var/lib/fluxbee/nodes/IO/IO.web@motherbee/config.json`; no field in this stage |

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
- **Next to Archi** (io-web-v2 §3), which listens on the motherbee's loopback. It does not reach
  Archi yet.

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

## What it serves

HTTP/1.1 only. The answer depends on the method and the path alone; a request body is never read.

| Request | Answer |
| --- | --- |
| `GET /`, `HEAD /` | 200, `text/html; charset=utf-8`: a minimal static page, with no version and nothing internal |
| `GET /healthz`, `HEAD /healthz` | 200, `text/plain`, `ok` |
| anything else (another path, another method) | 404, `text/plain`, `not found` |

A HEAD answer carries the GET answer's headers and no body.

Every answer carries these headers (`SECURITY_HEADERS` in `web.rs`), the edge door's set:

| Header | Value |
| --- | --- |
| `Content-Security-Policy` | `default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'` |
| `X-Content-Type-Options` | `nosniff` |
| `Referrer-Policy` | `no-referrer` |
| `Cache-Control` | `no-store` |
| `X-Robots-Tag` | `noindex, nofollow, noarchive` |

**The policy loads nothing,** not even from the page's own origin, and no page may frame it.
`base-uri`, `form-action` and `frame-ancestors` do not fall back to `default-src`, so each is
named. The page is plain text in HTML: no style, no script, no image and no link. It therefore
renders under its own policy, which a test checks (`the_page_loads_nothing_and_names_nothing_internal`).

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
| TLS handshake | 10 s | a client that stalls its handshake holds nothing longer |
| Request head | 30 s, from the handshake or the previous answer | one byte, or an h2 preface, never holds a connection longer; it also closes a keep-alive connection idle that long (hyper's head timer, as on the edge's door, A-87) |
| Head size | 128 KiB per connection buffer | twice the edge door's, so a head the door admitted fits with the headers the edge adds; a longer head gets 431 |
| Connections | 256 at once, handshakes included | one more is closed at accept, with a warning at most every 10 s |

The edge's own caps on proxied requests come first (io-web-v2 §11.2 item 2). A client that keeps
idle keep-alive connections must close them before the 30 s head timer does, or it may write into a
connection io.web is closing.

## Configuration

The contract (`IoWebAdapterConfigContract`) has **no field** in this stage:

- **the port** is the shared constant `IO_WEB_PORT`, the one the edge dials;
- **the certificate** is the motherbee's mesh leaf, at its fixed path;
- **who may connect** is the registry's connected ingress, never a list in a config.

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

- `web`, the port's state (below);
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

io.web tries its material and its port again every 30 s. It checks the registry every 10 s, so the
state follows the registry both ways.

### Logs

io.web logs to its unit's journal: `journalctl -u fluxbee-node-IO.web-motherbee`. The level is
`info`, with `io_web` at `debug`, unless `RUST_LOG` says otherwise.

| Line | Level | When |
| --- | --- | --- |
| `IO.web runs only as IO.web@motherbee in the root tenant; refusing to run` | error | at boot, with the name, the tenant and why, before it exits |
| `IO.web ready (managed control plane); its port opens once its config is in use` | info | at boot, with the config state, the port, the TLS directory and the registry |
| `IO.web serving: it admits only a connected ingress, over the mesh's mutual TLS` | info | the port serves, at first and after a recovery |
| `IO.web serves nobody, and nothing over plaintext` | error | with `code` and `detail`, once per change |
| `mesh client refused in the handshake: ...` | warn | a refused hive, with its role (logged by `fluxbee_sdk::mesh`) |
| `IO.web closed a connection after its handshake: its hive is not a connected ingress` | warn | the registry, read once more after the handshake, no longer admits the hive |
| `IO.web cut a connection: its hive is no longer a connected ingress` | info | the registry demoted or removed the hive |
| `IO.web at its connection cap; closing new connections` | warn | at most every 10 s, with the count |
| `IO.web answered` | debug | each request: hive, method, path, status, the edge's `X-Request-Id` |

**A request's path is logged without its query,** so no query string reaches the journal
(io-web-v2 §11.3).

io.web exchanges only system messages on the mesh (CONFIG_*, NODE_STATUS_*), which the router does
not copy to its stream, so nothing of it reaches the router stream yet.

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

## Not there yet

- Sessions from Cloud's one-time code, the operator's own way in, and Archi behind io.web
  (io-web-v2 §11.3).
- Any configurable field. The first ones, the key that signs sessions among them by `vault_ref`,
  come with the sessions.

## Links

- [io.web v2](../io-web-v2.md): the design, §11.2 for this release.
- [SY.edge](../nodes/sy-edge.md): the door, and the proxy that dials io.web.
- [SY.admin](../nodes/sy-admin.md): the web host declaration and `web_status`.
- [io-common](io-common.md): what every IO node shares.
