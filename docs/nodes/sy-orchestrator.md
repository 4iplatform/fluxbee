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

1. Refuses any argument and exits 2: systemd starts it with none, and one started by hand
   (`--version`, say) would otherwise run a second boot on the host (FINDINGS A-180). Then it
   takes `/var/run/fluxbee/sy-orchestrator.lock` (`json_router::instance_lock`, an flock until the
   process ends, the holder's PID inside): a second orchestrator exits there, naming the holder's
   PID; any other failure to lock is warned and it runs as before.
2. Reads `/etc/fluxbee/hive.yaml`; a missing or invalid file ends the process with an error. Its
   `ai` section is read untyped, so no `ai` ends the start. Its role's SY nodes and the blob's and
   dist's topology are the release's, in the code ([System nodes per role](#system-nodes-per-role)):
   a hive.yaml that still holds those keys loads, and one warning at the start names the ones it
   ignores (`warn_superseded_hive_keys`). Creates its directories (`ensure_dirs`), `dist/installation`
   among them. On the motherbee, writes the installation file's first version when its dist holds
   none, and records the values of the version its file holds when its record has none
   (`record_current_version_values_at_start`; [The installation file](#the-installation-file)).
   On every hive, logs what it holds at the installation file's place, and, once it holds a file,
   warns once that hive.yaml's `ai` is ignored (`report_installation_file_at_start`). Then it
   writes the PID file.
3. **`bootstrap_local`**, before joining the router:
   1. reads `dist/core/manifest.json` (missing or invalid, it is fatal) and lists the core
      components of its role (`core_components_listed_for_role`): on the motherbee every
      component of the manifest; on a spoke `rt-gateway`, the role's SY nodes and
      `sy-orchestrator`, a component the manifest does not name included;
   2. on a spoke, rewrites the core unit files if their template changed (`regen_local_core_units`);
   3. on the motherbee, makes sure the SSH join key and the mesh CA and TLS exist
      (`ensure_motherbee_ssh_key`, `ensure_motherbee_mesh_tls`), refreshes the per-role core
      trees spokes sync from (`materialize_role_core_trees`: it checks each binary against the
      manifest before it copies any, and leaves a role's tree as it was when one does not match),
      clears interrupted joins, and deletes the identity keys of hives the registry does not have
      (`sweep_orphan_identity_keys`);
   4. starts `rt-gateway` and waits up to 30 s for it and its SHM (`wait_for_router_ready`; fatal);
   5. reports an active ufw or firewalld (`report_active_host_firewalls`), applies the ingress'
      SSH table on the ingress role (`reconcile_ingress_ssh`, never fatal), and reconciles egress
      (fatal only on the egress role);
   6. starts Syncthing if blob or dist sync is enabled (a failure only warns). While
      `dist/vendor` does not hold the Syncthing its manifest describes, it runs the Syncthing
      already installed and reports the vendor in `vendor.awaiting_update`; with none installed,
      Syncthing does not start, as before (FINDINGS A-189, [Syncthing](#syncthing));
   7. checks each core binary of its role in `dist/core/bin` against the manifest, its size and
      sha256 (`check_core_dist_at_start`, FINDINGS A-186). One that is missing, of another size
      or hash, or not in the manifest stops nothing: each is a warning in the journal and an
      entry of `core.awaiting_update` in `/versions` ([Updates](#updates)), and the start goes on
      with what `/usr/bin` holds. The check comes after Syncthing, the only thing that brings
      what dist lacks: it used to be the start's first step, and fatal, so a spoke restarted
      while a release was syncing never started again;
   8. on a spoke, finishes the core update that installed it, if one did and it is not finished
      (`settle_core_update`, [Updates](#updates)): a node its role's list dropped is stopped and
      removed, a node the list added is installed;
   9. starts each SY node of its role in list order (`start_core_service`), using
      `restart` instead of `start` for a service whose binary a package upgrade replaced (its
      `/proc/<pid>/exe` link ends in "(deleted)", `exe_link_is_replaced`, FINDINGS A-45); a
      failure only warns;
   10. waits up to 60 s for those nodes to appear in the router (`wait_for_lifecycle_nodes`); a
       timeout only warns.

   `main` then checks again every minute each report that is open, the core's or the vendor's,
   until its tree matches (`run_dist_recheck_loop`); when the core's closes, it finishes a core
   update the start could not (a node whose binary was still on its way).
4. Joins the router as `SY.orchestrator` (`RouterDispatcher::connect_with_retry`, every 1 s).
5. Starts the admin worker and the system worker (`run_admin_worker`, `run_system_worker`). Each
   handles its messages one at a time, in arrival order.
6. Starts the managed-node reconcile loop (every 60 s) and, on the motherbee, the loop that takes
   Syncthing off public infrastructure once every spoke reports a reachable motherbee
   (`run_syncthing_options_switch_loop`).
7. **`finish_bootstrap`**: on the motherbee, waits for `sy-storage` to be active (30 s) and for its
   database to answer `STORAGE_METRICS_GET` over the mesh (30 s, `wait_for_storage_db_ready`);
   then, on every role, runs the managed-node reconcile once (`reconcile_persisted_custom_nodes`,
   the pass the 60 s loop repeats, [Managed nodes](#managed-nodes)) and hands the blob folders
   over.
8. Sets `booted`, logs `hive ready`, and starts the watchdog (every 5 s).

### ORCHESTRATOR_BOOTING

From step 4 to step 7 the orchestrator is on its router but not ready. `run_admin_worker` answers
every admin command from its own hive's SY.admin with `ORCHESTRATOR_BOOTING`, "the orchestrator is
still booting; retry shortly", until `booted` is set (any other caller gets `FORBIDDEN` first, see
[Messages](#messages)). System messages are handled during that time. SY.admin passes the code
through to its caller (over HTTP it arrives as a 500).

## System nodes per role

The release defines them, in the code: `json_router::release_settings` (`src/release_settings.rs`,
docs/hive-config-update-v1.md §3), one entry per role with its SY nodes in start order, the ones
the start waits for (`wait_for`), and the blob's and dist's topology. `system_nodes_for_role`
reads it. A list and its binaries arrive together: the list is in the orchestrator, which ships in
the same core as the binaries. A spoke's hive.yaml used to carry the lists, copied from the
motherbee's at its join and never written again, so a release's change to them reached no hive
that had joined before.

| Role | SY nodes, in start order | `wait_for` |
| --- | --- | --- |
| motherbee | SY.config.routes, SY.identity, SY.opa.rules, SY.admin, SY.architect, SY.storage, SY.cognition, SY.timer, SY.wf-rules, SY.frontdesk.gov, SY.vault | SY.config.routes, SY.identity, SY.opa.rules, SY.admin, SY.cognition, SY.timer, SY.vault |
| worker | SY.config.routes, SY.identity, SY.opa.rules, SY.cognition, SY.timer, SY.wf-rules | SY.config.routes, SY.identity, SY.opa.rules, SY.cognition, SY.timer |
| egress | SY.config.routes, SY.opa.rules | SY.config.routes |
| ingress | SY.config.routes, SY.opa.rules, SY.edge | SY.config.routes |

The table's tests keep its rules (`check_system_nodes`), the ones the orchestrator checked in each
hive.yaml at its start before:

- a list starts with `SY.config.routes` (the writer of the routing SHM);
- on the motherbee, `SY.vault` comes last, after each of its consumers (`SY.admin`,
  `SY.architect`, `SY.storage`, `SY.identity`, `SY.cognition`, `SY.frontdesk.gov`). SY.vault
  announces its secrets when it starts, and a router broadcast reaches only nodes already
  registered;
- no other role runs `SY.vault`;
- only `SY.*` names, no duplicates, and a `wait_for` entry is one of the nodes.

`add_hive` writes none of this into a spoke's hive.yaml: the spoke's orchestrator, of the same
release, has it. Golden tests hold the table to the lists `packaging/hive.yaml.example` shipped
and to the blob and dist flags the joins wrote ([Config](#config)). A release that drops a node
from a role, or adds one, reaches the spokes with the core update ([Updates](#updates)).

A node name maps to its unit by `name_to_service` (`release_settings::service_name`): drop `SY.`,
lowercase, dots to dashes, prefix `sy-` (`SY.frontdesk.gov` → `sy-frontdesk-gov`).

`get_node_status` reads an SY node from that unit, and the router (`RT.gateway` and its stream)
from `rt-gateway` (`local_node_unit`): the lifecycle comes from the unit and the router's LSA,
so a stopped or crashed one reads STOPPED or FAILED. The router's own name is no node of its LSA:
it counts as seen while the LSA entry for its hive is fresh (`node_seen_by_router`). `config` and
`state` read `{"managed": false}`; the health is still what the node answers to
`NODE_STATUS_GET`. A core node this hive's role does not run (not in its role's list; the
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
4. Copy the role's core binaries and manifest, the vendored Syncthing and the installation file
   (`sync_core_to_worker`, `sync_vendor_to_worker`, `sync_installation_to_worker`); write the
   spoke's hive.yaml with the motherbee as WAN uplink, the motherbee's paths and ports, and no key
   the release defines and no `ai` (`render_worker_hive_yaml`; the egress' and the ingress'
   likewise); push the TLS material (`push_hive_tls`) and, for a worker, the identity sync key.
5. Write and start the units, wait for the spoke's orchestrator on the mesh, send
   `ADD_HIVE_FINALIZE`, link Syncthing, apply `restrict_ssh` / `harden_ssh` if asked. On the
   spoke the finalize (`add_hive_finalize_local_flow`) runs a core update over what step 4 copied,
   which restarts nothing when nothing is new, then the vendor's: a vendor still on its way (its
   manifest or binary missing), with a Syncthing installed and running, is reported in
   `vendor.awaiting_update` and the finalize goes on (FINDINGS A-190). It links the motherbee's Syncthing, and waits for its core
   folder with what is left of the motherbee's wait (`finalize_probe_budget_ms`, a 10 s margin
   for the reply). Its answer's `timings_ms` gives each step's time.
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

A request with `ssh_password` and no `ssh_key` must say what happens to password logins at the
join's end: `harden_ssh` true turns them off, false leaves them on (`require_password_logins_choice`,
FINDINGS A-165). Without it the join is refused with `INVALID_REQUEST` before anything starts. Next
to an `ssh_key` the password only feeds `sudo`. A socket-only re-run touches no SSH and applies no
`harden_ssh`; when its request carries a password, its `warnings` say so
(`socket_only_join_ssh_warnings`).

A successful join answers `warnings`. A join that logged in with `ssh_password` (its key-first
probe failed and the password seeded the motherbee key) with `harden_ssh: false` says there that
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
(`runtime`, `core`, `vendor` or `config`) and the `manifest_hash` the hive should have
(`handle_system_update_message`). If the local manifest does not match yet, the answer is
`sync_pending` (Syncthing has not delivered it); a local version past the one named is refused
(`VERSION_MISMATCH`; `manifest_match`).

- **runtime**: verifies the runtime artifacts (missing ones also give `sync_pending`) and
  applies retention.
- **config** (stage 3c): the installation file, its version and the sha256 of its bytes. No binary
  step: once the hive's copy is that version, it restarts only its readers whose engine changed
  ([The installation file](#the-installation-file)). The same message with a `write`, on the
  motherbee, writes the file's next version.
- **vendor**: brings the running Syncthing in line with the vendored one
  (`ensure_blob_sync_runtime`), when sync is enabled. It is strict (`VendorPolicy::Strict`): a
  vendored binary that does not match `dist/vendor/manifest.json` fails the update, and nothing
  is installed or restarted (FINDINGS A-189). Past that check it closes `vendor.awaiting_update`.
- **core** (`apply_core_update_replying_first`): checks each core binary of its role that
  `dist/core/bin` holds against the manifest and fails the update on the first that does not
  match (`core_update_sets`), so an update never installs one; past that check it closes
  `core.awaiting_update`. A component of its role's list that dist holds no binary of is left
  out, as installed (`left_out` in the answer, the generation and `core-installed.json`), only
  when the update replaces sy-orchestrator, and never rt-gateway or sy-orchestrator: the release
  dropped it from the role, and the orchestrator the update installs settles it (below). Any
  other update refuses it, as before. Then it backs up each binary to replace under
  `/var/lib/fluxbee/orchestrator/core-bin.prev.local/update-<ms>/` with a `generation.json`,
  swaps the binaries, rewrites the units, and **answers first** (`phase: restarting`). With no
  binary and no unit new it restarts nothing, records the update as done and answers `ok`
  (FINDINGS A-190: a join's finalize runs one over what the join copied). Otherwise, two
  seconds later it restarts `rt-gateway` and the system nodes but the left-out ones, each behind
  a 30 s health gate (`restart_local_core_services_with_health_gate`), skipping itself. A failed
  gate restores the old binaries (`status: rollback`), stopping a node the update created before
  its binary goes. On success it records `core-installed.json` and then, if its own binary
  changed, schedules its own restart (`schedule_orchestrator_self_restart`,
  `systemd-run --on-active=2s`, accurate to 100 ms). It keeps 3 generations
  (`CORE_BACKUP_GENERATIONS_KEPT`) and records the outcome in
  `/var/lib/fluxbee/orchestrator/core-update-last.json`.
- **The settle** (docs/hive-config-update-v1.md §3): the update ran in the previous release's
  orchestrator, which knows only its own list. The orchestrator it installed finishes it at its
  start, before its nodes start (`settle_core_update`, `settle_core_generation`), on the
  generation whose `applied_manifest_hash` is what `core-installed.json` says the update
  installed:
  - each `sy-*` unit this orchestrator wrote (its template, `core_service_unit_contents`) whose
    node its list no longer names is stopped and disabled; its unit (under `units/`) and its
    binary go into the generation (`dropped_units`, `components`), then off the host;
  - each node of its list `/usr/bin` lacks, one the release added, is installed from dist once
    its binary there matches the manifest, and recorded as created by the update
    (`created_without_backup`);
  - a node still pending (its binary not yet in dist, or left out while this list names it), or
    an error, keeps the generation open. The dist recheck tries again when
    `core.awaiting_update` closes, for that update only (`open_core_settle`: one installed since,
    while this orchestrator still runs, is the next orchestrator's to settle); an error with dist
    already consistent waits for the next start. Once nothing is pending the generation records
    `settled_at_ms`.

  The outcome is logged and added to `core-update-last.json` as `settled` (`/versions`:
  `core.last_update.settled`). The motherbee has no generations (dpkg installs its core): nothing
  is settled there.
- **core_rollback** restores a stamped generation (`core_rollback_local`,
  `restore_core_generation`): the nodes its update created are stopped, then removed, unit and
  binary; each binary it replaced comes back, a dropped node's among them; the units of the nodes
  its release dropped come back. The orchestrator it restores starts them, its list naming them.
  `/versions` lists each generation's `created`, `dropped`, `left_out` and `settled_at_ms`.

**`core.awaiting_update`** in `/versions` (FINDINGS A-186) is null while dist holds every core
binary of the role as the manifest says. When the start's check finds one that does not, it is
`{reason: "dist_mismatch", message, since_ms, checked_at_ms, components}`, each component
`{component, reason, detail}`, `reason` one of `missing`, `size_mismatch`, `hash_mismatch`,
`not_in_manifest`, `invalid_entry` or `unreadable`. The hive runs what `/usr/bin` holds
meanwhile. It closes when a later check, every minute, or a core update's own check finds dist
consistent. A release still syncing closes it once Syncthing delivers the rest, and the core
update then installs it. A node the running orchestrator's list names that dist no longer has
(`not_in_manifest`, or `missing` once the motherbee pruned it from the role's tree) is one the
next release dropped: the core update that replaces this orchestrator leaves it out once Syncthing
has deleted it from dist (while dist still holds the previous build, the update refuses it as any
binary still syncing, and goes through when tried again), and the new orchestrator's list no
longer names it. It lives in the orchestrator's memory: each start checks
again.

**`vendor.awaiting_update`** (FINDINGS A-189) is the same for the vendored Syncthing, in the
vendor section of `/versions`, with one component, `syncthing`. It opens when the start, the
watchdog or a sync probe finds `dist/vendor` not holding the Syncthing its manifest describes
(the manifest or the binary missing, another size or hash, no `syncthing` entry), and the hive
keeps running the Syncthing already installed ([Syncthing](#syncthing)). It closes when a later
check, every minute, or any Syncthing setup that installs or keeps the vendored binary finds it
matching. The vendored binary then gets installed at the next start or `update
category=vendor`.

`SYSTEM_SYNC_HINT` (SY.admin's `sync_hint`) asks Syncthing to rescan a folder (`blob` or `dist`)
and waits until it is idle: default 30 s, at most 300 s.

## Syncthing

The orchestrator runs Syncthing as `fluxbee-syncthing.service` (`syncthing_unit_contents`):
user `fluxbee`, `UMask=0027`, `/var/lib/fluxbee/vendor/bin/syncthing` with the GUI and API on
`127.0.0.1:<api_port>` (default 8384) and its home in `/var/lib/fluxbee/syncthing`. Every link
is TCP to the motherbee's static address, so only the motherbee listens off loopback, on
22000/tcp; a spoke only dials (`syncthing_owned_listen_address`).

Before it starts the service (`ensure_blob_sync_runtime`), the orchestrator holds the vendored
binary, `dist/vendor/syncthing/syncthing`, to `dist/vendor/manifest.json`: its size and sha256
(`vendored_syncthing_at`). It installs the vendored binary when it matches and the installed
one differs or is missing (`plan_syncthing_binary`), and never installs one that does not
match. What happens then is each caller's (`VendorPolicy`, FINDINGS A-189):

- **The start, the watchdog and the sync probes** (`SYSTEM_SYNC_HINT`, a join's finalize probe)
  run the Syncthing already installed and report the vendor in `vendor.awaiting_update`
  ([Updates](#updates)). Syncthing is what brings the rest of the vendor, and a spoke does not
  enable its unit: before this, a spoke restarted while a release that changes the vendored
  Syncthing was syncing never started Syncthing again. With no Syncthing installed they fail as
  before, which the start and the watchdog only log.
- **`update category=vendor`, and the motherbee's setup for an ingress join,** fail.

| Folder | Holds | Who gets it |
| --- | --- | --- |
| `fluxbee-dist-core-<role>` | the core binaries for one role | each spoke its own role's (`folder_forbidden_for_role`) |
| `fluxbee-dist-vendor` | Syncthing | every spoke |
| `fluxbee-dist-policy` | the compiled user OPA policy | every spoke |
| `fluxbee-dist-installation` | `dist/installation`: the installation file ([The installation file](#the-installation-file)) | every spoke |
| `fluxbee-dist-runtimes` | runtime packages | workers only |
| `fluxbee-dist-agent-assets` | `blob/agent-assets`: SY.architect's agent assets, which ai.generic reads from disk | workers only |
| `fluxbee-blob` | `blob/active` | workers only, when the motherbee's blob sync is on, as the release has it (`peer_shares_active_blob`) |
| `fluxbee-blob-public` | `blob/public` | the ingress only (`peer_shares_public_blob`) |

Dist folders and `fluxbee-blob-public` are `sendonly` on the motherbee and `receiveonly` on the
spokes (`dist_syncthing_folder_type`, `blob_public_syncthing_folder_type`), so a spoke never
writes them. `fluxbee-blob` is `sendreceive`.

A folder added by a later release reaches the hives that joined before it at their next local
reconcile, without a re-join (`reconcile_syncthing_folders_xml`): `fluxbee-dist-policy` and
`fluxbee-dist-installation` are shared with every member of `fluxbee-dist-vendor`, and
`fluxbee-dist-agent-assets` with every member of `fluxbee-dist-runtimes`. A join links a new spoke
to each, and `remove_hive` takes it out of every `fluxbee-dist*` folder the config holds. The
agent-assets tree is owned like a dist tree (`ensure_dirs`): root's on
the motherbee, readable by the `fluxbee` group, and the `fluxbee` user's on a worker, where
Syncthing writes what it receives (FINDINGS A-72). `blob/` itself is no folder, so a file written
there and renamed into `blob/agent-assets` is never sent half-written.

## The installation file

The operator's settings for the whole installation, today `ai`: the default provider and each
provider's model ([hive-config-update-v1](../hive-config-update-v1.md) §4). One file,
`<dist.path>/installation/installation.json`, one version on every hive. Its type and reader are
the AI SDK's (`fluxbee_ai_sdk::installation`); each hive's readers read their own copy:
SY.admin, SY.architect, SY.cognition, SY.frontdesk.gov and ai.generic (a hive.yaml `ai` only while
the hive holds no file).

- **The writer is the motherbee's orchestrator, alone.** At its start, after `ensure_dirs` and
  before its boot (re)starts the SY nodes, when its dist holds no file
  (`write_installation_file_at_start`), it writes version 1 from its own hive.yaml's `ai`, the
  values every reader ran on: `InstallationFile::render`, which validates, written atomically
  (temp and rename), mode 0644, in `dist/installation`, which `ensure_dirs` made root's and
  readable by the `fluxbee` group. A hive.yaml without `ai` gives a file without one, the
  fallback engine. The version is recorded first, in
  `/var/lib/fluxbee/orchestrator/installation-versions.json` (its sha256, `written_at_ms`,
  `source: migration` and, since stage 3c, its `ai` values), so a file is never there without its
  record: that record is how a hand edit is caught. Every later version is the operator's (below).
- **An `ai` no reader can use** (an unknown provider, a default provider or any provider named
  without its model) writes nothing: a warning, `config.not_written` in `/versions`, and every
  reader stays on its hive.yaml. A file missing after a later version was recorded is not written
  again as version 1 either. None of this stops the start.
- **Once a file is there it is never rewritten at a start**, whatever hive.yaml says. Each start,
  on every hive, logs what the hive holds there, and once it holds a file warns once that
  hive.yaml's `ai` is ignored (keep it while the hive may go back to a release whose readers read
  it).
- **It reaches the spokes** through `fluxbee-dist-installation` ([Syncthing](#syncthing)), and a
  joining spoke gets the motherbee's file from its join, with the core and the vendor
  (`sync_installation_to_worker`, `INSTALLATION_SYNC_FAILED` when it cannot be copied; nothing to
  copy when the motherbee holds none), so its readers read it from their first start. The join
  writes no `ai` into a spoke's hive.yaml.
- **`/versions`:** each hive's snapshot has `config`: `file` (`absent`, `valid` or `invalid`),
  `version`, `sha256`, a valid copy's `ai` values and, for an invalid one, `error`; and `applied`,
  the config update it applied last (below; null before its first). The motherbee adds each
  copy's `state` (`add_installation_state_to_answer`, `installation_spoke_state`): `current` with
  the motherbee's file's hash, `behind` with another version it recorded, `pending` while the copy
  has not arrived, `edited_by_hand` otherwise (an invalid file, or a hash no version was recorded
  with). Its own `state` is `current` while its file's hash is its version's recorded one,
  `edited_by_hand` otherwise, and `pending` without a file, with `not_written` saying why.
  `unknown` means the record cannot be read (`records_error`). Its own `config` also lists
  `versions`, every version it recorded with its `sha256`, `written_at_ms`, `source` and `ai`
  (`installation_motherbee_section`). A hive on an older orchestrator has no `config`. The
  fleet's `comparison` does not include it.

**The operator's change** (stage 3c, [hive-config-update-v1](../hive-config-update-v1.md) §4, "As
built (stage 3c)"). SY.admin's `set_installation_ai` sends two `SYSTEM_UPDATE`s with category
`config`: the write to the motherbee's orchestrator, then the apply to every hive's.

- **The write**, `{category: "config", write: {ai, expected_version, replace_hand_edit?}}`
  (`is_installation_write`, `installation_write_answer`, `write_installation_operator_version`).
  The `ai` is taken exactly (`ai_settings::parse_operator_ai`). One write at a time. It refuses,
  writing nothing, with `error_code` and the reason:
  - `NOT_PRIMARY` on a spoke; `JOIN_IN_PROGRESS` while an `add_hive` or a `remove_hive` holds its
    hive's lock (`any_hive_topology_busy`): a join copies the file to its spoke;
  - `INSTALLATION_NOT_WRITTEN` without a file: the start writes version 1 while it is the only
    one recorded; a file missing after a later version is put back first;
  - `INSTALLATION_EDITED_BY_HAND` for a file no reader can use, or a hash its version was not
    recorded with, unless `replace_hand_edit` is true;
  - `VERSION_CONFLICT` when `expected_version` is not its version: the file's, or the last one
    recorded for a file no reader can use;
  - `INVALID_AI_SETTINGS` (`InstallationFile::render` validates); `INSTALLATION_UNCHANGED` when the
    current version holds these values (values compared, not bytes);
  - `INSTALLATION_RECORDS_UNREADABLE`, `INSTALLATION_UNREADABLE`, `INSTALLATION_WRITE_FAILED`.

  The next version is one past the last recorded, or past the file's when a hand edit raised it.
  It is recorded first in `installation-versions.json`, with `source: operator` and its `ai`
  values, then written as version 1 is; a record whose file could not be written is taken back.
  The answer is `{status: "ok", version, sha256, previous_version, previous_sha256}`. A version's
  `ai` is its values (`ai_settings::ai_values`: a file without `ai` records the fallback engine
  written out). Version 1 as stage 3b recorded it, without values, gets them from its file while
  its hash is still the recorded one, at the start and at the first write.
- **The apply**, `{category: "config", manifest_version, manifest_hash}`, on every hive
  (`apply_config_update_local`, under the update lock):
  - `sync_pending` until the hive's copy is that version with that hash
    (`local_system_manifest_state`);
  - it restarts only its start readers (`installation_start_readers`): `sy-cognition` and
    `sy-frontdesk-gov` on the motherbee, `sy-cognition` on a worker, none on an ingress or an
    egress; and of those, only the ones its role runs that systemd has active, since one that does
    not run reads the file at its next start;
  - only when their engine changed (`config_update_plan`): the version's default provider with its
    model (`ai_settings::engine_of`) against the one in
    `/var/lib/fluxbee/orchestrator/config-applied.json`, the update this hive applied last
    (`{version, sha256, applied_at_ms, ai, restarted}`). Without that record (a hive's first config
    update, or one it cannot read) every start reader restarts; the same version and hash again
    restarts nothing;
  - each restart behind the core's 30 s health gate (`restart_core_service_with_health_gate`).
    The record is written after the restarts, only when they all succeeded; otherwise the answer is
    `error` with `RESTART_FAILED`, and running the update again restarts them again. A record that
    cannot be written once they all restarted is `error` with `CONFIG_RECORD_FAILED`
    (`config_update_error_code`): the readers run the version, and the next update restarts them.

  The answer lists `restarted`, `unchanged` and `errors`. SY.admin and SY.architect are never
  restarted: they take a new version at their next use, and ai.generic at its next request.

## Host posture

What the code does today. Each hive's orchestrator records the alerts below in its own
`drift-alerts.jsonl`; SY.admin's drift alert reads ask the motherbee's, so a spoke's stay on the
spoke until the system's log (FINDINGS A-175).

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
  `ipv6_not_blocked`) in its own drift-alert file, which the motherbee does not read yet
  (FINDINGS A-175). A worker gets its default route via the egress `gateway_ip`. The motherbee
  is only checked (`EGRESS_MOTHERBEE_BYPASS`).
- **Internet probe** (`check_internet_reachable`): an HTTPS GET (curl, 5 s) of the hive's own
  `egress.reachability_url`, on the egress at boot and every drift tick, and at boot on a worker
  that routes through it and on the motherbee when they declare `gateway_ip`. A failure is a
  warning in the journal; on the egress, since 0.1.110, also an `internet_unreachable` drift alert
  in its own file (FINDINGS A-175). Without the URL the probe does not run and logs
  `internet_reachable=not_configured`, as it does after warning about a URL that is not a valid
  `https://` one: the code names no target of its own. A host without `curl` does not probe
  either and logs `not_probed` (`run_internet_probe`), never a `false` it did not measure.

## Watchdog

Every 5 s (`watchdog_tick`) the orchestrator checks `rt-gateway` and every SY service of its
role's list with `systemctl is-active`, and runs `systemctl start` on any that is not
active. A stopped core unit comes back within seconds. The same tick logs managed nodes that left
the router, verifies runtimes, runs blob GC when enabled, checks Syncthing and its peers, re-applies
egress and the ingress' SSH table, reports an active host firewall (`watchdog_host_rules`), and runs
the loopback check. While a join runs, its Syncthing is the join's: the motherbee's while an
`add_hive` or `remove_hive` is in flight, a spoke's while its finalize runs. The Syncthing check and
the peer reconcile leave it alone until the join is done, and the check asks again right before it
restarts Syncthing (`a_join_owns_syncthing`, FINDINGS A-194).

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

`/etc/fluxbee/hive.yaml` (`HiveFile`), the hive's own: `hive_id`, `role`, `wan` (`gateway_name`,
default `RT.gateway`; `listen`; `uplinks`), `storage.path`, `blob` (`path`, default
`/var/lib/fluxbee/blob`; `sync.api_port`, default 8384; `sync.data_dir`, default
`/var/lib/fluxbee/syncthing`; `gc`), `dist.path` (default `/var/lib/fluxbee/dist`),
`identity.sync.port` (default 9100), `government`, `egress` (with `reachability_url`, the
internet probe's target, no default; and `ssh_from_wan`, default true), `ingress.ssh_allow_from`
(the ingress' extra SSH sources), `ai`, read untyped: on the motherbee it is the installation
file's first version, and once a hive holds the file it is ignored
([The installation file](#the-installation-file)).

What the release defines for each role is in the code (`json_router::release_settings`,
[System nodes per role](#system-nodes-per-role)): the role's SY nodes, and the blob's and dist's
topology (`blob_runtime_from_hive`, `dist_runtime_from_hive`):

| Role | blob store | blob sync | `fluxbee-blob-public` | dist sync | tool |
| --- | --- | --- | --- | --- | --- |
| motherbee | on | on | on (sends) | on | Syncthing |
| worker | on | on | off | on | Syncthing |
| egress | off | off | off | on | Syncthing |
| ingress | off | on | on (receives) | on | Syncthing |

The `fluxbee-blob` folder exists only where the blob store is on, so the ingress has none: its blob
sync keeps Syncthing running, as its dist sync does.

These keys of an older hive.yaml (`system_nodes`, `blob.enabled`,
`blob.sync.enabled|public_enabled|tool`, `dist.sync.enabled|tool`) are ignored; the start names the
ones it finds in one warning (`warn_superseded_hive_keys`). Keep them while the hive may go back to
0.1.116 or earlier: that orchestrator does not start without `system_nodes`, and reads the blob's
and dist's flags from the file, with defaults that differ by role (a missing `blob.sync.enabled` is
off). A hive whose hive.yaml never had them, joined by 0.1.117 or later, goes back below 0.1.117
only after they are put back by hand (lab/logbook/HANDBOOK.md, "The rollback").

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
| `/var/lib/fluxbee/orchestrator/` | `core-bin.prev.local/`, `core-installed.json`, `core-update-last.json`, `config-applied.json` (the config update applied last), `deployments-history.jsonl`, `drift-alerts.jsonl`, `syncthing-posture.json`, and on the motherbee `installation-versions.json` |
| `/var/lib/fluxbee/dist/` | core, vendor, runtimes and policy trees, and `installation/installation.json` |
| `/var/lib/fluxbee/tls/` | mesh CA (motherbee) and the hive's TLS leaf |
| `/var/lib/fluxbee/ssh/motherbee.key` | the join key (motherbee) |
| `/etc/nftables.d/fluxbee-egress.nft`, `/etc/nftables.d/fluxbee-ingress.nft` | the egress' and the ingress' tables, loaded at boot by `fluxbee-egress-nft.service` and `fluxbee-ingress-nft.service` |
| `/var/lib/fluxbee/syncthing/` | the Syncthing home |

## Failure behavior

- **No router.** `wait_for_router_ready` gives up after 30 s and `main` exits with an error;
  systemd restarts it 5 s later.
- **Dist does not hold a core binary of the role as the manifest says.** No error: a warning per
  binary, `core.awaiting_update` in `/versions`, and the hive runs what `/usr/bin` holds until a
  core update can install it (FINDINGS A-186). A missing or invalid `dist/core/manifest.json` is
  still fatal.
- **`dist/vendor` does not hold the Syncthing its manifest describes.** The start and the
  watchdog run the Syncthing already installed, with a warning and `vendor.awaiting_update`; with
  none installed Syncthing does not start, logged at each attempt, as before. The orchestrator
  runs either way (FINDINGS A-189).
- **The installation file cannot be written, or is not valid.** No error: the motherbee's start
  warns and `/versions` shows `config.not_written` or the copy's `state`; the orchestrator runs
  either way, and so do the readers, each failing only its AI work with the reason.
- **A config update's reader does not restart.** The answer is `error` with `RESTART_FAILED` and
  the reasons, the record of the update applied last is left as it was, and the same update run
  again restarts the readers again. When they all restarted and the record cannot be written, the
  answer is `error` with `CONFIG_RECORD_FAILED`.
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
  hive, with the last core update, `core.awaiting_update` and `vendor.awaiting_update`, and each
  hive's installation file with the config update it applied last), `GET /installation` (the
  installation file's view), `GET /drift-alerts` on SY.admin.

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
- `lab/logbook/FINDINGS.md`: A-39, A-45, A-46, A-54, A-186, A-189; D32 in the join comments.
