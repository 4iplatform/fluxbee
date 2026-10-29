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

`SY.wf-rules` is in the `system_nodes.motherbee` and `system_nodes.worker` lists of
`/etc/fluxbee/hive.yaml`, but in neither `wait_for` list. Each hive's SY.wf-rules manages the
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

- Workflow names match `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$` (`workflowNamePattern`);
  anything else is `INVALID_WORKFLOW_NAME`.
- `metadata.json` (`WfRulesMetadata`): `version` (a counter), `hash` (`sha256:` of the
  definition), `workflow_name`, `workflow_type`, `wf_schema_version`, `compiled_at`,
  `guard_count`, `state_count`, `action_count`.
- When a compile does not give a version, the next one is the highest of the three slots plus 1
  (`NextVersion`).
- Apply moves `current` to `backup` and `staged` to `current` (`RotateToApply`). Rollback moves
  `backup` to `current` and the old `current` to `staged` (`RotateToRollback`).

## Operations

### Compile

`CompileWorkflow` (`node/compile.go`) checks the name, loads and validates the definition with
`wfcel.LoadDefinitionBytes` (`COMPILE_ERROR` on failure), and writes it to `staged`. Nothing
is published or deployed.

### Apply and rollback (the deploy)

`ApplyWorkflowAndDeploy` / `RollbackWorkflowAndDeploy` (`node/deploy.go`):

1. Rotates the slots (apply needs a staged version, else `NOTHING_STAGED`; with an explicit
   `version` it must match the staged one, else `VERSION_MISMATCH`; rollback needs a backup,
   else `NO_BACKUP`).
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
   Syncthing mirror.
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

   The answer carries `wf_node` (`{node_name, action, status, reason, error}`) and a `warning`
   when the package was published but the node step failed. The rule change stands either way.

On a worker the package has to arrive by Syncthing before the node can use it. SY.wf-rules asks
SY.admin for a `sync_hint` on the motherbee's `fluxbee-dist-runtimes` folder before the node
calls (`announceRuntimes`), and retries a node call refused with `RUNTIME_NOT_AVAILABLE`,
`RUNTIME_NOT_PRESENT` or `BASE_RUNTIME_NOT_AVAILABLE` for up to 45 s (`runtimeSyncBudget`,
`untilRuntimeSynced`). A whole request is bounded by 60 s (`wfRulesRequestBudget`); SY.admin
waits 75 s for the answer (`wf_rules_request_timeout`, overridable with
`JSR_ADMIN_WF_RULES_TIMEOUT_SECS`).

### Delete

`DeleteWorkflow` (`node/delete.go`):

- `WORKFLOW_NOT_FOUND` if no slot exists.
- Without `force`, it refuses while the WF node has running instances (`INSTANCES_ACTIVE`) or
  when it cannot tell (`INSTANCES_UNKNOWN`: the node's status timed out or its instance query
  failed). It counts instances by asking the WF node `WF_LIST_INSTANCES` with
  `status: running` directly (2 s timeout, `node/wf_client.go`).
- If the node exists it is removed with `kill_node` (`force: true`, `purge_instance: true`)
  through SY.admin; then the workflow directory is deleted and every published version of
  `wf.<workflow>` is removed from the motherbee's dist.

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
| `CONFIG_SET` (`SYSTEM`) | `config.operation`: `compile` (also `check`), `apply`, `rollback`, `compile_apply`, with `workflow_name`, `definition`, `version`, `auto_spawn`, `tenant_id`, `launched_by` | `SY.admin@motherbee` only |
| `CONFIG_GET` (`SYSTEM`) | returns `hive_id`, `state_dir`, the supported operations | `SY.admin@motherbee` only |
| `command` `delete_workflow` | `{workflow_name, force}` | `SY.admin@motherbee` only |
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

This is the standard path of A-68 (see `docs/wf-standard-path-v1.md`): SY.wf-rules acts on nodes
only through SY.admin.

## SY.admin HTTP routes

On the motherbee (`127.0.0.1:8080`). The admin sends the four write operations as a
`CONFIG_SET` with `operation` and `launched_by: operator`, delete as the `delete_workflow`
command, and reads as queries, all to `SY.wf-rules@<hive in the URL>`
(`handle_wf_rules_http`, `handle_wf_rules_query`).

| Route | Admin action |
| --- | --- |
| `POST /hives/{hive}/wf-rules` body `{workflow_name, definition, auto_spawn, tenant_id}` | `wf_rules_compile_apply` |
| `POST /hives/{hive}/wf-rules/compile` | `wf_rules_compile` |
| `POST /hives/{hive}/wf-rules/apply` body `{workflow_name, version?, auto_spawn?, tenant_id?}` | `wf_rules_apply` |
| `POST /hives/{hive}/wf-rules/rollback` | `wf_rules_rollback` |
| `POST /hives/{hive}/wf-rules/delete` body `{workflow_name, force?}` | `wf_rules_delete` |
| `GET /hives/{hive}/wf-rules` (`?workflow_name=` for one) | `wf_rules_list_workflows` / `wf_rules_get_workflow` |
| `GET /hives/{hive}/wf-rules/status?workflow_name=` | `wf_rules_get_status` |

The same operations exist without the hive prefix (`/wf-rules`, `/wf-rules/compile`, ...): the
hive then comes from a `hive` (or `target`) field in the body or the query string, and defaults
to the admin's own hive (`WfRulesRequest`, `wf_rules_request_from_query`).
Example definitions ship in `go/nodes/wf/examples/packages/wf.invoice/flow/definition.json`
and `.../wf.router/flow/definition.json`.

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
  `node/node.go`; `Recover`, `node/recover.go`). When SY.timer does not answer it starts
  `DEGRADED` and reconciles later. Timers use `client_ref` `wf:<instance_id>::<timer_key>`
  (`node/timer.go`).
- **Messages** (`Dispatch`, `node/dispatch.go`): `NODE_STATUS_GET`, `CONFIG_GET`,
  `CONFIG_SET`; `WF_HELP`, `WF_GET_INSTANCE`, `WF_LIST_INSTANCES`, `WF_GET_CODE`,
  `WF_CANCEL_INSTANCE`, answered only to SY.admin@motherbee and SY.wf-rules of the node's hive
  (`UNAUTHORIZED` otherwise); `TIMER_FIRED`, taken only from SY.timer of the node's hive. Any
  other message is correlated to an existing instance or starts a new one
  (`dispatchWorkflowEvent`, `node/correlate.go`); a reply (`*_RESPONSE`, `INVALID_INPUT`,
  `<msg>.error`) never starts one, and a notice or a reply is never answered.

## Config

SY.wf-rules has no operator config: `CONFIG_SET` carries operations, not settings. Inputs are
`/etc/fluxbee/hive.yaml` (`hive_id`) and the constants in `node/config.go`.

## Failure behavior

- Lock held, no identity SHM entry within 30 s, or no router: the process exits; systemd
  restarts it every 5 s.
- A package that cannot be published fails the request (`PACKAGE_PUBLISH_FAILED`) after the
  slots already rotated: `current` holds the new version.
- A node step that fails after publishing returns `status: ok` with `wf_node.action:
  restart_failed` (or `none`) and a `warning`; re-run apply once the cause is fixed.
- `ORCHESTRATOR_ERROR` when SY.admin or the orchestrator refuses a node query or a delete.

## Health

- `systemctl status sy-wf-rules`; `journalctl -u sy-wf-rules` (`resolved self system ILK from
  identity SHM`, and `sync_hint on motherbee: ...` when a worker's announce fails).
- `curl -s 127.0.0.1:8080/hives/<hive>/wf-rules` lists the workflows with their WF node state.
- A WF node: `GET /hives/<hive>/nodes/WF.<workflow>@<hive>/status`.
- `fluxbee-factory-reset` removes workflows with `POST /hives/<hive>/wf-rules/delete` and
  `force: true` on every motherbee and worker hive (`packaging/fluxbee-factory-reset`).

## Links

- [SY.timer](sy-timer.md): timers of WF instances.
- [Operations](../reference/operations.md): runtime publishing and dist sync.
- `docs/wf-standard-path-v1.md`, `docs/wf-rules-quickstart.md` (older design notes).
