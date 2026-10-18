# Architecture: reference

**Status:** as built in 0.1.79 (2026-10-17).
**Audience:** a developer joining the project who needs the system's shape before reading any
node's code.

This page is the map: hives, nodes, routers, the links between hives, the public door, the control
plane and the data that moves through the mesh. The sections below name the code that backs each
statement; paths are relative to the repository root. The code is the source of truth: where this
page and the code disagree, the code wins. Detailed pages are listed in §11.

## 1. The shape

```text
             internet: Fluxbee Cloud, webhooks, browsers
                                │ HTTPS 443
                      ┌─────────▼─────────┐
                      │ ingress           │
                      │ SY.edge           │
                      └─────────┬─────────┘
                                │
┌─────────────────────┐   ┌─────▼──────────────────────────┐   ┌──────────────────┐
│ worker              ├──►│ motherbee                      │◄──┤ egress           │
│ AI, IO and WF nodes │   │ SY.admin   Archi   SY.vault    │   │ outbound NAT     │
│ SY.identity replica │   │ SY.identity (primary)          │   │ for the workers  │
│ SY.cognition        │   │ SY.storage   PostgreSQL        │   └──────────────────┘
└─────────────────────┘   │ IO.cloud   IO.blob             │
                          └────────────────────────────────┘

Every hive also runs RT.gateway, SY.orchestrator, SY.config.routes and SY.opa.rules.
Arrows start at the hive that dials: WAN 9000/tcp (mutual TLS) and Syncthing 22000/tcp.
Workers also dial identity sync on 9100/tcp.
```

- A **hive** is one Linux host running Fluxbee. Exactly one hive is the **motherbee**; the others
  are spokes: workers, ingress and egress hives. The topology is a star: only the motherbee listens
  for other hives, and every spoke dials it.
- Inside a hive, every process that takes part is a **node** attached to the hive's **router** over
  a Unix socket. The router reads the hive's state from shared memory.
- Operators, Archi and Fluxbee Cloud act on the mesh through **SY.admin** on the motherbee.

## 2. Hives

### 2.1 What a hive is

A hive is defined by `/etc/fluxbee/hive.yaml` (`CONFIG_DIR` in `src/paths.rs`). The router refuses
to start without it (`ConfigError::MissingHive` in `src/config/mod.rs`). Its key fields are
`hive_id` and `role`.

- `hive_id: motherbee` is reserved for `role: motherbee`, and `role: motherbee` requires it
  (`main` in `src/bin/sy_orchestrator.rs`, against `PRIMARY_HIVE_ID` in
  `crates/fluxbee_sdk/src/protocol.rs`). Nodes reach the motherbee's services by that name.
- A joined hive's id is 1 to 64 letters, digits, `-` or `_` (`valid_hive_id`).
- A router only peers with the shared-memory regions of its own `hive_id`, found in the host's
  `/dev/shm` (`discover_peer_regions` in `src/router/mod.rs`), so a hive is one host.

The file also holds `system_nodes` (§2.2), `wan` (§5.1), `admin` and `architect` (§7),
`identity.sync` (§5.3), `blob` and `dist` (§5.4), `ai` (the hive's AI provider, `HiveAiConfig` in
`crates/fluxbee_ai_sdk`), `government.identity_frontdesk` (§7.3), `edge` (§6.1) and `egress` (NAT
on the egress hive; on the motherbee, the default route `add_hive` gives new workers).

### 2.2 Roles

`HiveRole` in `src/bin/sy_orchestrator.rs` has four roles. Each role's system nodes are the list
`system_nodes.<role>.nodes` in `hive.yaml`, in start order; the shipped lists are in
`packaging/hive.yaml.example`.

| Role | System nodes (shipped `hive.yaml`) | Created by | Purpose |
| --- | --- | --- | --- |
| `motherbee` | SY.config.routes, SY.identity, SY.opa.rules, SY.admin, SY.architect, SY.storage, SY.cognition, SY.timer, SY.wf-rules, SY.frontdesk.gov, SY.vault | the `.deb` and `fluxbee-firstboot` | control plane, secrets, PostgreSQL, the Cloud relay |
| `worker` | SY.config.routes, SY.identity, SY.opa.rules, SY.cognition, SY.timer, SY.wf-rules | `add_hive` | room for AI, IO and WF nodes |
| `ingress` | SY.config.routes, SY.opa.rules, SY.edge | `add_hive` with `role: ingress` | the public HTTPS door |
| `egress` | SY.config.routes, SY.opa.rules | `add_hive` with `role: egress` | outbound NAT for the workers |

Every role also runs `rt-gateway` (the router) and `sy-orchestrator`. A spoke's binaries are
exactly these two plus its role's SY services (`core_component_names_from_section`).

`validate_system_nodes` checks each list at boot. It must start with `SY.config.routes`, the writer
of the routing shared memory. The motherbee list must contain `SY.vault` after every vault consumer,
because the vault's start-up broadcast (one `VAULT_SECRET_CHANGED` per stored secret) reaches only
nodes already registered; no other role may run it. Only `SY.*` names are accepted
(`HIVE_YAML_NON_SY_LIFECYCLE_NODES` in `crates/fluxbee_sdk/src/managed_node.rs` is empty).

Some binaries also check the role themselves. SY.admin and SY.storage exit unless the role is
`motherbee`. SY.identity refuses any role but `motherbee` and `worker`. IO.cloud and IO.blob refuse
to run off the motherbee. SY.edge opens its public port only when the role is `ingress`
(`resolve_http_listen` in `src/bin/sy_edge.rs`).

### 2.3 How a hive is created

**The motherbee: the `.deb` and `fluxbee-firstboot`.** Only the motherbee is installed from the
package.

1. `packaging/build-deb.sh` builds one `.deb`. Each core binary goes to `/usr/bin` and to
   `/var/lib/fluxbee/dist/core/bin`, and their SHA-256 hashes go into `dist/core/manifest.json`. The
   runtimes in `packaging/base-nodes.json` are published under `dist/runtimes/<runtime>/<version>/`,
   the vendored Syncthing under `dist/vendor/`. Each core service gets a systemd unit with
   `Restart=always`.
2. `packaging/deb-postinst` creates the `fluxbee` system user and the directories, registers the base
   runtimes in the live runtime manifest (`fluxbee-seed-runtimes`), copies `hive.yaml.example` to
   `/etc/fluxbee/hive.yaml` when none exists, and enables `sy-orchestrator` only. A fresh install
   starts nothing.
3. The operator edits `hive.yaml` and runs `sudo fluxbee-firstboot` (`packaging/fluxbee-firstboot`).
   It creates the PostgreSQL role and the `fluxbee`, `fluxbee_identity` and `fluxbee_storage`
   databases, starts the orchestrator, stores the PostgreSQL URL in SY.vault through the admin API
   (`storage_postgres_url`), restarts SY.vault, SY.storage and SY.identity, waits until `GET /hives`
   answers `ok`, and launches each `boot: true` runtime in the root tenant: `IO.blob@motherbee`,
   `IO.cloud@motherbee` (`POST /hives/<hive>/nodes`).

**Spokes: `add_hive`.** Every other hive is joined from the motherbee with `add_hive`
(`POST /hives`). `accept_add_hive` checks the request and answers at once; the join runs in the
background (`spawn_background_join`) in `add_hive_flow`, `add_ingress_hive_flow` or
`add_egress_hive_flow` (`src/bin/sy_orchestrator.rs`). The request's `role` picks the flow; the
default is `worker`. A worker join:

1. issues the hive's mesh TLS leaf (`issue_hive_tls`) before it touches the box;
2. reaches the box over SSH with the motherbee's key, `/var/lib/fluxbee/ssh/motherbee.key`. The
   request's `ssh_key` or `ssh_password` is used only to install that key;
3. copies the role's core binaries (`sync_core_to_worker`) and the vendored Syncthing
   (`sync_vendor_to_worker`);
4. writes `/etc/fluxbee/hive.yaml` (`wan.uplinks` pointing at the motherbee, `wan.mtls: required`,
   `identity.sync.upstream`, `identity.sync.auth: required`), the TLS files, the identity HMAC key
   and an empty `/etc/fluxbee/sy-config-routes.yaml`;
5. writes the systemd units, then enables and starts `rt-gateway` and `sy-orchestrator`;
6. waits up to 60 s for the WAN link and up to 60 s for the new orchestrator to appear in the LSA,
   then finishes over the mesh with `ADD_HIVE_FINALIZE` (Syncthing pairing and the first dist sync);
7. removes the motherbee's key from the box's `authorized_keys` and drops the sudoers grant
   (`revoke_bootstrap_ssh_access`). After a join the motherbee does not SSH into the hive again.

The motherbee records each joined hive in `/var/lib/fluxbee/hives/<hive>/info.yaml`
(`write_hive_info`). An ingress join also needs `ingress.listen` and `ingress.tls_vault_key` (§6.1);
an egress join needs its NAT parameters (`resolve_add_hive_egress_section`).

### 2.4 The orchestrator

SY.orchestrator (`src/bin/sy_orchestrator.rs`) runs on every hive and supervises it. The router and
the SY nodes are separate systemd units: on SIGTERM the orchestrator exits and leaves them running
(`main`). On the motherbee the package enables only `sy-orchestrator` (`packaging/deb-postinst`); on
a spoke `add_hive` enables `rt-gateway` and `sy-orchestrator`. The orchestrator starts the rest at
every boot.

The boot (`bootstrap_local`, then `finish_bootstrap`):

1. Check its role's core binaries against the hashes in `dist/core/manifest.json`
   (`validate_core_manifest_for_bins`); a spoke also re-renders its core units.
2. Motherbee only: create the SSH join key and the mesh CA when missing, write its own TLS leaf
   (`ensure_motherbee_ssh_key`, `ensure_motherbee_mesh_tls`), and lay out one core tree per spoke
   role under `dist/core/<role>/` (`materialize_role_core_trees`).
3. Start `rt-gateway` and wait for a fresh heartbeat in its shared memory (`wait_for_router_ready`).
   Open the firewall ports (`ensure_core_firewall_local`, §9), apply the egress setup
   (`reconcile_egress`) and start Syncthing.
4. Start the `system_nodes.<role>.nodes` units in order, then wait up to 60 s for the `wait_for`
   nodes to register with the router (`wait_for_lifecycle_nodes`).
5. Attach to the router as `SY.orchestrator@<hive>`. Admin commands get `ORCHESTRATOR_BOOTING` until
   the boot ends.
6. Motherbee only: wait for SY.storage's database (`STORAGE_METRICS_GET`). Then relaunch the managed
   nodes recorded on disk and move those that follow `current` to the version it now names
   (`reconcile_persisted_custom_nodes`).

After the boot it runs these jobs:

- **Watchdog**, every 5 s (`watchdog_tick`). It starts `rt-gateway` or any system-node unit that is
  not active, logs AI, IO and WF nodes that left the router, verifies runtimes, reconciles the
  Syncthing peers (about once a minute) and the egress, and reports SY.admin or Archi bound off
  loopback (`watchdog_listener_check`).
- **Managed nodes**, every 60 s in their own task (`run_managed_runtime_reconcile_loop`). It
  relaunches a dead AI, IO or WF node whose `_system.relaunch_on_boot` is true.
- **Node lifecycle and joins**: `run_node`, start, restart, kill and remove (§3.4), `add_hive` and
  `remove_hive`. The motherbee's orchestrator takes them from SY.admin and forwards node actions for
  another hive to `SY.orchestrator@<hive>` (`forward_system_action_to_hive`).
- **Software updates**: `SYSTEM_UPDATE` names a category (`core`, `vendor` or `runtime`) and the
  manifest hash the motherbee expects (`handle_system_update_message`). A hive applies it only when
  its Syncthing-synced manifest has that hash, else it answers `sync_pending`. A core update replies
  first, then swaps the binaries and restarts the services (`apply_core_update_replying_first`),
  keeping three backup generations for `core_rollback` (`CORE_BACKUP_GENERATIONS_KEPT`).

## 3. Nodes

### 3.1 Names

A node name is `<PREFIX>.<name>@<hive>`, for example `IO.slack.support@worker1`. A node may
register without `@<hive>`: the SDK appends the `hive_id` from `hive.yaml` (`normalize_name` in
`crates/fluxbee_sdk/src/node_client.rs`), and so does the router (`normalize_name` in
`src/router/mod.rs`). The prefix is the node's kind; some kinds get special treatment in code:

| Prefix | What | Rules in code |
| --- | --- | --- |
| `RT` | the router and its own nodes (`RT.gateway@<hive>`, its stream) | The router refuses a `HELLO` with an `RT.` name unless the socket's peer process is the router itself (`rt_name_allowed`, from `peer_cred`). `run_node` refuses `RT.*` (`managed_spawn_disallowed_reason`). |
| `SY` | system services | systemd unit `sy-<name>` (`name_to_service`: `SY.config.routes` → `sy-config-routes`). Never launched by `run_node`. Each computes its ILK from its name (`deterministic_system_ilk_id` in `crates/fluxbee_sdk/src/identity.rs`). |
| `AI` | agents | The router treats `AI.*` as agents: cognition copies and memory packages (`is_agent_node`). |
| `IO` | channels to the outside world | Only `IO.*` may provision identities (`ILK_PROVISION` in `IdentityRuntime::new`, `src/bin/sy_identity.rs`). The edge's data path runs only to and from `IO.*` (`edge_data_path_allowed`). |
| `WF` | workflows | Must run a `workflow` package (`WF_RUNTIME_PACKAGE_REQUIRED`). |

### 3.2 UUIDs

Each connection carries a node UUID (`NodeUuidMode` in `crates/fluxbee_sdk/src/node_client.rs`).
`Persistent`, the default, reads it from `<uuid_persistence_dir>/<name without @hive>.uuid` and
creates it on first start; the SY nodes keep theirs in `/var/lib/fluxbee/state/nodes/`. `Ephemeral`
draws a new one per process and writes nothing; only the diagnostic binaries (`src/bin/*_diag.rs`)
use it.

The router allows one live connection per UUID. A second `HELLO` with a connected UUID waits up to
1 s for the first connection to close, then is refused (`claim_node_uuid`). A frame whose
`routing.src` is not the connection's UUID is dropped (`frame_is_from_connection`). The router
overwrites `routing.src_l2_name` with the name the connection registered (`handle_message`), and the
policies judge that name. A router's own UUID lives in
`/var/lib/fluxbee/state/<router name>/identity.yaml` (`RouterConfig::load_from_env`).

### 3.3 Attaching to the router

- Each router listens on `/var/run/fluxbee/routers/<router uuid>.sock` (mode 0666) for nodes and on
  `irp-<router uuid>.sock` (mode 0600) for its sibling routers (`Router::run`).
- A frame is a 4-byte big-endian length followed by JSON, at most 128 KiB (`MAX_FRAME_SIZE` in
  `crates/fluxbee_sdk/src/socket/connection.rs`). The WAN uses the same framing.
- A message is `{routing, meta, payload}` (`Message` in `crates/fluxbee_sdk/src/protocol.rs`).
  `routing.dst` is a node UUID or name, `"broadcast"`, or `null` to let the router choose.
- The node opens with `HELLO` (`meta.type: "system"`, payload `{uuid, name, version}`). The router
  checks that `routing.src` equals `payload.uuid`, applies the `RT.` rule, assigns a VPN, records the
  node in its shared memory and answers `ANNOUNCE` (`{uuid, name, status: "registered", vpn_id,
  router_name}`, `handle_node`). EOF or `WITHDRAW` ends the registration.
- The SDK's entry point, `RouterDispatcher::connect_with_retry` (`crates/fluxbee_sdk/src/rpc.rs`),
  reconnects by itself from 100 ms up to 30 s (`connection_manager_loop`). Go: `go/fluxbee-go-sdk`.

### 3.4 Managed nodes

AI, IO and WF nodes are managed: the orchestrator launches them from runtimes.

- **Runtimes.** A runtime version is a directory `dist/runtimes/<runtime>/<version>/` under
  `/var/lib/fluxbee`, listed in `dist/runtimes/manifest.json` with the runtime's `current` version
  (`src/runtime_manifest.rs`). A package is `full_runtime` (its own `bin/start.sh`), `config_only` or
  `workflow`; the last two run on a `runtime_base` (`PackageType` in `src/runtime_package.rs`).
  `fluxbee-scaffold` (`src/bin/fluxbee-scaffold.rs`) creates and packs one; `fluxbee-publish`
  (`src/bin/fluxbee-publish.rs`) installs it into the local `dist/runtimes/` and can ask a hive to
  take it (`--deploy`); SY.admin publishes too (`publish_runtime_package`). Only the motherbee's copy
  travels to other hives (§5.4).
- **Instances.** `run_node` (`POST /hives/{hive}/nodes`, `run_node_flow`) creates one. For another
  hive, the motherbee's orchestrator forwards the request. On the target hive it:
  1. refuses a node that is not core in the root tenant (`genesis_refusal`,
     `TENANT_ROOT_NOT_ALLOWED`) and a node whose tenant is not active (`tenant_gate_refusal`);
  2. resolves the runtime version (`current` by default) and its `bin/start.sh`
     (`resolve_runtime_spawn_entrypoint`);
  3. registers the node's ILK with SY.identity on the motherbee (`ILK_REGISTER`,
     `ensure_node_identity_registered`);
  4. writes `/var/lib/fluxbee/nodes/<KIND>/<node name>/config.json`: the runtime's config template,
     the caller's `config`, and a `_system` block that callers may not set
     (`build_node_system_block`: runtime, version, ILK, tenant, `relaunch_on_boot` and more);
  5. starts a transient systemd unit such as `fluxbee-node-AI.demo-motherbee` with `systemd-run`,
     `Restart=always` and the environment `FLUXBEE_NODE_NAME`, `FLUXBEE_NODE_ILK_ID`,
     `FLUXBEE_NODE_TENANT_ID` (`unit_from_node_name`, `build_managed_node_run_command`).
- **Live config.** A running node's behavior is set with `CONFIG_SET` and read with `CONFIG_GET`
  (`crates/fluxbee_sdk/src/node_config.rs`, `managed_control_plane.rs`). Only `SY.admin@motherbee`
  may send them (§7.3).
- **Workflows.** SY.wf-rules builds a workflow package and launches its WF node through SY.admin
  (`go/sy-wf-rules/node/node_lifecycle.go`). `run_node` on the bare `wf.engine` runtime is refused.

### 3.5 Base nodes

`packaging/base-nodes.json` is the one list of what the `.deb` ships beyond the core.
`packaging/build-deb.sh` builds and publishes each runtime; `packaging/fluxbee-firstboot` launches
the `boot: true` ones. Its `singletons` list is empty: IO.blob and IO.cloud are runtimes.

| Runtime | Binary | Launched by `fluxbee-firstboot` |
| --- | --- | --- |
| `io.api` | `io-api` | no |
| `io.blob` | `io-blob` | yes, as `IO.blob@motherbee` |
| `io.cloud` | `io-cloud` | yes, as `IO.cloud@motherbee` |
| `io.slack` | `io-slack` | no |
| `io.wapp` | `io-wapp` | no |
| `ai.generic` | `ai_node_runner` | no |
| `wf.engine` | `wf-generic` (Go) | no |
| `io.linkedhelper` | `io-linkedhelper` | no |

Only the core runs in the root tenant (`tnt:00000000-0000-0000-0000-000000000001`,
`DEFAULT_ROOT_TENANT_ID` in `crates/fluxbee_sdk/src/identity.rs`): `SY.*`, `RT.*` and the IO
runtimes `io.cloud`, `io.blob` and `io.web` (`is_core_node`, `CORE_IO_RUNTIMES`; `io.web` is not
built yet). Every other node is launched for a tenant.

## 4. Routers

### 4.1 What a router does

The router is the `rt-gateway` binary (`src/main.rs`, `src/router/`). It registers nodes, builds a
forwarding table (`rebuild_fib`) from its nodes, its siblings' nodes, static routes, the LSA and
hub-vouched reachability, applies VPN rules and the policies, and moves frames. It refuses to start
if its baked system policy does not load (`system_policy::ensure_system_policy_loaded`, called first
in `Router::run`).

It reads the hive's state from POSIX shared memory. Each region has one writer and readers use a
seqlock (`src/shm/mod.rs`).

| Region | Writer | What the router uses it for |
| --- | --- | --- |
| `/jsr-<router uuid>` | the router | its own nodes; sibling routers and the orchestrator read it |
| `/jsr-config-<hive>` | SY.config.routes, from `/etc/fluxbee/sy-config-routes.yaml` | static routes, VPN assignments, taps |
| `/jsr-lsa-<hive>` | the gateway router | the other hives' nodes, routes, VPNs and taps |
| `/jsr-opa-<hive>` | SY.opa.rules (`go/sy-opa-rules`) | the compiled user policy |
| `/jsr-identity-<hive>` | SY.identity | ILKs, tenants and channels: `src_ilk` checks and the frontdesk route |
| `/jsr-memory-<hive>` | SY.cognition | memory packages for deliveries to `AI.*` nodes |

The router refreshes its region's heartbeat every 5 s. A sibling region whose heartbeat is older
than 30 s is ignored (`HEARTBEAT_STALE_MS`).

### 4.2 One router per hive, or several

In practice a hive runs one router, `RT.gateway@<hive>`: the package ships one `rt-gateway` unit and
the orchestrator starts only that one. The code supports more routers on one host:

- `JSR_ROUTER_NAME` names a router; without it the name is `wan.gateway_name` (default
  `RT.gateway`), and the router with that name is the gateway (`is_gateway_name` in
  `src/config/mod.rs`).
- Every 5 s each router scans `/dev/shm` for live regions of its hive (`peer_discovery_loop`).
  Siblings connect over IRP, the `irp-<uuid>.sock` sockets; the lower UUID dials
  (`should_initiate_peer`).
- A node given the socket directory picks its router by hash. It sorts the `*.sock` files (skipping
  `irp-*`), starts at `fnv1a64(name without @hive) % count` and falls back to the next ones
  (`router_socket_candidates`). The Go SDK does the same (`go/fluxbee-go-sdk/client.go`).
- Only the gateway listens on `wan.listen`, dials `wan.uplinks`, writes `/jsr-lsa-<hive>` and sends
  the hive's LSA (`Router::new`, `Router::run`). The others re-read the LSA region every 5 s
  (`lsa_refresh_loop`).
- A frame for a sibling's node goes over IRP. A frame for another hive goes to the gateway over IRP,
  then over the WAN (`forward_to_hive`). Each IRP or WAN hop decrements `routing.ttl`
  (`send_to_peer_router`, `send_to_wan_peer`).

### 4.3 The router's stream

Each router runs one node of its own, its stream `RT.<router>.stream@<hive>` (`src/router/stream.rs`;
the name comes from `stream_node_name` in `crates/fluxbee_sdk/src/stream.rs`). It is the log plane.
The router hands each copy to a bounded queue and never waits. A disk thread keeps the records in
`/var/lib/fluxbee/state/<router>/stream/`, and the stream node delivers them in acked batches
(`STREAM_BATCH`, `STREAM_ACK`) to their sinks through normal routing. Nodes add records with
`STREAM_APPEND` (`fluxbee_sdk::stream::append`); the router accepts only the subjects that producer
may append (`subject_allowed`). A record waits at most 120 s and the stream keeps at most 32 MiB on
disk; past either limit records are dropped and counted (`STREAM_MAX_AGE_SECS`, `STREAM_MAX_BYTES`).
What the stream carries is in §8.

## 5. Between hives

### 5.1 WAN: 9000/tcp with mutual TLS

The motherbee's gateway listens (`wan.listen: "0.0.0.0:9000"` in `packaging/hive.yaml.example`).
Each spoke's gateway dials it (`wan.uplinks`, written by `add_hive`) and retries from 5 s up to 60 s
(`wan_connect_loop`). The `hive.yaml` that `add_hive` writes has no `wan.listen`.

`wan.mtls` sets the transport (`src/config/mod.rs`; applied in `Router::run` and
`accept_wan_connection`):

| Mode | Behavior |
| --- | --- |
| `disabled` | Plaintext. The default when the key is absent. |
| `permissive` | Accepts TLS (detected by the first byte, `0x16`) and plaintext; each plaintext peer is logged as unauthenticated. Without TLS material the router runs plaintext. |
| `required` | TLS only. Without TLS material every WAN connection fails; there is no fallback. The shipped example and every `add_hive` write `required`. |

The motherbee's orchestrator keeps a dedicated mesh CA in `/var/lib/fluxbee/tls/ca/`
(`ensure_mesh_ca`) and signs one leaf per hive, with CN and SAN set to the `hive_id`
(`MeshCa::issue_leaf` in `src/mesh_tls.rs`). Each hive reads `ca.crt`, `cert.crt` and `cert.key` from
`/var/lib/fluxbee/tls/<hive_id>/` (`MeshTlsMaterial::load_from_dir`). `add_hive` puts them there
(`push_hive_tls_inner`); a hive that loses its leaf is joined again.

The handshake (`handle_wan_connection`): under TLS, each side first checks that the other's
certificate chains to the mesh CA. Both sides send `HELLO` (`protocol`, `router_id`, `router_name`,
`hive_id`, timers). A `HELLO` whose `hive_id` differs from the hive in the peer's certificate is
refused; when `wan.authorized_hives` is set, a hive missing from it gets `WAN_REJECT`
(`HIVE_NOT_AUTHORIZED`). A different `protocol` (`MESH_PROTOCOL_VERSION`, `fluxbee/1.16`) is only
logged. Then come `WAN_ACCEPT` and each side's LSA.

A sender name learned from peer hive P is always rewritten to `<local name>@P`
(`canonical_wan_src_name`), so a hive can only speak as its own nodes: no spoke can pose as
`SY.admin@motherbee`.

### 5.2 Topology: LSA and multi-hop

Each gateway sends its hive's LSA when a WAN session opens, when one of its nodes connects or
disconnects, and every `hello_interval` (10 s) (`lsa_broadcast_loop`). The LSA holds the gateway's
nodes, its siblings' nodes, static routes, VPN assignments and taps (`build_local_lsa_payload`). The
receiver drops an LSA whose `hive` is not the peer's authenticated hive (`apply_lsa_payload`), keeps
the rest in `/jsr-lsa-<hive>` (`write_lsa_state`), and marks a hive stale after `dead_interval`
(40 s) without one (`lsa_stale_loop`). An LSA only describes its sender's hive, so a spoke learns the
motherbee's nodes but not the other spokes'.

Spoke-to-spoke traffic goes through the motherbee (design: Option B in
[edge-multihop-reachability-spec-v1.md](../edge-multihop-reachability-spec-v1.md)). A gateway with
two or more WAN peers sends each one a `WAN_REACHABILITY` message with the nodes of the other hives
it knows (`broadcast_reachability`). A spoke accepts it only from the motherbee's gateway
(`wan_reachability_voucher_allowed`, rule 6 in §7.3) and routes those nodes through the motherbee,
which relays the frames. A node learned this way gets data only: the router drops its protected
system actions (`prepare_local_delivery`, `src_via_hub`).

### 5.3 Identity sync: 9100/tcp with a per-hive HMAC

SY.identity on the motherbee is the primary, backed by PostgreSQL, and listens on `0.0.0.0:9100`
(`identity.sync.port`, `DEFAULT_IDENTITY_SYNC_PORT` in `src/bin/sy_identity.rs`). A worker's
SY.identity is a replica: it dials `identity.sync.upstream`, takes a full sync at boot, then
subscribes to deltas and pushes its own (`main` in `src/bin/sy_identity.rs`). Ingress and egress
hives run no SY.identity.

The channel is authenticated per hive (`src/mesh_hmac.rs`). The motherbee generates a 256-bit key for
each hive (`ensure_local_identity_key`), `add_hive` copies it to
`/var/lib/fluxbee/identity/keys/<hive>.key`, and each connection opens with a mutual HMAC
challenge-response bound to the hive id. `identity.sync.auth` means `required` unless it says
`disabled`, `off` or `none`; an absent or unknown value is `required` (`auth_mode_required`). A
replica without its key refuses to start.

### 5.4 Syncthing: software and blobs

The orchestrator runs Syncthing as the unit `fluxbee-syncthing`, as user `fluxbee`, with its home in
`/var/lib/fluxbee/syncthing` and its GUI and API on `127.0.0.1:8384` (`syncthing_unit_contents`).
Each spoke pairs with the motherbee only. The folders (`reconcile_syncthing_peer_xml`,
`dist_sync_folders_for_role`, `peer_shares_active_blob`, `peer_shares_public_blob`,
`folder_forbidden_for_role`):

| Folder ID | Path | Direction | Shared with |
| --- | --- | --- | --- |
| `fluxbee-dist-core-<role>` | `dist/core/<role>/` on the motherbee, `dist/core/` on the spoke | motherbee send-only, spoke receive-only | spokes of that role |
| `fluxbee-dist-vendor` | `dist/vendor/` | motherbee send-only, spoke receive-only | every spoke |
| `fluxbee-dist-policy` | `dist/policy/` | motherbee send-only, spoke receive-only | every spoke |
| `fluxbee-dist-runtimes` | `dist/runtimes/` | motherbee send-only, spoke receive-only | workers |
| `fluxbee-blob` | `blob/active/` | both ways | workers |
| `fluxbee-blob-public` | `blob/public/` | motherbee send-only, ingress receive-only | ingress |

So only the motherbee writes software, and an egress hive gets core, vendor and policy only.

Host posture stage 3 (`SYNCTHING_OWNED_OPTIONS`): a spoke switches once its motherbee device has a
static address that answers; the motherbee switches once every spoke has reported both
(`run_syncthing_options_switch_loop`). The switch turns off discovery, relays, NAT traversal, usage
and crash reports, automatic upgrades and STUN, and sets one listen address: `tcp://:22000` on the
motherbee, `tcp://127.0.0.1:22000` on spokes, which only dial (`syncthing_owned_listen_address`).

## 6. The public entry

### 6.1 SY.edge on the ingress hive

SY.edge (`src/bin/sy_edge.rs`) is the only process that serves the internet. It binds `edge.listen`
only when the role is `ingress` (`resolve_http_listen`). `add_hive` requires `ingress.listen` and
allows plaintext only with `allow_plaintext`, never on port 443 (`resolve_add_hive_ingress_section`);
otherwise the edge fetches its TLS certificate and key from SY.vault (`edge.tls_vault_key`, on
`edge.vault_hive`, default `motherbee`).

| Path | What |
| --- | --- |
| `/e/<ich>`, `/e/<ich>/...` | An externalized channel. The edge starts with no rows; SY.admin opens each one with `EDGE_OPEN_URL` (admin action `externalize`, `handle_externalize` in `src/bin/sy_admin.rs`). A row maps the channel's ICH to its owner node, the message family, the allowed methods and the auth mode (public or shared secret). The edge forwards the request by name to the owner, an `IO.*` node, and waits for its reply (`HANDLER_TIMEOUT_MS`, default 30 s). Rows persist in `/etc/fluxbee/edge.endpoints.json`. |
| `/public/<key>` | A public artifact, `GET` or `HEAD`, where `<key>` is 64 lowercase hex characters (`serve_public_artifact`). On the motherbee, SY.admin has IO.blob copy the bytes into `blob/public/` (`BLOB_CURATE`), Syncthing carries that folder to the ingress, and SY.admin registers the key on the edge (`EDGE_PUBLISH_BLOB`). An expired key answers 404. |
| `/healthz` | Liveness. |

The router fences the edge: only `SY.admin@motherbee` may command it, and its data path runs only to
and from `IO.*` nodes and never carries system messages (`vpn_allows_between`,
`edge_service_control_allowed`, `edge_data_path_allowed`, `edge_vault_read_allowed`). Design and
history: [edge-ingress-spec-v6.md](../edge-ingress-spec-v6.md).

### 6.2 IO.cloud, the relay for Fluxbee Cloud

`IO.cloud@motherbee` (`nodes/io/io-cloud/src/main.rs`) is the one door from Fluxbee Cloud into the
mesh. Cloud posts `{op, tenant_id, params}` to `https://<public host>/e/<ich>` with a bearer token.
SY.edge checks the token and forwards the request to IO.cloud. IO.cloud accepts traffic only from the
SY.edge it trusts (`config.io.edge_node`), answers the identity reads in `CLOUD_LOCAL_OPS` itself,
and relays the operations in `CLOUD_OP_ACTIONS` (`crates/fluxbee_sdk/src/cloud.rs`) to SY.admin as
`ADMIN_COMMAND`s. SY.admin checks each relayed action again (`authorize_cloud_relay`,
`enforce_cloud_relay_content`). The admin action `publish_cloud_endpoint` opens the door. The
contract for Cloud developers is [io-cloud-api.md](../io-cloud-api.md).

## 7. The control plane

### 7.1 SY.admin

SY.admin (`src/bin/sy_admin.rs`) runs on the motherbee only. It has two doors:

- **HTTP** on `127.0.0.1:8080` (`JSR_ADMIN_LISTEN`, else `admin.listen`, else that default). It has
  no authentication, so it belongs on loopback; the orchestrator reports it bound anywhere else
  (`LOOPBACK_ONLY_PROCESSES`). `GET /admin/actions` lists every action and its route
  (`admin_action_path_patterns`), for example `POST /hives/{hive}/nodes` for `run_node`.
- **`ADMIN_COMMAND`** messages over the mesh (`meta.type: "admin"`).
  `dispatch_internal_admin_command` authorizes each caller per action (for example
  `authorize_wf_rules_caller`, `authorize_cloud_relay`). Archi, IO.cloud and SY.wf-rules use it.

SY.admin does the work by messages. Node lifecycle goes to `SY.orchestrator@motherbee` as
`ADMIN_COMMAND`s (`action_routes_via_local_orchestrator`); the orchestrator accepts mutating ones
only from `SY.admin@<its hive>` (`is_allowed_admin_source_name`). Updates go to
`SY.orchestrator@<hive>` as `SYSTEM_UPDATE`; secrets to SY.vault; identity changes to SY.identity;
edge rows to SY.edge.

### 7.2 Archi

SY.architect (`src/bin/sy_architect.rs`) is Archi, the operator's AI assistant with a web UI
(`/`, `/api/chat`). It listens on `127.0.0.1:3000` (`JSR_ARCHITECT_LISTEN`, else `architect.listen`,
else `DEFAULT_ARCHITECT_LISTEN`) with no authentication, and acts through SY.admin with
`ADMIN_COMMAND`s.

### 7.3 Who may send what: the system policy

Two policy layers live in the router (`src/router/system_policy.rs`):

- **System layer: authority.** `policy/system.rego`, compiled to `policy/system.wasm` and
  `policy/system_route.wasm` and baked into the router with `include_bytes!`. No operator path can
  change it. At local delivery, the router evaluates it for every protected system action
  (`PROTECTED_SYSTEM_ACTIONS`) against the router-stamped sender name, and drops what it denies
  (`prepare_local_delivery`).
- **User layer: selection.** The operator's Rego, compiled to WASM by SY.opa.rules on the motherbee,
  carried to every hive in `dist/policy/opa/` and installed by each hive's SY.opa.rules into
  `/jsr-opa-<hive>` (`go/sy-opa-rules/distribution.go`). The router uses it to choose a destination
  when `routing.dst` is `null` (`resolve_target_with_identity`). By contract it may only narrow the
  system layer, never broaden it.

The rules of `fluxbee/system/allow`:

| Rule | Sender | May send |
| --- | --- | --- |
| 1 | `SY.admin@motherbee` | edge commands: `EDGE_OPEN_URL`, `EDGE_CLOSE_URL`, `EDGE_LIST_URLS`, `EDGE_PUBLISH_BLOB`, `EDGE_UNPUBLISH_BLOB` |
| 2 | `SY.admin@motherbee` | to any hive: `CONFIG_GET`, `CONFIG_SET`, `SYSTEM_UPDATE`, `SYSTEM_SYNC_HINT`, `CONFIG_CHANGED` |
| 3 | `SY.orchestrator@<any hive>` | the node lifecycle and inspection actions it forwards, `GET_VERSIONS`, `GET_RUNTIMES`, `GET_RUNTIME`, `SYSTEM_CORE_ROLLBACK`, `ADD_HIVE_FINALIZE`, `REMOVE_HIVE_CLEANUP` |
| 4 | `SY.admin@<same hive>` | every other protected action |
| 5 | `SY.vault@<same hive>` | `NODE_STATUS_GET` |
| 6 | `RT.gateway@motherbee` | `WAN_REACHABILITY_VOUCH` (§5.2) |

The second entrypoint, `fluxbee/system/frontdesk_route`, applies before the user layer when
`routing.dst` is `null`. It sends a message with no `src_ilk`, or from a `temporary` ILK, to the
identity frontdesk named in `government.identity_frontdesk` (default `SY.frontdesk.gov@<hive>`).

Nodes add their own caller checks on top. For example, SY.identity accepts each identity action
only from listed callers (`allowed_prefixes` in `IdentityRuntime::new`), and SY.vault accepts writes
only from SY.admin and SY.architect (`handle_put` and its siblings in `src/bin/sy_vault.rs`).

## 8. The data plane in brief

- **Identity.** An ILK (`ilk:<uuid>`) is an actor: a person, a node or a system service. A tenant
  (`tnt:<uuid>`) is a cost center; each ILK is charged to exactly one tenant. SY nodes compute their
  ILK from their name; managed nodes get theirs from `ILK_REGISTER` at `run_node`. An IO node
  provisions a `temporary` ILK for an unknown sender (`ILK_PROVISION`) and SY.frontdesk.gov
  completes the registration. The primary store is the `fluxbee_identity` database; workers keep
  replicas (§5.3). Reference: [identity.md](identity.md); model:
  [ilk-tenant-model-v1.md](../ilk-tenant-model-v1.md).
- **The message log.** The router where a message enters copies it to its stream under
  `storage.turns`, for every kind except `system`, `admin` and `query_response`
  (`should_publish_turn`). Every hive's streams deliver `storage.*` subjects to
  `SY.storage@motherbee` (`sinks_for`), which writes the `fluxbee_storage` database (table `turns`).
  Design: [router-stream-v1.md](../router-stream-v1.md); storage: [13-storage.md](storage.md).
- **Memory per communication.** A communication is `(thread_id, agent ILK)`. An agent's router copies
  both directions of its `AI.*` nodes' turns under `cognition.turns` to SY.cognition on the same hive
  (motherbee and workers), which keeps memory packages in `/jsr-memory-<hive>` and appends what it
  derives under `storage.cognition.*` for SY.storage. On delivery to an `AI.*` node the router
  attaches that communication's package as `meta.memory_package` and drops any package the sender
  set (`serialize_for_local_delivery`, `prepare_local_delivery`). Reference:
  [cognition.md](cognition.md); design: §6 of [router-stream-v1.md](../router-stream-v1.md).
- **LLM usage records.** Every model call made through the AI SDK appends one `storage.usage` record
  (`UsageRecorder` in `crates/fluxbee_ai_sdk/src/usage.rs`), which says which tenant pays and why
  (`TenantSource` in `crates/fluxbee_sdk/src/usage.rs`). SY.storage keeps them in `llm_usage`, and
  SY.admin serves them (`GET /hives/{hive}/usage`, `USAGE_QUERY`). Design: §7 of
  [router-stream-v1.md](../router-stream-v1.md).
- **Secrets.** Every secret lives in SY.vault on the motherbee (`src/bin/sy_vault.rs`): SQLite at
  `/var/lib/fluxbee/vault.db`, values encrypted with AES-256-GCM under
  `/etc/fluxbee/vault.master.key`. Nodes ask SY.vault for a secret at run time, and a change is
  broadcast as `VAULT_SECRET_CHANGED`. Node page: [sy-vault.md](../nodes/sy-vault.md).

## 9. Ports

| Port | Hive | Listener | Notes |
| --- | --- | --- | --- |
| 9000/tcp | motherbee | `rt-gateway`, `wan.listen` | WAN; spokes dial it; transport per `wan.mtls` (§5.1) |
| 9100/tcp | motherbee | SY.identity, `0.0.0.0:<identity.sync.port>` | identity replicas; per-hive HMAC (§5.3) |
| 22000/tcp | motherbee; spokes on 127.0.0.1 after the stage-3 switch | Syncthing | software and blobs (§5.4) |
| 443/tcp | ingress | SY.edge, `edge.listen` | the public HTTPS door (§6.1) |
| 8080/tcp | motherbee, 127.0.0.1 | SY.admin | no authentication |
| 3000/tcp | motherbee, 127.0.0.1 | SY.architect (Archi) | no authentication |
| 8384/tcp | every hive that runs Syncthing, 127.0.0.1 | Syncthing GUI and REST API | used by the orchestrator |
| 5432/tcp | motherbee | PostgreSQL | the URL `fluxbee-firstboot` stores points at `127.0.0.1:5432` |
| 22/tcp | a box being joined | sshd | the motherbee uses it only while `add_hive` runs, then revokes its access |

Nodes and routers inside a hive talk over Unix sockets and shared memory, never TCP. Where ufw or
firewalld is installed, the orchestrator opens the WAN port, 9100/tcp on the motherbee
(`ensure_core_firewall_local`), and 22000/tcp, 22000/udp and 21027/udp for Syncthing
(`syncthing_firewall_rules`), though after the stage-3 switch Syncthing uses TCP only. On an egress
hive nftables owns inbound traffic instead (`inet fluxbee_egress`, `reconcile_egress_nat`).

## 10. Limits to know

- The WAN accepts any leaf the mesh CA signed and binds it to the hive id it carries; there is no
  revocation list and no pinning (`MeshTlsMaterial::server_config`, `tls_accept`).
- Rule 3 of the system policy has no direction: any hive's orchestrator, the ingress's included, may
  send those actions to any hive (comment above `orchestrator_actions` in `policy/system.rego`).
- Hub-vouched reachability exists only on a spoke's gateway router, so a frame relayed spoke to
  motherbee to spoke cannot reach a node on that spoke's non-gateway router
  (`remote_source_from_gateway`).

## 11. Where to read next

- [protocol.md](protocol.md): the message format, framing, the handshakes and the system messages.
- [routing-and-policy.md](routing-and-policy.md): the FIB, names, VPNs, static routes, taps, and the
  system and user policies.
- [shared-memory.md](shared-memory.md): the six regions, their layouts and their writers.
- [identity.md](identity.md): ILKs, tenants, channels, registration and replication.
- [cognition.md](cognition.md): memory per communication and the memory package.
- [docs/nodes/](../nodes/): one page per SY node, named after its unit, for example
  [sy-vault.md](../nodes/sy-vault.md).
- [13-storage.md](storage.md): SY.storage and its PostgreSQL tables.

Design documents behind this page, with their history and decisions:
[router-stream-v1.md](../router-stream-v1.md), [ilk-tenant-model-v1.md](../ilk-tenant-model-v1.md),
[edge-ingress-spec-v6.md](../edge-ingress-spec-v6.md), [io-cloud-api.md](../io-cloud-api.md),
[edge-multihop-reachability-spec-v1.md](../edge-multihop-reachability-spec-v1.md),
[host-posture-and-exposure-spec-v1.md](../host-posture-and-exposure-spec-v1.md) and
[packaging-and-build.md](../packaging-and-build.md) (Spanish).
