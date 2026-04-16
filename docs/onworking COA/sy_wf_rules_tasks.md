# SY.wf-rules — Implementation Tasks

**Status:** planning / ready for implementation
**Date:** 2026-04-16
**Primary spec:** `docs/sy-wf-rules-spec.md`
**Target module:** `go/sy-wf-rules/`
**Reference models:** `go/sy-opa-rules/main.go`, `docs/sy-wf-rules-spec.md`

---

## 1) Goal

Implement `SY.wf-rules`, the system node that manages workflow source definitions for the `WF.*` family.

The node:

- receives workflow definition JSON via L2 (`CONFIG_SET`, command messages, query messages)
- validates definitions using the shared CEL validation package
- manages `staged/`, `current/`, `backup/` state per workflow
- materializes and publishes workflow runtime packages in `dist`
- coordinates explicit rollout with `SY.orchestrator`
- never writes under `/var/lib/fluxbee/nodes/`
- exposes query/status operations and an Admin surface proxied by `SY.admin`

One binary, one instance per hive, managing all workflows for that hive.

---

## 2) Frozen decisions

- **Source of truth:** workflow source/versioning lives in `SY.wf-rules`.
- **Runtime execution model:** `WF.<workflow_name>@<hive>` runs as runtime `wf.<workflow_name>`, with `runtime_base = "wf.engine"`.
- **WF code delivery:** `wf-generic` reads `flow/definition.json` from `_system.package_path` at boot.
- **No inline workflow code in managed config:** `workflow_definition` is not persisted in node `config.json`.
- **Instance binding:** `_system.runtime_version` / `_system.package_path` is fixed until explicit rollout.
- **Publication vs deployment:** publishing a newer package to `dist` does not deploy it by itself.
- **Crash/autorestart semantics:** if a WF node crashes before explicit rebind/restart completes, it restarts on the previously persisted binding.
- **Operations:** `compile`, `compile_apply`, `apply`, `rollback`, `delete_workflow`, `get_workflow`, `get_status`, `list_workflows`.
- **`check` alias:** silent synonym for `compile`.
- **State model:** one `current`, one `staged`, one `backup` per workflow.
- **No SHM:** this node does not use SHM for workflow delivery.
- **Package publication path:** reuse the standard Fluxbee install/publish implementation through code or a structured internal command path. No shell scripts.
- **Lifecycle ownership:** `SY.orchestrator` owns managed config, spawn/kill, and `/var/lib/fluxbee/nodes/...`.
- **Admin ownership:** `SY.admin` is gateway only. `SY.wf-rules` does compile/apply/publish work.
- **Delete semantics:** managed instance cleanup goes through orchestrator/admin lifecycle, not direct filesystem deletion.
- **Retention policy v1:** keep `current`, keep `backup`, keep any package version still referenced by a live or persisted node instance.
- **Language:** Go.

---

## 3) Implementation workstreams

Implementation is split into six workstreams:

1. Shared CEL validation package
2. `SY.wf-rules` node and filesystem state
3. Workflow package publication in `dist`
4. Orchestrator rollout / managed config binding
5. Query/Admin surface
6. Tests and installation

The task order at the end reflects these dependencies.

---

## 4) Shared CEL package (`go/pkg/wfcel`)

### WFRULES-CEL-1 — Create module
- [x] Create `go/pkg/wfcel/go.mod`
- [x] Module path: `github.com/4iplatform/json-router/pkg/wfcel`
- [x] Add dependencies required for CEL environment and JSON validation

### WFRULES-CEL-2 — WorkflowDefinition types
- [x] Define minimal types needed to validate workflow definitions:
  - `WorkflowDefinition`
  - `StateDefinition`
  - `TransitionDefinition`
  - `ActionDefinition`
- [x] Keep the type surface aligned with `wf-v1.md`
- [x] Support JSON marshal/unmarshal

### WFRULES-CEL-3 — CEL environment builder
- [x] Build `cel.Env` with `input`, `state`, `event`
- [x] Register `now()` helper as specified for WF v1 validation
- [x] Make environment construction reusable by both `sy-wf-rules` and `wf-generic`

### WFRULES-CEL-4 — Guard compilation
- [x] Implement `CompileGuard`
- [x] Compile all guards during definition validation
- [x] Return path-aware validation errors like `states[1].transitions[0].guard`

### WFRULES-CEL-5 — Full definition validation
- [x] Implement the 11 checks from `docs/sy-wf-rules-spec.md` section 6
- [x] Include JSON Schema validation for `input_schema`
- [x] Validate action types, timer durations, `$ref` roots, target states, and L2 names
- [x] Return `[]ValidationError{Path, Message}`

### WFRULES-CEL-6 — Tests
- [x] Valid definition passes
- [x] Each validation rule has at least one failing test
- [x] CEL typo returns correct path
- [x] `now()` is available

---

## 5) Module setup (`go/sy-wf-rules/`)

### WFRULES-SETUP-1 — Create module
- [x] Create `go/sy-wf-rules/go.mod`
- [x] Add deps:
  - `fluxbee-go-sdk`
  - `github.com/google/uuid`
  - shared package `pkg/wfcel`
- [x] Add local `replace` directives as needed

### WFRULES-SETUP-2 — Create scaffold
- [x] Create initial layout (filenames differ from original plan but all functionality is covered):

```text
go/sy-wf-rules/
├── go.mod
├── main.go
└── node/
    ├── config.go        (NodeConfig + RuntimeConfig)
    ├── service.go       (Service + Run/RunWithContext)
    ├── dispatch.go      (handleSystemMessage, handleCommand, handleQuery)
    ├── types.go         (request/response/payload types)
    ├── compile.go       (CompileWorkflow)
    ├── apply.go         (ApplyWorkflow)
    ├── deploy.go        (ApplyWorkflowAndDeploy, RollbackWorkflowAndDeploy)
    ├── rollback.go      (RollbackWorkflow)
    ├── delete.go        (DeleteWorkflow)
    ├── store.go         (Store, filesystem state)
    ├── orchestrator.go  (orchestratorClient interface + l2OrchestratorClient)
    ├── managed_config.go (buildManagedWFConfig)
    ├── package_publish.go (PublishWorkflowPackage, PurgeOldPackages)
    ├── wf_client.go     (wfNodeClient, CountRunningInstances)
    ├── query.go         (GetWorkflow, GetWorkflowStatus, ListWorkflowStatuses)
    └── status.go        (WorkflowStatusView, WFNodeStatus)
```

### WFRULES-SETUP-3 — Common config/state types
- [x] Define node config:
  - `hive_id`
  - `state_dir`
  - `orchestrator_target`
  - package publish dependencies / client
- [x] Define shared request/response structs

---

## 6) Filesystem state management

### WFRULES-STORE-1 — Directory layout
- [x] State root: `/var/lib/fluxbee/wf-rules/<workflow_name>/`
- [x] Manage subdirs:
  - `staged/`
  - `current/`
  - `backup/`
- [x] Create lazily on first compile for a workflow

### WFRULES-STORE-2 — Metadata
- [x] Define metadata struct matching the spec:
  - `version`
  - `hash`
  - `workflow_name`
  - `workflow_type`
  - `wf_schema_version`
  - `compiled_at`
  - `guard_count`
  - `state_count`
  - `action_count`
  - `error_detail`
- [x] `hash` uses `sha256:<hex>`

### WFRULES-STORE-3 — Atomic writes
- [x] Write `definition.json` and `metadata.json` atomically
- [x] Use temp file + rename semantics
- [x] Keep implementation simple and deterministic

### WFRULES-STORE-4 — Version allocation
- [x] Auto-increment logical version per workflow
- [x] Read latest known version from `staged`, then `current`, then `backup`
- [x] Start at `1` when empty

### WFRULES-STORE-5 — Read helpers
- [x] `ReadCurrentDefinition`
- [x] `ReadCurrentMetadata`
- [x] `ReadStagedDefinition`
- [x] `ReadStagedMetadata`
- [x] `ReadBackupDefinition`
- [x] `ReadBackupMetadata`
- [x] `WorkflowExists`
- [x] `ListWorkflows`

### WFRULES-STORE-6 — Apply rotation
- [x] Implement `RotateToApply(workflowName)`
- [x] Behavior:
  - delete old `backup/` if present
  - rename `current/` to `backup/` if present
  - rename `staged/` to `current/`

### WFRULES-STORE-7 — Rollback rotation
- [x] Implement rollback rotation:
  - delete `staged/` if present
  - rename `current/` to `staged/`
  - rename `backup/` to `current/`

### WFRULES-STORE-8 — Delete state
- [x] `DeleteWorkflowState(workflowName)`
- [x] Removes only `/var/lib/fluxbee/wf-rules/<workflow_name>/`

---

## 7) Package publication in `dist`

### WFRULES-PKG-1 — Package model
- [x] Define workflow package shape:

```text
/var/lib/fluxbee/dist/runtimes/wf.<workflow_name>/<version>/
├── package.json
├── flow/
│   └── definition.json
└── config/
    └── default-config.json   # optional
```

- [x] Ensure `package.json` includes runtime metadata required by the standard runtime model
- [x] Runtime name is `wf.<workflow_name>`
- [x] Version equals logical workflow version
- [x] `runtime_base = "wf.engine"`

### WFRULES-PKG-2 — Materialize package contents
- [x] Build package directory contents from `current/definition.json` + metadata
- [x] Generate `flow/definition.json`
- [x] Generate `package.json`
- [x] Support optional `config/default-config.json` if needed

### WFRULES-PKG-3 — Publish through standard install/publish path
- [x] Publish via direct Go filesystem I/O (no shell scripts) in `package_publish.go`
- [x] Return `PACKAGE_PUBLISH_FAILED` on failure
- Note: there is no shared platform-level install/publish library yet; `package_publish.go` is the canonical implementation for WF packages. If a shared library is introduced later, migrate then.

### WFRULES-PKG-4 — Publication result model
- [x] Return structured publication result:
  - runtime name
  - published version
  - package path
  - success/failure detail

### WFRULES-PKG-5 — Retention and purge
- [x] Enumerate versions under `dist/runtimes/wf.<workflow_name>/`
- [x] Keep:
  - version referenced by `current/`
  - version referenced by `backup/`
  - any version still referenced by a live or persisted WF node instance
- [x] Purge older unreferenced versions
- [x] Never purge a version still bound in managed config

### WFRULES-PKG-6 — Tests
- [x] Publish creates expected package layout
- [x] Version number matches workflow metadata version
- [x] Purge keeps `current`
- [x] Purge keeps `backup`
- [x] Purge keeps externally referenced version
- [x] Purge deletes safe stale version

---

## 8) Orchestrator integration

### WFRULES-ORCH-1 — Node status client
- [x] Node existence is determined via `GetNodeConfig` → `NODE_CONFIG_NOT_FOUND` error (implemented in `orchestrator.go`)
- [ ] **PENDING:** Add `GetNodeStatus(nodeL2Name string)` to detect if the WF node *process* is actively running (distinct from config existing). Send `NODE_STATUS_GET` directly to the WF node with 2s timeout. Used by `get_status` and `delete_workflow` to report `wf_node.running`. Currently `running` is inferred from `CountRunningInstances` timeout behavior.

### WFRULES-ORCH-2 — Managed config read client
- [x] Implement `GetNodeConfig(nodeL2Name string)`
- [x] Read current managed config for existing WF node
- [x] Use this only to preserve operational fields and inspect current binding

### WFRULES-ORCH-3 — Managed config update client
- [x] Implement `SetNodeConfig(nodeL2Name string, config map[string]any)`
- [x] This is the rebind step for an existing WF node
- [x] Managed config must carry:
  - operational fields
  - `_system.runtime`
  - `_system.requested_version`
  - `_system.runtime_version`
  - `_system.runtime_base`
  - `_system.package_path`

### WFRULES-ORCH-4 — Run node client
- [x] Implement `RunNode(nodeL2Name, runtimeName, version string, config map[string]any)`
- [x] Spawn path uses runtime `wf.<workflow_name>`
- [x] Initial spawn path must produce a managed node with fixed package binding

### WFRULES-ORCH-5 — Kill node client
- [x] Implement `KillNode(nodeL2Name string)`
- [x] Tolerate `NODE_NOT_FOUND`

### WFRULES-ORCH-6 — Build managed WF config
- [x] Implement builder for managed WF config
- [x] Preserve operational fields from existing config when present:
  - `tenant_id`
  - `sy_timer_l2_name`
  - `gc_retention_days`
  - `gc_interval_seconds`
  - other node-owned operational fields
- [x] Do not embed workflow source in config
- [x] Always bind the concrete package version in `_system`

### WFRULES-ORCH-7 — Explicit rollout sequence
- [x] Implement rollout helper for existing WF node:
  1. publish package
  2. read existing managed config
  3. build rebound config with new concrete version
  4. `set_node_config`
  5. `restart_node`
- [x] Retry `restart_node` once after 1 second if needed
- [x] Return:
  - `restarted`
  - `restart_failed`
  - structured error detail

### WFRULES-ORCH-8 — First deploy / auto_spawn path
- [x] If WF node does not exist and `auto_spawn=true`:
  - publish package
  - build managed config with defaults + fixed binding
  - `run_node` with runtime `wf.<workflow_name>`
- [x] If WF node does not exist and `auto_spawn=false`:
  - publish package only
  - return `wf_node.action = "none"`

### WFRULES-ORCH-9 — Publication vs deployment semantics
- [x] Keep package publication and node deployment as separate states in code
- [x] A published package without completed rebind/restart must not be treated as deployed
- [x] Crash/autorestart continues using the old binding: managed config in `/var/lib/fluxbee/nodes/` is only rewritten after explicit `SetNodeConfig` succeeds. The orchestrator owns process restart and reads the persisted binding — no code needed in sy.wf-rules for this property.

### WFRULES-ORCH-10 — Managed cleanup path for delete
- [x] `KillNode` called with `purge_instance: true` via orchestrator — this is the standard managed instance cleanup path (`delete.go`)
- [x] Does not delete `/var/lib/fluxbee/nodes/...` directly

### WFRULES-ORCH-11 — Tests
- [ ] Existing node apply publishes package then rebinds config then restarts
- [ ] Existing node apply preserves operational config
- [ ] Existing node apply binds `_system.package_path` to concrete version
- [ ] `restart_node` retry after 1 second works as specified
- [ ] `restart_failed` leaves package published but deployment incomplete
- [ ] auto_spawn=false path publishes package only, no orchestrator call

---

## 9) WF node query integration

### WFRULES-WF-1 — Active instance query client
- [x] Implement `QueryActiveInstances(wfNodeL2Name string)`
- [x] Send `WF_LIST_INSTANCES`
- [x] Payload:
  - `status_filter = "running"`
  - `limit = 0`
- [x] Timeout: 2 seconds

### WFRULES-WF-2 — Reachability semantics
- [x] On timeout/unreachable:
  - `reachable = false`
  - do not assume zero instances
- [x] Use this behavior in `get_status`, `list_workflows`, and `delete_workflow`

### WFRULES-WF-3 — Tests
- [x] Reachable response returns count
- [x] Timeout marks node unreachable
- [x] Delete path refuses with `INSTANCES_UNKNOWN` when unreachable

---

## 10) Core operations

### WFRULES-OP-1 — compile
- [x] Validate `workflow_name`
- [x] Parse `definition`
- [x] Run `wfcel.ValidateDefinition`
- [x] Build metadata
- [x] Write `staged/definition.json`
- [x] Write `staged/metadata.json`
- [x] Return compile response

### WFRULES-OP-2 — apply
- [x] Require `workflow_name`
- [x] Require `staged/`
- [x] Enforce optional version match
- [x] Rotate `staged -> current`, `current -> backup`
- [x] Materialize package from new `current`
- [x] Publish package
- [x] Determine WF node existence
- [x] If existing: explicit rebind + restart
- [x] If absent and `auto_spawn=true`: first deploy path
- [x] If absent and `auto_spawn=false`: leave package published only
- [x] Return apply response (CONFIG_SET and command paths both return full response)

### WFRULES-OP-3 — compile_apply
- [x] Execute `compile`
- [x] If compile succeeds, execute `apply`

### WFRULES-OP-4 — rollback
- [x] Require `workflow_name`
- [x] Require `backup/`
- [x] Rotate rollback state
- [x] Materialize package for restored `current`
- [x] Publish package if restored version is missing from `dist`
- [x] Explicitly rebind/restart existing WF node or apply first deploy semantics as needed
- [x] Return rollback response

### WFRULES-OP-5 — delete_workflow
- [x] Require `workflow_name`
- [x] Parse `force`
- [x] If `force=false`:
  - query active instances
  - unreachable => `INSTANCES_UNKNOWN`
  - count > 0 => `INSTANCES_ACTIVE`
- [x] Kill WF node through orchestrator lifecycle path
- [x] Request managed instance cleanup through orchestrator/admin standard path
- [x] Delete local workflow state
- [x] Purge package versions no longer referenced
- [x] Return delete response

### WFRULES-OP-6 — State consistency on partial failure
- [x] If package publish fails, do not proceed to rollout
- [x] If publish succeeds but rollout fails, keep `current/` and `backup/` consistent
- [x] Return partial success shape with `wf_node.action = "restart_failed"`
- [x] Do not pretend deployment occurred if only publication succeeded

---

## 11) Query handlers

### WFRULES-QRY-1 — get_workflow
- [x] Return current definition + metadata
- [x] `WORKFLOW_NOT_FOUND` if absent

### WFRULES-QRY-2 — get_status
- [x] Return:
  - current version/hash
  - staged version if present
  - last error
  - WF node status
  - active instances or timeout marker
- [x] Include enough status to distinguish:
  - package current version in `SY.wf-rules`
  - deployed node reachability

### WFRULES-QRY-3 — list_workflows
- [x] Enumerate workflows from local state
- [x] For each:
  - read current metadata
  - query WF node status / active instances

### WFRULES-QRY-4 — Optional deployment visibility
- [x] Expose, when available, the currently deployed/resolved version from managed config
- [x] This is useful to show publication state vs deployment state

---

## 12) Message dispatch

### WFRULES-DISP-1 — CONFIG_SET routing
- [x] Default operation is `compile`
- [x] Treat `check` as `compile`
- [x] Route: `compile`, `compile_apply`, `apply`, `rollback`
- [x] Unknown operation => `UNSUPPORTED_OPERATION` (default case in switch)

### WFRULES-DISP-2 — Command messages
- [x] `compile_workflow`
- [x] `apply_workflow`
- [x] `rollback_workflow`
- [x] `delete_workflow`

### WFRULES-DISP-3 — Query messages
- [x] `get_workflow`
- [x] `get_status`
- [x] `list_workflows`

### WFRULES-DISP-4 — System messages
- [x] `NODE_STATUS_GET`
- [x] `CONFIG_GET`
- [x] `CONFIG_SET`
- [x] `CONFIG_CHANGED` — routes to `handleConfigChanged`, subsystem filter `"wf-rules"`, supports compile/compile_apply/apply/rollback/check

---

## 13) Boot sequence

### WFRULES-BOOT-1 — Startup
- [x] Load hive config (hive_id derived from node full name returned by SDK)
- [x] Determine `SY.orchestrator@<hive>` (built in `BuildNodeConfig`)
- [x] Ensure `/var/lib/fluxbee/wf-rules/` and `/var/lib/fluxbee/dist/runtimes/` (`os.MkdirAll` in `Run`)
- [x] Acquire exclusive file lock at `/var/run/fluxbee/sy-wf-rules.lock` (`syscall.Flock`)
- [x] Load or create node UUID (handled by `fluxbee-go-sdk` with `UUIDMode: NodeUuidPersistent`)
- [x] Connect to router via `fluxbee-go-sdk`
- [x] Enter message loop (`RunWithContext`)

### WFRULES-BOOT-2 — Shutdown
- [x] Graceful shutdown on SIGTERM/SIGINT via `signal.NotifyContext` — `RunWithContext` returns `nil` on context cancel
- [x] File lock released on process exit (deferred `lockFile.Close()` + OS releases flock on fd close)
- Note: router connection close is handled by the SDK when the process exits; no explicit Close() call needed.

### WFRULES-BOOT-3 — Reconnect behavior
- [x] SDK reconnect behavior reused (no custom reconnect logic needed)
- [x] In-flight state not corrupted: message handlers are synchronous, no partial writes possible mid-reconnect

---

## 14) Admin integration (`SY.admin`)

### WFRULES-ADM-1 — `/admin/wf-rules/compile`
- [x] Forward to `SY.wf-rules` as `CONFIG_SET operation=compile`

### WFRULES-ADM-2 — `/admin/wf-rules/compile-apply`
- [x] Forward to `SY.wf-rules` as `CONFIG_SET operation=compile_apply`

### WFRULES-ADM-3 — `/admin/wf-rules/apply`
- [x] Forward to `SY.wf-rules` as `CONFIG_SET operation=apply`

### WFRULES-ADM-4 — `/admin/wf-rules/rollback`
- [x] Forward to `SY.wf-rules` as `CONFIG_SET operation=rollback`

### WFRULES-ADM-5 — `/admin/wf-rules/delete`
- [x] Forward to `SY.wf-rules` as command `delete_workflow`

### WFRULES-ADM-6 — `/admin/wf-rules/{name}`
- [x] Forward as query `get_workflow`

### WFRULES-ADM-7 — `/admin/wf-rules/{name}/status`
- [x] Forward as query `get_status`

### WFRULES-ADM-8 — `/admin/wf-rules`
- [x] Forward as query `list_workflows`

### WFRULES-ADM-9 — Preserve gateway-only role
- [x] Do not move compile/apply/publish logic into `SY.admin`
- [x] Admin remains proxy/control-plane only

---

## 15) Tests

### WFRULES-TEST-1 — compile happy path
- [x] Valid definition writes staged files
- [x] Metadata fields are correct

### WFRULES-TEST-2 — compile failures
- [x] CEL typo
- [x] invalid initial state
- [x] invalid action type
- [x] invalid workflow name
- [x] staged files are not written on failure

### WFRULES-TEST-3 — apply publication
- [x] Apply rotates staged to current
- [x] Package is materialized in `dist`
- [x] Package version equals metadata version

### WFRULES-TEST-4 — apply existing node rollout
- [x] Existing node config is rebound to concrete version
- [x] `_system.package_path` points to published package
- [x] `restart_node`

### WFRULES-TEST-5 — apply first deploy
- [x] Absent node + `auto_spawn=true` publishes package and spawns node
- [x] Absent node + `auto_spawn=false` publishes only

### WFRULES-TEST-6 — restart failure handling
- [x] Publish succeeds, restart fails
- [x] `current/` remains updated
- [x] response is partial success
- [x] deployment is not falsely reported as complete

### WFRULES-TEST-7 — rollback
- [x] No backup => `NO_BACKUP`
- [x] Rollback restores previous version
- [x] Rollback republishes/restores package if required

### WFRULES-TEST-8 — delete
- [x] Unreachable WF node => `INSTANCES_UNKNOWN`
- [x] Active instances => `INSTANCES_ACTIVE`
- [x] Force delete cleans local state
- [x] Force delete purges unreferenced packages

### WFRULES-TEST-9 — retention and purge
- [x] Keeps `current`
- [x] Keeps `backup`
- [x] Keeps externally referenced deployed version
- [x] Purges stale version

### WFRULES-TEST-10 — publication vs deployment state
- [x] Package published but rollout not complete leaves old deployed version intact
- [x] Crash/autorestart semantics: enforced by orchestrator owning the managed config; no sy.wf-rules code needed. Property is correct by design (SetNodeConfig only called after package publish succeeds).

### WFRULES-TEST-11 — query handlers
- [x] `get_workflow`
- [x] `get_status`
- [x] `list_workflows`
- [x] deployment status visibility when available

### WFRULES-TEST-12 — Admin proxy
- [x] Request contracts/docs cover all `wf-rules` Admin endpoints
- [x] Hive/target normalization is tested
- [x] Admin response envelope accepts `sy.wf-rules` ok/error payload shapes
- [ ] **PENDING:** End-to-end forwarding test of each Admin endpoint through router to L2 (requires integration test harness)

---

## 16) Installation

### WFRULES-INSTALL-1 — Binary/install script
- [x] Add `sy-wf-rules` to `scripts/install.sh`
- [x] Ensure state dir exists

### WFRULES-INSTALL-2 — Service wiring
- [x] Add `sy-wf-rules.service`
- [x] Follow same service pattern as other SY nodes

### WFRULES-INSTALL-3 — Identity registration
- [ ] **PENDING:** Confirm whether `SY.wf-rules` needs an `ILK_REGISTER` call at boot. Compare with `sy-opa-rules` and `sy-timer` — neither appears to do it. Document the finding and close this task.

---

## 17) Cleanup backlog

### WFRULES-CLEANUP-1 — `_system` version field convergence
- [x] Align docs, tasks, and code on one canonical field name for deployed workflow package version
- [x] Remove the remaining mismatch between `_system.resolved_version` in spec/task wording and `_system.runtime_version` in current core implementation
- [x] Keep publication state vs deployment state wording explicit after the naming cleanup

---

## 18) Remaining open items (post-audit 2026-04-16)

These items were identified during a code audit on 2026-04-16. All other tasks are complete.

### WFRULES-OPEN-1 — ORCH-11: Orchestrator integration tests

- [ ] Test: existing node apply → publishes package, rebinds managed config, restarts node
- [ ] Test: existing node apply preserves operational config fields (tenant_id, gc_*, sy_timer_l2_name)
- [ ] Test: `_system.package_path` in rebound config points to the newly published package
- [ ] Test: `restart_node` retry after 1s on first failure
- [ ] Test: `restart_failed` result when both restart attempts fail — package remains published, current/ is updated
- [ ] Test: `auto_spawn=false` path publishes package only, no orchestrator call at all

### WFRULES-OPEN-2 — GetNodeStatus for process liveness

- [ ] Add `GetNodeStatus(ctx, targetNode, wfNodeL2Name string) (running bool, err error)` to `orchestratorClient`
- [ ] Implementation: send `NODE_STATUS_GET` directly to the WF node L2 name with 2s timeout; `running=true` if response received, `running=false` on timeout
- [ ] Wire into `GetWorkflowStatus` and `ListWorkflowStatuses` so `wf_node.running` is accurate even when the node has a managed config but the process is down
- [ ] Wire into `DeleteWorkflow` force=false path as an additional guard (currently only `CountRunningInstances` is used)

### WFRULES-OPEN-3 — Shared receiver message drop (architectural note)

**Scope:** sy.wf-rules only — sy-opa-rules and sy-timer do not make outbound orchestrator RPC calls during message handling.

**Issue:** `orchestratorClient.request()` and the main `RunWithContext` loop share the same `receiver`. While the main loop is blocked in `handleMessage`, the orchestrator RPC client consumes messages from the socket in a tight loop, discarding any that don't match its `traceID`. Messages arriving during an orchestrator call (e.g., a concurrent health check) are silently dropped.

**Impact:** Low in practice — orchestrator RPC calls are short (3s timeout) and concurrent inbound traffic is rare for a system node. No data corruption risk.

- [ ] If/when this becomes a problem: introduce a message multiplexer goroutine (single `Recv` loop → fan-out by traceID to per-request buffered channels). This is a larger refactor; defer until observed in production.

### WFRULES-OPEN-4 — INSTALL-3: Identity registration confirmation

- [ ] Verify whether `SY.wf-rules` needs an `ILK_REGISTER` call at boot (compare with sy-opa-rules, sy-timer — neither does it). Document result and close.

### WFRULES-OPEN-5 — TEST-12: E2E Admin endpoint forwarding tests

- [ ] Integration test: each Admin HTTP endpoint (`/admin/wf-rules/...`) correctly forwards to `SY.wf-rules` via L2 and maps the response envelope

---

## 19) Suggested implementation order

1. **WFRULES-CEL-1 -> WFRULES-CEL-6**
2. **WFRULES-SETUP-1 -> WFRULES-SETUP-3**
3. **WFRULES-STORE-1 -> WFRULES-STORE-8**
4. **WFRULES-PKG-1 -> WFRULES-PKG-4**
5. **WFRULES-BOOT-1 -> WFRULES-BOOT-3**
6. **WFRULES-ORCH-1 -> WFRULES-ORCH-6**
7. **WFRULES-WF-1 -> WFRULES-WF-3**
8. **WFRULES-OP-1**
9. **WFRULES-OP-2 -> WFRULES-OP-4**
10. **WFRULES-PKG-5 -> WFRULES-PKG-6**
11. **WFRULES-OP-5 -> WFRULES-OP-6**
12. **WFRULES-QRY-1 -> WFRULES-QRY-4**
13. **WFRULES-DISP-1 -> WFRULES-DISP-4**
14. **WFRULES-TEST-1 -> WFRULES-TEST-12**
15. **WFRULES-ADM-1 -> WFRULES-ADM-9**
16. **WFRULES-INSTALL-1 -> WFRULES-INSTALL-3**

---

## 19) Ready-to-code checkpoint

This task file assumes the specification in `docs/sy-wf-rules-spec.md` is the only normative guide.

Before coding starts, the implementation team should treat these points as closed:

- package-native WF delivery
- fixed `_system.package_path` binding per node instance
- explicit rollout required for deployment
- no workflow source in managed config
- `SY.wf-rules` publishes workflow packages
- `SY.admin` remains a proxy only
- orchestrator owns managed node lifecycle and filesystem
