# Fluxbee — Master Rearchitecture Spec v4

**Status:** canonical rearchitecture proposal — contract-frozen implementation version (v4)  
**Date:** 2026-04-23  
**Audience:** `SY.architect`, `SY.admin`, `SY.orchestrator`, runtime/package developers, AI-agent designers, operator UX  
**Purpose:** define the final architecture above the working executor layer so Fluxbee can scale from human-described need to validated solution, generated artifacts, deterministic reconciliation, executable plan, and exact control-plane execution.

---

## 1. Executive summary

Fluxbee already proved one boundary that must remain intact:

- `executor_plan` as the exact operational artifact
- `SY.admin` as the AI executor home
- one OpenAI function per real admin action
- strict schemas and exact admin names
- visible execution trace in chat

That boundary stays exactly as defined in the executor pilot spec fileciteturn12file1 and its task doc fileciteturn12file2.

What changes in v3 is everything **above** that boundary.

The canonical pipeline is now:

```text
human need
  -> host
  -> designer
  -> solution_manifest
  -> design_auditor
  -> CONFIRM 1
  -> deterministic reconciler (Rust)
       -> actual_state_snapshot
       -> delta_report
       -> build_task_packets when artifacts are missing
  -> artifact loop
       -> real_programmer
       -> artifact_bundle
       -> artifact_auditor
       -> repair_packet
       -> repeat or approve
  -> plan_compiler AI
       -> executor_plan
  -> plan validation
  -> deterministic failure classifier
  -> residual AI classifier only if needed
  -> CONFIRM 2
  -> executor in SY.admin
  -> execution_report
  -> cookbook writes (verified successes only)
```

The decisive architectural rule is:

> **Deterministic code computes reality and deltas. AI generates designs, artifacts, and plans inside bounded contracts.**

---

## 2. Non-negotiable architecture decisions

### 2.1 Preserve the executor boundary

Keep as canonical:

- `executor_plan`
- executor in `SY.admin`
- one executor-visible function per real admin action
- exact admin names
- strict JSON schemas
- help as support, not primary contract
- stop-on-error
- chat-visible execution events

This is the only part already proved by the pilot and must not be weakened fileciteturn12file1 fileciteturn12file2.

### 2.2 `solution_manifest` stays declarative

The `solution_manifest` remains the canonical design artifact for one tenant solution.

It is:

- versionable
- diffable
- auditable
- rollbackable
- the source of desired state

It is not:

- an execution script
- a macro language
- a sequence of admin calls
- an `executor_plan`

This preserves the original role of the manifest spec as desired state plus reconciliation target fileciteturn12file0.

### 2.3 The reconciler is deterministic Rust code, not an AI agent

The old phrase "manifest compiler" is removed because it suggested an AI specialist in the middle.

The correct component is:

- **reconciler** (or `deterministic_reconciler`)

Its job is:

`desired_state + actual_state_snapshot -> delta_report`

It is responsible for:

- ownership-aware state comparison
- idempotent create/update/delete/no-op classification
- deterministic change decomposition
- artifact requirements detection
- blocking conditions and partial-snapshot safety decisions

It is not responsible for:

- creative interpretation
- architectural inference
- natural-language explanations
- code/package generation
- transforming deltas into chat-facing execution steps

### 2.4 AI is used only where generation or bounded repair is truly needed

AI remains appropriate for:

- solution design
- design audit
- artifact generation
- artifact repair
- plan compilation (`delta_report` -> `executor_plan`)
- residual failure classification when deterministic rules do not match

### 2.5 The real programmer and artifact auditor are one loop

No generated artifact may advance without passing structural validation.

Canonical loop:

`build_task_packet -> real_programmer -> artifact_bundle -> artifact_auditor -> repair_packet -> loop`

### 2.6 Human confirmation is exactly two checkpoints

There are only two operator pauses in the default pipeline.

#### CONFIRM 1
After design audit, before any artifact construction or delta-driven implementation.

The operator approves the **direction**.

#### CONFIRM 2
After `executor_plan` compilation and validation, before execution.

The operator approves the **operation**.

Everything else iterates internally and is shown only via trace panels unless a loop blocks.

### 2.7 Failure classification is deterministic first, AI second

Order of classification:

1. hardcoded deterministic matcher
2. residual AI classifier only if no deterministic rule matched
3. host/operator escalation if still unresolved

### 2.8 Learning is stratified, not global

Separate cookbooks are mandatory:

- `design_cookbook`
- `artifact_cookbook`
- `plan_compile_cookbook`
- `repair_cookbook`

### 2.9 No staged architecture drift

The implementation must not follow a "ship partial architecture and rewrite later" approach.

Rules:

- freeze contracts first
- define missing vocabularies before coding
- implement the full cross-layer contract in one coherent change package
- do not introduce temporary names or temporary middle abstractions that will later be renamed

This does **not** mean all code lands in one commit. It means the design contracts are frozen before implementation starts, so code is written once against the final shape.

---

## 3. Canonical artifacts

### 3.1 Design-layer artifacts

#### `solution_manifest`
Canonical declarative source of a tenant solution.

#### `design_audit_verdict`
Auditor output over the manifest.

### 3.2 Reconciler artifacts

#### `actual_state_snapshot`
Deterministic multi-call view of the live system used for reconciliation.

#### `delta_report`
Deterministic result of comparing `desired_state` to `actual_state_snapshot`.

### 3.3 Artifact-loop artifacts

#### `build_task_packet`
Bounded programming task derived from the manifest and delta.

#### `artifact_bundle`
Generated package/config/workflow/policy bundle.

#### `artifact_audit_verdict`
Structural validation result for the artifact bundle.

#### `repair_packet`
Focused feedback for another artifact-loop attempt.

### 3.4 Planning artifacts

#### `executor_plan`
Exact operational plan for the executor.

#### `plan_validation_report`
Result of validating the plan against executor-visible schemas and policy rules.

### 3.5 Execution artifacts

#### `execution_report`
Final result of executor run plus step outcomes.

### 3.6 Learning artifacts

#### `cookbook_entry_v2`
Layer-specific reusable pattern, stored only after verified success.

---

## 4. `solution_manifest` cleanup

The manifest must be split into two top-level blocks:

```json
{
  "manifest_version": "1.0",
  "solution": { ... },
  "desired_state": { ... reconciler-consumed state ... },
  "advisory": { ... design notes, sizing, rationale, optional constraints ... }
}
```

### 4.1 `desired_state`
Contains only information needed by the reconciler for the currently supported solution scope:

- topology/hives/vpns
- runtimes/packages
- nodes
- routing
- WF deployment state that is part of the solution
- OPA deployment state that is part of the solution
- ownership markers if needed

Explicitly out of scope for this architecture version:

- policy matrices / normative policy design
- identity / tenant lifecycle / ILK provisioning

Rule: if a manifest includes unsupported top-level `desired_state` sections such as `policy` or `identity`, host validation must fail before reconciliation. They must not be ignored silently.

### 4.2 `advisory`
Contains design metadata not consumed by reconciliation:

- sizing notes
- reasoning notes
- optional constraints not yet enforced
- operator-facing observations
- recommendations

### 4.3 Read rules

- **reconciler** reads only `desired_state`
- **design auditor** reads `desired_state` and `advisory`
- **host summary** may read both

---

## 5. AI actor roles and tools

### 5.1 Host (`SY.architect`)

Responsibilities:

- talk to operator
- maintain pipeline run state
- invoke AI actors and deterministic components
- render traces and summaries
- enforce the two CONFIRM checkpoints
- persist pipeline artifacts and references

The host does not design, program, reconcile, or execute directly.

### 5.2 Designer

Responsibility:

- transform human need into `solution_manifest`
- reason in desired state, not admin contracts

Allowed tools:

- `query_hive`
- `get_manifest_current`
- handbook/knowledge pack

Forbidden tools:

- `get_admin_action_help`
- admin mutation surface
- executor-visible function contracts

`get_manifest_current` is mandatory and must read the current manifest for an existing solution from storage.

### 5.3 Design auditor

Responsibility:

- review `solution_manifest`
- score completeness, consistency, and feasibility
- identify unresolved design gaps

Reads:

- `desired_state`
- `advisory`
- optional current manifest context

### 5.4 Real programmer

Responsibility:

- generate concrete artifacts requested by `build_task_packet`

Examples:

- runtime package contents
- workflow definitions
- OPA policy source
- config files
- wrapper scripts
- prompt files

The real programmer does **not**:

- compute system diff
- compile admin steps directly
- decide execution order outside the artifact task

### 5.5 Artifact auditor

Responsibility:

- validate artifact bundle structure before it enters planning or execution

What it validates in v1:

- required files exist
- file layout is valid
- naming is valid
- package metadata is structurally coherent
- runtime base references are coherent **against `build_task_packet.known_context`**
- known file/schema presence checks

Operational rules:

- the artifact auditor is a pure validator over `build_task_packet + artifact_bundle`
- it must not perform `query_hive` or any other live-state read
- if `known_context` is insufficient to validate a structural claim, that is a packet-construction defect, not a reason for the auditor to fetch live state

What it does **not** validate in v1:

- prompt quality
- semantic correctness of rego logic
- semantic correctness of workflow business logic
- whether config values are optimal
- whether generated text is "good"

This boundary is explicit so the system does not overclaim semantic validation.

### 5.6 Plan compiler

Responsibility:

- transform deterministic `delta_report` plus approved artifacts into `executor_plan`

Allowed tools:

- `get_admin_action_help`
- handbook/knowledge pack
- read-only contract catalog
- optional current executor schema catalog

The plan compiler does not compute the diff. It only converts the already-determined delta into exact operational steps.

### 5.7 Executor (`SY.admin`)

Unchanged from the pilot:

- execute `executor_plan`
- call exact admin-mapped functions
- no renaming
- no extra mutating steps
- use help only when allowed and needed
- emit execution events

---

## 6. `actual_state_snapshot` strategy

This section closes the biggest missing piece from v2.

### 6.1 Snapshot purpose

`actual_state_snapshot` is the deterministic input to the reconciler. It is not assumed to be globally atomic. It is a best-effort multi-call snapshot with explicit scope, timestamps, and completeness markers.

### 6.2 Snapshot v1 scope

Snapshot v1 covers the exact solution scope supported by this architecture version:

- hive reachability / liveness
- runtimes
- nodes
- routes
- VPNs
- WF deployed workflows / workflow status
- OPA deployed policy state sufficient to decide apply / update / no-op

Snapshot v1 explicitly excludes:

- policy matrix / normative policy state
- identity state
- deep secret state
- advanced deployment history

Those remain out of scope for this architecture version and must be rejected at manifest validation if placed in `desired_state`.

### 6.3 Snapshot sources

Recommended admin reads for snapshot v1:

#### Global / coordination reads
- `/inventory`
- `/inventory/summary`

#### Per-hive reads
- `/hives/{hive}/runtimes`
- `/hives/{hive}/nodes`
- `/hives/{hive}/routes` or equivalent canonical route list action
- `/hives/{hive}/vpns`
- `wf_rules_list_workflows`
- `wf_rules_get_status` for each workflow in scope when needed to disambiguate drift
- canonical OPA read/status action(s) sufficient to decide whether desired OPA state is absent, matches, or must be reapplied
- optional `/hives/{hive}/versions` when runtime/version drift must be confirmed

The concrete implementation may use direct admin actions rather than HTTP-like paths, but the snapshot contract must expose which source produced which section.

### 6.4 Snapshot structure

```json
{
  "snapshot_version": "0.1",
  "snapshot_id": "snap-20260423-001",
  "captured_at_start": "2026-04-23T20:10:00Z",
  "captured_at_end": "2026-04-23T20:10:04Z",
  "scope": {
    "hives": ["motherbee", "worker-220"],
    "resources": ["runtimes", "nodes", "routes", "vpns"]
  },
  "atomicity": {
    "mode": "best_effort_multi_call",
    "is_atomic": false
  },
  "hive_status": {
    "motherbee": {"reachable": true},
    "worker-220": {"reachable": false, "error": "timeout"}
  },
  "resources": {
    "motherbee": {
      "runtimes": {...},
      "nodes": {...},
      "routes": {...},
      "vpns": {...}
    }
  },
  "completeness": {
    "is_partial": true,
    "missing_sections": ["worker-220.routes", "worker-220.nodes"],
    "blocking": true
  }
}
```

### 6.5 Hive-down behavior

Rules:

- if a target hive required by `desired_state` is unreachable, snapshot is marked partial
- the reconciler must not silently compute destructive deltas for unreachable hives
- if missing data affects owned resources on that hive, `delta_report` is marked `blocked`
- no delete/remove operations may be emitted from missing visibility

Practical v1 rule:

- unreachable hive for solution-owned resources => `delta_report.status = blocked_partial_snapshot`
- reachable motherbee but unreachable worker => create/update operations targeting that worker may still be emitted only if they do not require reading current owned state from that hive; delete/replace operations stay blocked

### 6.6 Non-atomicity rule

The snapshot is not globally atomic.

To make that explicit:

- snapshot stores start/end timestamps
- per-section fetch timestamps may be included
- reconciler treats the snapshot as one run artifact
- any decision that depends on stronger guarantees must mark `requires_fresh_confirm` or `blocked`

This is acceptable for v1 because the goal is deterministic and auditable behavior, not distributed atomicity.

---

## 7. `delta_report` and canonical `compiler_class`

This is the contract between the reconciler and the plan compiler.

### 7.1 Rule

The plan compiler must never invent the operational meaning of a delta. The reconciler emits a canonical `compiler_class`, and that class maps to a predefined admin-step sequence policy.

### 7.2 `delta_report` structure

```json
{
  "delta_report_version": "0.1",
  "status": "ready",
  "manifest_ref": "manifest://acme-support/2.1.0",
  "snapshot_ref": "snapshot://snap-20260423-001",
  "summary": {
    "creates": 4,
    "updates": 2,
    "deletes": 1,
    "noops": 12,
    "blocked": 0
  },
  "operations": [
    {
      "op_id": "op-01",
      "resource_type": "runtime",
      "resource_id": "ai.support.demo",
      "change_type": "create",
      "ownership": "solution",
      "compiler_class": "RUNTIME_PUBLISH_AND_DISTRIBUTE",
      "desired_ref": "desired_state.runtimes.packages[0]",
      "actual_ref": null,
      "blocking": false,
      "payload_ref": "artifact://bundle-17"
    }
  ]
}
```

### 7.3 Canonical `compiler_class` vocabulary for v1

No generic `update` semantics are allowed. The reconciler must emit one of these canonical classes.

#### Topology
- `HIVE_CREATE`
- `VPN_CREATE`
- `VPN_DELETE`

#### Runtimes
- `RUNTIME_PUBLISH_ONLY`
- `RUNTIME_PUBLISH_AND_DISTRIBUTE`
- `RUNTIME_DELETE_VERSION`

#### Nodes
- `NODE_RUN_MISSING`
- `NODE_CONFIG_APPLY_HOT`
- `NODE_CONFIG_APPLY_RESTART`
- `NODE_RESTART_ONLY`
- `NODE_KILL`
- `NODE_RECREATE` (destructive, explicit-only via node reconcile policy)

#### Routing
- `ROUTE_ADD`
- `ROUTE_DELETE`
- `ROUTE_REPLACE`

#### WF
- `WF_DEPLOY_APPLY`
- `WF_DEPLOY_RESTART`
- `WF_REMOVE`

#### OPA
- `OPA_APPLY`
- `OPA_REMOVE`

If a desired change cannot be expressed with a canonical class, the reconciler must emit `blocked` rather than a vague class.

### 7.4 Canonical translation table: `compiler_class` -> admin-step sequence

This table must be treated as a frozen contract.

| compiler_class | Meaning | Canonical executor step sequence |
|---|---|---|
| `HIVE_CREATE` | missing hive | `add_hive` |
| `VPN_CREATE` | missing VPN membership/config | `add_vpn` |
| `VPN_DELETE` | remove VPN membership/config | `delete_vpn` |
| `RUNTIME_PUBLISH_ONLY` | package/version missing but no worker convergence needed | `publish_runtime_package` |
| `RUNTIME_PUBLISH_AND_DISTRIBUTE` | package/version missing and workers must converge | `publish_runtime_package` with explicit `sync_to` + `update_to` |
| `RUNTIME_DELETE_VERSION` | remove published runtime version | `remove_runtime_version` |
| `NODE_RUN_MISSING` | node instance absent | `run_node` |
| `NODE_CONFIG_APPLY_HOT` | persisted/live config update without restart | `node_control_config_set` |
| `NODE_CONFIG_APPLY_RESTART` | config update requiring restart | `node_control_config_set` -> `restart_node` |
| `NODE_RESTART_ONLY` | runtime/config already present but process restart required | `restart_node` |
| `NODE_KILL` | delete unmanaged/removed node | `kill_node` |
| `NODE_RECREATE` | node must be recreated from scratch | `kill_node` -> `run_node` |
| `ROUTE_ADD` | missing route | `add_route` |
| `ROUTE_DELETE` | obsolete route | `delete_route` |
| `ROUTE_REPLACE` | route change cannot be patched in place | `delete_route` -> `add_route` |
| `WF_DEPLOY_APPLY` | workflow missing or changed and no restart required beyond deploy semantics | `wf_rules_compile_apply` |
| `WF_DEPLOY_RESTART` | workflow change requires deploy plus explicit node restart semantics | `wf_rules_compile_apply` -> `restart_node` |
| `WF_REMOVE` | workflow solution-owned deployment removed | canonical workflow remove/delete action when available, otherwise blocked until such action is defined |
| `OPA_APPLY` | OPA state missing or changed | `opa_compile_apply` |
| `OPA_REMOVE` | solution-owned OPA state removed | canonical OPA remove action when available, otherwise blocked until such action is defined |

### 7.5 Deterministic semantics behind class selection

Examples:

- runtime version drift in desired state is not emitted as generic `update`; it becomes either `RUNTIME_PUBLISH_ONLY` or `RUNTIME_PUBLISH_AND_DISTRIBUTE`
- node config change is not emitted as generic `update`; it becomes either `NODE_CONFIG_APPLY_HOT` or `NODE_CONFIG_APPLY_RESTART`
- route change is not emitted as generic `update`; it becomes `ROUTE_REPLACE`

### 7.6 How restart semantics are determined

The reconciler must not guess restart requirements. It must use one of:

- explicit node/runtime metadata in desired state
- a runtime policy table maintained in code
- a canonical per-node-type rule set (for example `WF.*` => restart required, based on known behavior from current Fluxbee guidance) fileciteturn10file2

If restart semantics are unknown, emit `blocked`, not a generic node update.

### 7.7 Explicit semantics for `NODE_RECREATE`

`NODE_RECREATE` may never be inferred from drift alone.

The reconciler may emit `NODE_RECREATE` only when both conditions are true:

1. the change is classified as immutable according to the frozen restart/immutability table
2. the desired node explicitly opts into recreation through:

```json
{
  "reconcile_policy": {
    "on_immutable_change": "recreate"
  }
}
```

Default behavior is:

```json
{
  "reconcile_policy": {
    "on_immutable_change": "block"
  }
}
```

If the manifest does not opt in, immutable drift must produce a blocked reconciler operation such as `BLOCKED_IMMUTABLE_CHANGE`, not `NODE_RECREATE`.

---

## 8. Design loop

### 8.1 Inputs

- human need
- optional current manifest
- live context from `query_hive`
- design cookbook seeds/examples

### 8.2 Outputs

- `solution_manifest`
- `design_audit_verdict`

### 8.3 Stop conditions

The design loop must not be unbounded.

Canonical stop rules:

- maximum 3 design iterations per pipeline run
- if audit score does not improve by at least 1 point after a retry, stop
- if the same blocking issue signature appears twice, stop
- if the designer requests unavailable information twice, stop and escalate to host

On stop, the host presents:

- current design summary
- blocking issues
- recommended operator choices

---

## 9. Artifact loop

### 9.1 Inputs

- `build_task_packet`
- artifact cookbook seeds/examples
- approved design context

### 9.2 Outputs

- `artifact_bundle`
- `artifact_audit_verdict`
- optional `repair_packet`

### 9.3 Stop conditions

Canonical stop rules:

- maximum 3 artifact attempts per task packet
- if the same failure signature appears twice, stop
- if failure class is deterministic non-repairable (`ARTIFACT_TASK_UNDERSPECIFIED`, `ARTIFACT_UNSUPPORTED_KIND`), stop immediately

### 9.4 Artifact auditor scope in v1

Structural validation only.

Examples of valid checks:

- package layout
- file names
- required manifests
- required bundle members
- runtime base coherence
- basic schema presence

Examples of out-of-scope semantic checks in v1:

- whether `system.txt` is a good prompt
- whether a rego policy is normatively correct
- whether a workflow definition expresses the intended business process well
- whether model/config tuning is optimal

---

## 10. Plan compiler

### 10.1 Inputs

- `delta_report`
- approved artifact bundles
- plan compile cookbook seeds/examples
- admin action help/catalog

### 10.2 Output

- `executor_plan`

### 10.3 Hard rules

- no diffing
- no inventing operational meaning for deltas
- must respect canonical `compiler_class` translation table
- may refine exact args using admin help only within the boundary already fixed by `compiler_class`

Example:

- if `compiler_class = NODE_CONFIG_APPLY_RESTART`, the plan compiler may use help to build correct `node_control_config_set` args and `restart_node` args
- it may not decide to use `kill_node -> run_node` instead

---

## 11. Failure classification

### 11.1 Layer order

1. deterministic rule matcher in code
2. residual AI classifier
3. host/operator escalation

### 11.2 Canonical top-level failure classes

- `DESIGN_INCOMPLETE`
- `DESIGN_CONFLICT`
- `SNAPSHOT_PARTIAL_BLOCKING`
- `DELTA_UNSUPPORTED`
- `ARTIFACT_TASK_UNDERSPECIFIED`
- `ARTIFACT_CONTRACT_INVALID`
- `ARTIFACT_LAYOUT_INVALID`
- `PLAN_INVALID`
- `PLAN_CONTRACT_INVALID`
- `EXECUTION_ENVIRONMENT_MISSING`
- `EXECUTION_ACTION_FAILED`
- `EXECUTION_TIMEOUT`
- `POLICY_BLOCKED`
- `UNKNOWN_RESIDUAL`

### 11.3 Deterministic examples

| Pattern | Class |
|---|---|
| unknown action | `PLAN_INVALID` |
| missing required field | `PLAN_CONTRACT_INVALID` or `ARTIFACT_CONTRACT_INVALID` depending on stage |
| runtime not materialized | `EXECUTION_ENVIRONMENT_MISSING` |
| unreachable target hive during snapshot | `SNAPSHOT_PARTIAL_BLOCKING` |
| package missing required files | `ARTIFACT_LAYOUT_INVALID` |

The deterministic matcher must run before any AI-based classification.

---

## 12. Persistence model

### 12.1 Storage policy

Use the same storage base already used by Archi sessions and operations. Do not add a new platform.

### 12.2 Required DB extension

Add `pipeline_runs` with structured JSON state.

Suggested columns:

- `pipeline_run_id`
- `session_id`
- `status`
- `current_stage`
- `current_loop`
- `state_json`
- `created_at_ms`
- `updated_at_ms`
- `interrupted_at_ms`

### 12.3 Artifact storage

Intermediate artifacts live in filesystem/blob-root, with references stored in DB.

Examples:

- manifests
- snapshots
- delta reports
- artifact bundles
- repair packets
- executor plans
- execution reports
- cookbook JSON files

### 12.4 Recovery rule

If a `pipeline_run` is still `in_progress` on startup:

- mark it `interrupted`
- attach restart context
- present it to the operator when the session is resumed

---

## 13. Cookbook policy

### 13.1 Separate cookbooks

Required files:

- `design_cookbook_v1.json`
- `artifact_cookbook_v1.json`
- `plan_compile_cookbook_v1.json`
- `repair_cookbook_v1.json`

### 13.2 Write rule

Only write cookbook entries after verified success in that layer.

### 13.3 Seed rule

Seed manually **before** enabling the new actors.

Minimum seed set:

- 2-3 common design patterns
- 3-4 already-working plan compilation patterns from the current planner/programmer path fileciteturn10file0
- initial artifact patterns from known runtime/package/workflow cases where available fileciteturn10file1

Seed is mandatory. Do not wait for the new pipeline to learn from zero.

---

## 14. Human-in-the-loop policy

### 14.1 CONFIRM 1 payload

Host shows:

- concise manifest summary
- design audit verdict
- major topology/runtime/node choices
- unresolved advisory notes if any

Operator approves direction.

### 14.2 Internal loops remain silent unless blocked

The operator should not be interrupted for:

- artifact loop retries
- plan pre-validation retries
- deterministic classification

These appear in the trace panel only.

### 14.3 CONFIRM 2 payload

Host shows:

- concise executor summary
- destructive actions highlighted
- high-level plan outcome expectation
- validation result

Operator approves operation.

---

## 15. Mandatory implementation freeze before coding

Before implementation starts, the following contracts must be frozen together:

1. `solution_manifest` split into `desired_state` and `advisory`
2. `actual_state_snapshot` v1 scope and schema
3. `delta_report` schema
4. full `compiler_class` vocabulary for v1
5. canonical translation table from `compiler_class` to admin-step sequence
6. design loop stop conditions
7. artifact loop stop conditions
8. deterministic failure classification vocabulary and first matcher rules
9. `get_manifest_current` tool contract
10. cookbook file names and seed set
11. `pipeline_runs` persistence schema
12. canonical restart-required / hot-apply / immutable-change table by node type and runtime class

No implementation should begin until those contracts are frozen.

---

## 16. Implementation tracks after contract freeze

These are not phases in the sense of architecture drift. They are parallel work tracks against frozen contracts.

### Track A — Manifest and host contracts

- add `desired_state` / `advisory`
- add `get_manifest_current`
- extend host persistence with `pipeline_runs`
- host rendering for traces and interrupts

### Track B — Deterministic reconciler

- implement snapshot builder v1
- implement partial snapshot policy
- implement `delta_report`
- implement `compiler_class` emission

### Track C — Artifact loop

- implement `build_task_packet`
- implement `real_programmer`
- implement `artifact_auditor`
- implement `repair_packet`
- implement artifact loop trace handling

### Track D — Plan compiler

- rename current `fluxbee_programmer` role to `plan_compiler`
- bind it to `delta_report`
- enforce canonical translation table
- seed `plan_compile_cookbook`

### Track E — Failure classification

- deterministic matcher in code
- residual AI classifier
- escalation paths

### Track F — Cookbook seed and migration

- seed all cookbook files manually
- migrate old programmer entries to `plan_compile_cookbook`
- document any discarded entries

### Track G — Final pipeline assembly

- integrate both CONFIRM points
- recovery on interrupted runs
- end-to-end verification on full pipeline

These tracks may land incrementally in code, but only against the frozen contracts above.

---

## 17. Renames and removals

### 17.1 Rename

- current `fluxbee_programmer` role -> `plan_compiler`
- programmer cookbook -> `plan_compile_cookbook`
- programmer trace -> `plan_compile_trace`

This is necessary because the current component primarily generates/pre-validates `executor_plan`, not software artifacts fileciteturn10file0.

### 17.2 Add

- `designer`
- `design_auditor`
- `real_programmer`
- `artifact_auditor`
- deterministic reconciler
- deterministic failure classifier
- residual AI classifier

### 17.3 Remove from conceptual model

Remove the idea that one "programmer" can serve as:

- designer
- software generator
- plan compiler
- repair engine

That ambiguity is no longer allowed in the architecture.

Also deprecate `fluxbee_infrastructure_specialist` directly.

Decision:
- it is not a canonical actor in the target architecture
- its responsibilities are superseded by `real_programmer` + `artifact_auditor`
- any remaining implementation can exist only as migration compatibility code and must not be referenced in the new contracts

---

## 18. Final bottom line

The final architecture is:

- `solution_manifest` = declarative design source
- deterministic reconciler = `desired_state + actual_state_snapshot -> delta_report`
- artifact loop = bounded generation and structural validation
- `plan_compiler` = `delta_report -> executor_plan`
- executor = exact admin operator

The single most important contract in the middle is now explicit:

> **`compiler_class` is the frozen operational meaning of a delta.**

That contract prevents ambiguity between the reconciler and the plan compiler.

The single most important operational decision is also explicit:

> **no diffing in AI, no hidden plan semantics, no staged architecture drift.**

This is the architecture that preserves the only part already proved by the pilot while making the upper layers deterministic, auditable, and scalable.
