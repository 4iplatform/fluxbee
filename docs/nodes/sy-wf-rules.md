# SY.wf-rules

SY.wf-rules manages a hive's workflows. It keeps each workflow definition with a version
history, validates (compiles) new definitions, publishes each version as a runtime package
`wf.<workflow>` through SY.admin, and deploys the node that runs it, `WF.<workflow>@<hive>`, on
the WF engine runtime `wf.engine`. A workflow's rule is the source of truth; its WF node is
derived from it.

Source: `go/sy-wf-rules/` (`main.go`, `node/*.go`). Definition validation: `go/pkg/wfcel`. The
engine: `go/nodes/wf/wf-generic/`. Admin side: `handle_wf_rules_http`,
`authorize_wf_rules_node` and `authorize_wf_rules_package` in `src/bin/sy_admin.rs`.

| | |
| --- | --- |
| Binary | `/usr/bin/sy-wf-rules` (`packaging/build-deb.sh`, `GO_BINS` entry `sy-wf-rules:go/sy-wf-rules`) |
| Node name | `SY.wf-rules@<hive_id>` |
| Role | motherbee and worker |
| systemd unit | `sy-wf-rules.service` (`Restart=always`, `RestartSec=5`) |
| Listens on | the local router socket only; no TCP port |
| State | `/var/lib/fluxbee/wf-rules/<workflow>/`, lock `/var/run/fluxbee/sy-wf-rules.lock` |

## Where it runs

`SY.wf-rules` is in the release's lists for the motherbee and the worker
(`src/release_settings.rs`), but in neither `wait_for` list. Each hive's SY.wf-rules manages the
workflows and the WF nodes of that hive only. SY.admin exists only on the motherbee, so every
request reaches a worker's SY.wf-rules from `SY.admin@motherbee`.

## How it starts

`node.Run` in `go/sy-wf-rules/node/service.go`:

1. Takes an exclusive `flock` on `/var/run/fluxbee/sy-wf-rules.lock`; a second instance exits
   with `sy-wf-rules already running (lock held)`.
2. Reads `hive_id` from `/etc/fluxbee` and waits up to 30 s (polling every 250 ms) for its ILK
   in the identity SHM `/jsr-identity-<hive>` (`fluxbeesdk.WaitForSelfSystemIlkID`); otherwise
   it exits and systemd restarts it.
3. Connects to the router (`fluxbeesdk.ConnectWithRetry`, retry every 1 s) and creates
   `/var/lib/fluxbee/wf-rules` and `/var/lib/fluxbee/dist/runtimes` if missing
   (`DefaultRuntimeConfig`).
4. Serves messages until SIGINT/SIGTERM.

## The workflow store

`Store` in `node/store.go`, rooted at `/var/lib/fluxbee/wf-rules`:

```text
/var/lib/fluxbee/wf-rules/<workflow>/
  staged/   definition.json  metadata.json   # compiled, not applied
  current/  definition.json  metadata.json   # applied, the one deployed
  backup/   definition.json  metadata.json   # the previous current
```

- Workflow names match `^[a-z][a-z0-9]*(\.[a-z0-9]+)*$` (`workflowNamePattern`, the runtime
  naming policy, since the runtime is `wf.<workflow>`: no hyphen since 0.1.98); anything else is
  `INVALID_WORKFLOW_NAME`.
- `metadata.json` (`WfRulesMetadata`): `version` (a counter), `hash` (`sha256:` of the
  definition), `workflow_name`, `workflow_type`, `wf_schema_version`, `compiled_at`,
  `guard_count`, `state_count`, `action_count` (the catch's guards and actions count too).
- When a compile does not give a version, the next one is one above the highest of the three slots
  (`NextVersion`) and of the versions of `wf.<workflow>` this hive's dist holds: its version
  directories `dist/runtimes/wf.<workflow>/0.0.<n>/` and the versions `dist/runtimes/manifest.json`
  lists for it, `available` and `current` (`nextWorkflowVersion`, `highestPublishedVersion`; since
  0.1.133, FINDINGS A-281). On the motherbee that dist is its own; on a worker, its Syncthing
  mirror of the motherbee's. A delete whose purge left versions in the motherbee's dist
  (`packages_left`, see [Delete](#delete)) drops the slots, which would start again at 1: before
  0.1.133 the next deploy of the name published `0.0.1` over the `0.0.1` left there, SY.admin
  refused it (`VERSION_CONFLICT`, the definition differing: `PACKAGE_PUBLISH_FAILED`), and the
  name stayed blocked until the leftover was removed by hand. Above them, the deploy publishes, and
  its purge removes them, since no slot keeps them. A dist directory or a manifest that cannot be
  read refuses the compile (`WFRULES_ERROR`, naming it, "nothing was staged"): a version above
  what it holds is unknown. A compile that names its `version` stages that one.
- Apply moves `current` to `backup` and `staged` to `current` (`RotateToApply`). Rollback moves
  `backup` to `current` and the old `current` to `staged` (`RotateToRollback`).

## Operations

### Compile

`CompileWorkflow` (`node/compile.go`) checks the name, loads and validates the definition with
`wfcel.LoadDefinitionBytes` (`COMPILE_ERROR` on failure, naming the path: the checks of the spec's
§8.5, a menu's limits and its catch included), and writes it to `staged` as written: a known
option stays written by its name alone, and the WF node's load writes the catalog's words in.
Nothing is published or deployed.

### Check

`CheckWorkflow` (`node/compile.go`) validates a definition exactly as a compile does, with the
same code (`loadWorkflow`): the same name check, the same load and limits, the same error codes,
each error with its path. It is a read: it writes nothing (no staged definition, no package, no
file of the store) and takes no version (one in the request is ignored), so a check between a
compile and its apply leaves the staged and the current definitions as they were. SY.admin's
`wf_rules_check` sends it, as `opa_check` sends SY.opa-rules' check (wf-orchestration-v1 §5.5.1).

- **The request:** a `CONFIG_SET` whose `config` is a compile's fields with `operation: check` and a
  `check_id`:

  ```text
  {"operation": "check", "check_id": "<uuid>", "workflow_name": "invoice", "definition": {...}}
  ```

- **The answer:** a `CONFIG_RESPONSE`. Every answer to a check carries `operation: check` and echoes
  its `check_id`, which SY.admin matches it by (`sendNodeConfigResponse`):
  - a definition that loads: `ok: true`, `node_name`, `operation: check`, `check_id`,
    `workflow_name`, `guard_count` and `state_count`, with no `config_version`;
  - a refusal: a compile's error (`ok: false`, `state: error`, the request's `schema_version` and
    `config_version`, `error: {code, detail}`), with `operation` and `check_id`. The code is
    `INVALID_WORKFLOW_NAME`, `INVALID_CONFIG_SET` (no definition) or `COMPILE_ERROR`, whose detail
    names each error's path (`states[0].transitions[0].guard: ...`), the errors joined by
    semicolons.

### Apply and rollback (the deploy)

`ApplyWorkflowAndDeploy` / `RollbackWorkflowAndDeploy` (`node/deploy.go`):

1. Rotates the slots (apply needs a staged version, else `NOTHING_STAGED`; with an explicit
   `version` it must match the staged one, else `VERSION_MISMATCH`; rollback needs a backup,
   else `NO_BACKUP`). First it loads the definition it is about to publish, the staged one or the
   backup, with `wfcel.LoadDefinitionBytes`, as the WF node will: one staged or backed up by an
   earlier release may no longer load (a known option written whole, say). It is then
   `COMPILE_ERROR`, naming the path, and nothing rotates or is published.
2. Publishes the `current` definition as a runtime package (`publishWorkflowPackage`,
   `node/package_publish.go`): it sends `publish_runtime_package` to `SY.admin@motherbee` with
   an inline package and `set_current: true`. The package is:
   - `package.json`: `name: wf.<workflow>`, `version: 0.0.<n>` (the counter as semver,
     `workflowRuntimeVersion`), `type: workflow`, `runtime_base: wf.engine`;
   - `flow/definition.json`: the definition;
   - `config/default-config.json`: `sy_timer_l2_name: SY.timer@<hive>`,
     `gc_retention_days: 7`, `gc_interval_seconds: 3600`.

   A failure here is `PACKAGE_PUBLISH_FAILED`.
3. Removes obsolete versions of `wf.<workflow>` from the motherbee's dist, keeping the current
   version, the backup version and the version the WF node is bound to
   (`purgeWorkflowPackages`). Removal goes through SY.admin's `remove_runtime_version` on the
   motherbee (`purgeOnPrimary`), because on a worker `dist/runtimes` is a receive-only
   Syncthing mirror. When the node's config read (`get_node_config`) does not answer, the version
   it is bound to is unknown and nothing is removed (`boundRuntimeVersionForWorkflow`): SY.admin
   refuses to remove only a version a node runs, so a stopped node would lose its own. The next
   apply or rollback removes what this one left.

   Since 0.1.133 SY.admin removes a version only once it read the version's use on every hive
   that may run it, up to 30 s (`JSR_ADMIN_ORCH_TIMEOUT_SECS`), and refuses it otherwise
   (`RUNTIME_USAGE_UNKNOWN`, FINDINGS A-281), so each removal waits up to 35 s for SY.admin's
   answer (`runtimeRemovalTimeout`; 5 s before). An answer that comes later is a `TIMEOUT`
   (SY.admin may still remove the version): SY.admin first waits for its manifest's lock, which
   another removal holds across its own read of the use, and a publish across its install. The
   purge stops at the first removal refused or not answered: what refused it holds for the next.
   Here, between the publish and the node's step, the whole purge is bounded by 15 s
   (`deployPurgeBudget`, `purgeAfterPublish`), so the node's step keeps 40 s at least, as before
   0.1.133 when its purge stopped at a refused removal (the read, then that removal's 5 s); the
   deploy goes on whatever the purge leaves, which is logged (`purge left wf.<workflow>
   <version>: <code>: <detail>`), and the next deploy tries it again.
4. Deploys `WF.<workflow>@<hive>` (`deployPublishedWorkflow`), always through SY.admin's node
   actions for this hive (`node/node_lifecycle.go`):
   - node config exists: `set_node_config` with only the new runtime binding (`runtime`,
     `runtime_version`, `runtime_base: wf.engine`, `package_path`; the node's own config is
     merged, not replaced), then `restart_node`. Result action `restarted`;
   - no node yet, `auto_spawn: true` and a `tenant_id`: `run_node` with runtime
     `wf.<workflow>`, the published version, config `{sy_timer_l2_name, gc_retention_days,
     gc_interval_seconds, tenant_id}` (`buildManagedWFConfig`) and the `launched_by` SY.admin
     stamped;
   - no node and no `auto_spawn`: nothing (`auto_spawn disabled`); no `tenant_id`: nothing,
     with a warning (`tenant_id required for first deploy`).

   The answer carries `wf_node` (`{node_name, action, status, reason, error}`), and a `warning`
   when the package was published but the node step failed. The rule change stands either way,
   but a node that does not run the definition refuses the deploy, `WF_NODE_NOT_DEPLOYED`, with
   both (`deployAnswer`, 0.1.95): only `auto_spawn` off, with no node, is no failure.

On a worker the package has to arrive by Syncthing before the node can use it. SY.wf-rules asks
SY.admin for a `sync_hint` on the motherbee's `fluxbee-dist-runtimes` folder before the node
calls (`announceRuntimes`), and retries a node call refused with `RUNTIME_NOT_AVAILABLE`,
`RUNTIME_NOT_PRESENT` or `BASE_RUNTIME_NOT_AVAILABLE` for up to 45 s (`runtimeSyncBudget`,
`untilRuntimeSynced`). A whole request is bounded by 60 s (`wfRulesRequestBudget`); SY.admin
waits 75 s for the answer (`wf_rules_request_timeout`, overridable with
`JSR_ADMIN_WF_RULES_TIMEOUT_SECS`), and Archi 90 s for SY.admin's. Within the 60 s, a deploy's
publish takes up to 5 s and its purge up to 15 s, and its node step has the rest, 40 s at least;
a delete's reads of the node take up to 22 s (`get_node_config` and `get_node_status`, 10 s
each, the count 2 s) and its kill up to 25 s (`nodeDeleteTimeout`), and its purge has the rest:
a removal it cuts there is named `TIMEOUT` in `packages_left`.

### Delete

`DeleteWorkflow` (`node/delete.go`):

- `WORKFLOW_NOT_FOUND` if no slot exists.
- With `force` or without, a config read of the node (`get_node_config`) that SY.admin or the
  orchestrator refuses, or that does not answer in time, refuses the delete with
  `ORCHESTRATOR_ERROR`, and nothing changes: whether the node exists is unknown. Before 0.1.133 a
  config read that did not answer was taken for no node: a forced delete left the node running
  with its instances, where no delete reached it any more, and dropped its workflow and packages.
- Without `force`, it refuses while the WF node has running instances (`INSTANCES_ACTIVE`) or
  when it cannot tell (`INSTANCES_UNKNOWN`, saying why and "delete with force to remove it
  anyway"): the node is not `RUNNING` (its state named: `STARTING`, `STOPPED`, `FAILED` or
  `UNKNOWN`), its status read (`get_node_status`) or its instance count failed or got no answer
  (the read and how named), or the node refused the count (its refusal named). It counts
  instances by asking a `RUNNING` WF node `WF_LIST_INSTANCES` with `status: running`, `limit: 0`
  and `count_only: true` directly (2 s timeout, `node/wf_client.go`), which the node answers
  with a `COUNT(*)` and no rows; a count that is not a whole number from 0 is a count that
  failed (`instanceCount`). A node with a config is deleted only once it counted none
  (`instancesUnconfirmed`). Before 0.1.137 it asked for the whole list, which passed the frame
  past about 350 running instances: the node answered `INTERNAL_ERROR`, and a busy workflow's
  delete `INSTANCES_UNKNOWN` (FINDINGS A-284). `limit` stays 0 for an engine before 0.1.137,
  which ignores `count_only` and lists every running instance, so its count is still the whole
  count during a deploy (the spec's §5.7). A node that crashed, was stopped or is restarting
  keeps its instances in its store, which the kill's purge removes: before 0.1.133 (A-270) such a
  node was taken for one with none, and a delete a second after a `restart_node` read `STARTING`
  and purged them.
- A workflow whose WF node has no config (never deployed, or already purged) has none to lose,
  and is deleted without a node step.
- If the node exists it is removed with `kill_node` (`force: true`, `purge_instance: true`)
  through SY.admin; then the workflow directory is deleted, and the versions of `wf.<workflow>`
  are removed from the motherbee's dist (`purgeWorkflowPackages`, through SY.admin's
  `remove_runtime_version`, each removal waiting up to 35 s: see the deploy's step 3): every
  version this hive's dist lists, and each version its slots named (staged, backup, current,
  read before the directory goes: `slotPackageVersions`), the current last. On a worker this
  hive's dist is a Syncthing copy of the motherbee's, which lists a version published there
  10-60 s later; before 0.1.133 a delete within that window removed nothing and answered
  `packages_left: []`. A version the motherbee does not hold (a staged one never applied) SY.admin
  answers `RUNTIME_VERSION_NOT_FOUND` before its read of the version's use, which counts as
  removed. A failure up to the directory's deletion (the config read, the kill, the deletion
  itself) fails the delete as before, with its code.
- The answer: `{status: ok, deleted: true, wf_node: {node_name, action, status, reason},
  packages_left: [...], warning}`. Once the node and the directory are gone the workflow is
  deleted (asked again, the delete answers `WORKFLOW_NOT_FOUND`), so since 0.1.133 a purge that
  cannot finish does not fail it: the answer names what stayed. The purge stops at the first
  removal SY.admin refuses or does not answer in time, and `packages_left` names each version it
  did not remove, `{runtime, version, error_code, detail, retryable, unanswered_hives}`:
  - the one it stopped at, with SY.admin's code and detail: `RUNTIME_USAGE_UNKNOWN` while a hive
    that may run the version cannot be read (a worker down or restarting, a join under way;
    FINDINGS A-281), or `RUNTIME_IN_USE` while a node runs it; or `TIMEOUT` when no answer came
    in time (SY.admin may still remove it: `remove_runtime_version` then answers
    `RUNTIME_VERSION_NOT_FOUND`), or `TRANSPORT_ERROR` when SY.admin was not reached;
  - each one it had still to remove, `PURGE_STOPPED`;
  - or, when its versions could not be listed, one entry with the runtime, no version
    (`version: ""`) and `DIST_UNREADABLE`: nothing was removed.

  `retryable` says that `remove_runtime_version` asked again for that version, nothing changed
  first, may remove it: SY.admin's own word when its refusal gives one (`payload.retryable` of
  `RUNTIME_USAGE_UNKNOWN`: false while a hive not read is a join that ended and when the
  motherbee could not read its own nodes or its registry, true otherwise); without it, true for
  `RUNTIME_USAGE_UNKNOWN` and `INVENTORY_UNAVAILABLE`; true for `TIMEOUT`, `TRANSPORT_ERROR` and
  `PURGE_STOPPED`, but a `PURGE_STOPPED` after a `RUNTIME_USAGE_UNKNOWN` that is not retryable is
  not either, since its removal reads the same hives (`purgeStoppedAt`); false for any other
  refusal (`RUNTIME_IN_USE`: stop or move the node first) and for `DIST_UNREADABLE`.
  `unanswered_hives` is SY.admin's list of the hives whose nodes it could not read, each
  `{hive, reason, detail}`, when its refusal names any (`RUNTIME_USAGE_UNKNOWN`, and
  `RUNTIME_IN_USE` with a hive not read besides).

  `packages_left` is `[]` when every version the delete knew of went (those this hive's dist
  lists and those its slots named), and is always there: an answer without it is from an
  SY.wf-rules before 0.1.133. On a worker whose copy lags, the motherbee's dist may hold another
  version neither names (an older leftover of the same name): `GET
  /hives/motherbee/runtimes/wf.<workflow>` lists them. With any left, `warning` says the workflow
  is deleted, how many versions of `wf.<workflow>` stayed in the motherbee's dist and where the
  purge stopped, and that `remove_runtime_version` removes each once every hive answers and no
  node runs it, and a later deploy of the same name purges them too; with `DIST_UNREADABLE`, that
  this hive's `dist/runtimes/wf.<workflow>` must be fixed first, since until it can be listed a
  compile of the name that names no version is refused too. To remove them by hand, go in
  `packages_left`'s order, which puts the runtime's current last as far as this hive's copy of
  the manifest tells: SY.admin refuses the current one (`RUNTIME_CURRENT_CONFLICT`) while another
  version remains, so one refused so goes after the others. Nothing removes them in the
  background. Before 0.1.133 such a delete answered `PACKAGE_PUBLISH_FAILED` though the
  workflow, its node and its state were gone: the factory reset counted it as failed, and running
  it again never removed the versions left, since the workflow was no longer listed; deployed
  again, the name published its `0.0.1` over the one left there and was refused (see
  [The workflow store](#the-workflow-store)).
  SY.admin's `wf_rules_delete` passes the answer on whole as its `payload` (adding
  `pages_left_up`), over HTTP, over the mesh and as an Archi plan step's `result_preview`; a
  plan's answer cut to fit the frame keeps `packages_left` in short (each version's `runtime`,
  `version`, `error_code` and `retryable`, the first one's `detail` cut, the hives not read with
  their `reason` and `next`, in 1 KiB: SY.admin's `packages_left_essentials`).

### Queries

- `get_workflow {workflow_name}`: the current definition, version, hash, `compiled_at`.
- `list_workflows`: per workflow: current version and hash, published version (from the local
  `dist/runtimes/manifest.json`), deployed version, whether the WF node runs and answers, its
  health and active instances.
- `get_status {workflow_name}`: the same for one workflow plus `staged_version` and the WF node
  block.

## Messages and who may send them

`handleMessage` in `node/service.go` and `node/dispatch.go`:

| Message | Content | Who may send |
| --- | --- | --- |
| `CONFIG_SET` (`SYSTEM`) | `config.operation`: `compile`, `apply`, `rollback`, `compile_apply`, with `workflow_name`, `definition`, `version`, `auto_spawn`, `tenant_id`, `launched_by`; or `check`, read-only, with `check_id`, `workflow_name` and `definition` (see Check) | `SY.admin@motherbee` only |
| `CONFIG_GET` (`SYSTEM`) | returns `hive_id`, `state_dir`, the supported operations | `SY.admin@motherbee` only |
| `command` `delete_workflow` | `{workflow_name, force}`; answered `{status, deleted, wf_node, packages_left, warning}` (see Delete) | `SY.admin@motherbee` only |
| `command` `compile_workflow`, `apply_workflow`, `rollback_workflow` | as the operations above | `SY.admin@motherbee` only |
| `query` `get_workflow`, `list_workflows`, `get_status` | see above | `SY.admin@motherbee` only |
| `NODE_STATUS_GET` (`SYSTEM`) | answered `HEALTHY` | anyone the router lets through |

The origin check is `fromAdmin`: the router-stamped sender must be `SY.admin@motherbee`
(`PrimaryAdminNode` in `go/fluxbee-go-sdk/protocol.go`); otherwise the answer is
`UNAUTHORIZED`. For `CONFIG_GET`/`CONFIG_SET` the router's system policy already allows only that
sender (rule 2 of `policy/system.rego`).

In the other direction, SY.admin's A-62 table bounds what `SY.wf-rules@<hive>` may ask of it
(`authorize_mesh_caller`; nothing at all from an ingress or egress hive):

- the node actions `run_node`, `restart_node`, `kill_node`, `set_node_config`,
  `get_node_config` and `get_node_status`, only for a `WF.*` node of that same hive, and only to
  bind or launch `wf.<that workflow>` on base `wf.engine` with a package path under
  `/var/lib/fluxbee/dist/runtimes/wf.<workflow>/` (`authorize_wf_rules_node`);
- `publish_runtime_package` of the inline package it builds (`type: workflow`,
  `runtime_base: wf.engine`, `name: wf.<workflow>`, never `wf.engine` itself) and
  `remove_runtime_version` of a `wf.<workflow>`, each with only the fields SY.wf-rules sends, and
  both refused while a `WF.<workflow>` is connected on another hive, which SY.admin reads from
  shared memory (`authorize_wf_rules_package`, `workflow_nodes_elsewhere`);
- `sync_hint` of `fluxbee-dist-runtimes` on the motherbee (`announceRuntimes`), with only its own
  fields and a `timeout_ms` of at most 5000.

A workflow's package is one runtime for every hive, `wf.<workflow>`, and SY.admin sees another
hive's `WF.<workflow>` only while it is connected: one that is stopped, or on a hive the LSA holds
stale, is not seen (FINDINGS A-304). A deploy of the same name here then stages above that hive's
versions too (`highestPublishedVersion` counts every version of `wf.<workflow>` the dist holds),
and its purge, as a delete here, asks SY.admin to remove them: SY.admin removes only a version no
node of any hive may run on (FINDINGS A-281), and a node whose unit does not run is not counted.
Before 0.1.133 such a deploy published `0.0.1`, which SY.admin refused (`VERSION_CONFLICT`) while
that hive still held its own `0.0.1`.

This is the standard path of A-68 (see `docs/wf-standard-path-v1.md`): SY.wf-rules acts on nodes
only through SY.admin.

## SY.admin HTTP routes

On the motherbee (`127.0.0.1:8080`). The admin sends the four write operations as a
`CONFIG_SET` with `operation` and `launched_by: operator`, the check as a `CONFIG_SET` with
`operation: check` and a fresh `check_id` (and no version), delete as the `delete_workflow`
command, and reads as queries, all to `SY.wf-rules@<hive in the URL>`
(`handle_wf_rules_http`, `send_wf_rules_check`, `handle_wf_rules_query`). Of a check's answers it
takes only the one that echoes its `check_id` ([SY.admin](sy-admin.md#checking-a-definition)).

| Route | Admin action |
| --- | --- |
| `POST /hives/{hive}/wf-rules` body `{workflow_name, definition, auto_spawn, tenant_id}` | `wf_rules_compile_apply` |
| `POST /hives/{hive}/wf-rules/check` body `{workflow_name, definition}` | `wf_rules_check` |
| `POST /hives/{hive}/wf-rules/compile` | `wf_rules_compile` |
| `POST /hives/{hive}/wf-rules/apply` body `{workflow_name, version?, auto_spawn?, tenant_id?}` | `wf_rules_apply` |
| `POST /hives/{hive}/wf-rules/rollback` | `wf_rules_rollback` |
| `POST /hives/{hive}/wf-rules/delete` body `{workflow_name, force?}` | `wf_rules_delete` |
| `POST /hives/{hive}/wf-rules/check` body `{workflow_name, definition}` | `wf_rules_check` |
| `GET /hives/{hive}/wf-rules` (`?workflow_name=` for one) | `wf_rules_list_workflows` / `wf_rules_get_workflow` |
| `GET /hives/{hive}/wf-rules/status?workflow_name=` | `wf_rules_get_status` |

The same operations exist without the hive prefix (`/wf-rules`, `/wf-rules/compile`, ...): the
hive then comes from a `hive` (or `target`) field in the body or the query string, and defaults
to the admin's own hive (`WfRulesRequest`, `wf_rules_request_from_query`).
Example definitions ship in `go/nodes/wf/examples/packages/wf.invoice/flow/definition.json`,
`.../wf.router/flow/definition.json` and `.../wf.sales.invoice/flow/definition.json`.

## The WF engine runtime (`wf.engine`)

`wf-generic` (`go/nodes/wf/wf-generic/`) is the binary every WF node runs. It is baked into the
.deb as the runtime `wf.engine` with `boot: false` (`packaging/base-nodes.json`); the
orchestrator refuses `run_node` of a `WF.*` node on a runtime that is not a workflow package
(`WF_RUNTIME_PACKAGE_REQUIRED` in `src/bin/sy_orchestrator.rs`).

- **Start.** The orchestrator launches it with the node name in `FLUXBEE_NODE_NAME`; the config
  is `config.json` in the node's managed directory (`main.go`). With `_system.package_path` set
  (managed package mode) the definition is the package's `flow/definition.json`
  (`Config.PackageDefinitionPath`).
- **Config** (`OperatorConfig` in `node/config.go`, unknown keys refused):
  `workflow_definition_path`, `db_path` (default `wf_instances.db` next to `config.json`),
  `sy_timer_l2_name` (`SY.timer@<hive>`, the default and the only value it takes), `tenant_id`, `gc_retention_days` (default 7),
  `gc_interval_seconds` (default 3600). A config or definition it cannot use leaves the node up
  in `FAILED_CONFIG`, answering control messages only.
- **Run.** It opens the SQLite instance store, keeps its definition by hash, recovers running
  instances (each on the definition it was born with), runs again the inbound events a crash
  left unfinished, reconciles its timers with SY.timer and starts a GC loop (`startEngine`,
  `node/node.go`; `Recover`, `node/recover.go`). When SY.timer does not answer, or the timer index
  cannot be read, it starts `DEGRADED` and reconciles later. When the store does not open, or fails
  during recovery, the
  engine does not run and the node stays up (FINDINGS A-217): `DEGRADED` with `store_unavailable`,
  or `FAILED_CONFIG` `invalid_config` when the `db_path` itself cannot hold the store
  (`storeFailure`; `dbPathProblem` and `pathFault` in `node/store.go`, a read-only file system
  being the disk's fault); it refuses every event (`NOT_TAKEN`
  `unavailable`, `refuseWhileStoreDown`) and opens the store again every 30 s. Timers use
  `client_ref` `wf:<instance_id>::<timer_key>` (`node/timer.go`), from 60 s to 366 days ahead.
  A schedule or a reschedule goes without waiting, and is kept until SY.timer answers: one
  SY.timer does not take (the router's `UNREACHABLE` or `TTL_EXCEEDED`, SY.timer's refusal, no
  answer in 30 s, a send that did not go) marks its timer in the index, which goes again after 5,
  10, 20 and 40 s, then each minute, until SY.timer takes it, the node `DEGRADED` meanwhile; one
  less than 60 s from its time is fired by the node itself, and one SY.timer holds at a later time
  (a move it did not take) is moved to its time (`node/timer_requests.go`, FINDINGS A-254 and its
  review).
- **Messages** (`Dispatch`, `node/dispatch.go`): `NODE_STATUS_GET`, `CONFIG_GET`, `CONFIG_SET`;
  `WF_HELP`, `WF_GET_INSTANCE`, `WF_LIST_INSTANCES` (with `count_only`, only how many, a
  `COUNT(*)`; a payload it cannot read, or a status no instance has, `INVALID_REQUEST`),
  `WF_GET_CODE`, `WF_CANCEL_INSTANCE`, answered only to SY.admin@motherbee and SY.wf-rules of the
  node's hive (`UNAUTHORIZED` otherwise); `TIMER_FIRED`, taken only from SY.timer of the node's
  hive, and its `TIMER_RESPONSE` to the node's own timer requests, on a channel of its own
  (`wfRouteProfile`). Any other message is taken only from a node the definition names
  (`start_from`, an `event_match.src`, a target of its sends), and reaches the running instance
  on its thread or, from a `start_from` node, starts a new one on it (`dispatchWorkflowEvent`,
  `node/correlate.go`); a reply (`*_RESPONSE`, `NOT_TAKEN`, `<msg>.error`) never starts one, and
  a notice or a reply is never answered. A message it takes nowhere gets `NOT_TAKEN`, with its
  reason (`no_thread`, `no_conversation`, `invalid_input`, `not_expected` or `unavailable`): from
  an `IO.*` node, any message; from any other node, only a named request, and only `no_thread`,
  `invalid_input` and `unavailable`.

## Config

SY.wf-rules has no operator config: `CONFIG_SET` carries operations, not settings. Inputs are
`/etc/fluxbee/hive.yaml` (`hive_id`) and the constants in `node/config.go`.

## Failure behavior

- Lock held, no identity SHM entry within 30 s, or no router: the process exits; systemd
  restarts it every 5 s.
- A package that cannot be published fails the request (`PACKAGE_PUBLISH_FAILED`) after the
  slots already rotated: `current` holds the new version.
- A node step that fails after publishing refuses the deploy, `WF_NODE_NOT_DEPLOYED` (SY.admin
  502), with `wf_node.action` `restart_failed` (or `none`), the `warning` and what is current;
  re-run apply once the cause is fixed.
- `ORCHESTRATOR_ERROR` when SY.admin or the orchestrator refuses a node's config read
  (`get_node_config`) or a delete's `kill_node`, and when a delete's config read does not answer
  in time (whether the node exists is unknown; nothing changes, with `force` or without). A
  status read it refuses makes a delete without `force` `INSTANCES_UNKNOWN`, naming the refusal.
- A delete whose purge cannot remove every version of `wf.<workflow>` is done all the same and
  answers ok, naming each version left in `packages_left` with why and whether asking again may
  remove it (`retryable`), and a `warning` (see Delete). They stay in the motherbee's dist until
  `remove_runtime_version` removes them, once every hive answers and no node runs them, or a later
  deploy of the same name purges them. An apply's or a rollback's purge that cannot finish only
  logs what it left.
- A compile that cannot read this hive's dist or its runtimes manifest stages nothing
  (`WFRULES_ERROR`): the next version, above what they hold, is unknown.

## Health

- `systemctl status sy-wf-rules`; `journalctl -u sy-wf-rules` (`resolved self system ILK from
  identity SHM`; `sync_hint on motherbee: ...` when a worker's announce fails; `purge left
  wf.<workflow> <version>: <code>: <detail>` or `purge of wf.<workflow> skipped: ...` when an
  apply's or a rollback's purge left versions).
- `curl -s 127.0.0.1:8080/hives/<hive>/wf-rules` lists the workflows with their WF node state.
- A WF node: `GET /hives/<hive>/nodes/WF.<workflow>@<hive>/status`.
- `fluxbee-factory-reset` removes workflows with `POST /hives/<hive>/wf-rules/delete` and
  `force: true` on every motherbee and worker hive (`packaging/fluxbee-factory-reset`), and
  prints each version a delete names in `packages_left` (`left in the motherbee's dist: ...`),
  which a re-run does not remove, and counts them in the line it logs
  (`state/factory-reset.log`: `packages_left=<n>`, beside `problems=<n>`).

## Links

- [SY.timer](sy-timer.md): timers of WF instances.
- [Operations](../reference/operations.md): runtime publishing and dist sync.
- `docs/wf-standard-path-v1.md`, `docs/wf-rules-quickstart.md` (older design notes).
