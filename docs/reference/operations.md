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
   - copy `/etc/fluxbee/hive.yaml.example` to `hive.yaml` if there is none;
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

On a development host, `scripts/install.sh [<version>]` does steps 1 and 3 from the local checkout:
it builds the package with `packaging/build-deb.sh` and installs it with
`apt-get install --reinstall`. Steps 4 and 5 stay the operator's.

## Ports and posture

| Port | Who | Bound where | Source |
| --- | --- | --- | --- |
| 8080/tcp | SY.admin HTTP | `127.0.0.1` (`admin.listen`) | `src/bin/sy_admin.rs` |
| 3000/tcp | SY.architect UI/API | `127.0.0.1` (`architect.listen`) | `packaging/hive.yaml.example` |
| 9000/tcp | WAN between hives (gateway router) | `wan.listen`; set on the motherbee only | [rt-gateway](../nodes/rt-gateway.md) |
| 9100/tcp | identity sync | `0.0.0.0`, motherbee only | [SY.identity](../nodes/sy-identity.md) |
| 9200/tcp | IO.web, for the ingress' edge only (mutual TLS, a connected ingress) | `0.0.0.0`, motherbee only, once the operator starts it | [IO.web](../io/io-web.md) |
| 22000/tcp | Syncthing | the motherbee; spokes dial it and listen on `127.0.0.1` | `SYNCTHING_SYNC_PORT_TCP` in `src/bin/sy_orchestrator.rs` |
| 8384/tcp | Syncthing REST | `127.0.0.1` | Syncthing unit written by the orchestrator |
| 5432/tcp | PostgreSQL | the distribution's default; `fluxbee-firstboot` uses `127.0.0.1` | `packaging/fluxbee-firstboot` |
| `edge.listen` (e.g. 443) | SY.edge, the public door | ingress hives only | `packaging/hive.yaml.example` |

The orchestrator writes no rule to `ufw` or `firewalld`. An active one is a `posture` drift alert,
`host_firewall_active` (at boot and about every minute), since it may close 9000, 9100, 9200, 22000
or 443 without anyone seeing it. SSH on the DMZ hosts is the product's own
([control-path-hardening-v1.md](../control-path-hardening-v1.md) §6):

- **The ingress** accepts SSH (the ports `sshd -T` reports, IPv4 and IPv6) only from loopback, the
  motherbee's address in its `wan.uplinks` and `ingress.ssh_allow_from`, and drops all other SSH;
  nothing else is filtered (`table inet fluxbee_ingress`, reconciled at boot and about every
  minute, loaded at boot by `fluxbee-ingress-nft.service`). Only a complete input set changes it.
  An input that fails (hive.yaml, `sshd -T`, an uplink that does not resolve), a bad entry or an
  empty allow set keeps the rules in force and raises `ingress_ssh_input_failed` (a warning),
  `ingress_ssh_rules_failed` or `ingress_ssh_allow_set_empty`; a failed load raises
  `ingress_ssh_rules_failed`. Recovery: the hypervisor console, or `ingress.ssh_allow_from` in the
  ingress' hive.yaml and a restart of `sy-orchestrator`.
- **The egress** keeps SSH on its WAN interface by default (`egress.ssh_from_wan: true`, D31); with
  `false` its `fluxbee_egress` table drops SSH arriving on `wan_iface`, on the ports `sshd -T`
  reports, else the ones it already drops.

Admin and Archi have no authentication and must stay on loopback; the orchestrator checks the bound
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
| `ssh_allow_from` | only with `role: ingress`: the networks besides the motherbee that may reach SSH on the ingress host, each an IPv4 or IPv6 address or a CIDR with its host bits clear; written into its hive.yaml as `ingress.ssh_allow_from`. SY.admin refuses it on another role or malformed (`INVALID_REQUEST`, `add_hive_ssh_allow_from_error`) |
| `egress.ssh_from_wan` | only with `role: egress`: `false` drops SSH arriving on the egress's `wan_iface` (default `true`); written into its hive.yaml |
| `harden_ssh`, `restrict_ssh` | SSH hardening after the join (both default to false; `restrict_ssh` follows `harden_ssh`) |
| `require_dist_sync`, `dist_sync_probe_timeout_secs` | fail the join if the dist folders do not sync (probe default 45 s) |

Checks before anything starts: `INVALID_HIVE_ID`, `INVALID_ADDRESS`, `MISSING_WAN_LISTEN` (the
motherbee needs `wan.listen`), `JOIN_IN_PROGRESS`, `HIVE_EXISTS`. The old `ssh_access` field is
refused: SSH exists only inside the join (`reject_removed_add_hive_fields`). No list in hive.yaml
says which hives may join: adding a hive is what registers it, and the motherbee's router admits
only registered hives (FINDINGS A-48).

The answer is `{"status": "accepted", "join_id", "poll": "GET /hives/<id>"}` (HTTP 202): the join
runs in the background and its progress is in `/var/lib/fluxbee/hives/<id>/info.yaml`, read by
`GET /hives/<id>` (`status` joining, connected, pending, failed or interrupted; a `join` block
with phase and error). The `join` block survives every rewrite of the entry until the join ends
with `phase: done`; an orchestrator restart mid-join ends it at the next boot with
`JOIN_INTERRUPTED`, keeping the status the join had reached (`joining` becomes `interrupted`).
Posting the same request again resumes a pending, interrupted or failed join.

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
section. An egress join reads the box before it installs anything: `edge_ip`, when the request
omits it, is the single address `lan_iface` carries inside `lan_cidr` (none or several fail the
join, asking for an explicit one), an explicit one must be on `lan_iface`, and both interfaces
must exist (`EGRESS_LAN_LEG_INVALID`; `EGRESS_LAN_LEG_NOT_VERIFIED` when the box cannot be
read). Once the egress' orchestrator is on the mesh it reads
again that the `inet fluxbee_egress` table is loaded, forwarding is on, IPv6 is disabled and the
LAN leg is in place, and reports it (`egress_nat_applied`, `egress_ipv4_forwarding`,
`egress_ipv6_blocked`, `egress_lan_leg_ok`); any of them missing fails the join with
`EGRESS_NAT_NOT_READY` (`EGRESS_NAT_NOT_VERIFIED` when the box cannot be read). The interfaces and their addresses are the operator's: the orchestrator
never configures them, and the egress fails its own boot on any of these.

**SSH checks before the install.** An ingress join reads the motherbee's source address over the
bootstrap session (`SSH_CONNECTION`) and stops with `SSH_SOURCE_NOT_ALLOWED` when the ingress' SSH
rules would not admit it: the message names the address to add through `ssh_allow_from`. Its later
SSH steps (hardening, key revoke) therefore come from an allowed address. An egress join with
`egress.ssh_from_wan: false` stops with `SSH_FROM_WAN_LOCKOUT` when the box routes the session's
client through `wan_iface` (`ip -o route get <client>`), where the join's packets come in; join it
through another interface. Either check answers `SSH_SOURCE_NOT_VERIFIED` when it cannot read the
session.

**Internet probe.** The egress, a worker that routes through it and the motherbee check the
internet with an HTTPS GET of their own `egress.reachability_url` (the egress at boot and every
drift tick, the others at boot); a failure is a warning in that hive's journal. There is no
default: without the URL the probe does not run and logs `internet_reachable=not_configured`. An
egress join takes `egress.reachability_url` from the request, else the motherbee's, and refuses
anything but an `https://` URL; a worker join copies the motherbee's with `gateway_ip`. A hive
joined before gets it by adding the line to its hive.yaml and restarting `sy-orchestrator`.

A successful join's result (`join.result` in `GET /hives/<id>`) carries `warnings`. A join that
logged in with `ssh_password` (its key-first probe failed and the password seeded the motherbee
key) and did not harden SSH says there that password logins stay enabled on the box, and how to
close them: `PasswordAuthentication no` in
`/etc/ssh/sshd_config.d/00-fluxbee-hardening.conf` and a restart of ssh, which is what
`harden_ssh: true` does.

**Remove a hive:** `DELETE /hives/{hive}` (`remove_hive_flow`) sends `REMOVE_HIVE_CLEANUP` to the
spoke over the mesh (8 s). The spoke answers once its cleanup is scheduled as a transient unit of
its own (`fluxbee-remove-hive-cleanup-<ms>`), which deletes its TLS material and identity key,
stops the Fluxbee services and the managed nodes and deletes its node state, and runs to its end
after the spoke's orchestrator has stopped; an ingress then drops its SSH table
(`inet fluxbee_ingress`) and `fluxbee-ingress-nft.service`, an egress its NAT table and files. An
egress or ingress the cleanup did not reach keeps them, and the answer's `warning` says what to
remove from its console. The motherbee then unpairs Syncthing, deletes
`hives/<id>/` and the hive's identity key under `/var/lib/fluxbee/identity/keys/`
(`identity_key_cleanup` in the answer). When the spoke did not answer, the motherbee restarts its
`sy-identity` to close any session the removed hive still holds (`identity_restart`). The
motherbee itself cannot be removed. The registry is the authority: at boot the motherbee deletes
the identity key of every hive with no `hives/<id>/`.

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
| `fluxbee-dist-agent-assets` | `blob/agent-assets` | `blob/agent-assets` | workers only |

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
| config, state, status | `GET`/`PUT /hives/{hive}/nodes/{name}/config`, `GET .../state`, `GET .../status` | for an SY node or the router, `status` reads its packaged unit (`sy-<name>`, `rt-gateway`) and the router's LSA, with `config` and `state` not managed and no `status_version` (the read writes nothing); a core node this hive's role does not run answers `NODE_NOT_FOUND` |

`run_node` refuses: `SY.*`/`RT.*` names; any node in the root tenant except the core IO runtimes
`io.cloud`, `io.blob`, `io.web` (`TENANT_ROOT_NOT_ALLOWED`, `genesis_refusal`); a core IO runtime
under any name but its own, `IO.cloud@motherbee`, `IO.blob@motherbee`, `IO.web@motherbee`, or one of
those names on another runtime (`CORE_IO_NAME_MISMATCH`), and a core IO node outside the root
tenant (`TENANT_ROOT_REQUIRED`; both `core_io_refusal`); a tenant that is not active; a `WF.*` node
whose runtime is not a workflow package (`WF_RUNTIME_PACKAGE_REQUIRED`). Start, restart and the
boot relaunch refuse the same root-tenant cases.

**Full teardown** (`kill_node` with `purge_instance`, plus `force` to send SIGKILL first):

1. `TIMER_PURGE_OWNER` to the hive's SY.timer (a failure stops the purge:
   `TIMER_PURGE_FAILED`);
2. stops the unit, deletes the node directory, its UUID file and its identity-map entry;
3. one `teardown_node {node_name, ilk_id}` to `SY.admin@motherbee`, which checks that the node is
   on the caller's hive and the ILK is the node's, deletes the secrets dedicated to the ILK, each
   in its own tenant, then marks and purges the ILK, and reports each step (`ilk_deleted`,
   `ilk_delete`, `ilk_delete_error`, `vault_secrets_purged`; an error answer never reads `ok`;
   [SY.admin](../nodes/sy-admin.md#node-teardown)). The orchestrator waits 120 s for it (150 s for a
   spoke's purge); with no answer the outcome is `unknown` (`TEARDOWN_UNKNOWN`, no counts) and names
   the ILK to check, since SY.admin may still finish. The node's identity-map entry is already gone
   then, so a failed or unknown step is not finished by purging again: once the node is gone from
   the mesh, run `teardown_node` again with the reported `ilk_id`;
4. reports routes, VPNs and taps that still name the node (`routing_references`); it does not
   remove them.

Keeping nodes alive: every 60 s the orchestrator relaunches a managed node that should run
(`relaunch_on_boot`), whose unit is inactive and that the router does not see
(`MANAGED_RECONCILE_INTERVAL_SECS`); every 5 s its watchdog starts `rt-gateway` and any system
service that is not active (`watchdog_tick`).

## Orchestrator boot

`bootstrap_local` then `finish_bootstrap` in `src/bin/sy_orchestrator.rs`:

1. validates the core manifest and binaries; spokes re-render their units; the motherbee checks
   its SSH key and mesh TLS, rebuilds the per-role core trees and deletes the identity keys of
   hives the registry does not have;
2. starts `rt-gateway` and waits up to 30 s for it (`wait_for_router_ready`);
3. the host firewall report, the ingress' SSH table (never fatal), egress NAT (fatal on an egress
   hive, as is a missing LAN leg, forwarding off or IPv6 not disabled), Syncthing;
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

It keeps the installation, `/etc/fluxbee`, the vault master key, TLS, the spokes (with their SSH
and NAT tables, which their orchestrators keep applying), routes, VPNs, taps and the history.
Re-running is safe; a summary goes to
`/var/lib/fluxbee/state/factory-reset.log`.

## Certificates (ACME)

The public edge's certificate comes from an ACME certificate authority, Let's Encrypt unless
configured otherwise, validated with TLS-ALPN-01 on the :443 the edge already opens. SY.admin on
the motherbee orders and renews it, and the edge serves it from SY.vault and reloads it without a
restart ([SY.admin](../nodes/sy-admin.md#the-acme-client), [SY.edge](../nodes/sy-edge.md); design:
[docs/acme-edge-certificates-v1.md](../acme-edge-certificates-v1.md)). The certified name is the
host of `admin.public_base_url`; the CA comes from the `acme:` section. Nothing in the code names
a domain or a CA.

**Set up a fresh installation:**

1. **DNS.** An A record for the name to the ingress' public address, and an AAAA record if the
   ingress has a public IPv6 address. A record that points elsewhere makes the CA validate the
   wrong host.
2. **Port 443/tcp** open on the ingress to the whole internet: the CA validates from several
   places it does not publish. Port 80 is not used.
3. **The ingress' secret name.** Its `edge.tls_vault_key` must start with `edge_tls` (new
   installations use `edge_tls`); SY.admin writes no other secret.
4. **The motherbee's hive.yaml:**

   ```yaml
   admin:
     public_base_url: "https://hive-k3m9x7q2.example.com"   # port 443, a DNS name with a dot
     public_edge_node: "SY.edge@ingress1"   # needed when more than one ingress is connected
   acme:
     terms_agreed: true                     # you accept the CA's terms
     # optional: directory_url, staging_directory_url, profile, contact, trust_anchor
   ```

5. **Restart SY.admin** when it is idle (`sudo systemctl restart sy-admin`): a restart cuts
   executor plans and relayed Cloud commands in flight.
6. **Read `GET /acme/status`.** `config.state` must be `on`; when it is `off`, `config.reason`
   says why (the same line is in `journalctl -u sy-admin`). `config.names` are the certified
   names, `terms_url` is the terms you agreed to, and `edge.live` is what the edge serves.
7. **Optionally, `POST /acme/test`** (it creates the staging account and arms the edge, so confirm
   it first): a staging order with the real names, issued and checked but never installed. Its
   result is `last_test` in the status; a staging root is reported as untrusted, as expected.
8. The first check runs 2 min after SY.admin starts. An edge with no certificate gets one ordered
   then; `last_attempt` and `issued` in the status show the result, and `renewal.renew_at` when
   the next renewal is due. A failure shows in `last_attempt` and backs off 1 h, 2 h, 4 h and so
   on, up to 24 h.

**Move a hand-seeded certificate to ACME.** The check never replaces a certificate it did not
issue: it alerts (`foreign_certificate` once the leaf passes two thirds of its lifetime,
`name_not_covered` when it does not cover the name) and leaves it. `POST /acme/renew`, confirmed,
orders now: the new certificate goes into the edge's secret, the hand-seeded one stays in the
vault as the previous version, and the edge reloads. From then on the check renews it by itself.
The same call certifies a new name after `public_base_url` changed and SY.admin restarted.

**Add a web host to the certificate** (Archi's, [io-web-v2](../io-web-v2.md) §11.1). One
certificate carries the main name and every host `web:` declares, each a label before the main
name:

1. **DNS.** A record for `archi.<main name>` pointing at the ingress, like the main name's (a CNAME
   of it works), made by hand.
2. **The motherbee's hive.yaml:** `web: {archi: true}`. A bare `web:` declares nothing.
3. **Restart SY.admin** when it is idle. Its log names the declared host, and the edge gets the
   host's route within a minute. The served certificate does not cover the host yet: the check
   raises `name_not_covered` (warning) and orders nothing, since the leaf is not due and the cap
   holds automatic orders.
4. **`POST /acme/test`**, confirmed: the staging order carries every name, and `last_test` says
   which one the CA did not validate, if any.
5. **`POST /acme/renew`**, confirmed (forced, past the cap). Then `config.names` lists every name,
   `edge.names_missing` is empty, and `name_not_covered` closes.

When the CA validates the main name but not the added one, an order goes on with the main name
alone only when the edge would lose nothing it serves, or cannot wait: no certificate for the
main name, one that already lacks the added name, or one in the last sixth of its lifetime;
`last_attempt.detail` says why. Otherwise the attempt fails (`last_attempt.failed_names` names
the host) and the backoff retries every name, so a certificate for both names is kept; 48 h past
its renewal point, after such a failure, the main name is renewed alone. A `POST /acme/renew`
while the main name is served and not due just fails: fix the host's DNS and renew again.
Removing `web:` (and restarting) takes the host's route off the edge within a minute and its name
off the certificate at the next renewal.

**Roll back.** `POST /hives/motherbee/vault/secrets/<the edge's secret>/rollback` puts the previous
version back, then `POST /acme/reload` makes the edge serve it. The check then sees a hand-seeded
leaf and leaves it. Roll the vault back before downgrading to a version that cannot renew. The
vault keeps one previous version, so after two renewals the hand-seeded certificate is no longer
there: keep its files. A downgrade below 0.1.84 cannot read the `acme.json` 0.1.84 writes: it sets
the file aside and judges the served leaf hand-seeded until a `POST /acme/renew`.

**When the edge refuses a certificate SY.admin issued**, SY.admin rolls the secret back at once,
raises `install_refused` (critical) and orders nothing until you act: find the cause in
`install_refused.detail` and the edge's journal, then `POST /acme/renew`. A refused first
certificate has no previous version to go back to: delete the secret
(`DELETE /hives/motherbee/vault/secrets/<the edge's secret>`) before renewing. A refusal because
the edge could not read SY.vault (`transient`) is not a refusal: the certificate stays and the
next check reloads it.

**"The vault holds a version the edge does not serve."** SY.admin writes the edge's secret only
over the write the edge serves (version and `updated_at`), after one reload. When a renewal stops
with this, the vault holds something the edge refused or never loaded: `POST /acme/reload` to see
why the edge keeps its certificate, then roll the secret back (or delete one the edge never
served) and renew.

**An edge that answers `status: error`** (no public listener, a `disk` or `plaintext` edge asked
to reload) is a configuration error, shown in `edge.config_error`: nothing is ordered and no
`edge_unreachable` is raised. `acme.trust_anchor`, when set, is trusted for every connection to
the CA as well as for the issued chain.

**Alerts.** The certificate conditions (`renewal_failed`, `renewal_overdue`, `expiring`,
`name_not_covered`, `foreign_certificate`, `install_refused`, `edge_without_certificate`,
`edge_unreachable`) are in `GET /acme/status` while open, and in `GET /drift-alerts` with
`category: certificate`: one row when a condition opens and an `info` row when it closes. The
expiry conditions run without `acme:` too.

**When the motherbee is down,** nothing renews and nothing alerts. An external monitor of the
public name's certificate expiry, run from outside the installation, covers that.

## The web door

The public edge serves each host `web:` declares only through io.web
([io-web-v2](../io-web-v2.md) §11): the edge on the ingress takes the request and proxies it to
`IO.web@motherbee`. `GET /web/status` on the motherbee shows the four pieces of the door, each a
state, never an error ([SY.admin](../nodes/sy-admin.md#the-web-doors-status)):

| Piece | Ready | Otherwise |
| --- | --- | --- |
| `web`: the host | `state: declared`, the host in `hosts` | `absent`: add `web: {archi: true}` to the motherbee's hive.yaml and restart SY.admin; `invalid`: `reason` says why |
| `certificate` | `hosts_missing: []` | `POST /acme/test`, then `POST /acme/renew` ("Add a web host to the certificate" in [Certificates](#certificates-acme)). `served` is the leaf at the ACME client's last look; `GET /acme/status` asks the edge afresh |
| `routes`: the edge's answer to the last push | `state: acknowledged`, `acknowledged_at` within the last minutes (SY.admin pushes every 60 s), and `upstream.state: ready`, the edge's way to io.web | `no_edge`: no `admin.public_edge_node` and not exactly one connected ingress (`detail` says which); `no_answer`: the edge is down or older than 0.1.84; `refused`: the edge's journal says why; `upstream.state: unavailable`: the edge has no way to io.web and answers every web host 502, `upstream.reason` says what is missing on the ingress (its mesh TLS material, `hive_id`, `wan.uplinks`), then restart `sy-edge` there. `upstream.last_failure` is the last request io.web did not answer (`at`, `kind`); the edge's journal says why (`io.web did not answer proxied requests`) |
| `io_web` | `state: running`, `answer.health_state: HEALTHY` | `absent`: io.web starts only by an explicit launch, once ([Node lifecycle](#node-lifecycle)): `POST /hives/motherbee/nodes` with `node_name: IO.web@motherbee`, `runtime: io.web` and the root tenant as `tenant_id`; `stopped`: `start_node`; `failed` or `silent`: `journalctl -u fluxbee-node-IO.web-motherbee`, then `restart_node`; `unknown`: the orchestrator did not answer (`view_error`), retry |

With all four ready, the edge's upstream included, `https://archi.<main name>/` answers io.web's
page from outside, with a valid certificate. A recent `routes.upstream.last_failure` with
everything else ready means io.web refused the edge or did not answer it: the edge's journal on the
ingress and io.web's on the motherbee say why.

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
| `GET /drift-alerts`, `GET /hives/{hive}/drift-alerts` | the history the orchestrators record (`category` `core`, `egress`, `posture`, `syncthing`), and `certificate`: SY.admin's certificate conditions (`record_drift_alert`), a row with `state: open` when one opens and an `info` row with `state: closed` when it closes |
| `GET /acme/status` | the edge's certificate: ACME's configuration, the names, the open conditions ([Certificates](#certificates-acme)) |
| `GET /web/status` | the web door: the declared host, the certificate's names, the edge's answer to the web routes with its way to io.web, io.web ([The web door](#the-web-door)) |
| `GET /config/storage/metrics` | SY.storage's inbox |
| `systemctl --failed`, `journalctl -u <unit>` | per host |

`lab/ops.py health [--since HH:MM]` runs on every host and passes only when all of `failed`,
`denials`, `srcdrops`, `streamlost`, `usagelost` and `usagerej` are 0 (what each counts:
[rt-gateway](../nodes/rt-gateway.md#what-it-logs-that-labopspy-health-counts)). Two lines from
SY.admin follow:

```text
edge-cert  acme=on names=hive-k3m9x7q2.fluxbee.ai,archi.hive-k3m9x7q2.fluxbee.ai conditions=none
web        host=archi.hive-k3m9x7q2.fluxbee.ai route=acknowledged upstream=ready io.web=running
```

- `edge-cert` (`GET /acme/status`): a `critical` condition fails the check.
- `web` (`GET /web/status`): the declared host (`none` without `web:`, `invalid` for a section
  that cannot work), the edge's answer to the routes (`routes.state`), the edge's way to io.web
  (`routes.upstream.state`: `ready`, `unavailable`, or `unknown` when the last push was not
  acknowledged), with the kind and time of its last failure when there is one
  (`upstream=ready(head@2026-10-23T14:02:11Z)`), and io.web's state, with its health when it
  reports one other than `HEALTHY` (`io.web=running(DEGRADED)`). It never fails the check: io.web
  runs only once the operator starts it, and the edge answers again within a minute of a restart.

`lab/ops.py versions` and `lab/ops.py opa-status` print the version and policy view per hive.

## Links

- [docs/host-posture-and-exposure-spec-v1.md](../host-posture-and-exposure-spec-v1.md): ports,
  exposure and what is still open.
- `packaging/base-nodes.json`: the base node set (which runtimes ship, which boot).
- `lab/ops.py`: the operator's routine on the lab.
