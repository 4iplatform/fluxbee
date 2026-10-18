# Operations: deploy and lifecycle

How a Fluxbee mesh is installed, grown, upgraded and checked. Every statement points to the code
that does it; the operator's own routine on the lab is `lab/ops.py`, used here as an example.

The ground rules, from the code:

- Only the **motherbee** is installed from the `.deb`. Worker, ingress and egress hives
  ("spokes") are bootstrapped from the motherbee by `add_hive` and updated by it afterwards
  (`add_hive_flow`, `sync_core_to_worker` in `src/bin/sy_orchestrator.rs`).
- Every operator action goes through **SY.admin**, the HTTP API on the motherbee at
  `127.0.0.1:8080` (`admin.listen`; `JSR_ADMIN_LISTEN` overrides it). SY.admin passes node and hive
  actions to the motherbee's `SY.orchestrator`, which runs them or forwards them to the target
  hive's orchestrator (`build_admin_request`, `action_routes_via_local_orchestrator` in
  `src/bin/sy_admin.rs`; `forward_system_action_to_hive` in `src/bin/sy_orchestrator.rs`).
- Software reaches the spokes through Syncthing folders the motherbee serves; there is no SSH
  delivery after a hive has joined.

## Roles and their services

`/etc/fluxbee/hive.yaml` names the hive (`hive_id`), its `role` and, per role, the system nodes
to start in order (`system_nodes.<role>.nodes`) and the ones the boot waits for (`wait_for`).
`validate_system_nodes` enforces that each list starts with `SY.config.routes`, that only the
motherbee runs `SY.vault`, and that on the motherbee every vault consumer comes before
`SY.vault` (see [SY.vault](../nodes/sy-vault.md)). The shipped lists (`packaging/hive.yaml.example`):

| Role | System nodes (start order) |
| --- | --- |
| motherbee | SY.config.routes, SY.identity, SY.opa.rules, SY.admin, SY.architect, SY.storage, SY.cognition, SY.timer, SY.wf-rules, SY.frontdesk.gov, SY.vault |
| worker | SY.config.routes, SY.identity, SY.opa.rules, SY.cognition, SY.timer, SY.wf-rules |
| egress | SY.config.routes, SY.opa.rules |
| ingress | SY.config.routes, SY.opa.rules, SY.edge |

Every role also runs `rt-gateway` (started first) and `sy-orchestrator`. The node pages:
[rt-gateway](../nodes/rt-gateway.md), [SY.config.routes](../nodes/sy-config-routes.md),
[SY.identity](../nodes/sy-identity.md), [SY.opa.rules](../nodes/sy-opa-rules.md),
[SY.vault](../nodes/sy-vault.md), [SY.storage](../nodes/sy-storage.md),
[SY.cognition](../nodes/sy-cognition.md), [SY.timer](../nodes/sy-timer.md),
[SY.wf-rules](../nodes/sy-wf-rules.md).

## Install the motherbee

1. **Build** the package: `packaging/build-deb.sh <version>` writes
   `dist/fluxbee_<version>_amd64.deb`. It builds every Rust and Go binary, stages them in
   `/usr/bin` and `/var/lib/fluxbee/dist/core/bin` with a `dist/core/manifest.json` that holds
   their sha256, seeds the base runtimes of `packaging/base-nodes.json` under
   `dist/runtimes/<runtime>/<version>/`, and refuses to build without a vendored Syncthing unless
   `FLUXBEE_ALLOW_NO_SYNCTHING=1`.
2. **Publish** it to an apt repository: `scripts/apt-repo-publish.sh --deb <file> [--serve]`
   keeps a flat, unsigned repository (clients use `deb [trusted=yes] http://<host>:8900 ./`); every
   version stays listed.
3. **Install**: `apt-get install fluxbee` (it depends on `postgresql`, `openssl`, `curl`,
   `python3`). The maintainer scripts (`packaging/deb-preinst`, `deb-postinst`):
   - create the `fluxbee` system user and the directories under `/var/lib/fluxbee`;
   - register the base runtimes into the live runtimes manifest (`fluxbee-seed-runtimes`, below);
   - copy `/etc/fluxbee/hive.yaml.example` to `hive.yaml` if there is none, and run
     `fluxbee-migrate-config` on it;
   - enable `sy-orchestrator`. A fresh install starts nothing; it prints
     `Edit /etc/fluxbee/hive.yaml, then run: sudo fluxbee-firstboot`.
4. **Edit** `/etc/fluxbee/hive.yaml` (`hive_id: motherbee`, `role: motherbee`, the `ai` provider,
   `wan.listen`).
5. **First boot**: `sudo fluxbee-firstboot` (`packaging/fluxbee-firstboot`):
   1. starts PostgreSQL, creates the role (`FLUXBEE_DB_USER`/`FLUXBEE_DB_PASSWORD`, default
      `fluxbee`/`fluxbee`) and the databases `fluxbee`, `fluxbee_identity`, `fluxbee_storage`;
   2. starts `sy-orchestrator`, which starts the router and the system nodes and keeps
      restarting until the database secret exists;
   3. waits for SY.admin and stores the secret `storage_postgres_url` (resource type `postgres`,
      root tenant `tnt:00000000-0000-0000-0000-000000000001`) with
      `POST /hives/<hive>/vault/secrets`;
   4. restarts `sy-vault`, then `sy-storage` and `sy-identity` so they read it, then the
      orchestrator, until `GET /hives` answers `status: ok` (up to six attempts);
   5. launches one instance of each `boot: true` runtime of `base-nodes.json` in the root
      tenant: `IO.blob@motherbee` and `IO.cloud@motherbee`;
   6. prints what is still missing: the AI provider key for the core (`openai` or `anthropic`
      in the root tenant) and the per-tenant secrets of IO nodes.

`scripts/install.sh` is the development path: it always builds from the source tree and installs
on the local host; it shares `packaging/base-nodes.json` with the `.deb` build.

## Ports and posture

| Port | Who | Bound where | Source |
| --- | --- | --- | --- |
| 8080/tcp | SY.admin HTTP | `127.0.0.1` (`admin.listen`) | `src/bin/sy_admin.rs` |
| 3000/tcp | SY.architect UI/API | `127.0.0.1` (`architect.listen`) | `packaging/hive.yaml.example` |
| 9000/tcp | WAN between hives (gateway router) | `wan.listen`; set on the motherbee only | [rt-gateway](../nodes/rt-gateway.md) |
| 9100/tcp | identity sync | `0.0.0.0`, motherbee only | [SY.identity](../nodes/sy-identity.md) |
| 22000/tcp+udp, 21027/udp | Syncthing sync and discovery | spokes dial the motherbee | `SYNCTHING_*_PORT_*` in `src/bin/sy_orchestrator.rs` |
| 8384/tcp | Syncthing REST | `127.0.0.1` | Syncthing unit written by the orchestrator |
| 5432/tcp | PostgreSQL | the distribution's default; `fluxbee-firstboot` uses `127.0.0.1` | `packaging/fluxbee-firstboot` |
| `edge.listen` (e.g. 443) | SY.edge, the public door | ingress hives only | `packaging/hive.yaml.example` |

The orchestrator opens the WAN port, and on the motherbee the identity sync port, with `ufw` or
`firewalld` when present (`ensure_core_firewall_local`); Syncthing's ports are opened when
Syncthing is set up. On egress hives it opens nothing: nftables owns inbound there. Admin and
Archi have no authentication and must stay on loopback; the orchestrator checks the bound
sockets about every minute and reports a service bound off loopback (report only). The full
exposure model is in [docs/host-posture-and-exposure-spec-v1.md](../host-posture-and-exposure-spec-v1.md).

## Join a hive (`add_hive`)

`POST /hives` → action `add_hive` → `SY.orchestrator@motherbee` (`handle_hive_paths`,
`accept_add_hive`). Body:

| Field | |
| --- | --- |
| `hive_id` | 1 to 64 characters `[A-Za-z0-9_-]` |
| `address` | the host to join |
| `ssh_user` | required, `^[a-z_][a-z0-9_-]*$` |
| `ssh_password` or `ssh_key` | how to install the motherbee's key; `ssh_key` is an unencrypted PEM private key |
| `role` | `worker` (default), `egress` or `ingress` (`resolve_add_hive_role`) |
| `harden_ssh`, `restrict_ssh` | SSH hardening after the join (both default to false; `restrict_ssh` follows `harden_ssh`) |
| `require_dist_sync`, `dist_sync_probe_timeout_secs` | fail the join if the dist folders do not sync (probe default 45 s) |

Checks before anything starts: `INVALID_HIVE_ID`, `INVALID_ADDRESS`, `MISSING_WAN_LISTEN` (the
motherbee needs `wan.listen`), `WAN_NOT_AUTHORIZED`, `JOIN_IN_PROGRESS`, `HIVE_EXISTS`. The old
`ssh_access` field is refused: SSH exists only inside the join (`reject_removed_add_hive_fields`).

The answer is `{"status": "accepted", "join_id", "poll": "GET /hives/<id>"}` (HTTP 202): the join
runs in the background and its progress is in `/var/lib/fluxbee/hives/<id>/info.yaml`, read by
`GET /hives/<id>` (`status` joining, connected, pending, failed or interrupted; a `join` block
with phase and error). Posting the same request again resumes a pending, interrupted or failed
join.

What a worker join does (`add_hive_flow`):

1. Issues the hive's certificate from the motherbee's mesh CA (`issue_hive_tls`).
2. Gets SSH access with the motherbee key `/var/lib/fluxbee/ssh/motherbee.key` (installing it
   with the given password or key) and waits for the host to settle, up to 900 s
   (`wait_for_host_to_settle`; `HOST_NOT_SETTLED` is retryable).
3. Copies the role's core binaries and `manifest.json` from the motherbee's
   `/var/lib/fluxbee/dist/core/` (`sync_core_to_worker`) and the vendored Syncthing
   (`sync_vendor_to_worker`).
4. Writes the spoke's `hive.yaml`: uplink to the motherbee's `wan.listen`, `wan.mtls: required`,
   `identity.sync.upstream` with `auth: required`, the `ai`, blob and dist sections and the role's
   `system_nodes`; pushes the TLS files to `/var/lib/fluxbee/tls/<hive>/`; on a worker, creates and
   copies the identity sync key (`distribute_hive_identity_key_inner`).
5. Writes one systemd unit per core service (`core_service_unit_contents`), starts `rt-gateway`
   and `sy-orchestrator`, waits for the WAN link and for the spoke's orchestrator to appear.
6. Sends `ADD_HIVE_FINALIZE` over the mesh, which pairs the two Syncthing instances
   (`add_hive_finalize_via_socket`).
7. Marks the hive `connected` and revokes its own SSH access: removes the motherbee key from
   `authorized_keys` and the temporary sudoers file (`revoke_bootstrap_ssh_access`).

Egress and ingress joins (`add_egress_hive_flow`, `add_ingress_hive_flow`) follow the same steps
without the identity key; an ingress join also needs `ingress.listen` and writes the `edge`
section.

**Remove a hive:** `DELETE /hives/{hive}` (`remove_hive_flow`) sends `REMOVE_HIVE_CLEANUP` to the
spoke over the mesh (8 s), which stops the Fluxbee services and deletes its node state; the
motherbee then unpairs Syncthing and deletes `hives/<id>/`. The motherbee itself cannot be
removed.

## Publish runtimes

Runtimes are versioned artifacts under `/var/lib/fluxbee/dist/runtimes/<runtime>/<version>/`,
listed in `dist/runtimes/manifest.json` with `available` versions and a `current` pointer.

- `scripts/publish-runtime.sh --runtime <name> --version <v> --binary <path> [--set-current]
  [--prune-missing] [--dist-root <dir>]` copies a binary, writes `bin/start.sh` and merges the
  manifest. `fluxbee-publish <package dir> [--deploy <hive>]` (`src/bin/fluxbee-publish.rs`)
  publishes a package directory and always sets it current.
- Over HTTP: `POST /admin/runtime-packages/publish` (`publish_runtime_package`, motherbee only)
  with an `inline_package` or `bundle_upload` source; optional `sync_to[]` and `update_to[]`
  follow-ups.
- **U-3, the seed merge.** The `.deb` does not ship `manifest.json`: the manifest is operator
  state. `deb-preinst` snapshots it on upgrade; `fluxbee-seed-runtimes` (run by `deb-postinst`)
  registers each base runtime into the live manifest through `publish-runtime.sh`, leaves a
  hot-published `current` alone when its directory still exists, restores the snapshot only if
  the live manifest is missing or unreadable, and fails if it registered nothing. Repair:
  `sudo fluxbee-seed-runtimes && sudo systemctl restart sy-orchestrator`.

### How dist reaches the other hives

The orchestrator declares one Syncthing folder per kind of content (`dist_sync_folders_for_role`),
`sendonly` on the motherbee and receive-only on spokes:

| Folder | Motherbee path | Spoke path | Roles |
| --- | --- | --- | --- |
| `fluxbee-dist-core-<role>` | `dist/core/<role>` | `dist/core` | every spoke role |
| `fluxbee-dist-vendor` | `dist/vendor` | `dist/vendor` | every spoke role |
| `fluxbee-dist-policy` | `dist/policy` | `dist/policy` | every spoke role |
| `fluxbee-dist-runtimes` | `dist/runtimes` | `dist/runtimes` | workers only |

The motherbee rebuilds `dist/core/<role>` at every start of its orchestrator
(`materialize_role_core_trees`). A spoke's receive-only copy is never edited locally: the
orchestrator reverts local changes through Syncthing's `/rest/db/revert`.

**Announcing a package to workers.** There is no announce message: the motherbee's Syncthing
announces a change after its own scan. To make a worker pull a package now, scan the folder **on
the motherbee**: `POST /hives/motherbee/sync-hint {"channel":"dist","folder_id":"fluxbee-dist-runtimes","wait_for_idle":true}`
(`SYSTEM_SYNC_HINT`, which calls Syncthing's `/rest/db/scan`). SY.wf-rules does exactly this
before deploying a WF node on a worker (`announceRuntimes` in `go/sy-wf-rules/node/deploy.go`,
whose comment notes that a scan on the worker does not make it pull).

`POST /hives/{hive}/update {"category":"runtime","manifest_version":<n>,"manifest_hash":"<sha256>"}`
sends `SYSTEM_UPDATE` to that hive's orchestrator: it answers `sync_pending` (HTTP 202) until its
copy of the manifest has that hash and the artifacts are present, then reloads the manifest and
applies retention; it does not restart nodes. `run_node` reads the manifest from disk.

## Upgrade

### The motherbee

`apt-get install fluxbee=<version>`. On upgrade (`deb-prerm`, `deb-postinst`):

- only the writers of the runtimes manifest stop: `sy-orchestrator`, `sy-admin`, `sy-wf-rules`;
  the router, identity, vault and the rest keep serving on the old binaries while dpkg unpacks;
- the seed runs, `/var/lib/fluxbee/orchestrator/core-installed.json` is removed, and the
  orchestrator starts again;
- its boot (`bootstrap_local`) restarts every core service whose running binary was replaced
  (`/proc/<pid>/exe` ends in ` (deleted)`), in start order, records it as the last core update
  (`via: package`), rebuilds the per-role core trees for the spokes, and moves managed nodes that
  follow `current` onto the new version (`reconcile_persisted_custom_nodes` with `Rebind`).

### The spokes (core update)

`POST /hives/{hive}/update {"category":"core","manifest_version":0,"manifest_hash":"<sha256 of the motherbee's dist/core/manifest.json>"}`:

- The spoke checks that its synced `dist/core/manifest.json` has that hash; otherwise
  `sync_pending`.
- `prepare_core_update` backs up the binaries that differ to
  `/var/lib/fluxbee/orchestrator/core-bin.prev.local/`, swaps them into `/usr/bin` and re-renders
  the units (`regen_local_core_units`).
- It answers first (`phase: restarting`), then restarts `rt-gateway` and the system services in
  order, checking each; a failed check rolls the binaries back. A replaced `sy-orchestrator`
  restarts itself through a transient `systemd-run` timer. The outcome is in
  `core-update-last.json`; what is installed, in `core-installed.json`.
- `POST /hives/{hive}/core/rollback {"generation":"latest"}` (or `{"to_manifest_hash"}`, with
  `dry_run`) goes back to a backed-up generation.
- `GET /hives/{hive}/versions` (and `GET /versions` for every connected hive) compares core,
  vendor and runtimes; for core the installed hash wins over the synced one.

The lab routine `lab/ops.py deploy <version>` does all of this in order: snapshots every VM,
installs the version on the motherbee, waits for its core to report it, waits until each spoke's
orchestrator answers, sends the core update to every spoke, waits until each spoke reports the
motherbee's hash, then runs `health` (`cmd_deploy`).

## Node lifecycle

Managed nodes (`AI.*`, `IO.*`, `WF.*`) run as transient `systemd-run` units named
`fluxbee-node-<name>` with `Restart=always`, and their state lives in
`/var/lib/fluxbee/nodes/<KIND>/<node name>/config.json` (`_system` block written by the
orchestrator). SY nodes and the router are not managed this way and cannot be launched with
`run_node`.

| Action | Route | Notes |
| --- | --- | --- |
| launch | `POST /hives/{hive}/nodes {node_name, runtime, runtime_version, tenant_id, config}` | `runtime_version` defaults to `current`; registers the node's ILK (type `agent`) with SY.identity, writes `config.json`, starts the unit with `FLUXBEE_NODE_NAME`, `FLUXBEE_NODE_ILK_ID`, `FLUXBEE_NODE_TENANT_ID` |
| start / restart | `POST /hives/{hive}/nodes/{name}/start`, `.../restart` | need an existing config (`NODE_CONFIG_NOT_FOUND`) |
| stop | `DELETE /hives/{hive}/nodes/{name}` with `{"purge_instance": false}` | there is no separate stop action; the node is stopped and not relaunched at boot; its config, ILK, secrets and timers stay |
| remove | `DELETE /hives/{hive}/nodes/{name}` with `{"purge_instance": true}` | full teardown, below |
| remove a stopped instance | `DELETE /hives/{hive}/nodes/{name}/instance` | refused while running (`NODE_INSTANCE_RUNNING`) |
| config, state, status | `GET`/`PUT /hives/{hive}/nodes/{name}/config`, `GET .../state`, `GET .../status` | |

`run_node` refuses: `SY.*`/`RT.*` names; any node in the root tenant except the core IO runtimes
`io.cloud`, `io.blob`, `io.web` (`TENANT_ROOT_NOT_ALLOWED`, `genesis_refusal`); a tenant that is
not active; a `WF.*` node whose runtime is not a workflow package (`WF_RUNTIME_PACKAGE_REQUIRED`).

**Full teardown** (`kill_node` with `purge_instance`, plus `force` to send SIGKILL first):

1. `TIMER_PURGE_OWNER` to the hive's SY.timer (a failure stops the purge:
   `TIMER_PURGE_FAILED`);
2. stops the unit, deletes the node directory, its UUID file and its identity-map entry;
3. through SY.admin: `delete_ilk` then `purge_ilk` for the node's ILK, then `vault_list` with the
   ILK as owner and `vault_delete` for each secret it owns;
4. reports routes, VPNs and taps that still name the node (`routing_references`); it does not
   remove them.

Keeping nodes alive: every 60 s the orchestrator relaunches a managed node that should run
(`relaunch_on_boot`), whose unit is inactive and that the router does not see
(`MANAGED_RECONCILE_INTERVAL_SECS`); every 5 s its watchdog starts `rt-gateway` and any system
service that is not active (`watchdog_tick`).

## Orchestrator boot

`bootstrap_local` then `finish_bootstrap` in `src/bin/sy_orchestrator.rs`:

1. validates the core manifest and binaries; spokes re-render their units; the motherbee checks
   its SSH key and mesh TLS and rebuilds the per-role core trees;
2. starts `rt-gateway` and waits up to 30 s for it (`wait_for_router_ready`);
3. firewall, egress NAT (fatal on an egress hive), Syncthing;
4. starts `system_nodes.<role>.nodes` in order and waits up to 60 s for the `wait_for` list
   (`LIFECYCLE_NODES_BOOTSTRAP_TIMEOUT_SECS`; a timeout only warns);
5. on the motherbee, waits for `sy-storage` (30 s) and for its database to answer
   `STORAGE_METRICS_GET` (30 s); failing either ends the orchestrator, which systemd restarts;
6. relaunches persisted nodes and hands over the blob folders.

Until step 6 ends, admin actions get `ORCHESTRATOR_BOOTING` (HTTP 500 through
`error_code_to_http_status`); retry. On SIGTERM the orchestrator exits without stopping the hive.

## Factory reset

`sudo fluxbee-factory-reset` on the motherbee (`packaging/fluxbee-factory-reset`) is a dry run
that lists what it would remove; `--yes` executes after you type the `hive_id` (or pass
`--confirm-hive <id>`). It needs every hive alive and works through SY.admin:

1. deletes every workflow with `force` on motherbee and worker hives;
2. kills and purges every node not in the system set (`SY.*`, `RT.*` and the `boot: true`
   instances of `base-nodes.json`), on every hive;
3. marks and purges every non-system ILK, deletes every vault secret except
   `storage_postgres_url`, `edge_channel_secret:*` and `edge_tls*`, marks and purges every tenant
   but the root;
4. clears the user OPA policy on every hive;
5. truncates every table of `fluxbee_storage` with SY.storage and SY.cognition stopped, removes
   SY.cognition's cache and memory region and the nodes' conversation state, and clears
   Archi's sessions.

It keeps the installation, `/etc/fluxbee`, the vault master key, TLS, the spokes, routes, VPNs,
taps and the history. Re-running is safe; a summary goes to
`/var/lib/fluxbee/state/factory-reset.log`.

## Health checks

| Check | What it tells |
| --- | --- |
| `GET /health` | SY.admin answers; nothing else |
| `GET /hives` | the join records in `hives/*/info.yaml`; `status: ok` only means the motherbee's orchestrator answered |
| `GET /hives/{hive}` | one hive's join record (`status`, `join`) |
| `GET /inventory`, `/inventory/summary`, `/inventory/{hive}` | the live view from the router's LSA; answers while the orchestrator boots |
| `GET /hives/{hive}/nodes`, `.../nodes/{name}/status` | nodes and one node's lifecycle and health |
| `GET /hives/{hive}/versions`, `GET /versions` | core, vendor and runtimes per hive |
| `GET /hives/{hive}/opa/status` | user policy in sync, routers loaded |
| `GET /hives/{hive}/streams` | the router streams' backlog and sinks |
| `GET /config/storage/metrics` | SY.storage's inbox |
| `systemctl --failed`, `journalctl -u <unit>` | per host |

`lab/ops.py health [--since HH:MM]` runs on every host and passes only when all of `failed`,
`denials`, `srcdrops`, `streamlost`, `usagelost` and `usagerej` are 0 (what each counts:
[rt-gateway](../nodes/rt-gateway.md#what-it-logs-that-labopspy-health-counts)).
`lab/ops.py versions` and `lab/ops.py opa-status` print the version and policy view per hive.

## Links

- [docs/host-posture-and-exposure-spec-v1.md](../host-posture-and-exposure-spec-v1.md): ports,
  exposure and what is still open.
- `packaging/base-nodes.json`: the base node set (which runtimes ship, which boot).
- `lab/ops.py`: the operator's routine on the lab.
