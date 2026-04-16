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
- **Instance binding:** `_system.resolved_version` / `_system.package_path` is fixed until explicit rollout.
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
- [ ] Create initial layout:

```text
go/sy-wf-rules/
├── go.mod
├── main.go
├── node/
│   ├── node.go
│   ├── config.go
│   ├── dispatch.go
│   ├── handlers.go
│   ├── store.go
│   ├── validate.go
│   ├── package_publish.go
│   ├── orchestrator.go
│   ├── wfnode.go
│   ├── queries.go
│   └── responses.go
└── node_test.go
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
- [ ] `ReadCurrentDefinition`
- [ ] `ReadCurrentMetadata`
- [ ] `ReadStagedDefinition`
- [x] `ReadStagedMetadata`
- [ ] `ReadBackupDefinition`
- [ ] `ReadBackupMetadata`
- [x] `WorkflowExists`
- [x] `ListWorkflows`

### WFRULES-STORE-6 — Apply rotation
- [ ] Implement `RotateToApply(workflowName)`
- [ ] Behavior:
  - delete old `backup/` if present
  - rename `current/` to `backup/` if present
  - rename `staged/` to `current/`

### WFRULES-STORE-7 — Rollback rotation
- [ ] Implement rollback rotation:
  - delete `staged/` if present
  - rename `current/` to `staged/`
  - rename `backup/` to `current/`

### WFRULES-STORE-8 — Delete state
- [ ] `DeleteWorkflowState(workflowName)`
- [ ] Removes only `/var/lib/fluxbee/wf-rules/<workflow_name>/`

---

## 7) Package publication in `dist`

### WFRULES-PKG-1 — Package model
- [ ] Define workflow package shape:

```text
/var/lib/fluxbee/dist/runtimes/wf.<workflow_name>/<version>/
├── package.json
├── flow/
│   └── definition.json
└── config/
    └── default-config.json   # optional
```

- [ ] Ensure `package.json` includes runtime metadata required by the standard runtime model
- [ ] Runtime name is `wf.<workflow_name>`
- [ ] Version equals logical workflow version
- [ ] `runtime_base = "wf.engine"`

### WFRULES-PKG-2 — Materialize package contents
- [ ] Build package directory contents from `current/definition.json` + metadata
- [ ] Generate `flow/definition.json`
- [ ] Generate `package.json`
- [ ] Support optional `config/default-config.json` if needed

### WFRULES-PKG-3 — Publish through standard install/publish path
- [ ] Reuse standard Fluxbee package install/publish implementation
- [ ] Do not shell out to ad hoc scripts
- [ ] Publish via code or structured internal command path only
- [ ] Return `PACKAGE_PUBLISH_FAILED` on failure

### WFRULES-PKG-4 — Publication result model
- [ ] Return structured publication result:
  - runtime name
  - published version
  - package path
  - success/failure detail

### WFRULES-PKG-5 — Retention and purge
- [ ] Enumerate versions under `dist/runtimes/wf.<workflow_name>/`
- [ ] Keep:
  - version referenced by `current/`
  - version referenced by `backup/`
  - any version still referenced by a live or persisted WF node instance
- [ ] Purge older unreferenced versions
- [ ] Never purge a version still bound in managed config

### WFRULES-PKG-6 — Tests
- [ ] Publish creates expected package layout
- [ ] Version number matches workflow metadata version
- [ ] Purge keeps `current`
- [ ] Purge keeps `backup`
- [ ] Purge keeps externally referenced version
- [ ] Purge deletes safe stale version

---

## 8) Orchestrator integration

### WFRULES-ORCH-1 — Node status client
- [ ] Implement `GetNodeStatus(nodeL2Name string)`
- [ ] Parse enough status to know:
  - node exists or not
  - unit active or not
  - config exists or not

### WFRULES-ORCH-2 — Managed config read client
- [ ] Implement `GetNodeConfig(nodeL2Name string)`
- [ ] Read current managed config for existing WF node
- [ ] Use this only to preserve operational fields and inspect current binding

### WFRULES-ORCH-3 — Managed config update client
- [ ] Implement `SetNodeConfig(nodeL2Name string, config map[string]any)`
- [ ] This is the rebind step for an existing WF node
- [ ] Managed config must carry:
  - operational fields
  - `_system.runtime`
  - `_system.requested_version`
  - `_system.resolved_version`
  - `_system.runtime_base`
  - `_system.package_path`

### WFRULES-ORCH-4 — Run node client
- [ ] Implement `RunNode(nodeL2Name, runtimeName, version string, config map[string]any)`
- [ ] Spawn path uses runtime `wf.<workflow_name>`
- [ ] Initial spawn path must produce a managed node with fixed package binding

### WFRULES-ORCH-5 — Kill node client
- [ ] Implement `KillNode(nodeL2Name string)`
- [ ] Tolerate `NODE_NOT_FOUND`

### WFRULES-ORCH-6 — Build managed WF config
- [ ] Implement builder for managed WF config
- [ ] Preserve operational fields from existing config when present:
  - `tenant_id`
  - `sy_timer_l2_name`
  - `gc_retention_days`
  - `gc_interval_seconds`
  - other node-owned operational fields
- [ ] Do not embed workflow source in config
- [ ] Always bind the concrete package version in `_system`

### WFRULES-ORCH-7 — Explicit rollout sequence
- [ ] Implement rollout helper for existing WF node:
  1. publish package
  2. read existing managed config
  3. build rebound config with new concrete version
  4. `set_node_config`
  5. `kill_node`
  6. `run_node`
- [ ] Retry `run_node` once after 1 second if needed
- [ ] Return:
  - `restarted`
  - `restart_failed`
  - structured error detail

### WFRULES-ORCH-8 — First deploy / auto_spawn path
- [ ] If WF node does not exist and `auto_spawn=true`:
  - publish package
  - build managed config with defaults + fixed binding
  - `run_node` with runtime `wf.<workflow_name>`
- [ ] If WF node does not exist and `auto_spawn=false`:
  - publish package only
  - return `wf_node.action = "none"`

### WFRULES-ORCH-9 — Publication vs deployment semantics
- [ ] Keep package publication and node deployment as separate states in code
- [ ] A published package without completed rebind/restart must not be treated as deployed
- [ ] Crash/autorestart must continue using the old binding until rollout completes

### WFRULES-ORCH-10 — Managed cleanup path for delete
- [ ] Implement standard delete path for WF managed instance through orchestrator/admin lifecycle
- [ ] Do not delete `/var/lib/fluxbee/nodes/...` directly

### WFRULES-ORCH-11 — Tests
- [ ] Existing node apply publishes package then rebinds config then restarts
- [ ] Existing node apply preserves operational config
- [ ] Existing node apply binds `_system.package_path` to concrete version
- [ ] `kill_node` failure is tolerated and `run_node` still attempted
- [ ] `run_node` retry after 1 second works as specified
- [ ] `restart_failed` leaves package published but deployment incomplete

---

## 9) WF node query integration

### WFRULES-WF-1 — Active instance query client
- [ ] Implement `QueryActiveInstances(wfNodeL2Name string)`
- [ ] Send `WF_LIST_INSTANCES`
- [ ] Payload:
  - `status_filter = "running"`
  - `limit = 0`
- [ ] Timeout: 2 seconds

### WFRULES-WF-2 — Reachability semantics
- [ ] On timeout/unreachable:
  - `reachable = false`
  - do not assume zero instances
- [ ] Use this behavior in `get_status`, `list_workflows`, and `delete_workflow`

### WFRULES-WF-3 — Tests
- [ ] Reachable response returns count
- [ ] Timeout marks node unreachable
- [ ] Delete path refuses with `INSTANCES_UNKNOWN` when unreachable

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
- [ ] Require `workflow_name`
- [ ] Require `staged/`
- [ ] Enforce optional version match
- [ ] Rotate `staged -> current`, `current -> backup`
- [ ] Materialize package from new `current`
- [ ] Publish package
- [ ] Determine WF node existence
- [ ] If existing:
  - explicit rebind + restart
- [ ] If absent and `auto_spawn=true`:
  - first deploy path
- [ ] If absent and `auto_spawn=false`:
  - leave package published only
- [ ] Return apply response

### WFRULES-OP-3 — compile_apply
- [ ] Execute `compile`
- [ ] If compile succeeds, execute `apply`

### WFRULES-OP-4 — rollback
- [ ] Require `workflow_name`
- [ ] Require `backup/`
- [ ] Rotate rollback state
- [ ] Materialize package for restored `current`
- [ ] Publish package if restored version is missing from `dist`
- [ ] Explicitly rebind/restart existing WF node or apply first deploy semantics as needed
- [ ] Return rollback response

### WFRULES-OP-5 — delete_workflow
- [ ] Require `workflow_name`
- [ ] Parse `force`
- [ ] If `force=false`:
  - query active instances
  - unreachable => `INSTANCES_UNKNOWN`
  - count > 0 => `INSTANCES_ACTIVE`
- [ ] Kill WF node through orchestrator lifecycle path
- [ ] Request managed instance cleanup through orchestrator/admin standard path
- [ ] Delete local workflow state
- [ ] Purge package versions no longer referenced
- [ ] Return delete response

### WFRULES-OP-6 — State consistency on partial failure
- [ ] If package publish fails, do not proceed to rollout
- [ ] If publish succeeds but rollout fails, keep `current/` and `backup/` consistent
- [ ] Return partial success shape with `wf_node.action = "restart_failed"`
- [ ] Do not pretend deployment occurred if only publication succeeded

---

## 11) Query handlers

### WFRULES-QRY-1 — get_workflow
- [ ] Return current definition + metadata
- [ ] `WORKFLOW_NOT_FOUND` if absent

### WFRULES-QRY-2 — get_status
- [ ] Return:
  - current version/hash
  - staged version if present
  - last error
  - WF node status
  - active instances or timeout marker
- [ ] Include enough status to distinguish:
  - package current version in `SY.wf-rules`
  - deployed node reachability

### WFRULES-QRY-3 — list_workflows
- [ ] Enumerate workflows from local state
- [ ] For each:
  - read current metadata
  - query WF node status / active instances

### WFRULES-QRY-4 — Optional deployment visibility
- [ ] Expose, when available, the currently deployed/resolved version from managed config
- [ ] This is useful to show publication state vs deployment state

---

## 12) Message dispatch

### WFRULES-DISP-1 — CONFIG_SET routing
- [x] Default operation is `compile`
- [x] Treat `check` as `compile`
- [ ] Route:
  - `compile`
  - `compile_apply`
  - `apply`
  - `rollback`
- [ ] Unknown operation => `UNSUPPORTED_OPERATION`

### WFRULES-DISP-2 — Command messages
- [x] `compile_workflow`
- [ ] `apply_workflow`
- [ ] `rollback_workflow`
- [ ] `delete_workflow`

### WFRULES-DISP-3 — Query messages
- [ ] `get_workflow`
- [ ] `get_status`
- [x] `list_workflows`

### WFRULES-DISP-4 — System messages
- [x] `NODE_STATUS_GET`
- [x] `CONFIG_GET`
- [x] `CONFIG_SET`
- [ ] `CONFIG_CHANGED`

---

## 13) Boot sequence

### WFRULES-BOOT-1 — Startup
- [ ] Load hive config
- [ ] Determine `SY.orchestrator@<hive>`
- [ ] Ensure `/var/lib/fluxbee/wf-rules/`
- [ ] Acquire file lock
- [ ] Load or create node UUID
- [ ] Connect to router via `fluxbee-go-sdk`
- [ ] Enter message loop

### WFRULES-BOOT-2 — Shutdown
- [ ] Graceful shutdown on SIGTERM/SIGINT
- [ ] Close router connection
- [ ] Release file lock

### WFRULES-BOOT-3 — Reconnect behavior
- [ ] Reuse current SDK reconnect behavior
- [ ] Ensure in-flight node state is not corrupted by reconnect

---

## 14) Admin integration (`SY.admin`)

### WFRULES-ADM-1 — `/admin/wf-rules/compile`
- [ ] Forward to `SY.wf-rules` as `CONFIG_SET operation=compile`

### WFRULES-ADM-2 — `/admin/wf-rules/compile-apply`
- [ ] Forward to `SY.wf-rules` as `CONFIG_SET operation=compile_apply`

### WFRULES-ADM-3 — `/admin/wf-rules/apply`
- [ ] Forward to `SY.wf-rules` as `CONFIG_SET operation=apply`

### WFRULES-ADM-4 — `/admin/wf-rules/rollback`
- [ ] Forward to `SY.wf-rules` as `CONFIG_SET operation=rollback`

### WFRULES-ADM-5 — `/admin/wf-rules/delete`
- [ ] Forward to `SY.wf-rules` as command `delete_workflow`

### WFRULES-ADM-6 — `/admin/wf-rules/{name}`
- [ ] Forward as query `get_workflow`

### WFRULES-ADM-7 — `/admin/wf-rules/{name}/status`
- [ ] Forward as query `get_status`

### WFRULES-ADM-8 — `/admin/wf-rules`
- [ ] Forward as query `list_workflows`

### WFRULES-ADM-9 — Preserve gateway-only role
- [ ] Do not move compile/apply/publish logic into `SY.admin`
- [ ] Admin remains proxy/control-plane only

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
- [ ] Apply rotates staged to current
- [ ] Package is materialized in `dist`
- [ ] Package version equals metadata version

### WFRULES-TEST-4 — apply existing node rollout
- [ ] Existing node config is rebound to concrete version
- [ ] `_system.package_path` points to published package
- [ ] `kill_node` then `run_node`

### WFRULES-TEST-5 — apply first deploy
- [ ] Absent node + `auto_spawn=true` publishes package and spawns node
- [ ] Absent node + `auto_spawn=false` publishes only

### WFRULES-TEST-6 — restart failure handling
- [ ] Publish succeeds, restart fails
- [ ] `current/` remains updated
- [ ] response is partial success
- [ ] deployment is not falsely reported as complete

### WFRULES-TEST-7 — rollback
- [ ] No backup => `NO_BACKUP`
- [ ] Rollback restores previous version
- [ ] Rollback republishes/restores package if required

### WFRULES-TEST-8 — delete
- [ ] Unreachable WF node => `INSTANCES_UNKNOWN`
- [ ] Active instances => `INSTANCES_ACTIVE`
- [ ] Force delete cleans local state
- [ ] Force delete purges unreferenced packages

### WFRULES-TEST-9 — retention and purge
- [ ] Keeps `current`
- [ ] Keeps `backup`
- [ ] Keeps externally referenced deployed version
- [ ] Purges stale version

### WFRULES-TEST-10 — publication vs deployment state
- [ ] Package published but rollout not complete leaves old deployed version intact
- [ ] Crash/autorestart semantics modeled as old binding until explicit rollout succeeds

### WFRULES-TEST-11 — query handlers
- [ ] `get_workflow`
- [ ] `get_status`
- [ ] `list_workflows`
- [ ] deployment status visibility when available

### WFRULES-TEST-12 — Admin proxy
- [ ] Each Admin endpoint forwards correct L2 message

---

## 16) Installation

### WFRULES-INSTALL-1 — Binary/install script
- [ ] Add `sy-wf-rules` to `scripts/install.sh`
- [ ] Ensure state dir exists

### WFRULES-INSTALL-2 — Service wiring
- [ ] Add `sy-wf-rules.service`
- [ ] Follow same service pattern as other SY nodes

### WFRULES-INSTALL-3 — Identity registration
- [ ] Register with `SY.identity` at boot following standard SY node behavior

---

## 17) Suggested implementation order

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

## 18) Ready-to-code checkpoint

This task file assumes the specification in `docs/sy-wf-rules-spec.md` is the only normative guide.

Before coding starts, the implementation team should treat these points as closed:

- package-native WF delivery
- fixed `_system.package_path` binding per node instance
- explicit rollout required for deployment
- no workflow source in managed config
- `SY.wf-rules` publishes workflow packages
- `SY.admin` remains a proxy only
- orchestrator owns managed node lifecycle and filesystem
