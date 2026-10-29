# SY.orchestrator

SY.orchestrator is the supervisor of one hive. It runs on every hive, whatever the role. It
starts the hive's core in order and keeps it running, launches and relaunches the managed nodes
(`AI.*`, `IO.*`, `WF.*`), applies updates that arrive through Syncthing, runs Syncthing itself,
and owns the host posture pieces that exist today. On the motherbee it also joins and removes the
other hives (`add_hive`, `remove_hive`). It is the only unit the `.deb` enables: it brings up the
rest.

Source: `src/bin/sy_orchestrator.rs`. Packaging: `packaging/build-deb.sh`, `packaging/deb-postinst`.

| | |
| --- | --- |
| Binary | `/usr/bin/sy-orchestrator` (`RUST_BINS` entry `sy-orchestrator:sy_orchestrator`) |
| Node name | `SY.orchestrator@<hive>` |
| Role | every role: motherbee, worker, ingress, egress |
| systemd unit | `sy-orchestrator.service`, `Restart=always`, `RestartSec=5`, `TimeoutStopSec=15`; the only unit `deb-postinst` enables |
| Listens on | nothing: it connects to the local router socket; no TCP port |
| State | `/var/lib/fluxbee/orchestrator/`, `/var/lib/fluxbee/hives/`, `/var/lib/fluxbee/nodes/`, `/var/lib/fluxbee/state/orchestrator/`, PID in `/var/run/fluxbee/orchestrator.pid` |

## Where it runs

- `main` accepts `role` motherbee, worker, egress or ingress. Any other value logs
  `SY.orchestrator supports only role=motherbee|worker|egress|ingress; exiting` and exits 0.
- `role: motherbee` requires `hive_id: motherbee` (`PRIMARY_HIVE_ID`); any other role may not use
  that id. Both are errors.
- On the motherbee the unit is the packaged one (`gen_unit`, `After=network.target`). On a spoke
  the orchestrator writes its own units (`core_service_unit_contents`), where `sy-orchestrator`
  and `sy-edge` get `After=network.target rt-gateway.service` and `Wants=rt-gateway.service`.

## How it starts

`main`, in order:

1. Reads `/etc/fluxbee/hive.yaml`; a missing or invalid file, an invalid `system_nodes` section or
   an invalid `ai` section ends the process with an error. Creates its directories
   (`ensure_dirs`) and writes the PID file.
2. **`bootstrap_local`**, before joining the router:
   1. checks the sha256 of every core binary of its role against `dist/core/manifest.json`
      (`validate_core_manifest_for_bins`; fatal);
   2. on a spoke, rewrites the core unit files if their template changed (`regen_local_core_units`);
   3. on the motherbee, makes sure the SSH join key and the mesh CA and TLS exist
      (`ensure_motherbee_ssh_key`, `ensure_motherbee_mesh_tls`), refreshes the per-role core
      trees spokes sync from (`materialize_role_core_trees`), clears interrupted joins, and
      deletes the identity keys of hives the registry does not have
      (`sweep_orphan_identity_keys`);
   4. starts `rt-gateway` and waits up to 30 s for it and its SHM (`wait_for_router_ready`; fatal);
   5. reports an active ufw or firewalld (`report_active_host_firewalls`), applies the ingress'
      SSH table on the ingress role (`reconcile_ingress_ssh`, never fatal), and reconciles egress
      (fatal only on the egress role);
   6. starts Syncthing if blob or dist sync is enabled (a failure only warns);
   7. starts each `system_nodes.<role>.nodes` service in list order (`start_core_service`), using
      `restart` instead of `start` for a service whose binary a package upgrade replaced (its
      `/proc/<pid>/exe` link ends in "(deleted)", `exe_link_is_replaced`, FINDINGS A-45); a
      failure only warns;
   8. waits up to 60 s for those nodes to appear in the router (`wait_for_lifecycle_nodes`); a
      timeout only warns.
3. Joins the router as `SY.orchestrator` (`RouterDispatcher::connect_with_retry`, every 1 s).
4. Starts the admin worker and the system worker (`run_admin_worker`, `run_system_worker`). Each
   handles its messages one at a time, in arrival order.
5. Starts the managed-node reconcile loop (every 60 s) and, on the motherbee, the loop that takes
   Syncthing off public infrastructure once every spoke reports a reachable motherbee
   (`run_syncthing_options_switch_loop`).
6. **`finish_bootstrap`**: on the motherbee, waits for `sy-storage` to be active (30 s) and for its
   database to answer `STORAGE_METRICS_GET` over the mesh (30 s, `wait_for_storage_db_ready`);
   then, on every role, runs the managed-node reconcile once (`reconcile_persisted_custom_nodes`,
   the pass the 60 s loop repeats, [Managed nodes](#managed-nodes)) and hands the blob folders
   over.
7. Sets `booted`, logs `hive ready`, and starts the watchdog (every 5 s).

### ORCHESTRATOR_BOOTING

From step 4 to step 7 the orchestrator is on its router but not ready. `run_admin_worker` answers
every admin command from its own hive's SY.admin with `ORCHESTRATOR_BOOTING`, "the orchestrator is
still booting; retry shortly", until `booted` is set (any other caller gets `FORBIDDEN` first, see
[Messages](#messages)). System messages are handled during that time. SY.admin passes the code
through to its caller (over HTTP it arrives as a 500).

## System nodes per role

There are no defaults in the code: `system_nodes_for_role` requires a
`system_nodes.<role>` section in hive.yaml with a `nodes` list (start order) and an optional
`wait_for` list. The packaged template (`packaging/hive.yaml.example`):

| Role | `nodes`, in start order |
| --- | --- |
| motherbee | SY.config.routes, SY.identity, SY.opa.rules, SY.admin, SY.architect, SY.storage, SY.cognition, SY.timer, SY.wf-rules, SY.frontdesk.gov, SY.vault |
| worker | SY.config.routes, SY.identity, SY.opa.rules, SY.cognition, SY.timer, SY.wf-rules |
| egress | SY.config.routes, SY.opa.rules |
| ingress | SY.config.routes, SY.opa.rules, SY.edge |

`add_hive` copies the motherbee's section for the new hive's role into that hive's hive.yaml
(`render_system_nodes_yaml`).

`validate_system_nodes` refuses a list that:

- is empty, or does not start with `SY.config.routes` (the writer of the routing SHM);
- on the motherbee, lacks `SY.vault`, or lists one of its consumers (`SY.admin`, `SY.architect`,
  `SY.storage`, `SY.identity`, `SY.cognition`, `SY.frontdesk.gov`) after it. SY.vault announces
  its secrets when it starts, and a router broadcast reaches only nodes already registered;
- on any other role, contains `SY.vault`;
- has a non-`SY.*` name (the allowlist of other names is empty), duplicates, or a `wait_for`
  entry that is not in `nodes`.

A node name maps to its unit by `name_to_service`: drop `SY.`, lowercase, dots to dashes, prefix
`sy-` (`SY.frontdesk.gov` → `sy-frontdesk-gov`).

`get_node_status` reads an SY node from that unit, and the router (`RT.gateway` and its stream)
from `rt-gateway` (`local_node_unit`): the lifecycle comes from the unit and the router's LSA,
so a stopped or crashed one reads STOPPED or FAILED. The router's own name is no node of its LSA:
it counts as seen while the LSA entry for its hive is fresh (`node_seen_by_router`). `config` and
`state` read `{"managed": false}`; the health is still what the node answers to
`NODE_STATUS_GET`. A core node this hive's role does not run (not in its `system_nodes`; the
orchestrator runs everywhere) answers `NODE_NOT_FOUND`. The files some SY nodes keep under
`/var/lib/fluxbee/nodes/SY/` play no part, and a status read writes nothing for a core node: its
`status_version` is null. The managed listing (`list_nodes` with `managed`) shows those nodes the
same way, with `managed: false`.

## Managed nodes

`run_node` (from SY.admin) ends in `run_node_flow`:

1. Names are `<local>@<hive>`; `SY.*` and `RT.*` names are refused
   (`managed_spawn_disallowed_reason`).
2. A node may not run in the root tenant unless it is core: `SY.*`, `RT.*`, or an `IO.*` node on
   `io.cloud`, `io.blob` or `io.web` (`is_core_node`, `genesis_refusal`:
   `TENANT_ROOT_NOT_ALLOWED`). The mirror (`core_io_refusal`): a core IO runtime runs only as its
   own node, `IO.cloud@motherbee`, `IO.blob@motherbee` or `IO.web@motherbee`, and none of those
   names runs another runtime (`CORE_IO_NAME_MISMATCH`); and only in the root tenant
   (`TENANT_ROOT_REQUIRED`). A tenant that is not active is refused (`TENANT_NOT_ACTIVE`). Start,
   restart and the boot relaunch check the same rules (a refused node that still runs is left
   running).
3. A node for another hive is forwarded to that hive's orchestrator as `SPAWN_NODE`.
4. The runtime comes from `/var/lib/fluxbee/dist/runtimes/manifest.json`; the entry point is
   `/var/lib/fluxbee/dist/runtimes/<runtime>/<version>/bin/start.sh` (`runtime_start_script`).
   A node whose config already exists is refused (`NODE_ALREADY_EXISTS`).
5. The node gets an ILK: `ILK_REGISTER` to `SY.identity@motherbee` with `ilk_type: "agent"` and
   the node's tenant (`ensure_node_identity_registered`).
6. Its config is written to `/var/lib/fluxbee/nodes/<KIND>/<name@hive>/config.json`
   (`node_instance_dir`, `write_json_atomic`): the runtime's config template, the request's
   `config`, `tenant_id`, and a `_system` block.
7. It starts as a transient unit:
   `systemd-run --unit fluxbee-node-<name> --setenv=FLUXBEE_NODE_NAME=... [--setenv=FLUXBEE_NODE_ILK_ID=...] [--setenv=FLUXBEE_NODE_TENANT_ID=...] --collect --property Restart=always --property RestartSec=5 <start.sh>`
   (`build_managed_node_run_command`; `unit_from_node_name` turns characters outside
   `[A-Za-z0-9._-]` into `-`).

`_system` (`build_node_system_block`): `managed_by: "SY.orchestrator"`, `node_name`, `hive_id`,
`relaunch_on_boot`, `config_version`, `created_at_ms`, `updated_at_ms`,
`requested_runtime_version` (default `current`), `launched_by`, and when known `runtime`,
`runtime_version`, `runtime_base`, `package_path`, `ilk_id`, `tenant_id`.

An ILK id is taken and stored in its canonical form, `ilk:<uuid>` lowercase and hyphenated
(`canonical_prefixed_id`), the only form SY.identity and SY.admin take: the request's `ilk_id`, the
one SY.identity answers, the identity map (an older map's other forms are read canonical), and
`_system.ilk_id`.

- **`runtime_version`** is the version the node was last launched on. Every launch writes it
  before it launches the node, under the node's lock: `run_node` in the config it creates; start,
  restart and the reconcile with `persist_node_launch`, in one write with `relaunch_on_boot: true`
  and, for a package on a base runtime that moves to another version, that version's
  `package_path` (where the WF engine reads its definition). Nothing is written when the config
  already says all of it. The node reads the version when it starts (`NODE_HELP`), and status
  reports it (`runtime.resolved_version`; FINDINGS A-101). A launch whose version cannot be
  written does not happen: start and restart answer `CONFIG_WRITE_FAILED`, and the reconcile logs
  "could not record the runtime version in the node's config; not launching it".
- **`relaunch_on_boot`** is `true` at spawn. `kill_node` sets it `false` once the unit is
  stopped (`clear_relaunch_on_boot`); with `purge_instance`, and in `remove_node_instance`, that is
  before the purge's first step, so a purge that stops partway leaves the node stopped (FINDINGS
  A-121). A node with no config has nothing to clear. `start_node` and `restart_node` set it
  `true` before they launch the node, in the same write as the version, so it cannot race a
  conversion the node makes of its config at boot (FINDINGS A-102); a start of a node that already
  runs sets it alone (`set_node_relaunch_on_boot`). A crash does not change it, nor a launch that
  fails: after `START_FAILED` or `RESTART_FAILED` the reconcile tries the node again every pass
  until `kill_node`, as the answer's `hint` says.
- **`launched_by`** is the request's `launched_by` (up to 128 characters of `[A-Za-z0-9._-@]`),
  else `system` (`launched_by_from_payload`). SY.admin always stamps it: `cloud`, `archi`,
  `operator`, or the caller's name. For Cloud's node actions SY.admin also sends
  `require_tenant_id` / `require_launched_by`, and a node that does not match answers
  `NODE_NOT_FOUND`.
- **Relaunch.** At boot and every 60 s (`run_managed_runtime_reconcile_loop`),
  `reconcile_persisted_custom_nodes` takes each persisted node with a runtime and
  `relaunch_on_boot: true`. It holds the node's lock (`try_lock_node`: a node an action holds waits
  for the next pass), and reads under it the node's config again and the runtimes manifest: a pass
  is long, and a copy of the manifest read at its start could move a node back to the version an
  older manifest named. Two passes never run at once: the boot's takes `reconcile_pass`, waiting
  for a loop pass that runs, and the loop skips its tick while the boot's holds it. A node whose
  unit is not active and that the router does not see is launched once what its runtime names is
  ready on this hive; until then it waits, logged once ("node is down, and what its runtime names
  is not ready on this hive; it is launched once it is"). A running node may be moved, below.
- **Stop and removal.** `kill_node` runs `systemctl stop` and `reset-failed` on the unit. With
  `purge_instance`, and in `remove_node_instance` (refused while the node runs), it also removes
  the instance directory and sends one `teardown_node {node_name, ilk_id}` to
  `SY.admin@motherbee` (`teardown_node_via_admin`, 120 s, since each of SY.admin's store calls can
  take 30 s; [control-path hardening §2](../control-path-hardening-v1.md)). SY.admin checks that
  the node is on the caller's hive and that the ILK is the node's, deletes the secrets dedicated to
  the ILK, then marks and purges the ILK. The answer reports each step in `ilk_deleted`,
  `ilk_delete` (with `ilk_delete_error` when that step is not `ok`) and `vault_secrets_purged`
  (`teardown_node_report`). An error answer never reads `ok`: a refusal before any step
  (`NODE_ILK_MISMATCH`, `UNAUTHORIZED`, `INVALID_REQUEST`, an older SY.admin), or a request that
  never reached SY.admin (`TEARDOWN_NOT_DELIVERED`), makes both steps errors with its code;
  `TEARDOWN_INCOMPLETE` names the step that failed; `not_found` (the ILK was already gone) is an
  `ok` ILK step with nothing deleted. With no answer (a timeout above all) the outcome is unknown,
  never "nothing deleted": both steps read `unknown` with `TEARDOWN_UNKNOWN`, `ilk_deleted` and the
  counts are null, and the error names the ILK to check (`teardown_node_unknown`). The motherbee
  waits 150 s for a spoke's purging `KILL_NODE` or `REMOVE_NODE_INSTANCE`
  (`TEARDOWN_FORWARD_TIMEOUT_SECS`). The node's identity-map entry is already gone by then, so a
  retry of the purge does not find the ILK again: the answer's `ilk_delete.ilk_id` is what to clean
  up by hand.

### A running node and a new runtime version

A running node moves to what its runtime names now, on the reconcile's next pass
(`reconcile_step`, `running_node_move`; FINDINGS A-39, A-109). What its runtime names
(`launch_target`) is a version and the start script that version runs:

- the version is the pointer's for a node that follows `current`
  (`requested_runtime_version: current`), and its pin otherwise;
- the start script is the version's own `bin/start.sh`, or, for a package on a base runtime
  (`config_only`, `workflow`), its base's `current` one. A package's pin is its own version: its
  base always follows `current`, and nothing pins it.

What a unit runs is read from systemd, not from the config (`live_unit`): its ExecStart
(`systemctl show -p ExecStart`), and the version its launch wrote in its environment,
`FLUXBEE_NODE_RUNTIME_VERSION` (`build_managed_node_run_command`, since 0.1.88). A full runtime's
ExecStart names its version, since the version is in the start script's path; a package's
ExecStart is its base's start script, the same for every version of the package, so only that
variable shows a package's version.

The move:

- **When:** at the orchestrator's boot and every 60 s, for a node whose unit is active:
  - its ExecStart does not run the target's start script, compared as
    `managed_node_restart_command` does. That is a package whose base's `current` moved, pinned
    or not (every WF node on `wf.engine`, Archi's config-only AI packages on `ai.generic`), or a
    full runtime on another version than its runtime names: a new `current` it follows, or a
    `set_node_config` that rebinds a running full-runtime node, which this pass relaunches within
    a minute without a restart;
  - or its unit carries another version than the target: a package whose own version changed, a
    `set_node_config` rebind of a package node included, for a unit launched since 0.1.88;
  - or, when neither says what version it runs (a package unit launched before 0.1.88, or no
    ExecStart), it follows `current` and the pointer names another version than its
    `_system.runtime_version`. A package node launched before 0.1.88 and rebound with
    `set_node_config` is not seen until its next launch.

  A `.deb` upgrade, a core update (this hive's orchestrator restarts only when its own binary
  changed), or a runtime published with `--set-current` reaches every running node within a
  minute of the new version reaching its hive.
- **A config that only lags is written, not relaunched:** a unit that runs the target while
  `_system.runtime_version` names another version (a launch before 0.1.88 did not write it, FINDINGS
  A-101) gets the version written under its lock, and keeps running: "running node runs the
  version its runtime names, but its config named another; recorded it, the node keeps running".
  Status reports the version at once; the node reads it for `NODE_HELP` at its next start.
- **Only when that is ready on this hive** (`runtime_version_not_ready`), the readiness every
  launch checks: run, start and restart refuse with `RUNTIME_NOT_PRESENT`, which SY.wf-rules
  retries, before they stop or start anything (`runtime_readiness_refusal`). Ready is:
  - what the runtime listing reports as the version's `readiness` (`runtime_present`,
    `base_runtime_ready`, `start_sh_executable`);
  - for a package on a base runtime, the files a start requires (`package.json`, and a workflow's
    `flow/definition.json`);
  - the binary the start script runs (`start_script_entry`). Every publish script
    (`scripts/publish-runtime.sh` and the `publish-*-runtime.sh` built on it) writes the line
    `exec "$SCRIPT_DIR/<binary>" "$@"`, and `start_script_entry` reads the first such line;
    `publish-runtime.sh` refuses a `--start-script` whose first such line does not run the binary
    it publishes. A start script in a version without a `package.json` must have it: one that does
    not is not ready, and the reason says the binary is not known. A package's own start script
    (its version has the `package.json` every package carries) is the package's business.

  A spoke gets the manifest and the files through Syncthing, in any order. Until then the node
  stays as it is, and the orchestrator logs once per target "running node is not on what its
  runtime names now, and that is not ready on this hive; it stays as it is until it is", with the
  reason.
- **How:** the checks of any launch come first (the root-tenant rules, the tenant gate, the entry
  point and its preflight); a node they refuse keeps running. Then `persist_node_launch` writes
  the version (with its `package_path`, for a package that moves to another version), and the unit
  is stopped and created again at the new start script, carrying the new version. The journal
  has:
  - for a new version in the config, "running node follows 'current' and the pointer moved;
    relaunching it on the new version" and "runtime 'current' pointer moved; rebinding node to the
    new version";
  - otherwise, "running node's unit runs another start script or version than its runtime names
    now; relaunching it on what its runtime names";
  - then "persisted custom node relaunched during reconcile".
- **Once per move:** the next pass finds the unit running the target's start script and version,
  and the config naming it, and leaves the node.
- **Never:** a node that runs what its runtime names. A pinned node keeps its pin: the pointer does
  not move it, but a new pin set with `set_node_config` does, as above.

`restart_node` moves a node at once. It restarts the unit in place only when it runs the planned
start script and carries the planned version; otherwise it creates the unit again.

## Hives: add_hive and remove_hive

Both are motherbee-only ("add_hive/remove_hive are motherbee-only").

`add_hive` (`accept_add_hive`) checks the request, writes
`/var/lib/fluxbee/hives/<hive>/info.yaml` with status `joining`, and answers `accepted` with a
`join_id` and `poll: GET /hives/<hive>`. The join runs in the background (`run_background_join`).
A join already running answers `JOIN_IN_PROGRESS`; a connected hive, `HIVE_EXISTS`. `role` is
`worker` (default), `egress` or `ingress`; `ssh_access` is refused ("SSH exists only inside
add_hive and is always revoked at its end (D32)").

The worker join (`add_hive_flow`), in order:

1. If the spoke's orchestrator is already on the mesh, finish over the mesh without SSH.
2. Issue the hive's mesh TLS leaf (`issue_hive_tls`), before any SSH.
3. SSH in with `/var/lib/fluxbee/ssh/motherbee.key`, seeding it with the request's `ssh_key` or
   `ssh_password` if needed; wait for the box to settle (cloud-init, apt jobs; up to 900 s,
   `wait_for_host_to_settle`; `HOST_NOT_SETTLED` is retryable); install a temporary sudoers
   grant (`/etc/sudoers.d/fluxbee-orchestrator`).
4. Copy the role's core binaries and manifest, and the vendored Syncthing
   (`sync_core_to_worker`, `sync_vendor_to_worker`); write the spoke's hive.yaml with the
   motherbee as WAN uplink; push the TLS material (`push_hive_tls`) and, for a worker, the
   identity sync key.
5. Write and start the units, wait for the spoke's orchestrator on the mesh, send
   `ADD_HIVE_FINALIZE`, link Syncthing, apply `restrict_ssh` / `harden_ssh` if asked.
6. Mark the hive `connected`, then revoke the SSH access: remove the sudoers file and the
   motherbee key from `authorized_keys` (`revoke_bootstrap_ssh_access`). A failed join revokes
   too (`best_effort_revoke_bootstrap`), except after a transient failure, where the key stays so
   a retry can enter.

The unit test `ssh_happens_only_inside_add_hive` fails the build if SSH is used anywhere else in
the file (D32, FINDINGS A-54). Egress and ingress joins have their own flows
(`add_egress_hive_flow`, `add_ingress_hive_flow`). The egress join reads the box twice
(`egress_host_report_command`, FINDINGS A-1). Before it installs anything it takes `edge_ip`
from it (`egress_edge_ip_on_host`): an omitted one is the single address `lan_iface` carries
inside `lan_cidr`, an explicit one must be on `lan_iface`; none or several addresses, or a missing
interface, fail the join with `EGRESS_LAN_LEG_INVALID` (`EGRESS_LAN_LEG_NOT_VERIFIED` when the
box cannot be read). Once the egress' orchestrator is on the mesh, before the SSH controls, it
reads that the `inet fluxbee_egress` table is loaded, forwarding is on, IPv6 is disabled and the
LAN leg is in place, and reports what it read (`egress_nat_applied`, `egress_ipv4_forwarding`,
`egress_ipv6_blocked`, `egress_lan_leg_ok`); any of them missing fails the join with
`EGRESS_NAT_NOT_READY` (`EGRESS_NAT_NOT_VERIFIED` when the box cannot be read).

**SSH on the DMZ hosts** ([control-path hardening §6](../control-path-hardening-v1.md)). An
ingress join takes `ssh_allow_from` (an array of IP addresses or networks, ingress only;
`resolve_add_hive_ssh_allow_from`; any other role refuses it with `INVALID_REQUEST`) and writes it
into the ingress' hive.yaml as `ingress.ssh_allow_from`. Before it installs anything (once the box
has settled, before the sudoers grant), it reads the motherbee's source address over the
bootstrap session (`SSH_CONNECTION_COMMAND`, outside sudo) and checks it against the allow set the
ingress will apply: the uplink address the join writes, plus `ssh_allow_from`
(`ingress_join_ssh_source_refusal`). An address outside it fails the join with
`SSH_SOURCE_NOT_ALLOWED`, naming the address to add through `ssh_allow_from`
(`SSH_SOURCE_NOT_VERIFIED` when it cannot be read), so the join's later SSH steps (hardening, key
revoke) always come from an allowed address. The answer lists the set as `ingress_ssh_allow`. An
egress join takes `egress.ssh_from_wan` (default true) and writes it into the egress' hive.yaml;
with `false` it refuses, with `SSH_FROM_WAN_LOCKOUT`, a join whose packets come in on `wan_iface`:
the interface the box routes the session's client through, as `ip -o route get <client>` names it
(`EGRESS_SSH_ARRIVAL_COMMAND`, `egress_join_ssh_from_wan_refusal`). It answers
`egress_ssh_from_wan`.

The URL the egress' internet probe GETs is the request's `egress.reachability_url`, else the
motherbee's own `egress.reachability_url`. It must be an `https://` URL with a host
(`validate_reachability_url`, checked when the join is accepted). The join writes it into the
egress' hive.yaml (`render_egress_hive_egress_yaml`) and answers it as `egress_reachability_url`.
With neither, the egress gets none and does not probe. A worker join copies the motherbee's
`reachability_url` into the worker's `egress` section along with `gateway_ip`
(`render_worker_egress_yaml`).

A successful join answers `warnings`. A join that logged in with `ssh_password` (its key-first
probe failed and the password seeded the motherbee key) and did not harden SSH says there that
password logins stay enabled on the box, and how to close them (`add_hive_ssh_warnings`, FINDINGS
A-2): removing the motherbee key does not.

`remove_hive` (`remove_hive_flow`) refuses the local hive, sends `REMOVE_HIVE_CLEANUP` to the
spoke's orchestrator (8 s), unlinks the spoke from Syncthing and deletes
`/var/lib/fluxbee/hives/<hive>/`. The spoke answers once systemd has accepted the cleanup, which
runs one second later (timer accuracy 100 ms) in a transient unit of its own
(`remove_hive_cleanup_command`, `systemd-run` like the orchestrator's self-restart), so stopping
sy-orchestrator does not cut it short: it deletes the spoke's TLS material and identity key first,
then stops its core services, Syncthing and the managed nodes, and clears the node state. An
ingress, by its own role, then drops its SSH table and `fluxbee-ingress-nft.service`
(`ingress_teardown_script`), once its orchestrator is stopped and cannot load them again. A spoke
takes `REMOVE_HIVE_CLEANUP` only from `SY.orchestrator@motherbee`, as its router stamps the
sender. There is no SSH fallback: an offline spoke is cleaned up locally only, and the answer's
`warning` says what stays on a removed egress or ingress the cleanup did not reach.
`ADD_HIVE_FINALIZE` and `REMOVE_HIVE_CLEANUP` are refused on the motherbee
(`spoke_only_action_refusal`).

The registry is the authority on which hives exist (FINDINGS A-48). With the hive's directory,
`remove_hive` deletes its identity HMAC key (`identity_key_cleanup`), so it can never authenticate
to :9100 again; a deleted key ends no session already open, so when the cleanup did not reach the
box it restarts the motherbee's `sy-identity` (`identity_restart`). At every boot the motherbee
deletes the key of any hive with no directory under `hives/`, when both listings read completely
(`sweep_orphan_identity_keys`).

## Updates

SY.admin's `update` sends `SYSTEM_UPDATE` to `SY.orchestrator@<hive>` with a `category`
(`runtime`, `core` or `vendor`) and the `manifest_hash` the hive should have
(`handle_system_update_message`). If the local manifest does not match yet, the answer is
`sync_pending` (Syncthing has not delivered it).

- **runtime**: verifies the runtime artifacts (missing ones also give `sync_pending`) and
  applies retention.
- **vendor**: brings the running Syncthing in line with the vendored one
  (`ensure_blob_sync_runtime`), when sync is enabled.
- **core** (`apply_core_update_replying_first`): backs up each binary to replace under
  `/var/lib/fluxbee/orchestrator/core-bin.prev.local/update-<ms>/` with a `generation.json`,
  swaps the binaries, rewrites the units, and **answers first** (`phase: restarting`). Two
  seconds later it restarts `rt-gateway` and the system nodes, each behind a 30 s health gate
  (`restart_local_core_services_with_health_gate`), skipping itself. A failed gate restores the
  old binaries (`status: rollback`). If its own binary changed it schedules its own restart
  (`schedule_orchestrator_self_restart`, `systemd-run --on-active=2s`, accurate to 100 ms). It
  keeps 3 generations (`CORE_BACKUP_GENERATIONS_KEPT`) and records the outcome in
  `/var/lib/fluxbee/orchestrator/core-update-last.json` and `core-installed.json`.
- **core_rollback** restores a stamped generation (`core_rollback_local`).

`SYSTEM_SYNC_HINT` (SY.admin's `sync_hint`) asks Syncthing to rescan a folder (`blob` or `dist`)
and waits until it is idle: default 30 s, at most 300 s.

## Syncthing

The orchestrator runs Syncthing as `fluxbee-syncthing.service` (`syncthing_unit_contents`):
user `fluxbee`, `UMask=0027`, `/var/lib/fluxbee/vendor/bin/syncthing` with the GUI and API on
`127.0.0.1:<api_port>` (default 8384) and its home in `/var/lib/fluxbee/syncthing`. Every link
is TCP to the motherbee's static address, so only the motherbee listens off loopback, on
22000/tcp; a spoke only dials (`syncthing_owned_listen_address`).

| Folder | Holds | Who gets it |
| --- | --- | --- |
| `fluxbee-dist-core-<role>` | the core binaries for one role | each spoke its own role's (`folder_forbidden_for_role`) |
| `fluxbee-dist-vendor` | Syncthing | every spoke |
| `fluxbee-dist-policy` | the compiled user OPA policy | every spoke |
| `fluxbee-dist-runtimes` | runtime packages | workers only |
| `fluxbee-dist-agent-assets` | `blob/agent-assets`: SY.architect's agent assets, which ai.generic reads from disk | workers only |
| `fluxbee-blob` | `blob/active` | workers only, when blob sync is on (`peer_shares_active_blob`) |
| `fluxbee-blob-public` | `blob/public` | the ingress only (`peer_shares_public_blob`) |

Dist folders and `fluxbee-blob-public` are `sendonly` on the motherbee and `receiveonly` on the
spokes (`dist_syncthing_folder_type`, `blob_public_syncthing_folder_type`), so a spoke never
writes them. `fluxbee-blob` is `sendreceive`.

A folder added by a later release reaches the hives that joined before it at their next local
reconcile, without a re-join (`reconcile_syncthing_folders_xml`): `fluxbee-dist-policy` is shared
with every member of `fluxbee-dist-vendor`, and `fluxbee-dist-agent-assets` with every member of
`fluxbee-dist-runtimes`. The agent-assets tree is owned like a dist tree (`ensure_dirs`): root's on
the motherbee, readable by the `fluxbee` group, and the `fluxbee` user's on a worker, where
Syncthing writes what it receives (FINDINGS A-72). `blob/` itself is no folder, so a file written
there and renamed into `blob/agent-assets` is never sent half-written.

## Host posture

What the code does today:

- **SSH on the ingress** (`reconcile_ingress_ssh`, [control-path hardening
  §6](../control-path-hardening-v1.md)): a `table inet fluxbee_ingress` whose input chain accepts
  SSH, on the ports `sshd -T` reports (`port` lines and each `listenaddress`'s port), from loopback
  and from the allow set only, and drops all other SSH, IPv4 and IPv6; nothing else is filtered
  (`ingress_nft_ruleset`). The allow set is the motherbee's address from the hive's own
  `wan.uplinks` (a hostname is resolved, IPv4 and IPv6) plus `ingress.ssh_allow_from`,
  deduplicated and without an entry another one covers (`ssh_allow_set`). It is reconciled at
  boot and about every 60 s (at once when a loaded table vanished), like the egress table: add +
  flush + define in one `nft -f`, saved in `/etc/nftables.d/fluxbee-ingress.nft` once nft has
  loaded it, and loaded at boot by `fluxbee-ingress-nft.service` (`apply_nft_table`,
  `ensure_nft_boot_unit`). The sources are inline, never a named set: `flush table` keeps a named
  set's elements. Only a complete input set renders a new table (`ingress_ssh_action`), and an
  unchanged one is neither loaded again nor logged while its table is there. When `sshd -T` fails
  only because `/run/sshd` is missing (at boot before ssh.service is up, or on a socket-activated
  host before its first connection), the orchestrator creates it as ssh.service would (root, 0755)
  and asks once more (`sshd_listen_ports_with`). An input that fails (hive.yaml, `sshd -T`, an
  uplink that does not resolve), an invalid `ssh_allow_from` entry or an empty allow set never
  narrows or loosens the table: the rules in
  force stay, the saved ruleset is loaded again if the table is gone, and a `posture` alert is
  recorded (`ingress_ssh_input_failed`, a warning; `ingress_ssh_rules_failed`;
  `ingress_ssh_allow_set_empty`); a failed load records `ingress_ssh_rules_failed`. Recovery: the
  hypervisor console, or `ingress.ssh_allow_from` in the ingress' hive.yaml and an orchestrator
  restart.
- **SSH on the egress' WAN**: with `egress.ssh_from_wan: false` (default true, D31) the
  `fluxbee_egress` table's input chain drops SSH, on the same ports, arriving on `wan_iface`
  (`egress_nft_ruleset`). When `sshd -T` cannot say, it keeps the ports the saved ruleset drops
  (`egress_wan_ssh_ports`); with none saved it drops port 22 and records `egress_ssh_ports_unknown`.
- **Host firewalls** (`report_active_host_firewalls`): the orchestrator writes no rule to ufw or
  firewalld. At boot and about once a minute it records a `posture` alert `host_firewall_active`
  when `ufw status` says `Status: active` or the `firewalld` unit runs, since either may close
  9000, 9100, 9200, 22000 or 443 without anyone seeing it.
- **Loopback check** (`watchdog_listener_check`): about once a minute it runs `ss -H -tlnp` and,
  if `sy-admin` or `sy-architect` listens off loopback, logs a warning and records a `posture`
  drift alert. It changes nothing.
- **Egress** (`reconcile_egress`): on the egress role an nftables table `inet fluxbee_egress`
  (forward only LAN→WAN, IPv6 dropped, masquerade; input accepted), sysctls, and
  `fluxbee-egress-nft.service` to load it at boot. nftables matches interfaces by name, so the
  table loads even when one is missing. After applying, it reads back what it set and checks the
  LAN leg (`serving_problems`, FINDINGS A-1): forwarding on, IPv6 disabled, `wan_iface` and
  `lan_iface` present, `edge_ip` (always in its hive.yaml, never guessed) on `lan_iface`. Any of
  them missing fails the boot with what is missing; after boot the watchdog logs each on every
  drift tick and records an `egress` drift alert (`lan_leg_missing`, `ipv4_forwarding_off`,
  `ipv6_not_blocked`). A worker gets its default route via the egress `gateway_ip`. The motherbee
  is only checked (`EGRESS_MOTHERBEE_BYPASS`).
- **Internet probe** (`check_internet_reachable`): an HTTPS GET (curl, 5 s) of the hive's own
  `egress.reachability_url`, on the egress at boot and every drift tick, and at boot on a worker
  that routes through it and on the motherbee when they declare `gateway_ip`. A failure is a
  warning in the journal. Without the URL the probe does not run and logs
  `internet_reachable=not_configured`, as it does after warning about a URL that is not a valid
  `https://` one: the code names no target of its own. A host without `curl` does not probe
  either and logs `not_probed` (`run_internet_probe`), never a `false` it did not measure.

## Watchdog

Every 5 s (`watchdog_tick`) the orchestrator checks `rt-gateway` and every service of its
`system_nodes` list with `systemctl is-active`, and runs `systemctl start` on any that is not
active. A stopped core unit comes back within seconds. The same tick logs managed nodes that left
the router, verifies runtimes, runs blob GC when enabled, checks Syncthing and its peers, re-applies
egress and the ingress' SSH table, reports an active host firewall (`watchdog_host_rules`), and runs
the loopback check.

To keep a core unit stopped, for a test or a repair, mask it for this boot and stop it:
`systemctl mask --runtime <unit>` (the lab notes in `lab/DEPLOYMENTS.md` and
`lab/logbook/2026-10-16.md` do this). The watchdog has no mask check: its `systemctl start` fails
and it logs `service restart failed` every 5 s until `systemctl unmask`.

## Messages

**Admin commands** (`msg_type: admin`, sent by SY.admin), handled by `handle_admin`: hive_status,
get_storage, set_storage, list_nodes, list_tenant_nodes, list_versions, get_versions,
list_runtimes, get_runtime, run_node, start_node, restart_node, kill_node, remove_node_instance,
get_node_config, set_node_config, get_node_state, get_node_status, node_help, list_deployments,
get_deployments, list_drift_alerts, get_drift_alerts, record_drift_alert, list_hives, get_hive,
add_hive, remove_hive, core_rollback. Anything else answers `NOT_IMPLEMENTED`. Every admin
message, reads included, is accepted only from `SY.admin@<this hive>` as the router stamps the
sender (`is_allowed_admin_source_name`, checked first in `run_admin_worker`), else `FORBIDDEN` and
a warning in the log ([control-path hardening §3](../control-path-hardening-v1.md)). SY.admin
addresses only its own hive's orchestrator, and a spoke runs no SY.admin, so a spoke's
orchestrator takes no admin message at all.

**`record_drift_alert`** is how SY.admin's certificate conditions reach the drift store, which
only the orchestrator writes ([ACME design §7](../acme-edge-certificates-v1.md)). Past the inbox
check above, its flow takes it only from `SY.admin@motherbee` as the router stamps the sender
(`certificate_condition_from_request`), else `FORBIDDEN`. The payload is
`{category, kind, severity, hive, detail, state}`:

- `category` is `certificate`, and `kind` one of `renewal_failed`, `renewal_overdue`, `expiring`,
  `name_not_covered`, `foreign_certificate`, `install_refused`, `edge_without_certificate`,
  `edge_unreachable` (`CERTIFICATE_DRIFT_ALERT_KINDS`);
- `severity` is `info`, `warning` or `critical`; `state` is `open` or `closed`; `hive` is a hive
  id; `detail` is not empty.

Anything else answers `INVALID_REQUEST`. The row carries `state`, and a `closed` row is written
with severity `info`. It goes through the store's dedupe (`drift_alerts_with`): a condition's row
is not written when the newest row of the same hive, category and kind, from the last hour, has
the same state. Open → closed → open within the hour writes all three rows; a second open right
after an open is not written. The detectors' rows keep their rule: any row of the same alert from
the last hour keeps a new one out. The answer is `{status: "ok", recorded}`, with
`recorded: false` for such a repeat; a failed write answers `DRIFT_ALERT_WRITE_FAILED`.

**System messages** (`handle_system_message`), mostly between orchestrators: `SYSTEM_UPDATE`,
`SYSTEM_CORE_ROLLBACK`, `SYSTEM_SYNC_HINT`, `SPAWN_NODE`, `KILL_NODE`, `START_NODE`,
`RESTART_NODE`, `REMOVE_NODE_INSTANCE`, `NODE_CONFIG_SET`, `NODE_CONFIG_GET`, `NODE_STATE_GET`,
`NODE_STATUS_GET`, `NODE_HELP`, `GET_VERSIONS`, `GET_RUNTIMES`, `GET_RUNTIME`, `LIST_NODES`,
`INVENTORY_REQUEST`, `ADD_HIVE_FINALIZE`, `REMOVE_HIVE_CLEANUP`. `RUNTIME_UPDATE` is refused.
The router delivers these protected actions only from allowed origins (`policy/system.rego`:
`SY.orchestrator@motherbee` for the node and handshake actions, and also this hive's own
orchestrator for the reads, rule (3); `SY.admin@motherbee` for
`SYSTEM_UPDATE` and `SYSTEM_SYNC_HINT`, rule (2)).

**`node_help` and `NODE_HELP`** (`node_help_flow`): a node's self-description
([agent-tools-v1](../agent-tools-v1.md) §3.3), asked the way its status is.

- **Its own task:** the admin and system workers take one message at a time, so the flow runs in
  a task of its own that sends the answer itself (`spawn_node_help`): an admin answer to SY.admin,
  `NODE_HELP_RESPONSE` to an orchestrator. Inline, the node's probe (3 s) or another hive's forward
  (7 s) held every request behind it, and SY.admin's 5 s reads (`hive_status`, `web_status`)
  answered 504. add_hive's join leaves the admin worker the same way (PB-7).
- **A node of another hive:** the motherbee forwards `NODE_HELP` to that hive's orchestrator and
  waits 7 s (`NODE_HELP_FORWARD_TIMEOUT_SECS`); no answer is `NODE_HELP_FAILED`.
- **A node of this hive:** whether it is here is decided as for `get_node_status`
  (`local_node_view`), else `NODE_NOT_FOUND`. Only a node that `ai.generic` runs is asked
  (`NODE_HELP_RUNTIMES`): its config.json's `_system.runtime_base`, else `_system.runtime`. Any
  other node, a core node included, is answered at once with no help (`node_help_unasked_reason`).
  A connected `ai.generic` node gets `NODE_HELP` and 3 s to answer (`NODE_HELP_PROBE_TIMEOUT_SECS`).
- **The answer:** `{status: "ok", node_name, help, reason}`. `help` is the node's
  `NODE_HELP_RESPONSE`, or null with the reason: what runs the node does not answer `NODE_HELP`, it
  is not connected, it did not answer within 3 s, or it answered with an error. None of these is
  an error, and a silence says only that: a busy AI node can miss the 3 s too.

## Config

`/etc/fluxbee/hive.yaml` (`HiveFile`): `hive_id`, `role`, `wan` (`gateway_name`, default
`RT.gateway`; `listen`; `uplinks`), `storage.path`, `blob` (`enabled`, default
true; `path`, default `/var/lib/fluxbee/blob`; `sync`, default off, tool `syncthing`, `api_port`
8384, `data_dir`; `gc`), `dist` (`path`, default `/var/lib/fluxbee/dist`; sync on by default),
`identity.sync.port` (default 9100), `government`, `system_nodes`, `egress` (with
`reachability_url`, the internet probe's target, no default; and `ssh_from_wan`, default true),
`ingress.ssh_allow_from` (the ingress' extra SSH sources), `ai`.

Environment: `JSR_LOG_LEVEL`; `ORCH_DEFAULT_TENANT_ID` (tenant for a spawn that names none);
`JSR_ORCH_SYSTEM_FORWARD_TIMEOUT_SECS` and `JSR_ORCH_CORE_ROLLBACK_FORWARD_TIMEOUT_SECS`
(forward timeouts); `FLUXBEE_ADD_HIVE_REQUIRE_DIST_SYNC` / `JSR_ADD_HIVE_REQUIRE_DIST_SYNC` and
`FLUXBEE_ADD_HIVE_DIST_SYNC_TIMEOUT_SECS` / `JSR_ADD_HIVE_DIST_SYNC_TIMEOUT_SECS` (join checks).

## State on disk

| Path | What |
| --- | --- |
| `/var/lib/fluxbee/hives/<hive>/info.yaml` | the hive registry (motherbee): role, address, status, join phase, Syncthing device |
| `/var/lib/fluxbee/nodes/<KIND>/<name@hive>/config.json` | managed node config with `_system` |
| `/var/lib/fluxbee/state/orchestrator/identity-node-ilk-map.json` | node → ILK and tenant, used on relaunch |
| `/var/lib/fluxbee/orchestrator/` | `core-bin.prev.local/`, `core-installed.json`, `core-update-last.json`, `deployments-history.jsonl`, `drift-alerts.jsonl`, `syncthing-posture.json` |
| `/var/lib/fluxbee/dist/` | core, vendor, runtimes and policy trees |
| `/var/lib/fluxbee/tls/` | mesh CA (motherbee) and the hive's TLS leaf |
| `/var/lib/fluxbee/ssh/motherbee.key` | the join key (motherbee) |
| `/etc/nftables.d/fluxbee-egress.nft`, `/etc/nftables.d/fluxbee-ingress.nft` | the egress' and the ingress' tables, loaded at boot by `fluxbee-egress-nft.service` and `fluxbee-ingress-nft.service` |
| `/var/lib/fluxbee/syncthing/` | the Syncthing home |

## Failure behavior

- **No router.** `wait_for_router_ready` gives up after 30 s and `main` exits with an error;
  systemd restarts it 5 s later.
- **No database (motherbee).** If `sy-storage` is not active, or its database does not answer
  within 30 s, `finish_bootstrap` fails and the process exits; until then admin callers get
  `ORCHESTRATOR_BOOTING`.
- **A system node fails to start.** Logged; the watchdog keeps trying every 5 s.
- **SIGTERM or SIGINT.** It exits at once and leaves every service running ("managed services
  keep running"); the next boot takes them over.
- **A core update fails its health gate.** The old binaries come back (`status: rollback`).

## Health

- `systemctl status sy-orchestrator`; `journalctl -u sy-orchestrator` shows `hive ready` when the
  boot ended.
- `curl -s http://127.0.0.1:8080/hive/status` on the motherbee: `hive_id`, `pid`, `uptime_ms`
  of the motherbee's orchestrator.
- `GET /hives`, `GET /hives/{hive}` (join status), `GET /versions` (core, runtime and vendor per
  hive, with the last core update), `GET /drift-alerts` on SY.admin.

## Links

- [SY.admin](sy-admin.md): sends every admin command and the updates.
- [SY.edge](sy-edge.md): the ingress role's door.
- [SY.vault](sy-vault.md): why it starts last.
- [The router](rt-gateway.md), [SY.identity](sy-identity.md),
  [SY.config.routes](sy-config-routes.md): what it starts first.
- [Operations](../reference/operations.md) and [07-operaciones](../history/07-operaciones.md): install,
  `add_hive` fields, upgrades.
- [Host posture spec](../host-posture-and-exposure-spec-v1.md): the posture stages.
- [Software distribution spec](../history/software-distribution-spec.md) and
  [runtime lifecycle spec](../history/runtime-lifecycle-spec.md): dist and runtimes.
- `lab/logbook/FINDINGS.md`: A-39, A-45, A-46, A-54; D32 in the join comments.
