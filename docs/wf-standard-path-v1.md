# WF nodes on the standard path — v1

Status: design, 2026-10-16. Finding A-68 (`lab/logbook/FINDINGS.md`). Target release: 0.1.70.

A `WF.*` node is a workflow node. It works at the same level as an AI or IO node and gets nothing
more. Two things kept it off the standard path:
- **who launches it:** SY.wf-rules, straight to SY.orchestrator;
- **how it boots:** the Go node has its own control plane.

The analysis behind this design also found a broken path from Archi and a latent privilege in the
system policy. Both are fixed here too.

## 1. What is wrong today (verified in code at 0.1.69)

1. **SY.wf-rules bypasses SY.admin.** A workflow deploy enters the admin (`wf_rules_*`), then SY.wf-rules
   manages the node by sending SPAWN_NODE, RESTART_NODE, NODE_CONFIG_GET/SET and KILL_NODE straight
   to `SY.orchestrator@<its hive>` (`go/sy-wf-rules/node/orchestrator.go`), plus NODE_STATUS_GET to
   the WF node. All of them are protected SYSTEM actions admitted only by rule (4) of
   `policy/system.rego`. Consequences:
   - it breaks "every user action goes through SY.admin";
   - every deployed WF node gets `launched_by: "system"` (the orchestrator's default), not who asked;
   - nothing the admin checks or records applies to these launches.
2. **The Go WF node has its own control plane.** IO and AI use the SDK's
   (`crates/fluxbee_sdk/src/managed_control_plane.rs`). The WF node:
   - decodes `_system` strictly and rewrites it from its own struct, so any key the orchestrator adds
     crash-loops it (stage 4's `launched_by`, patched by adding the field plus a key-pinning test);
   - exits on a bad config or a bad workflow definition, and systemd relaunches it every 5 s, so
     the reason only reaches journald;
   - always answers HEALTHY;
   - answers CONFIG_SET errors with `error.detail`, which SY.admin does not read (the reason is lost);
   - persists non-atomically, with mode 0644 and an extra root `config_version`.
3. **Deploying or deleting a workflow from Archi is broken.**
   - The admin runs mesh ADMIN_COMMANDs in a serial loop, and a `wf_rules_*` request waits there
     inline for SY.wf-rules.
   - SY.wf-rules calls the admin back to publish the package; that call queues behind the waiting
     request.
   - After 5 s SY.wf-rules gives up with `PACKAGE_PUBLISH_FAILED`, with `current` already rotated,
     and the loop later runs the stale publish.
   - A delete fails the same way on the orchestrator's teardown callbacks.
   - Executor plans run inline too.
   - Only the operator's HTTP works, because each connection gets its own task.
4. **Rule (4) also names `WF.orch.diag`,** an E2E diagnostic identity, with near-admin SYSTEM
   authority on its hive. A workflow named `orch.diag` is deployed as `WF.orch.diag@<hive>`, and a
   workflow can send SYSTEM messages (`send_message` with `meta.type: system`), so it could spawn
   and kill nodes. Only the operator deploys workflows, but this authority has no reason to exist.

## 2. Design

### 2.1 SY.admin runs every mesh command in its own task

`run_internal_admin_loop` spawns a task per ADMIN_COMMAND instead of awaiting each inline. The
operator's HTTP server already runs the same handlers concurrently (one task per connection). The
one exception the review found is the runtime manifest: `publish_runtime_package` and
`remove_runtime_version` each read, decide and rewrite it, and only the serial loop kept two mesh
calls apart. A `runtime_manifest_lock` now serializes them across their awaits. The publication
lock is held across awaits too, but nothing it waits on calls back into it. This removes the whole
class of call-back deadlocks:
- the 0.1.69 teardown case (`kill_node`, `remove_node_instance`), which spawned only those two
  actions;
- `wf_rules_*` from Archi;
- executor plans;
- the two-process timeout cycle between the admin's loop and the orchestrator's serial admin worker
  during a teardown, since the teardown's `delete_ilk` no longer waits behind an inline command.

`internal_command_calls_back` goes away. A command that fails before any reply is built (a
request it cannot parse, a transport error) now answers `INTERNAL_ERROR`, where the caller used
to wait for its own timeout.

**Known limit:** the admin relays every node action through the motherbee orchestrator's admin
worker, which runs one action at a time. A long action, such as a `core_rollback` or a forward to a
hive that does not answer (45 s), delays the others, a WF deploy included. Today SY.wf-rules on a
worker talked to its own orchestrator; through the admin it shares that queue like every other
launcher. Letting the orchestrator run node actions concurrently (per-node locks already exist) is
future work.

### 2.2 SY.wf-rules goes through SY.admin

SY.wf-rules sends ADMIN_COMMANDs to `SY.admin@motherbee`, with `target` = its own hive. One per node
action:

| Need | Today | After |
|---|---|---|
| does the node exist, which version is bound | NODE_CONFIG_GET → orchestrator | `get_node_config` |
| rebind to the new package | NODE_CONFIG_SET → orchestrator | `set_node_config` (same top-level `runtime`, `runtime_version`, `requested_runtime_version`, `runtime_base`, `package_path`) |
| first deploy | SPAWN_NODE → orchestrator | `run_node` |
| restart after rebind | RESTART_NODE → orchestrator | `restart_node` |
| delete | KILL_NODE (purge) → orchestrator | `kill_node` with `purge_instance` |
| running and health | NODE_STATUS_GET → WF node | `get_node_status` |
| active instances | WF_LIST_INSTANCES → WF node | unchanged |

**WF_LIST_INSTANCES stays direct.** Reading the instance list of a workflow it owns is a read of
that node's domain data, not an action on the node, and it is not a protected SYSTEM action. It is
the one recorded exception.

**The admin's gate for SY.wf-rules** (the analogue of IO.cloud's IO.*-only rule). It covers the six
node actions above; the gate leaves its other actions alone (`publish_runtime_package`,
`remove_runtime_version`).
- `SY.wf-rules@<hive>` may run them only on a `WF.<x>@<hive>` node of its own hive. The node name
  and every target the request names must say that hive.
- What it binds or launches is the node's own package on the WF engine:
  - runtime `wf.<x>`, required on `run_node`;
  - `runtime_base` `wf.engine`;
  - `package_path` under `dist/runtimes/wf.<x>/`.
- Anything else answers `UNAUTHORIZED`.
- `run_node`, today refused to every mesh caller but IO.cloud and Archi, is allowed under this gate.

**SY.wf-rules takes its operations from the admin only.** Its compile, apply, rollback, delete and
query requests, and CONFIG_GET/SET, are accepted only from `SY.admin@motherbee` (the
router-stamped sender), and anything else answers `UNAUTHORIZED`. Before, any node could ask it to
deploy or delete WF nodes, which it now does under its own gate at the admin.

**SY.wf-rules no longer writes the dist tree.** It removes obsolete package versions through the
admin's `remove_runtime_version` on every hive, the motherbee included. Before, on the motherbee it
deleted them and rewrote the manifest itself, racing the admin's own writes.

**The current version goes last.** The admin refuses to remove a runtime's current version while
others remain (`RUNTIME_CURRENT_CONFLICT`). SY.wf-rules orders by its own copy of the manifest, and
a version refused as current is tried again once the others are gone. A worker's copy can lag
behind the motherbee's: after a rollback the 0.1.70 gate saw a delete on worker1 stop at that
refusal and leave the packages behind.

**A rebind carries only the binding.** `set_node_config` merges, so the node's operator config stays
as it is without being copied back. Copying it carried along the keys an older WF node had written
at the top of its file.

**Who launched it.**
1. The admin stamps the requester on the `wf_rules_*` request it forwards:
   - the operator's HTTP: `operator`;
   - Archi: `archi`;
   - an executor plan (no caller): `operator`, as for other plan steps.
2. SY.wf-rules carries that value into `run_node`.
3. For a `run_node` from `SY.wf-rules`, the admin keeps the `launched_by` it carries only when it is
   `operator` or `archi`. Anything else, `cloud` above all, is recorded as `SY.wf-rules@<hive>`:
   `cloud` would give Fluxbee Cloud's relay start and kill rights on the node.

Known gaps:
- an executor plan Archi submits is recorded as `operator`, as for every plan step;
- another mesh caller of `wf_rules_*` is recorded as `SY.wf-rules@<hive>`.

**Timeouts, nested so each caller outlasts what it waits for:**
- **SY.wf-rules** gives each request one deadline of 60 s, shared by every call it makes: the
  publish, the purge, then the rebind and restart, or the launch. The retries while a package
  syncs to a worker (up to 45 s) stop at that deadline. The 0.1.70 gate measured more than 20 s
  when the worker's runtimes folder was retrying a failed pull: a workflow delete just before
  makes Syncthing fail on the deleted directories and retry. The 0.1.70 build had 35 s and 20 s,
  and its update on worker1 timed out waiting.
- **Per call within it:**
  - `get_node_config`, `set_node_config`, `get_node_status`: 10 s;
  - `run_node`, `restart_node`: 15 s, inside the 45 s budget for the package to reach a worker;
  - `kill_node` with purge: 25 s, as for IO.cloud;
  - the admin calls for publish and remove: 5 s.
- **SY.admin** waits 75 s for SY.wf-rules (`JSR_ADMIN_WF_RULES_TIMEOUT_SECS`).
- **Archi** waits 90 s for the admin on `wf_rules_*` (`JSR_ARCHITECT_WF_RULES_TIMEOUT_SECS`).

The 30 s Archi used to wait, the same as the admin's own wait, lost the answer of a deploy near the
limit.

**A worker deploy brings its package to the hive first.**
- A package published on the motherbee reaches a worker through Syncthing. Two delays are built
  in there:
  - the motherbee's watcher aggregates changes (10 s, up to a minute when the runtimes folder is
    busy, as after a core update or other deploys);
  - a worker whose folder failed a pull waits before it retries. A workflow delete makes it fail
    once on the deleted directories: "not empty", parent before children.
- The 0.1.71 gate measured 62 s from publish to the package's files on worker1, with the manifest
  there after 12 s.
- So on a worker, right after publishing, SY.wf-rules asks the admin for two `sync_hint`s on
  `fluxbee-dist-runtimes`:
  1. on the motherbee, which scans now and announces the package, waiting up to 5 s;
  2. on its own hive, which scans and pulls now, waiting up to 20 s for it to settle.
- While a node call still answers that the package is not here, it nudges its hive again before
  each retry.
- This is an explicit, bounded step of the deploy. If it does not settle, the retries below still
  wait for the package, and say so when it never comes.

**A workflow package is not on a hive until its definition is.**
- Syncthing can deliver a package's manifest entry and `package.json` before `flow/definition.json`.
- Before, the WF node crash-looped until the file came, which fixed itself; it now waits in
  FAILED_CONFIG, and nothing would retry it.
- So the orchestrator answers `RUNTIME_NOT_PRESENT` for a workflow package without its definition,
  and SY.wf-rules retries while the package syncs. It checks on `run_node`, on every start and
  restart (the 60 s reconcile included), and before `set_node_config` binds a version.

### 2.3 The system policy loses SY.wf-rules and WF.orch.diag

- Rule (4) becomes `SY.admin` only, so a same-hive admin is the only non-orchestrator origin of
  protected actions.
- Both wasm artifacts are recompiled from the rego, and the policy truth-table tests follow.
- The observability-only frozen markers in `sy_config_routes` mirror the new rule.
- `docs/04-routing.md`'s authority table is corrected.

The diag tools that relied on WF.orch.diag's authority stop working:
- `orch_system_diag` and the `orchestrator_*_e2e.sh` scripts that drive it;
- `blob_sync_diag`, whose SYSTEM_SYNC_HINT was already denied.

They go to the cleanup catalog and are not deleted here. The `wf.orch.diag` runtime fixture that
other scripts use is unaffected.

### 2.4 The Go SDK gets the managed control plane; the WF node moves onto it

**SDK** (`go/fluxbee-go-sdk/managed_control_plane.go`), a port of the parts of the Rust module that
nodes actually use:
- **Path:** `<root>/<KIND>/<node@hive>/config.json`; a name without `@` is an error.
- **Effective config:** the document without `_system` (flat, or wrapped in `config`).
- **States:** `UNCONFIGURED`, `CONFIGURED`, `FAILED_CONFIG`, with `last_error {code, message}`.
- **Bootstrap:**
  - an absent file is UNCONFIGURED;
  - invalid JSON, a non-object, or a contract rejection is FAILED_CONFIG;
  - only spawn keys (`tenant_id`) is UNCONFIGURED, unless the contract says it runs on what the
    orchestrator gives it.
- **Persist:**
  - copies `_system` verbatim and sets only `_system.config_version`;
  - writes the flat shape atomically (temp file + rename), keeping the file's mode;
  - refuses to write over a file it cannot parse, whose `_system` would be lost.
- **Health:** HEALTHY when CONFIGURED, DEGRADED otherwise.

The Go contract interface also receives a read-only view of `_system`. The WF node finds its
package there (`package_path`, `hive_id`). Rust nodes do not need it because they get their identity
from the environment. **The view is read leniently:** a node reads the keys it needs and ignores
the rest, so a new orchestrator key can never stop a node.

**WF node:**

*Boot.*
1. Bootstrap the control plane.
2. Always connect to the router.
3. When CONFIGURED, start the engine as today.
4. Otherwise run control-plane only:
   - **NODE_STATUS_GET:** DEGRADED plus `last_error`, which `get_node_status` surfaces.
   - **CONFIG_GET:** the state and the error.
   - **CONFIG_SET:** validated and persisted; a restart applies it.
   - **WF_\* requests:** answered with `node_not_configured`.
   - **Workflow messages:** logged and dropped, as IO nodes do with traffic they do not answer.

*Validation.*
- Validating the config includes loading the workflow definition, so a missing or broken
  definition is FAILED_CONFIG `invalid_definition`, not a crash.
- What is persisted is the operator's config (`gc_*`, `sy_timer_l2_name`, `db_path`, `tenant_id`,
  and `workflow_definition_path` in local-dev mode only), never values derived from `_system`.

*CONFIG_SET.*
- Replace mode.
- A version must advance: `0` means current + 1, and an older one is `stale_config_version`. IO
  rejects 0; WF keeps it as "next".
- "Current" is the newer of the node's version and the one on disk, since the orchestrator's
  `set_node_config` raises the version on disk without telling the node.
- Lowercase codes in `{code, message}`, the family's shape.
- The running config is not swapped; a restart applies it (WF stays boot-time only).
- The reply carries the node's own state, unchanged, with `restart_required` and
  `pending_restart`. A node in FAILED_CONFIG stays there and DEGRADED until restarted. The recovery
  is CONFIG_SET (or `set_node_config`) plus `restart_node`, or a redeploy.

*Status, SDK, and what stays fatal.*
- NODE_STATUS_GET answers the lifecycle health.
- The SDK's msg_type checks become case-insensitive, as Rust's (audit #14).
- Engine start failures (opening the store, recovery) stay fatal and are out of scope.

The orchestrator's `_system` key-pinning test (`the_system_block_keys_are_the_ones_the_wf_node_decodes`)
and its Go twin are replaced by a Go test: a `_system` key the node does not know is ignored, and
survives a CONFIG_SET.

### 2.5 Smaller fixes on the same path

- A mesh `wf_rules_*` query without a hive returns an error to the caller instead of no reply.
- **SY.wf-rules reads a WF node's refusal of a WF_* request** (`{ok:false, error:<code>,
  detail}`), e.g. `node_not_configured`, as an answer, not a timeout. A delete without force says
  what the node answered.

## 3. Not in scope (recorded in A-68)

- **IO family health:** IO nodes' NODE_STATUS_GET ignores the lifecycle (a FAILED_CONFIG IO node
  answers HEALTHY), and io.slack does not answer NODE_STATUS_GET. This belongs to the IO-family
  review.
- **Fatal engine-start paths in WF:** opening the store and recovery.
- **Router names:** any local process may still register any node name at HELLO (P-1).
- **The admin's mesh surface:**
  - any node may still ask the admin for most actions (`kill_node`, `restart_node`,
    `set_node_config`…), since only IO.cloud and SY.wf-rules are gated;
  - a workflow can send such an ADMIN_COMMAND (`send_message` with `meta.type: admin`);
  - removing rule (4)'s extra names closes the SYSTEM path, not this one. That belongs to the
    postponed security work (A-22).
- **Other unlocked read-modify-writes:** the OPA version counter (`next_opa_version`) has no lock
  either. It is reachable only by operator actions (the HTTP API could already race it).
- **WF configs written before 0.1.70:** a WF node that once got a direct CONFIG_SET wrote
  `config_version` (and its package's definition path) at the top of its file. The strict operator
  config now reads that as FAILED_CONFIG `invalid_config: unknown field "config_version"`, visible
  in `get_node_status`, and it is fixed with `set_node_config?replace=true` and a restart. PROD has
  no WF nodes; a DEV hive might.

## 4. Validation

**Unit tests:**
- the Go SDK control plane (port of the Rust tests, plus `_system` preservation, atomic write and
  the mode);
- the WF node:
  - a bad config or definition is FAILED_CONFIG and the node stays up;
  - a `_system` key it does not know is ignored and kept;
  - health follows the state;
  - CONFIG_SET errors carry a message;
  - versions must advance;
- SY.wf-rules: every node action goes to the admin, and `launched_by` is carried;
- the admin:
  - every mesh command runs off the loop;
  - the wf-rules gate (WF.* on its own hive only);
  - `launched_by` stamping on `wf_rules_*` and its pass-through on `run_node`;
- the policy truth table without SY.wf-rules and WF.orch.diag.

**On 8.x (0.1.70):**
- the WF lifecycle E2E on motherbee and on worker1 (compile, deploy, status, update, rollback,
  delete);
- `launched_by: operator` on the deployed node;
- a broken config leaves the node up, DEGRADED with its reason and no restart loop, and a fixed
  config brings it back;
- an unknown `_system` key does not stop it;
- the new system policy is loaded.
