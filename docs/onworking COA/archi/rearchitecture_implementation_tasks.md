# Fluxbee Rearchitecture — Implementation Task List

**Status:** ready for implementation — contracts must be frozen (Phase 0) before any Track begins  
**Date:** 2026-04-23 (rev 2)**  
**Spec source:** `fluxbee_rearchitecture_master_spec_v4.md`  
**Codebase entry points:** `src/bin/sy_architect.rs`, `src/bin/sy_admin.rs`

---

## Dependency order

```

Phase 0 (contracts) → must complete before any track starts
Track A  (manifest + host)           → unblocked after Phase 0
Track B  (reconciler)                → unblocked after Phase 0; integration needs Track A
Track C  (artifact loop)             → unblocked after Phase 0; integration needs Track A
Track D  (plan compiler rename)      → unblocked after Phase 0; can start in parallel
Track E  (failure classifier)        → unblocked after Phase 0
Track F  (seed data preparation)     → unblocked immediately, no prerequisites; blocks agent readiness
Track H  (designer + design auditor) → unblocked after Phase 0; integration needs Track A
Track G  (final assembly)            → blocked until A+B+C+D+E+F+H are complete

```

---

## Phase 0 — Contract Freeze

All 12 contracts must be written as signed-off design decisions before any implementation begins.
These are design tasks, not code tasks. Output is a frozen decision document per item.

### [x] CFZ-1 — solution_manifest desired_state / advisory schema
**Deliverable:** JSON schema or typed Rust definition for `desired_state` v1 scope.
- Must enumerate allowed top-level sections: topology, runtimes, nodes, routing, wf_deployments, opa_deployments, ownership
- Must enumerate rejected sections: policy, identity
- Must enumerate allowed fields per section with types and optionality
- Must define ownership marker format per resource class

### [x] CFZ-2 — actual_state_snapshot v1 schema + exact admin actions
**Deliverable:** Rust struct definition + JSON example per spec section 6.4, **plus** a frozen table of exact admin actions used to build the snapshot.

Struct fields:

- `snapshot_version`, `snapshot_id`, `captured_at_start/end`
- `scope.hives[]`, `scope.resources[]`
- `atomicity.mode = "best_effort_multi_call"`, `is_atomic: false`
- `hive_status: HashMap<String, HiveSnapshotStatus>` with `reachable`, `error`
- `resources: HashMap<String, HiveResources>` with runtimes, nodes, routes, vpns, wf_state, opa_state
- `completeness.is_partial`, `missing_sections[]`, `blocking`

Exact admin action table (frozen — must not drift):

| Scope | Admin action | Global or per-hive | Required | On failure |
|---|---|---|---|---|
| global | `inventory` | global | yes | SNAPSHOT_PARTIAL_BLOCKING |
| global | `inventory/summary` | global | no (diagnostic) | warn only |
| per-hive | `list_runtimes` | per-hive | yes | mark hive section missing |
| per-hive | `list_nodes` | per-hive | yes | mark hive section missing |
| per-hive | `list_routes` | per-hive | yes | mark hive section missing |
| per-hive | `list_vpns` | per-hive | yes if VPN in scope | mark hive section missing |
| per-hive | `wf_rules_list_workflows` | per-hive | yes if WF in scope | mark hive section missing |
| per-hive | OPA status read (TBD exact action name) | per-hive | yes if OPA in scope | mark hive section missing |
| per-hive | `get_versions` / `list_versions` | per-hive | optional (version drift) | skip, no block |

Rule: if a required action fails for a hive that has solution-owned resources → that hive section is marked missing and `completeness.blocking = true`. Non-required action failures produce warnings only.

### [x] CFZ-3 — delta_report schema
**Deliverable:** Rust struct definition + JSON example per spec section 7.2.
- `delta_report_version`, `status` (ready / blocked_partial_snapshot / blocked_unsupported)
- `manifest_ref`, `snapshot_ref`
- `summary`: creates, updates, deletes, noops, blocked
- `operations[]`: op_id, resource_type, resource_id, change_type, ownership, compiler_class, desired_ref, actual_ref, blocking, payload_ref, notes

### [x] CFZ-4 — compiler_class full vocabulary
**Deliverable:** Rust enum `CompilerClass` with all values per spec section 7.3:
- Topology: HIVE_CREATE, VPN_CREATE, VPN_DELETE
- Runtimes: RUNTIME_PUBLISH_ONLY, RUNTIME_PUBLISH_AND_DISTRIBUTE, RUNTIME_DELETE_VERSION
- Nodes: NODE_RUN_MISSING, NODE_CONFIG_APPLY_HOT, NODE_CONFIG_APPLY_RESTART, NODE_RESTART_ONLY, NODE_KILL, NODE_RECREATE, BLOCKED_IMMUTABLE_CHANGE
- Routing: ROUTE_ADD, ROUTE_DELETE, ROUTE_REPLACE
- WF: WF_DEPLOY_APPLY, WF_DEPLOY_RESTART, WF_REMOVE
- OPA: OPA_APPLY, OPA_REMOVE
- Meta: NOOP, BLOCKED_PARTIAL_SNAPSHOT, SCOPE_NOT_SUPPORTED

### [x] CFZ-5 — compiler_class → admin-step translation table (frozen)
**Deliverable:** frozen translation table per spec section 7.4, extended with risk classification.

Rules:

- This is the contract between reconciler and plan compiler.
- Must be represented as code (match arm or lookup table) in a shared module.
- Plan compiler must import and use this table; must not redefine it.
- Any `compiler_class` not in the table => plan compiler returns error, not improvisation.

Each `compiler_class` must also declare its **risk class**:

| compiler_class | Admin-step sequence | Risk class |
|---|---|---|
| HIVE_CREATE | `add_hive` | non_destructive |
| VPN_CREATE | `add_vpn` | non_destructive |
| VPN_DELETE | `delete_vpn` | **destructive** |
| RUNTIME_PUBLISH_ONLY | `publish_runtime_package` | non_destructive |
| RUNTIME_PUBLISH_AND_DISTRIBUTE | `publish_runtime_package` + `sync_to` + `update_to` | non_destructive |
| RUNTIME_DELETE_VERSION | `remove_runtime_version` | **destructive** |
| NODE_RUN_MISSING | `run_node` | non_destructive |
| NODE_CONFIG_APPLY_HOT | `node_control_config_set` | non_destructive |
| NODE_CONFIG_APPLY_RESTART | `node_control_config_set` → `restart_node` | restarting |
| NODE_RESTART_ONLY | `restart_node` | restarting |
| NODE_KILL | `kill_node` | **destructive** |
| NODE_RECREATE | `kill_node` → `run_node` | **destructive** |
| ROUTE_ADD | `add_route` | non_destructive |
| ROUTE_DELETE | `delete_route` | **destructive** |
| ROUTE_REPLACE | `delete_route` → `add_route` | **destructive** |
| WF_DEPLOY_APPLY | `wf_rules_compile_apply` | non_destructive |
| WF_DEPLOY_RESTART | `wf_rules_compile_apply` → `restart_node` | restarting |
| WF_REMOVE | canonical remove action (blocked if unavailable) | **destructive** |
| OPA_APPLY | `opa_compile_apply` | non_destructive |
| OPA_REMOVE | canonical remove action (blocked if unavailable) | **destructive** |
| BLOCKED_IMMUTABLE_CHANGE | none — stop | blocked_until_operator |

Risk classes:

- `non_destructive`: safe to auto-proceed after CONFIRM 2
- `restarting`: causes service interruption; highlighted in CONFIRM 2 summary
- `destructive`: irreversible or data-loss risk; explicitly called out in CONFIRM 2 with individual confirmation
- `blocked_until_operator`: must not advance; pipeline pauses and presents to operator

The host must use this risk class when building the CONFIRM 2 payload (TA-8).

### [x] CFZ-6 — design loop stop conditions
**Deliverable:** signed-off numeric policy:
- max 3 iterations
- score must improve >= 1 point per iteration (define score scale: 0-10)
- same blocking issue signature twice => stop
- designer requests unavailable info twice => escalate

Define how design audit "score" is produced: structured field in `design_audit_verdict.score` (integer 0-10) with `blocking_issues[]` containing issue signatures (short string keys, not free text).

### [x] CFZ-7 — artifact loop stop conditions
**Deliverable:** signed-off numeric policy:
- max 3 attempts per build_task_packet
- same failure signature twice (based on failure_class + failing_field) => stop
- failure class in [ARTIFACT_TASK_UNDERSPECIFIED, ARTIFACT_UNSUPPORTED_KIND] => stop immediately

### [x] CFZ-8 — failure classification vocabulary + first deterministic rules
**Deliverable:** complete `FailureClass` enum + initial matcher table.
- Classes per spec section 11.2
- Deterministic rules per 11.3 + extension:
  - "runtime not materialized" => EXECUTION_ENVIRONMENT_MISSING
  - "unknown action" => PLAN_INVALID
  - "missing required field X" at plan stage => PLAN_CONTRACT_INVALID
  - "missing required field X" at artifact audit => ARTIFACT_CONTRACT_INVALID
  - "package missing required files" => ARTIFACT_LAYOUT_INVALID
  - hive unreachable during snapshot => SNAPSHOT_PARTIAL_BLOCKING
  - action timeout => EXECUTION_TIMEOUT
  - admin returns HTTP 4xx not matching known patterns => EXECUTION_ACTION_FAILED
  - AI call fails => UNKNOWN_RESIDUAL (escalate)

### [x] CFZ-9 — get_manifest_current tool contract
**Deliverable:** tool definition: name, inputs, outputs, error cases.
- Input: `solution_id: Option<String>` — if None, uses current session's active solution
- Output: `solution_manifest` JSON (full document including desired_state + advisory)
- Error if no manifest exists for solution_id: returns `{found: false, solution_id}`
- Error if storage read fails: propagates as tool error
- This is a read-only tool; must never mutate manifest

### [x] CFZ-10 — cookbook file names and seed set
**Deliverable:** file layout + minimum seed content for each cookbook:
- `{state_dir}/cookbook/design_cookbook_v1.json`
- `{state_dir}/cookbook/artifact_cookbook_v1.json`
- `{state_dir}/cookbook/plan_compile_cookbook_v1.json`
- `{state_dir}/cookbook/repair_cookbook_v1.json`
- Each file: JSON array of `CookbookEntryV2` (see CFZ-10 for shape)
- Seed content: at minimum, collect 3-4 entries per cookbook from existing working cases

### [x] CFZ-11 — pipeline_runs persistence schema
**Deliverable:** SQL table definition + Rust struct for `PipelineRunRecord`.

```sql
CREATE TABLE pipeline_runs (
  pipeline_run_id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  solution_id TEXT,
  status TEXT NOT NULL,           -- in_progress | blocked | completed | failed | interrupted
  current_stage TEXT NOT NULL,    -- design | reconcile | artifact_loop | plan_compile | confirm_1 | confirm_2 | execute | verify
  current_loop INTEGER DEFAULT 0,
  current_attempt INTEGER DEFAULT 0,
  state_json TEXT NOT NULL,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  interrupted_at_ms INTEGER
);

```
`state_json` holds serialized `PipelineRunState` with refs to all active artifacts.

### [x] CFZ-13 — CookbookEntryV2 schema (frozen)
**Deliverable:** single canonical JSON schema for all cookbook entries across all layers.

Without this, each track writes cookbooks with a different shape and agents cannot read them reliably.

```json
{
  "layer": "design | artifact | plan_compile | repair",
  "pattern_key": "string — unique key for this pattern, e.g. 'ai_support_dedicated_worker'",
  "trigger": "string — one sentence describing when to use this pattern",
  "inputs_signature": {},
  "successful_shape": {},
  "constraints": [],
  "do_not_repeat": [],
  "failure_class": null,
  "recorded_from_run": null,
  "seed_kind": "manual | observed"
}

```

Field rules:

- `layer`: mandatory, must be one of the four values
- `pattern_key`: mandatory, lowercase with underscores, unique per layer
- `trigger`: mandatory, one sentence, no JSON
- `inputs_signature`: optional summary of input context that makes this pattern relevant
- `successful_shape`: the successful artifact shape, plan shape, or design shape — layer-specific content
- `constraints`: list of constraints that must hold for this pattern to apply
- `do_not_repeat`: known mistakes or anti-patterns paired with this scenario
- `failure_class`: only for repair cookbook entries — the `FailureClass` this entry addresses
- `recorded_from_run`: pipeline_run_id if observed; null if manually seeded
- `seed_kind`: `"manual"` for hand-written seeds; `"observed"` for pipeline-generated entries

All four cookbooks (design, artifact, plan_compile, repair) use this exact shape. No layer-specific top-level fields.

### [x] CFZ-12 — restart / hot-apply / immutable-change table by node type
**Deliverable:** frozen Rust table mapping node type prefix and runtime class to reconcile semantics.
- WF.* => config changes require restart (NODE_CONFIG_APPLY_RESTART by default)
- AI.* => config changes are hot by default (NODE_CONFIG_APPLY_HOT by default)
- IO.* => config changes are hot by default
- SY.* => restart required (system nodes are conservative)
- Immutable fields per node type (e.g., runtime name cannot change without recreate)
- If type is unknown => emit BLOCKED_IMMUTABLE_CHANGE (never guess)

---

## Track A — Manifest and Host Contracts

**Blocked by:** Phase 0  
**Primary file:** `src/bin/sy_architect.rs`

### [x] TA-1 — Rust types for solution_manifest v2
- Add `SolutionManifestV2` struct with `manifest_version`, `solution`, `desired_state: DesiredStateV2`, `advisory: AdvisoryV2`
- `DesiredStateV2`: topology, runtimes, nodes, routing, wf_deployments, opa_deployments, ownership_config
- `AdvisoryV2`: `serde_json::Value` (free-form, not reconciler-facing)
- Add `validate_manifest_v2(manifest: &SolutionManifestV2) -> Result<(), ManifestValidationError>`:
  - Rejects unknown top-level `desired_state` keys (policy, identity, etc.)
  - Validates required fields per section
  - Validates ownership markers if present
- Acceptance: unit test — manifest with `desired_state.identity` field is rejected with `UNSUPPORTED_DESIRED_STATE_SECTION`

### [x] TA-2 — Manifest storage (save / load)
- `save_manifest(state_dir, solution_id, manifest) -> Result<PathBuf>`: writes JSON to `{state_dir}/manifests/{solution_id}/{version}.json`
- `load_manifest(state_dir, solution_id, version: Option<&str>) -> Result<Option<SolutionManifestV2>>`: reads latest or specific version
- Version: incrementing integer or timestamp-based string
- DB: `manifest_refs` table with `solution_id`, `version`, `path`, `created_at_ms`
- Acceptance: save → load returns same content

### [x] TA-3 — get_manifest_current tool
- Implement `GetManifestCurrentTool` as `FunctionTool`
- Tool name: `get_manifest_current`
- Input: `{ "solution_id": "<optional>" }`
- Calls `load_manifest(state_dir, resolved_solution_id, None)`
- Returns full manifest JSON or `{ "found": false }`
- Register in designer's tool registry only (not in Archi general tools)
- Acceptance: call with existing solution_id returns manifest; call with unknown returns found=false

### [x] TA-4 — pipeline_runs DB table
- Implement `ensure_pipeline_runs_table(db) -> Result<Table>`
- Implement `save_pipeline_run(table, record) -> Result<()>`
- Implement `load_pipeline_run(table, run_id) -> Result<Option<PipelineRunRecord>>`
- Implement `list_interrupted_runs(table, session_id) -> Result<Vec<PipelineRunRecord>>`
- `PipelineRunRecord` per CFZ-11 schema
- Acceptance: save → load returns same record; status update persists

### [x] TA-5 — PipelineRunState machine
- Add `PipelineRunState` struct to `ArchitectState`:
  - `active_runs: Arc<Mutex<HashMap<String, PipelineRunRecord>>>`
- Add `PipelineStage` enum: Design, DesignAudit, Confirm1, Reconcile, ArtifactLoop, PlanCompile, PlanValidation, Confirm2, Execute, Verify, Completed, Failed, Blocked, Interrupted
- Implement `advance_pipeline_run(state, run_id, new_stage)`: updates DB + in-memory state
- Implement `block_pipeline_run(state, run_id, reason, failure_class)`: transitions to Blocked
- Acceptance: state machine transitions are logged and persisted

### [x] TA-6 — Host trace rendering for multi-actor pipeline
- Add `renderPipelineTrace(trace)` JS function in embedded UI:
  - Shows each completed stage as a row: stage name, status icon, actor, duration
  - Expandable detail per stage (artifact refs, audit scores, loop counts)
  - Similar visual to existing `renderExecutorResult` but at pipeline level
- Backend: add `pipeline_trace: Option<Vec<PipelineTraceEvent>>` to chat output JSON
- `PipelineTraceEvent`: stage, actor, status, duration_ms, summary, artifact_ref
- Acceptance: operator sees pipeline stages updating in trace panel during internal loops

### [x] TA-7 — CONFIRM 1 flow
**Important:** CONFIRM routing must be driven by `pipeline_run.current_stage`, not by legacy pending flags.

Legacy flags (`programmer_pending`, `plan_compile_pending`) remain for backward compatibility with free-form chat flows only. They must not be used as the routing criterion for pipeline runs.

Logic in CONFIRM handler:
1. Check if there is an active pipeline run for the session in `current_stage == Confirm1`
2. If yes: this is a pipeline CONFIRM 1
   - Transition pipeline to Reconcile stage
   - Trigger snapshot builder (Track B)
   - Return `{ mode: "pipeline", stage: "reconcile_started" }`

3. If no pipeline run at Confirm1: fall through to legacy `plan_compile_pending` check (backward compat)
4. If no legacy pending either: fall through to SCMD pending check

Host shows CONFIRM 1 payload per spec 14.1: manifest summary, design audit verdict, main topology/runtime/node choices, advisory warnings, any blocking issues.

Acceptance: CONFIRM 1 in a design-complete pipeline transitions to Reconcile stage; CONFIRM in a legacy free-form chat session still works as before.

### [x] TA-8 — CONFIRM 2 flow
**Important:** same routing rule as TA-7 — check `pipeline_run.current_stage == Confirm2` first.

Logic in CONFIRM handler:
1. Check if there is an active pipeline run for the session in `current_stage == Confirm2`
2. If yes: this is a pipeline CONFIRM 2
   - Retrieve `executor_plan` from `pipeline_run.state_json`
   - Execute via `execute_executor_plan_with_context`
   - Transition to Execute stage

3. If no pipeline run at Confirm2: fall through to legacy check

Host shows CONFIRM 2 payload per spec 14.3:

- Plan summary (N steps, resources affected)
- Destructive ops explicitly listed (all ops with risk_class == `destructive` per CFZ-5 table)
- Restarting ops highlighted (risk_class == `restarting`)
- Validation result
- Any blocked ops and why

Acceptance: CONFIRM 2 triggers executor using plan from pipeline state; legacy free-form CONFIRM still works.

### [x] TA-9 — Interrupted run recovery
- On startup: `list_interrupted_runs` for all sessions
- Mark any `in_progress` runs as `interrupted` in DB
- When session is opened: check for interrupted runs, present recovery options:
  - Resume from last checkpoint
  - Discard and start over
- Acceptance: server restart with in_progress run → user sees recovery prompt on next message

---

## Track B — Deterministic Reconciler

**Blocked by:** Phase 0  
**Primary file:** new module `src/reconciler/mod.rs` (or inline in `sy_architect.rs` if monolith policy)  
**Nature:** pure Rust, no AI calls

### [x] TB-1 — SnapshotBuilder: Rust struct + admin calls
- Implement `SnapshotBuilder` struct with `context: ArchitectAdminToolContext`
- Implement `build_snapshot(hives: &[String], scope: SnapshotScope) -> Result<ActualStateSnapshot>`
- Makes parallel admin calls per hive:
  - `inventory` (global summary)
  - per hive: `list_runtimes`, `list_nodes`, `list_routes`, `list_vpns`
  - if WF in scope: `wf_rules_list_workflows` per hive
  - if OPA in scope: OPA status read per hive
- Assembles `ActualStateSnapshot` per CFZ-2 schema
- Times each section fetch; stores in `captured_at_start/end`
- Acceptance: calling against live motherbee returns populated snapshot with correct hive_status

### [x] TB-2 — Partial snapshot policy
- If a target hive from `desired_state.topology.hives` is unreachable:
  - Mark `hive_status[hive].reachable = false`
  - Mark `completeness.is_partial = true`
  - Add missing sections to `completeness.missing_sections`
- `completeness.blocking = true` if any required hive for solution-owned resources is missing
- `build_snapshot` does not fail on hive timeout; it records the failure and continues
- Acceptance: snapshot builder with one mocked-down hive produces partial snapshot with blocking=true

### [x] TB-3 — DeltaReport Rust types
- `DeltaReport` struct per CFZ-3
- `DeltaOperation` struct: op_id, resource_type, resource_id, change_type (Create/Update/Delete/Noop/Blocked), ownership, compiler_class, desired_ref, actual_ref, blocking, payload_ref, notes
- `DeltaReportStatus`: Ready, BlockedPartialSnapshot, BlockedUnsupported
- All types: `Serialize`, `Deserialize`, `Clone`, `Debug`
- Acceptance: can round-trip through JSON

### [x] TB-4 — Hive and VPN reconciler
- `reconcile_topology(desired: &DesiredTopology, snapshot: &ActualStateSnapshot) -> Vec<DeltaOperation>`
- For each desired hive: if absent in snapshot -> emit HIVE_CREATE
- For each desired VPN: if absent -> emit VPN_CREATE; if absent in desired but present and solution-owned -> emit VPN_DELETE
- Ownership check: never emit delete for `ownership = "external"` or `ownership = "system"`
- Acceptance: unit test with fixture — desired has hive "worker-new", snapshot has none → creates HIVE_CREATE op

### [x] TB-5 — Runtime reconciler
- `reconcile_runtimes(desired: &[DesiredRuntime], snapshot: &ActualStateSnapshot) -> Vec<DeltaOperation>`
- For each desired runtime/package:
  - If absent in snapshot -> emit RUNTIME_PUBLISH_ONLY or RUNTIME_PUBLISH_AND_DISTRIBUTE
  - Selection: DISTRIBUTE if desired specifies `sync_to` or if multi-hive topology
  - If present but version differs -> emit RUNTIME_PUBLISH_AND_DISTRIBUTE
  - If present and version matches -> emit NOOP
- `artifact_requirement` field in op: points to required `build_task_packet` if package source is `inline_package` or not yet available
- Acceptance: unit test — desired runtime not in snapshot → RUNTIME_PUBLISH_AND_DISTRIBUTE; same version in snapshot → NOOP

### [x] TB-6 — Node reconciler + restart semantics table
- `reconcile_nodes(desired: &[DesiredNode], snapshot: &ActualStateSnapshot, restart_table: &RestartTable) -> Vec<DeltaOperation>`
- For each desired node:
  - If absent -> emit NODE_RUN_MISSING
  - If present with config drift -> lookup restart_table:
    - hot-capable node type + non-immutable field -> NODE_CONFIG_APPLY_HOT
    - non-hot node type or immutable field change -> NODE_CONFIG_APPLY_RESTART
    - immutable field + no recreate policy -> BLOCKED_IMMUTABLE_CHANGE
    - immutable field + `reconcile_policy.on_immutable_change = "recreate"` -> NODE_RECREATE
  - If absent in desired but present and solution-owned -> NODE_KILL
- `RestartTable` per CFZ-12: match on node name prefix (WF.* / AI.* / IO.* / SY.*) and field class
- Config hash comparison: compare `node.config` fields declared in desired_state against current `node_config` from snapshot; ignore runtime-internal fields not declared
- Acceptance: unit test battery — 6 scenarios covering each class emitted

### [x] TB-7 — Route reconciler
- `reconcile_routes(desired: &[DesiredRoute], snapshot: &ActualStateSnapshot) -> Vec<DeltaOperation>`
- Route identity key: (hive, prefix, action_type) — if same key with different fields -> ROUTE_REPLACE (delete + add)
- New route -> ROUTE_ADD
- Route absent in desired but present and solution-owned -> ROUTE_DELETE
- Acceptance: unit test — desired has new prefix, snapshot missing it → ROUTE_ADD

### [x] TB-8 — WF reconciler
- `reconcile_wf(desired: &[DesiredWfDeployment], snapshot: &ActualStateSnapshot, restart_table: &RestartTable) -> Vec<DeltaOperation>`
- For each desired WF deployment:
  - If absent -> WF_DEPLOY_APPLY
  - If present but workflow definition hash differs:
    - If restart required per restart_table (WF.* default) -> WF_DEPLOY_RESTART
    - Otherwise -> WF_DEPLOY_APPLY
  - If absent in desired but present and solution-owned -> WF_REMOVE
    - If no canonical remove action available -> emit BLOCKED with note
- Acceptance: unit test — desired WF not in snapshot → WF_DEPLOY_APPLY; definition changed → WF_DEPLOY_RESTART

### [x] TB-9 — OPA reconciler
- `reconcile_opa(desired: &[DesiredOpaDeployment], snapshot: &ActualStateSnapshot) -> Vec<DeltaOperation>`
- For each desired OPA deployment:
  - If absent -> OPA_APPLY
  - If policy hash changed -> OPA_APPLY (reapply)
  - If absent in desired but present and solution-owned -> OPA_REMOVE
    - If no canonical remove action -> emit BLOCKED with note
- Acceptance: unit test — desired OPA missing → OPA_APPLY; same hash in snapshot → NOOP

### [x] TB-10 — Artifact gap detection
- After reconciler runs, scan all `DeltaOperation` entries with `compiler_class` requiring a runtime artifact (RUNTIME_PUBLISH_ONLY, RUNTIME_PUBLISH_AND_DISTRIBUTE, WF_DEPLOY_APPLY/RESTART):
  - Check if an approved `artifact_bundle` already exists for the required resource
  - If not: emit a `BuildTaskPacket` for that operation
- `BuildTaskPacket` per spec 6.4: task_id, artifact_kind, source_delta_op, runtime_name, target_kind, requirements, constraints, context_refs
- Return `Vec<BuildTaskPacket>` alongside the `DeltaReport`
- Acceptance: reconciler on a desired state with unpackaged inline runtime emits a BuildTaskPacket pointing to the delta op

### [x] TB-11 — Reconciler entry point
- `run_reconciler(context, manifest_ref, snapshot) -> Result<ReconcilerOutput>`
- `ReconcilerOutput`: `delta_report: DeltaReport`, `build_task_packets: Vec<BuildTaskPacket>`, `diagnostics: Vec<String>`
- Validates manifest v2 shape first (reject unsupported sections)
- Calls all reconciler sub-functions (TB-4 through TB-9)
- Merges all operations, assigns op_ids
- Computes summary counts
- Sets `delta_report.status`:
  - BLOCKED if any snapshot hive is down and affects owned resources
  - BLOCKED if any op emits `BLOCKED_*` class and is marked as blocking
  - READY otherwise
- Acceptance: full reconciler run against fixture returns deterministic DeltaReport

### [x] TB-12 — Reconciler unit tests
- One fixture set per resource type (TB-4 through TB-9)
- Each fixture: `(manifest_desired_state, actual_state_snapshot) -> expected DeltaReport`
- Must include idempotency test: running reconciler twice on same inputs produces same output
- Must include ownership test: solution-owned resources get deletes; external/system resources get NOOPs not deletes
- Must include partial snapshot test: blocking partial snapshot produces blocked DeltaReport not silent deletes
- Acceptance: all tests pass without any OpenAI call

---

## Track C — Artifact Loop

**Blocked by:** Phase 0  
**Primary file:** `src/bin/sy_architect.rs` (new structs + tools)  
**Depends on:** Track B (build_task_packets feed the loop)

### [x] TC-1 — BuildTaskPacket Rust struct

```rust
struct BuildTaskPacket {
    task_packet_version: String,
    task_id: String,
    artifact_kind: ArtifactKind,  // RuntimePackage, WorkflowDefinition, OpaBundleSource, ConfigBundle, PromptAssets
    source_delta_op: String,
    runtime_name: Option<String>,
    target_kind: String,          // inline_package | bundle_upload | workflow_def | opa_source
    requirements: ArtifactRequirements,
    constraints: ArtifactConstraints,
    context_refs: Vec<String>,
    known_context: ArtifactKnownContext,  // available_runtimes, running_nodes
    attempt: u32,
    max_attempts: u32,
    cookbook_context: Vec<Value>,
}

```

- `Serialize`, `Deserialize`, `Clone`
- `save_task_packet(state_dir, packet) -> Result<PathBuf>`: writes to `{state_dir}/task_packets/{task_id}.json`

### [x] TC-2 — ArtifactBundle Rust struct

```rust
struct ArtifactBundle {
    bundle_version: String,
    bundle_id: String,
    source_task_id: String,
    artifact_kind: ArtifactKind,
    status: ArtifactBundleStatus,  // pending | approved | rejected
    summary: String,
    artifact: Value,              // kind-specific content (publish_request, wf_def, opa_source, etc.)
    assumptions: Vec<String>,
    verification_hints: Vec<String>,
    content_digest: String,       // sha256 of canonical artifact JSON
    generated_at_ms: u64,
    generator_model: String,
}

```

- `save_artifact_bundle(state_dir, bundle) -> Result<PathBuf>`
- `load_artifact_bundle(state_dir, bundle_id) -> Result<Option<ArtifactBundle>>`
- Content digest: `sha256(serde_json::to_string_pretty(&bundle.artifact)?)`

### [x] TC-3 — ArtifactAuditVerdict Rust struct

```rust
struct ArtifactAuditVerdict {
    verdict_id: String,
    source_bundle_id: String,
    source_task_id: String,
    status: AuditStatus,          // Approved | Repairable | Rejected | Blocked
    failure_class: Option<FailureClass>,
    findings: Vec<AuditFinding>,
    blocking_issues: Vec<String>,
    repair_hints: Vec<RepairHint>,
}

struct AuditFinding {
    code: String,                 // e.g. MISSING_FILE, INVALID_RUNTIME_BASE
    field: Option<String>,
    message: String,
    severity: AuditSeverity,      // Error | Warning | Info
}

```

### [x] TC-4 — RepairPacket Rust struct

```rust
struct RepairPacket {
    repair_packet_version: String,
    repair_id: String,
    source_task_id: String,
    previous_bundle_id: String,
    previous_content_digest: String,
    failure_class: FailureClass,
    evidence: RepairEvidence,
    required_corrections: Vec<String>,
    do_not_repeat: Vec<String>,
    must_preserve: Vec<String>,
    retry_allowed: bool,
    attempt: u32,
}

```

- Built by the artifact loop controller from the `ArtifactAuditVerdict`

### [x] TC-5 — RealProgrammerTool: AI tool for artifact generation
- Implement `RealProgrammerTool` as `FunctionTool`
- Tool name: `fluxbee_real_programmer`
- System prompt: focused on artifact generation only — no design decisions, no admin ops
  - Reads `build_task_packet` as structured input
  - Must call `submit_artifact_bundle` exactly once
  - Can reference artifact cookbook context injected into prompt
  - Prompt includes handbook packaging conventions
- `submit_artifact_bundle` function schema: bundle type-specific, strict, `additionalProperties: false`
- For `RuntimePackage`: validates files map, package.json structure, runtime_base presence
- For `WorkflowDefinition`: validates workflow schema fields
- After call: compute content_digest, build `ArtifactBundle`, store to filesystem
- Acceptance: given a build_task_packet for a config_only AI runtime, produces a valid ArtifactBundle with package.json + default-config.json + system.txt

Current code status:

- `RealProgrammerTool` now exists as `fluxbee_real_programmer`.
- The host-side runner uses a strict `submit_artifact_bundle` tool, injects artifact cookbook context plus handbook text into the prompt, and converts the submitted payload into canonical `ArtifactBundle` records with computed digest / metadata.
- The current supported artifact contract is intentionally still `runtime_package`, but that runtime-package path now supports both `inline_package` and `bundle_upload` targets; other artifact kinds remain outside the implemented scope for now.

### [x] TC-6 — ArtifactAuditor: structural validator

**Scope boundary (mandatory, must be documented in code):**
> `ArtifactAuditor` validates structural and contract-level correctness only. It does not validate semantic quality of prompts, business correctness of configs, normative correctness of Rego logic, or real-world adequacy of workflow definitions. If an artifact passes the auditor, it means the artifact is structurally valid — not that it will work as intended.

This boundary must be a doc comment on the function, not just an inline note, so it is visible to every future implementer.

- Implement `ArtifactAuditor` as a pure function: `audit_artifact(packet: &BuildTaskPacket, bundle: &ArtifactBundle) -> ArtifactAuditVerdict`
- No AI calls — deterministic structural checks only
- Per artifact kind:
  - RuntimePackage: required files (package.json, config/default-config.json), package.json has name/version/type/runtime_base, runtime_base in packet.known_context.available_runtimes, naming convention (lowercase, dots allowed)
  - WorkflowDefinition: required top-level fields, valid step references (no dangling refs)
  - OpaBundle: required rego files present, basic rego syntax check (regex for package declaration)
  - ConfigBundle: required files per packet.requirements.required_files
- Repair hints: for each MISSING_FILE finding, hint = `add file {path}`; for INVALID_RUNTIME_BASE, hint = `set runtime_base to one of: {known_context.available_runtimes}`
- Acceptance: audit of a bundle missing package.json returns Repairable verdict with MISSING_FILE finding and add-file repair hint

### [x] TC-7 — Artifact loop controller
- Implement `run_artifact_loop(context, task_packets: Vec<BuildTaskPacket>) -> Result<Vec<ApprovedArtifact>>`
- For each task_packet:
  ```

  for attempt in 0..packet.max_attempts (default 3):
      bundle = run_real_programmer(context, packet, repair_packet.as_ref())
      verdict = audit_artifact(&packet, &bundle)
      if verdict.status == Approved:
          register approved artifact
          break
      if verdict.status == Rejected or Blocked:
          return Err with verdict
      if same failure signature as previous attempt:
          return Err (stop early, same error repeated)
      repair_packet = build_repair_packet(&verdict, attempt+1)
      update packet.attempt += 1
  return Err if max_attempts exceeded
  ```

- `ApprovedArtifact`: bundle_id, task_id, artifact_kind, content_digest, path
- `save_approved_artifact_registry(state_dir, pipeline_run_id, artifacts)`: JSON file linking delta ops to approved bundles
- Acceptance: artifact loop with a failing bundle (missing file) recovers on attempt 2 after repair packet directs programmer to add the file

Current code status:

- `run_pipeline_artifact_loop(...)` is implemented and now calls `run_real_programmer_with_context(...)` instead of the deprecated infrastructure specialist bridge.
- It persists task packets, bundles, repair packets, approved artifact registry, and trace events; repeated repair signatures stop early as required.
- Approved runtime-package bundles are now finalized into canonical `publish_source` payloads before plan compilation:
  - `inline_package` keeps the generated file map as `publish_source.kind = inline_package`
  - `bundle_upload` archives the approved package into a promoted blob zip and records `publish_source.kind = bundle_upload` with the resolved `blob_path`
- This task remains open only because support is still intentionally limited to the currently implemented artifact kinds, not because the loop wiring is missing.

### [x] TC-8 — Artifact storage
- Artifacts stored at `{state_dir}/pipeline/{run_id}/artifacts/{bundle_id}/`
- Each bundle: `bundle.json` (ArtifactBundle metadata), `artifact/` subdirectory with artifact files
- Task packets stored at `{state_dir}/pipeline/{run_id}/task_packets/{task_id}.json`
- Repair packets stored at `{state_dir}/pipeline/{run_id}/repairs/{repair_id}.json`
- Acceptance: after artifact loop, all produced files are readable from filesystem

### [x] TC-9 — Artifact loop trace rendering in chat
- Emit `ArtifactLoopTraceEvent` per attempt:
  ```rust
  struct ArtifactLoopTraceEvent {
      task_id: String,
      artifact_kind: String,
      attempt: u32,
      status: String,   // generating | auditing | approved | repairable | rejected | blocked
      verdict: Option<String>,
      failure_class: Option<String>,
      findings_count: u32,
      bundle_id: Option<String>,
      timestamp_ms: u64,
  }
  ```

- Add `artifact_loop_trace: Option<Vec<ArtifactLoopTraceEvent>>` to pipeline trace
- Frontend: `renderArtifactLoopTrace(events)` — show each attempt with status icon and expand for findings
- Collapsed by default; auto-expand if any rejected or blocked
- Acceptance: operator sees artifact loop retries in trace panel without being interrupted

Current code status:

- `artifact_loop_trace` is already persisted into pipeline state during the artifact loop.
- The frontend now renders it as a collapsed `Agent activity · artifact_loop` panel, and auto-expands it when a repairable/rejected/blocked status appears.
- Confirm2 payloads and artifact-loop block responses now surface that trace directly, so the operator can inspect internal retries without leaving the chat flow.

---

## Track D — Plan Compiler

**Blocked by:** Phase 0  
**Primary file:** `src/bin/sy_architect.rs`  
**Nature:** mostly renaming + wiring changes; AI prompt changes

### [x] TD-1 — Rename fluxbee_programmer to plan_compiler in code
- Rename Rust identifiers:
  - `ArchitectProgrammerTool` -> `PlanCompilerTool`
  - `programmer_pending` -> `plan_compile_pending` in `ArchitectState`
  - `ProgrammerPendingPlan` -> `PlanCompilePending`
  - `ProgrammerTrace` -> `PlanCompileTrace`
  - `ProgrammerTraceStep` -> `PlanCompileTraceStep`
  - `run_programmer_with_context` -> `run_plan_compiler_with_context`
  - `PROGRAMMER_SYSTEM_PROMPT_BASE` -> `PLAN_COMPILER_SYSTEM_PROMPT_BASE`
  - `PROGRAMMER_COOKBOOK_BLOB_PATH` -> `PLAN_COMPILE_COOKBOOK_PATH`
  - `programmer_cookbook_path` -> `plan_compile_cookbook_path`
- Rename tool name in `FunctionToolDefinition`: `"fluxbee_programmer"` -> `"fluxbee_plan_compiler"`
- Update Archi system prompt references
- Update trace field names: `programmer_trace` -> `plan_compile_trace` in JSON output
- Acceptance: no remaining `programmer` identifiers except in comments explaining the old name for migration context

### [x] TD-2 — Bind plan compiler to delta_report

**Legacy path policy:** the free-form chat path (no delta_report) is explicitly `#[deprecated]` from this point. It must not receive new features or prompt improvements. All future investment goes into the delta_report path. The legacy path exists only for backward compatibility with existing free-form sessions and will be removed after Track G E2E tests pass.

Mark in code:

```rust
// DEPRECATED: free-form task path — use delta_report path for all new pipeline flows.
// This path is preserved only for backward compatibility with pre-rearchitecture sessions.
// Do not add features here.

```

- Change `PlanCompilerTool` input args schema:
  - Add `delta_report: Option<Value>` field — when provided, plan compiler consumes delta_report instead of free-form task description
  - Keep `task` and `hive` **for backward compat only** — mark deprecated in schema description
- When `delta_report` is provided (pipeline path):
  - Inject delta_report as structured context into plan compiler prompt
  - Inject approved artifact refs
  - Plan compiler prompt: "You receive a delta_report. Translate each operation to executor plan steps using the compiler_class translation table. Do not interpret design intent. Do not add steps not justified by a delta operation."
- When called from pipeline (Track G): always pass delta_report
- When called from free-form chat (legacy path): no delta_report, old behavior, no new development
- Acceptance: plan compiler receiving a delta_report with 3 operations produces an executor_plan with the correct step sequence per the translation table

Implementation note:

- `PlanCompilerTool` now accepts `delta_report` and `approved_artifacts`, injects them into the plan compiler prompt, and routes both pipeline and legacy free-form calls through the same prevalidation/retry transaction helper.
- The free-form path still exists and still stages `plan_compile_pending`; that is intentional backward compatibility.

### [x] TD-3 — Enforce compiler_class translation table in plan compiler
- Add `validate_plan_against_delta(plan: &Value, delta: &DeltaReport) -> Result<()>`:
  - For each delta operation, check that the produced executor plan contains the steps mandated by the translation table
  - Rejects plan if it uses `kill_node -> run_node` for an op classified as `NODE_CONFIG_APPLY_RESTART`
  - Rejects plan if it adds extra steps not justified by any delta op
- Pre-validation calls this function before storing `plan_compile_pending`
- Acceptance: plan with wrong step sequence for a NODE_CONFIG_APPLY_RESTART op is rejected at pre-validation

### [x] TD-4 — Plan compiler cookbook update
- Rename cookbook file from old path to `plan_compile_cookbook_v1.json`
- Update `plan_compile_cookbook_path()` and all read/write calls
- Acceptance: existing cookbook entries survive rename; new entries written to new path

### [x] TD-5 — Rename plan_compile_trace in chat output
- `programmer_trace` -> `plan_compile_trace` in the `handle_run_ai_chat` output JSON
- Update frontend `renderResponsePayload` to check for `plan_compile_trace` (keep backward compat check for `programmer_trace` during migration)
- Acceptance: chat response from plan compiler invocation has `plan_compile_trace` field

### [x] TD-6 — Seed plan_compile_cookbook (see also Track F)
- Extract 3-4 entries from existing successful programmer/plan-compiler runs (those in current `programmer-v1.json`)
- Convert to `CookbookEntryV2` format with `layer: "plan_compile"`, `pattern_key`, `trigger`, `successful_shape`
- Write to `plan_compile_cookbook_v1.json`
- Acceptance: cookbook is non-empty before the first new pipeline run

### [x] TD-8 — Archi host system prompt update for pipeline orchestration

The current Archi system prompt was written for the old single-programmer architecture. It must be rewritten to be compatible with the new pipeline model. Without this, Archi will continue offering the old flow, call deprecated tools, and confuse the two CONFIRM semantics.

What changes:

- Remove all references to `fluxbee_programmer` — replace with `fluxbee_plan_compiler` for the legacy free-form path
- Remove references to `fluxbee_infrastructure_specialist`
- Add pipeline intent detection guidance:
  - When operator intent is clear simple/multi-step mutation → call `fluxbee_plan_compiler` directly
  - When operator intent is deployment/topology/solution design that needs manifest/reconcile → call `fluxbee_start_pipeline` directly
  - When operator intent is a quick query, SCMD, or status check → do not start pipeline
  - When operator intent is ambiguous → ask before choosing
- Add `StartPipelineTool` to Archi's available tools
- Keep CONFIRM 1 context only as compatibility/recovery: "When a pipeline is at Confirm1 stage, CONFIRM approves the design direction — not execution."
- Add CONFIRM 2 context: "When a pipeline is at Confirm2 stage, CONFIRM approves execution of the listed plan steps. Highlight destructive and restarting ops explicitly before asking for confirmation."
- Keep `"Reply CONFIRM to execute or CANCEL to discard"` for direct plan_compiler results; that is the single confirmation for simple operations.

Acceptance: Archi, given a clear mutation request, calls plan_compiler without asking first. Given a broad design request, starts pipeline planning without asking first.

Current code status:

- The Archi host prompt now distinguishes direct plan_compiler mutation intents from broad manifest/reconcile pipeline intents.
- `fluxbee_start_pipeline` no longer stages a start confirmation; it launches internal planning directly.
- `fluxbee_plan_compiler` is now described as the primary executor-plan mutation path for clear operations.

### [x] TD-7 — Deprecate infrastructure_specialist
- Remove `ArchitectInfrastructureSpecialistTool` from Archi's tool registry
- Add deprecation note in code comment: "superseded by real_programmer + artifact_auditor per rearchitecture spec v4"
- Do not delete code yet — keep as dead code for migration safety; mark with `#[allow(dead_code)]`
- Acceptance: Archi no longer calls fluxbee_infrastructure_specialist; existing sessions are unaffected

---

## Track E — Failure Classification

**Blocked by:** Phase 0 (CFZ-8)  
**Primary file:** new module `failure_classifier.rs` or section in `sy_architect.rs`

### [x] TE-1 — FailureClass enum and deterministic matcher

Note: `PolicyBlocked` is removed. Policy and identity are out of scope for this architecture version (per spec v4 section 4.1). If those sections appear in a manifest, they are rejected by `validate_manifest_v2` before reconciliation, not by the failure classifier.

`SnapshotSectionUnsupported` is added for cases where the snapshot builder encounters a requested resource scope it cannot yet read (e.g. a future resource type added to desired_state before the snapshot supports it).

```rust
enum FailureClass {
    DesignIncomplete,
    DesignConflict,
    SnapshotPartialBlocking,
    SnapshotSectionUnsupported,   // scope requested but not yet implemented in snapshot builder
    DeltaUnsupported,
    ArtifactTaskUnderspecified,
    ArtifactContractInvalid,
    ArtifactLayoutInvalid,
    PlanInvalid,
    PlanContractInvalid,
    ExecutionEnvironmentMissing,
    ExecutionActionFailed,
    ExecutionTimeout,
    UnknownResidual,
}

fn classify_failure_deterministic(
    stage: PipelineStage,
    error_text: &str,
    error_code: Option<&str>,
    context: &FailureContext,
) -> Option<FailureClass>

```

- Returns `None` if no deterministic rule matches (caller then invokes AI classifier)
- Pattern matching on error_text (lowercase, trimmed):
  - contains "runtime not materialized" -> ExecutionEnvironmentMissing
  - contains "unknown action" -> PlanInvalid
  - contains "missing required field" and stage is PlanValidation -> PlanContractInvalid
  - contains "missing required field" and stage is ArtifactAudit -> ArtifactContractInvalid
  - contains "package missing" OR "required file" -> ArtifactLayoutInvalid
  - error_code == "SNAPSHOT_PARTIAL_BLOCKING" -> SnapshotPartialBlocking
  - error_code == "EXECUTION_TIMEOUT" -> ExecutionTimeout
- Acceptance: unit test for each deterministic rule

### [x] TE-2 — Residual AI classifier
- Implement `classify_failure_ai(context, stage, error_payload) -> Result<FailureClass>`
- Uses a focused AI call with: stage context, error text, recent trace summary, artifact refs
- Prompt: "Classify this failure into exactly one class from the enum. Return JSON: {class: '...', confidence: 0-1, reasoning: '...'}. Do not invent new classes."
- If confidence < 0.6 or model returns unknown class -> return UnknownResidual
- Acceptance: AI classifier called only when deterministic matcher returns None

### [x] TE-3 — Failure routing table
- Given `FailureClass` -> route to repair loop:
  ```

  DesignIncomplete / DesignConflict -> design loop (if < max iterations)
  ArtifactContractInvalid / ArtifactLayoutInvalid -> artifact loop repair
  PlanInvalid / PlanContractInvalid -> plan compiler retry
  ExecutionEnvironmentMissing -> check if artifact missing -> artifact loop or plan compiler
  ExecutionActionFailed -> failure auditor decision (may need operator)
  ExecutionTimeout -> retry executor (transient)
  SnapshotPartialBlocking -> escalate to operator
  UnknownResidual -> escalate to operator
  ```

- Implement `route_failure(class, pipeline_state) -> FailureRoutingDecision`
- `FailureRoutingDecision`: RetryArtifact, RetryPlanCompile, RetryDesign, RetryExecutor, EscalateToOperator
- Acceptance: routing table test — each class returns expected decision

### [x] TE-4 — Host escalation on unresolvable failure
- When routing decision is EscalateToOperator:
  - Block pipeline run
  - Emit structured message to operator:
    - failed stage
    - failure class
    - what was attempted (N attempts)
    - recommended next action
  - Host presents options: retry from this stage / go back to design / discard
- Acceptance: UnknownResidual failure in artifact loop causes pipeline to pause and show operator escalation message

---

## Track F — Seed Data Preparation

**Unblocked:** can start immediately — no prerequisites  
**No code changes** — data preparation only  
**Blocks agent readiness:** designer, plan compiler, real programmer, and repair loop cannot operate usefully until the seed files exist. Do not treat this as optional or deferrable.

### [x] TF-1 — Seed design_cookbook_v1.json
- Write 2-3 entries covering most common solution topologies:
  1. Single-tenant AI support system: motherbee + dedicated worker, AI.chat + AI.support + WF.router + IO.slack
  2. Basic AI + IO topology: motherbee only, AI.chat + IO.api
  3. Multi-channel with routing: motherbee + worker, AI multi-instance + IO.slack + IO.email + WF.router with tenant prefix routing
- Format per `CookbookEntryV2`: kind=design, pattern_key, trigger, inputs_signature, successful_shape, constraints, recorded_from_run=null (manual seed)

### [x] TF-2 — Seed artifact_cookbook_v1.json
- Write entries from known working runtime/package patterns:
  1. config_only AI runtime over AI.common: package.json shape, default-config.json shape, system.txt
  2. workflow runtime over wf.engine: package.json, workflow_definition.json shape
  3. OPA bundle basic: package.json, policy.rego structure
- Include known anti-patterns in `do_not_repeat`: missing runtime_base, wrong naming (no prefix)

### [x] TF-3 — Seed plan_compile_cookbook_v1.json
- Convert 3-4 successful entries from current `programmer-v1.json` cookbook
- Translate to CookbookEntryV2 format with `layer: "plan_compile"`
- Add new entries covering:
  1. publish + run_node + add_route (most common pattern)
  2. publish + distribute (sync_to + update_to) + run_node
  3. config_set + restart_node
- Discard any entry where the steps are out of lifecycle order

### [x] TF-5 — Rewrite handbook_fluxbee.md for new architecture

The current `handbook_fluxbee.md` was written as a guide for `fluxbee_programmer` — it explains how to generate executor plans directly from a task description. That model is superseded. The handbook is injected into designer, plan_compiler, and real_programmer prompts, so if it's not updated, agents will receive contradictory or obsolete instructions.

This is a full rewrite, not an update. The new handbook must:

**Section 1 — Fluxbee mental model** (keep and update)
- Hives, runtimes, nodes, routes — keep this section, it's correct
- Update the runtime/node relationship section to reflect naming and ownership semantics

**Section 2 — Actor guide (new)**
Three focused sub-sections, one per AI actor that reads this handbook:

*Designer section:*
- Think in desired state — topology, runtimes, nodes, routing
- Do not generate executor_plan or admin calls
- Use `query_hive` to understand live state before designing
- Use `get_manifest_current` when extending an existing solution
- Produce `solution_manifest` with `desired_state` + `advisory` split
- Naming rules, ownership rules, what goes in desired_state vs advisory

*Plan compiler section:*
- Receives a `delta_report` — not a free-form task description
- Translates `compiler_class` values to executor plan steps using the frozen translation table
- Must not invent steps not in the delta
- Use `get_admin_action_help` to build exact args; never guess field names
- Lifecycle order: publish/distribute → run_node → config → routes
- `query_hive` allowed for read-only state confirmation only

*Real programmer section:*
- Receives a `build_task_packet` — not a design task
- Generates the artifact specified in the packet
- Must not make design decisions beyond the packet scope
- Required files per artifact kind (runtime_package: package.json, default-config.json, system.txt)
- Naming rules, runtime_base rules, no-invention rule

**Section 3 — Naming convention** (keep and update)
- `TIPO.nombre@hive` pattern stays
- Add ownership convention per resource type

**Section 4 — Lifecycle order** (keep)
- publish → sync/update → run_node → config → routes
- Still canonical

**Remove:** all sections describing `fluxbee_programmer` as a single generalist agent; all examples showing free-form task-to-plan generation; all references to the old `programmer` role.

Acceptance: designer prompted with this handbook produces manifests with correct desired_state structure; plan_compiler prompted with this handbook does not try to interpret design intent from delta ops.

### [x] TF-4 — Seed repair_cookbook_v1.json
- Write entries from known repeated failure/fix pairs:
  1. Missing package.json -> add package.json with correct fields
  2. Invalid runtime_base -> check known runtimes, correct runtime_base
  3. Wrong node naming (missing prefix) -> rename with correct TIPO.name@hive format
  4. run_node before publish_runtime_package -> reorder steps
- Format per CookbookEntryV2 with `layer: "repair"`, `failure_class` field

---

## Track H — Designer and Design Auditor

**Blocked by:** Phase 0 (CFZ-1, CFZ-6, CFZ-9, CFZ-10, CFZ-13)  
**Primary file:** `src/bin/sy_architect.rs`  
**Depends on:** Track A (manifest storage, get_manifest_current, pipeline_runs)  
**Note:** TG-1 uses these agents internally — Track G is blocked until Track H is complete.

This is the largest missing track. TG-1 needs a designer agent and a design_auditor agent. They are internal pipeline runners, not public tools exposed to Archi.

### [x] TH-1 — Designer agent runner for solution manifest generation

- Input args: `task: String` (human need description), `solution_id: Option<String>` (existing solution to extend), `context: Option<String>` (additional constraints from operator)
- Allowed tools available to the designer AI during its run:
  - `query_hive` (read-only, already exists)
  - `get_manifest_current` (TA-3)
  - handbook/knowledge pack injected into system prompt
  - design cookbook injected into system prompt (from `design_cookbook_v1.json`)
- Forbidden: `get_admin_action_help`, any mutating tool, executor tools
- System prompt (`DESIGNER_SYSTEM_PROMPT`):
  - Role: produce a `solution_manifest` in the v2 `desired_state` + `advisory` format
  - Think in desired state: topology, runtimes, nodes, routing, wf_deployments, opa_deployments
  - Do not produce executor_plan, SCMD, admin calls, or operational payloads
  - Must call `submit_solution_manifest` exactly once
  - May call `query_hive` multiple times to understand current environment
  - May call `get_manifest_current` to read existing manifest when extending
  - Include design cookbook entries as examples
  - Include handbook sections: naming convention, runtime/node relationships, lifecycle order
- `submit_solution_manifest` function schema: full `SolutionManifestV2` structure, strict, `additionalProperties: false`
- After call: validate manifest via `validate_manifest_v2`; store via `save_manifest`; return `DesignerOutput { manifest, solution_id, human_summary }`
- `DesignerOutput` stored in pipeline run state
- Acceptance: given "deploy AI support for acme on worker-220", produces a valid `SolutionManifestV2` with correct `desired_state` sections and no `executor_plan` content

Current code status:

- The designer is implemented as an internal runner used by `run_design_loop(...)`.
- It runs with `query_hive` + `get_manifest_current`, injects handbook + design cookbook context, validates the returned manifest via `validate_manifest_v2`, persists it, and returns `solution_manifest + solution_id + human_summary`.
- It is not registered in `ArchitectAdminReadToolsProvider`; Archi cannot call it directly.

### [x] TH-2 — Designer trace
- `DesignerTrace` struct: task, solution_id, query_hive_calls (count), manifest_version, section_count, validation_result
- Stored in pipeline trace per stage
- Frontend: `renderDesignerTrace(trace)` — collapsed panel showing task sent, live queries made, manifest sections produced
- Acceptance: designer trace appears in pipeline trace panel after designer completes

Current code status:

- `DesignerTrace` now exists in backend output and is emitted by the internal designer runner.
- `run_design_loop(...)` accumulates per-iteration `designer_traces` and persists them in pipeline state.
- The frontend now renders the designer trace as a collapsed "Agent activity · designer" panel in chat responses that came from `fluxbee_start_pipeline`.

### [x] TH-3 — Design loop controller
- Implement `run_design_loop(context, task, solution_id, operator_context) -> Result<DesignLoopOutput>`
- `DesignLoopOutput`: manifest, audit_verdict, iterations_used, stopped_reason
- Loop per CFZ-6 stop conditions:
  ```

  for iteration in 0..MAX_DESIGN_ITERATIONS (default 3):
      manifest = run_designer(context, task, solution_id, feedback.as_ref())
      verdict = run_design_auditor(context, &manifest)
      if verdict.status == Pass:
          break
      if score did not improve vs previous iteration:
          break (stop_reason = NoScoreImprovement)
      if same blocking_issue_signature appeared twice:
          break (stop_reason = RepeatedBlocker)
      feedback = build_design_feedback(&verdict)
  if blocked: escalate to host
  ```

- `feedback` passed to next iteration is a structured string extracted from `design_audit_verdict.findings`
- Acceptance: design loop with an intentionally incomplete manifest (missing topology) iterates and improves on second attempt

Current code status:

- `run_design_loop(...)` now exists and is wired into pipeline start.
- It persists the manifest and `design_audit_{iteration}.json` per iteration, carries structured feedback into the next designer pass, and stops on `pass`, `reject`, no score improvement, repeated blocker signature, or max iterations.
- It also stores `design_loop_trace` in pipeline state for later frontend rendering.

### [x] TH-4 — Design auditor agent runner for manifest review

- Input: full `SolutionManifestV2` JSON + optional previous audit context
- System prompt (`DESIGN_AUDITOR_SYSTEM_PROMPT`):
  - Role: review a solution_manifest and produce a `design_audit_verdict`
  - Check desired_state for: completeness, internal consistency, ownership clarity, topology feasibility
  - Check advisory for: unresolved risks, weak assumptions
  - Must call `submit_design_audit_verdict` exactly once
  - Do not suggest executor steps or admin operations
  - Score 0-10 on completeness and consistency
  - List blocking issues with short string keys (not prose paragraphs)
- Reads: `desired_state` + `advisory`; optionally `get_manifest_current` to compare against prior version
- `submit_design_audit_verdict` schema:
  ```json
  {
    "status": "pass | revise | reject",
    "score": 0-10,
    "blocking_issues": ["ISSUE_KEY_1", "ISSUE_KEY_2"],
    "findings": [{"code": "string", "section": "string", "message": "string", "severity": "error|warning|info"}],
    "summary": "one paragraph in plain language for the operator"
  }
  ```

- `design_audit_verdict.score` drives loop stop conditions (CFZ-6)
- `blocking_issues` strings are used for dedup detection (same key twice = stop)
- Acceptance: auditor on a manifest missing all node definitions returns `revise` verdict with score <= 3 and blocking issue `NODES_MISSING`

Current code status:

- The design auditor is implemented as an internal runner used by `run_design_loop(...)`.
- It reads a full manifest plus optional prior context, forces one `submit_design_audit_verdict` call, validates `status`/`score`/`findings`, and returns a `DesignAuditVerdict`.
- It is not registered in `ArchitectAdminReadToolsProvider`; Archi cannot call it directly.

### [x] TH-5 — DesignAuditVerdict Rust struct

```rust
struct DesignAuditVerdict {
    verdict_id: String,
    manifest_version: String,
    status: DesignAuditStatus,   // Pass | Revise | Reject
    score: u8,                   // 0-10
    blocking_issues: Vec<String>,
    findings: Vec<DesignFinding>,
    summary: String,
    produced_at_ms: u64,
}

struct DesignFinding {
    code: String,
    section: String,
    message: String,
    severity: AuditSeverity,
}

```

- Stored in pipeline state; persisted to `{state_dir}/pipeline/{run_id}/design_audit_{iteration}.json`
- Acceptance: struct round-trips through JSON; verdict stored and loadable

### [x] TH-6 — CONFIRM 1 summary builder
- Implement `build_confirm1_summary(manifest, verdict) -> Confirm1Summary`
- `Confirm1Summary`: solution name, topology summary (N hives, M nodes, K routes), main runtimes listed, audit score, blocking issues if any, advisory highlights
- This is what the operator sees before CONFIRM 1 (per spec 14.1)
- Host renders as a structured chat message (not raw JSON)
- Must highlight any `verdict.status == Revise` warnings even if proceeding
- Acceptance: CONFIRM 1 message includes hive count, node count, audit score, and any blocking issues in readable form

Current code status:

- `build_confirm1_summary(...)` now exists and is covered by tests for hive/node/route counts, runtime extraction, advisory highlights, and warning surfacing.
- The summary is now persisted in pipeline state. It is rendered only if an older/recovered run is still parked at Confirm1; new runs auto-advance to Confirm2.

### [x] TH-7 — Design loop trace rendering
- `DesignLoopTraceEvent` per iteration: iteration number, status (designing/auditing/pass/revise), score, blocking_issue_count, stopped_reason
- Frontend: `renderDesignLoopTrace(events)` — show iterations with scores improving over iterations
- Collapsed by default; auto-expand if stopped early
- Acceptance: operator sees design loop iterations in trace panel

Current code status:

- `DesignLoopTraceEvent` now exists and is persisted in pipeline state as `design_loop_trace`.
- The frontend now renders it as a collapsed "Agent activity · design_loop" panel in chat responses from `fluxbee_start_pipeline`.

---

## Track G — Final Pipeline Assembly

**Blocked by:** Tracks A + B + C + D + E + F all substantially complete  
**Primary file:** `src/bin/sy_architect.rs`

### [x] TG-1 — Connect design loop → CONFIRM 1 → reconciler

**Requires Track H complete** (designer runner, design auditor runner, `run_design_loop` all implemented and tested).

**Pipeline trigger — how it starts:**

The operator does not need a special command. Archi detects deployment/configuration intent from natural language and offers to start a pipeline.

Trigger flow:
1. Operator sends a message describing a need ("quiero desplegar soporte para acme", "necesito agregar un nodo WF", etc.)
2. Archi classifies the intent as pipeline-eligible (new solution, topology change, node/route addition that requires design)
3. Archi responds with a brief summary of what it understood and asks: "¿Querés que diseñe la solución completa y te la presente para revisión antes de ejecutar?"
4. Operator confirms (free text "sí", "ok", "adelante", etc.)
5. Archi calls `start_pipeline(task, solution_id)` internal tool — this creates the pipeline run in Design stage

**What makes intent pipeline-eligible vs free-form:**

Archi system prompt (TD-8) must define this distinction explicitly:

- Pipeline-eligible: intent involves creating a solution, changing topology, or broad desired-state work that benefits from manifest + reconciliation
- Plan compiler direct path: clear mutations, including quick one-step and multi-step admin-backed changes that do not need manifest design

Archi must not ask for permission before starting internal planning when the intent is clear. The operator confirms once before execution, after an executor plan exists. `Confirm1` remains only as a compatibility/recovery handler for older in-flight runs.

- Add `start_pipeline_run(state, session_id, solution_id) -> Result<PipelineRunRecord>`
- Add `StartPipelineTool` as an internal Archi tool:
  - Input: `task: String`, `solution_id: Option<String>`
  - Starts the design loop directly; no pending pre-confirmation
- Call `run_design_loop(context, task, solution_id, operator_context)` (TH-3):
  - Internally calls the designer runner and design auditor runner per iteration
  - Returns `DesignLoopOutput { manifest, verdict, iterations_used, stopped_reason }`
- If design loop stops with `verdict.status != Pass`: transition to Blocked; present blocking issues to operator
- If design loop completes with pass verdict: persist `Confirm1` compatibility state, then auto-advance to Reconcile without operator input.
- Reconcile stores delta_report + task_packets in pipeline state and continues to ArtifactLoop or PlanCompile.
- Acceptance: full design → reconcile → plan compile sequence produces a Confirm2 executor plan without human intervention before the final execution confirmation.

Current code status:

- `fluxbee_start_pipeline` now exists as an internal host tool.
- It now launches the design loop immediately. The old pending-start helpers remain only as compatibility code.
- The current execution path is: create pipeline run in `Design`, execute `run_design_loop(...)`, auto-advance through `Reconcile`, optional `ArtifactLoop`, and `PlanCompile`, then return the operator-facing Confirm2 plan.
- `handle_pipeline_confirm1(...)` remains only for compatibility/recovery of older runs already parked at Confirm1.
- The full pipeline path is therefore now `StartPipelineTool -> Design loop -> Reconcile -> ArtifactLoop? -> PlanCompile -> Confirm2`.

### [x] TG-2 — Connect reconciler → artifact loop → plan compiler
- After reconciler: if `build_task_packets` non-empty, transition to ArtifactLoop stage; call `run_artifact_loop`
- After artifact loop: approved artifacts registered; transition to PlanCompile stage
- Invoke `run_plan_compiler_with_context` with delta_report + approved artifact refs
- Plan compiler produces executor_plan; store in pipeline state; transition to PlanValidation stage
- Run plan validator; run failure classifier on any issues
- On validation pass: transition to Confirm2 stage; present CONFIRM 2 payload
- Acceptance: reconciler with 2 task_packets → artifact loop → plan compiler → executor_plan stored in pipeline

Current code status:

- Reconcile → PlanCompile is now wired for the no-artifact case: when `build_task_packets` is empty, the pipeline auto-invokes the plan compiler using `delta_report`, persists `executor_plan`, advances through `PlanValidation`, and emits a Confirm2 payload with destructive/restarting highlights.
- ArtifactLoop now runs through the canonical `real_programmer -> artifact_auditor` path for the currently supported `runtime_package` scope:
  - `inline_package` and `bundle_upload` targets are both finalized into canonical `publish_source` payloads after approval
  - approved artifacts are expanded before plan compilation so the plan compiler receives concrete `publish_source` refs instead of raw bundle ids only
- The artifact loop is also visible in the chat UI via `artifact_loop_trace`.
- This task remains open only because artifact-kind coverage is still intentionally partial outside `runtime_package`.

### [x] TG-3 — Connect plan compiler → CONFIRM 2 → executor
- On CONFIRM 2: retrieve executor_plan from pipeline state; call `execute_executor_plan_with_context`
- Store execution_report; transition to Verify stage
- On execution success: check cookbook eligibility; write cookbook entries if approved
- On execution failure: run failure classifier; route per routing table
- Acceptance: CONFIRM 2 triggers executor using plan from pipeline state; result stored as execution_report

Current code status:

- CONFIRM 2 now loads `executor_plan` from pipeline state, executes it, advances through `Execute -> Verify`, classifies/routs execution failures through the deterministic failure classifier, and finalizes the run as `Completed`, `Blocked`, or `Failed` as appropriate.
- Successful verified runs now persist `verification_verdict` and append an observed `plan_compile` cookbook entry when eligible.

### [x] TG-4 — E2E test: design → execute success
- Fixture: human input "deploy AI support for tenant acme on worker-220 with slack integration"
- Expected flow: designer produces manifest → design audit passes → CONFIRM 1 → reconciler produces delta → artifact loop produces packages → plan compiler produces plan → CONFIRM 2 → executor succeeds
- Acceptance: full pipeline completes without human intervention between CONFIRM 1 and CONFIRM 2

### [x] TG-5 — E2E test: artifact repair loop
- Fixture: real_programmer intentionally produces a bundle with missing package.json on first attempt
- Expected flow: artifact auditor catches it → repair_packet generated → second attempt succeeds → approved
- Acceptance: pipeline recovers without operator intervention; trace shows 2 attempts

### [x] TG-6 — E2E test: interrupted run recovery
- Start a pipeline; kill process mid-artifact-loop; restart server; open same session
- Expected flow: host detects interrupted run → presents recovery options to operator
- Acceptance: operator can resume or discard; pipeline state is intact

### [x] TG-7 — E2E test: partial snapshot blocking
- Mock one hive as unreachable during snapshot build
- Expected flow: snapshot partial → reconciler blocks → host presents SNAPSHOT_PARTIAL_BLOCKING to operator
- Acceptance: no destructive ops emitted; operator sees clear error about which hive is down

### [x] TG-8 — Verification auditor v1
- Implement `verify_execution_result(execution_report: &ExecutionReport) -> VerificationVerdict`
- `VerificationVerdict`: `{ eligible_for_cookbook: bool, reason: String, verified_success: bool }`
- V1 logic is deterministic — no AI needed:
  - `verified_success = true` if `execution_report.summary.status == "done"` AND `execution_report.summary.failed_step_id == None`
  - `eligible_for_cookbook = true` if `verified_success` AND `execution_report.summary.completed_steps == execution_report.summary.total_steps`
  - All other cases: `eligible_for_cookbook = false` with reason string
- Called from TG-3 after executor completes
- If `eligible_for_cookbook`: for each layer that participated in this run (plan_compile always; artifact if artifact loop ran; design if design loop ran), write cookbook entry via `append_cookbook_entry(layer, entry)`
- Cookbook entries built from: `delta_report` ops → plan_compile entry; approved `artifact_bundles` → artifact entries; `solution_manifest` → design entry if loop ran cleanly
- Acceptance: successful full pipeline run writes at minimum one `plan_compile` cookbook entry; failed pipeline writes nothing

Current code status:

- `verify_execution_result(...)` is now implemented deterministically and wired into the CONFIRM 2 success path.
- Eligible runs now append observed `design`, `plan_compile`, and `artifact` cookbook entries only after verification passes; failed or unverified runs write nothing.

---

## Testing summary

| Layer | Test type | Location |
|---|---|---|
| Reconciler (Track B) | Unit tests with fixtures | `tests/reconciler/` |
| Artifact auditor (Track C) | Unit tests per artifact kind | `tests/artifact_auditor/` |
| Failure classifier (Track E) | Unit tests per rule | `tests/failure_classifier/` |
| Plan compiler vs delta | Integration test | manual / E2E |
| Full pipeline | E2E tests (TG-4 to TG-7) | manual with test harness |

All reconciler and classifier tests must pass without any AI call.

---

---

## Fase 2 — Refinamiento de comportamiento

**Fecha:** 2026-04-25  
**Origen:** diagnóstico de sesión de debug — fallo en `run_node` / exceso de preguntas al operador  
**Principios:**
- Archi es coordinador puro — conoce Fluxbee, ordena el trabajo, nunca ejecuta mutaciones
- Toda mutación (simple o compleja) pasa por el plan_compiler
- Los agentes iteran internamente hasta budget o límite — no preguntan al operador entre iteraciones
- El handbook y el help de admin son la fuente de conocimiento operativo — deben ser completos
- El resultado de cada tarea es visible y explícito en el chat

---

## Track I — Archi como coordinador puro

**Objetivo:** Archi pierde todas las herramientas de escritura directa. El plan_compiler es el único ejecutor de mutaciones. Archi lee el handbook para conocer Fluxbee y conversar bien con el operador.

**Depende de:** Track K (prompts), Track L (handbook)

### [x] TI-1 — Remover tools de escritura del provider de Archi

Sacar de `ArchitectAdminReadToolsProvider::register_tools`:

- `ArchitectSystemWriteTool` (`fluxbee_system_write`)
- `ArchitectSetNodeConfigTool` (`fluxbee_set_node_config`)
- `ArchitectDeployWorkflowTool` (`fluxbee_deploy_workflow`)
- `ArchitectDeployOpaPolicyTool` (`fluxbee_deploy_opa_policy`)
- `ArchitectPublishRuntimePackageTool` (`fluxbee_publish_runtime_package`)

Le quedan a Archi:

- `fluxbee_system_get` — lectura del sistema
- `fluxbee_plan_compiler` — único path para mutaciones
- `fluxbee_start_pipeline` — para soluciones complejas

Los agentes designer / design_auditor quedan sólo como implementación interna del pipeline. No se registran en el provider público de Archi.

Acceptance: Archi no puede stagear ninguna mutación directa. Cualquier intento va por plan_compiler.

### [x] TI-2 — Inyectar handbook en el prompt de Archi

Hoy `ARCHI_SYSTEM_PROMPT` es una constante estática. Cambiar a una función `build_archi_prompt(handbook: Option<&str>) -> String` que:

- Incluye el handbook si está disponible (`/etc/fluxbee/handbook_fluxbee.md` o path de dev)
- Lo inyecta al final del prompt base, igual que Designer y RealProgrammer

Acceptance: Archi conoce los naming conventions, las acciones disponibles y los campos de run_node con tenant_id.

### [x] TI-3 — Actualizar descripción de `fluxbee_plan_compiler` tool

La descripción actual dice "DEPRECATED free-form path". Cambiar para reflejar que es el único camino de mutaciones:

- Eliminar la palabra DEPRECATED de la description
- Aclarar: "usar para TODA mutación — simple (1 step) o compleja (N steps)"
- Ejemplos en la description: "crear un nodo", "agregar una ruta", "publicar un runtime", etc.

Acceptance: el modelo entiende que `fluxbee_plan_compiler` es la herramienta para cualquier operación que cambie estado, sin importar la complejidad.

### [x] TI-4 — Regla anti-pregunta en ARCHI_SYSTEM_PROMPT

Agregar regla explícita: "Cuando el intent de mutación del operador es claro y tenés los datos necesarios, llamá `fluxbee_plan_compiler` directamente. No pedís confirmación previa ni preguntás si debés proceder."

Distinguir:

- **Intent ambiguo** (operador no especificó hive, tenant, nombre) → una pregunta específica sobre el dato que falta
- **Intent claro con datos completos** → llamar plan_compiler directamente

Acceptance: dado "creá el nodo AI.support@motherbee con runtime ai.common y tenant tnt:xxx", Archi llama plan_compiler sin preguntas.

### [x] TI-5 — Eliminar confirmación previa para iniciar pipeline

`fluxbee_start_pipeline` ya no stagea un `pending_pipeline_start` ni pregunta "¿lo lanzo?". Si Archi decide usar el pipeline, el tool inicia directamente el design loop interno y devuelve el estado resultante.

Acceptance: el operador no debe confirmar antes de que corran los agentes internos de diseño.

### [x] TI-6 — Colapsar Confirm1/Confirm2 a una sola confirmación final

La estrategia nueva pide una sola confirmación del operador: cuando el executor plan ya está listo. El código todavía conserva `Confirm1` por compatibilidad del pipeline previo:

- `Confirm1` aprueba dirección de diseño y dispara Reconcile/Artifact/PlanCompile.
- `Confirm2` aprueba ejecución del executor plan.

Implementado: los pipelines nuevos avanzan automáticamente después del design loop exitoso por Reconcile, ArtifactLoop si aplica, y PlanCompile hasta dejar el run en `Confirm2`. `Confirm1` queda sólo como handler de compatibilidad/recovery para runs viejos que ya hayan quedado estacionados ahí.

Acceptance: una tarea compleja clara no requiere intervención humana entre `fluxbee_start_pipeline` y el plan listo para ejecutar.

---

## Track J — Token budget por tarea

**Objetivo:** Cada tarea tiene un presupuesto de 5000 tokens para todos los agentes invocados (sin contar Archi). Si se supera, los loops retornan error estructurado sin preguntar al operador.

**Constante inicial:** `TASK_AGENT_TOKEN_BUDGET: u32 = 5_000` — hardcodeada. Cuando esté estable se expone en CONFIG SET/GET del nodo SY.architect.

### [x] TJ-1 — Capturar tokens usados por cada llamada AI

En `FunctionCallingRunner` o en los wrappers de agentes, capturar `usage.total_tokens` de cada response de OpenAI. El SDK ya tiene acceso a este campo en la response.

Agregar a `FunctionCallingResult`:

```rust
pub tokens_used: u32,  // total tokens consumidos en este run

```

Acceptance: cada llamada a `run_with_input` retorna `tokens_used` en su resultado.

### [x] TJ-2 — Acumulador de tokens por tarea y constante de budget

Agregar en `sy_architect.rs`:

```rust
pub const TASK_AGENT_TOKEN_BUDGET: u32 = 5_000;

```

Cada tarea (pipeline run o plan_compiler call) mantiene un acumulador `tokens_used: u32` que suma los tokens de todos los agentes invocados (designer, design_auditor, real_programmer, plan_compiler). Archi no cuenta.

Acceptance: el acumulador se inicializa en 0 al empezar una tarea y suma correctamente. Para pipeline, design → artifact → plan_compile comparten el total acumulado; para plan_compiler directo el acumulador inicia en 0.

### [x] TJ-3 — Check de budget en cada loop de agente

En los loops que invocan agentes (design loop, artifact loop, plan_compile):

- Antes de cada invocación de agente, verificar `accumulated_tokens < TASK_AGENT_TOKEN_BUDGET`
- Si se supera: retornar `FailureClass::UnknownResidual` con mensaje:
  ```json
  { "error": "BUDGET_EXCEEDED", "tokens_used": N, "budget": 5000, "stage": "artifact_loop" }
  ```

- No preguntar al operador — solo reportar

Acceptance: un loop que gasta más de 5000 tokens se detiene con error explícito sin iteraciones adicionales.

### [x] TJ-4 — Incluir token usage en el reporte de tarea al operador

Cuando una tarea completa (éxito o fallo), incluir en el mensaje al operador:

```

Tokens usados: 1.847 / 5.000
Agentes involucrados: designer (412t), design_auditor (289t), plan_compiler (1.146t)

```

Acceptance: el operador puede ver cuánto gastó cada tarea para ajustar el budget o el prompt.

Nota de implementación: el reporte incluye `design`, `artifact`, `plan_compile`, `total` y `budget` en payloads de pipeline. El formato textual final del frontend puede mejorarse después; el dato ya queda estructurado.

---

## Track K — Revisión de prompts

**Objetivo:** Cada prompt es concreto, sin hedging, y solo contiene reglas del responsable de ese agente. Las reglas de WF/OPA que hoy están en Archi se mueven al plan_compiler que es quien genera esos steps.

### [x] TK-1 — Reescribir ARCHI_SYSTEM_PROMPT

Remover del prompt de Archi:

- Todas las reglas específicas de WF (`wf_rules_*`, `workflow_name` format, WF `definition` schema completo)
- Las reglas de OPA
- Las reglas de `CONFIG_SET` / `CONFIG_GET` por nodo específico
- Las referencias a `example_scmd` como guía de ejecución (ya no ejecuta)

Agregar/mantener:

- Quién es Archi: coordinador, conoce Fluxbee, ordena el trabajo al plan_compiler
- Cuándo usar `fluxbee_plan_compiler` vs `fluxbee_start_pipeline`
- Regla de no preguntar cuando el intent es claro (ver TI-4)
- Distinguir runtime de nodo, hive de nodo, etc. (conocimiento para conversar bien)

Acceptance: el prompt de Archi tiene menos de 600 palabras y no contiene schemas de acciones de admin.

### [x] TK-2 — Refocalizar PLAN_COMPILER_SYSTEM_PROMPT

Agregar:

- "Sos el único agente que ejecuta mutaciones en Fluxbee — simple o complejo"
- Reglas específicas de WF (las que se sacan de Archi van acá)
- Reglas de OPA, CONFIG_SET
- Énfasis en `get_admin_action_help` como primera acción antes de generar cualquier step
- El plan de 1 step es tan válido como uno de N steps — no inventar complejidad

Acceptance: el plan_compiler sabe manejar `{"task": "crear nodo AI.support@motherbee con runtime ai.common y tenant tnt:xxx"}` y produce el plan correcto sin consultar al operador.

### [x] TK-3 — Limpiar y enfocar DESIGNER_SYSTEM_PROMPT

Revisar que el designer solo hable de solution_manifest. Ninguna mención de executor_plan, admin steps, o ejecución.

### [x] TK-4 — Limpiar DESIGN_AUDITOR_SYSTEM_PROMPT

Confirmar que el auditor no propone cambios — solo clasifica y puntúa. Criterios de `revise` vs `reject` claros y cortos.

### [x] TK-5 — Limpiar REAL_PROGRAMMER_SYSTEM_PROMPT

Confirmar que el programmer no decide nada de topología — solo genera el artifact bundle para lo que le pide el packet. Sin preguntas.

### [x] TK-6 — Revisar descriptions de tools de Archi

- `fluxbee_system_get`: acortar description — hoy tiene ~500 palabras, bajar a ~100. Los paths de ejemplo quedan en el handbook.
- `fluxbee_start_pipeline`: acotar a "soluciones multi-recurso que necesitan manifest + reconcile". Un solo nodo NO va por acá.
- `fluxbee_plan_compiler`: expandir — "toda mutación de estado, desde 1 step hasta N steps".

### [x] TK-7 — Reducir schemas embebidos en prompt de plan_compiler

El `PLAN_COMPILER_SYSTEM_PROMPT` no debe inyectar el `request_contract` completo de las 66 acciones en cada run, porque compite con el budget de 5000 tokens. El prompt mantiene catálogo compacto y obliga a llamar `get_admin_action_help` antes de emitir steps.

Acceptance: el plan_compiler recibe nombres/descripciones compactas y consulta help detallado bajo demanda.

---

## Track L — Handbook y admin help

**Objetivo:** El handbook es la fuente operativa completa para todos los agentes. El help de admin llega al modelo en formato interpretable.

### [x] TL-1 — Sección Archi en el handbook

Agregar sección "Para Archi — operaciones directas frecuentes":

- Ciclo de vida de un nodo: `run_node` (crear), `start_node` (reiniciar existente), `restart_node` (reiniciar corriendo), `kill_node` (detener)
- `run_node` completo con `tenant_id`, todos los campos opcionales
- `add_route` con todos los campos
- `kill_node` con `purge_instance`
- La diferencia crítica: `get_node_config` (snapshot en disco) vs `node_control_config_get` (live del nodo corriendo)
- Cuándo usar plan_compiler directo vs fluxbee_start_pipeline

### [x] TL-2 — Completar gaps del handbook

Los gaps marcados con `[ ]` al final del handbook:

- `tenant_id` en run_node: documentar que es requerido para nodos AI/IO multi-tenant y no se puede agregar después
- `executor_fill`: documentar qué campos puede completar el executor automáticamente
- Topologías multi-hive: cuándo un nodo vive en `worker-*` vs `motherbee`
- Convenciones de naming para nodos IO con tenant

### [x] TL-3 — Reformatear respuesta de `get_admin_action_help`

`PlanCompilerHelpTool::call()` devuelve ahora una capa `help` LLM-readable además del `raw` original:

- Los campos requeridos vs opcionales son visibles
- El `example_scmd` o ejemplo es legible para el modelo
- `path_patterns` se marcan explícitamente como templates
- El modelo recibe instrucciones de usar `example_scmd` y `request_contract`, sin inventar campos

Acceptance: el plan_compiler puede interpretar la respuesta de get_admin_action_help para run_node y generar el step correcto con tenant_id.

### [ ] TL-5 — Capturar `get_admin_action_help("run_node")` real post-cambio

Pendiente operativo en ambiente real: llamar `get_admin_action_help("run_node")`, guardar el JSON completo y confirmar que la capa `help` generada contiene `tenant_id`, `runtime_version`, `node_name` y ejemplo ejecutable.

Acceptance: queda evidencia real de que el modelo recibe un help legible y suficiente para `run_node`.

### [x] TL-4 — Verificar que `fluxbee_system_get` puede llamar `get_admin_action_help`

Confirmar que Archi puede llamar `GET /admin/actions/run_node` vía `fluxbee_system_get` y obtener el mismo help que usa el plan_compiler. Así Archi puede responder preguntas sobre qué es posible.

---

## Orden de implementación sugerido

```

TL-1, TL-2  →  Handbook completo (contenido, sin código)
TL-3, TL-4  →  Help format + verificación
TI-1        →  Sacar write tools de Archi
TI-2        →  Archi lee handbook
TK-1..TK-6  →  Revisar todos los prompts y tool descriptions
TI-3, TI-4  →  Actualizar plan_compiler desc + regla anti-pregunta
TJ-1..TJ-4  →  Token budget
E2E         →  Probar flujo completo con una sola confirmación final

```

---

## Open items (not blockers for task list)

- `get_manifest_current` multi-tenant detail: contract defined at CFZ-9; if multiple solutions are active in one session, the tool resolves by `solution_id`; if none provided, uses the most recent active pipeline run's `solution_id`. Implementation detail deferred to Track A.
- WF_REMOVE and OPA_REMOVE: `compiler_class` values emitting BLOCKED until canonical remove actions are defined in the admin surface. Reconciler emits these as BLOCKED ops; plan compiler receives them as BLOCKED and skips to avoid partial execution.
- `fluxbee_infrastructure_specialist`: deprecated per spec v4 section 17.3 (TD-7). Code deletion deferred to after Track G E2E tests pass — kept as `#[allow(dead_code)]` in the interim.
- OPA exact admin action name for status read: TBD in CFZ-2 freeze — the snapshot builder must use whatever admin action is canonical for reading current OPA deployment state; if not yet defined, OPA snapshot section is optional and its absence does not block snapshot completeness.
- `DesignAuditVerdict.score` scale: CFZ-6 defines the 0-10 scale and improvement threshold. The design auditor prompt must align with that scale — calibration may need one tuning pass after first real pipeline runs.
- Legacy free-form chat path (TD-2): marked deprecated; targeted for removal after Track G E2E tests confirm the pipeline path covers all cases.
