# Fluxbee — Master Rearchitecture Spec v2

**Status:** canonical rearchitecture proposal  
**Date:** 2026-04-23  
**Audience:** `SY.architect`, `SY.admin`, `SY.orchestrator`, runtime/package developers, AI-agent designers, operator UX  
**Purpose:** redefine the architecture above the working executor layer so Fluxbee can scale from human-described need to validated solution, generated artifacts, deterministic reconciliation, executable plan, and exact control-plane execution.

---

## 1. Executive summary

Fluxbee already proved one critical boundary:

- `executor_plan` as a sequential operational artifact
- `SY.admin` as the executor home
- one OpenAI function per real admin action
- strict action schemas
- visible execution trace in chat

That boundary stays.

What must change is everything **above** it.

The current architecture still mixes at least four different concerns under the old `programmer` idea:

1. solution design
2. artifact generation
3. plan compilation
4. error repair

That ambiguity is the main scaling risk.

This spec fixes it by making the pipeline explicit and layered:

```text
human need
  -> host
  -> designer
  -> solution_manifest
  -> design_auditor
  -> CONFIRM 1
  -> deterministic reconciler/compiler (Rust)
       -> desired_state + actual_state -> delta_report
       -> build_task_packets when artifacts are missing
  -> artifact loop
       -> real_programmer
       -> artifact_bundle
       -> artifact_auditor
       -> repair_packet
       -> repeat or approve
  -> plan_compiler AI
       -> delta_report + approved artifacts -> executor_plan
  -> plan validation / failure classifier / residual audit
  -> CONFIRM 2
  -> executor in SY.admin
  -> execution_report
  -> final verification
  -> cookbook writes (verified successes only)
```

The decisive architectural rule is:

> **Deterministic code computes the delta. AI generates, repairs, and compiles within bounded artifacts.**

That rule sharply lowers failure risk, improves testability, and keeps the working executor boundary intact.

---

## 2. Why this rearchitecture exists

Fluxbee aims to support an end-to-end path from a human need to a running system, potentially including:

- tenant solution design
- topology and hive decisions
- runtime/package generation
- workflow and policy generation
- runtime publication
- node lifecycle orchestration
- routing and configuration
- validation and learning

The old pilot work was valuable because it exposed the real boundaries.

### 2.1 What the pilot already proved

The pilot execution layer proved that the last mile should be exact, not improvised:

- function calling must map 1:1 to real admin actions
- action names should stay identical to admin names
- schemas should be strict
- help is a support mechanism, not the primary contract
- host and executor must stay separate

This stays as the operational core.

### 2.2 What the pilot did not solve

The pilot did not solve the upper architecture:

- how a solution is designed
- how design becomes artifacts
- how artifacts are audited
- how desired state is compared to actual state
- how a delta becomes an `executor_plan`
- how retries and failures are classified
- where human confirmation belongs
- how reusable knowledge should be stored by layer

This spec solves those gaps.

---

## 3. Core architecture decisions

These are mandatory decisions, not optional recommendations.

### 3.1 Preserve the working executor boundary

Keep as canonical:

- `executor_plan`
- executor in `SY.admin`
- 1:1 admin action tools
- strict schemas
- stop-on-error
- chat-visible execution events

### 3.2 `solution_manifest` stays declarative

The `solution_manifest` remains the canonical design artifact.

It is not:

- a script
- a macro language
- a sequence of admin calls
- an execution plan

It is the desired state for one tenant solution.

### 3.3 The manifest compiler is deterministic code, not an AI specialist

This is the most important change in v2.

The manifest compiler must be Rust code that computes:

`desired_state + actual_state -> delta_report`

It is responsible for:

- diffing
- ownership-aware reconciliation
- idempotent classification of create/update/delete/no-op
- emitting deterministic resource deltas
- identifying missing artifacts required to satisfy desired state

It is **not** responsible for:

- creative interpretation
- architecture invention
- plan wording
- runtime content generation
- error explanation in natural language

### 3.4 AI is used only where generation or bounded repair is actually needed

AI remains appropriate for:

- solution design
- artifact generation
- artifact repair
- converting deterministic deltas into an `executor_plan`
- residual failure classification when deterministic rules do not match

### 3.5 The real programmer and the artifact auditor are inseparable

Fluxbee must not introduce a software/artifact generator without its validator.

The canonical phase is the **artifact loop**:

`build_task_packet -> real_programmer -> artifact_bundle -> artifact_auditor -> repair_packet -> loop`

No generated artifact may advance to planning or execution without passing the artifact auditor.

### 3.6 Human confirmation is reduced to two checkpoints

There are exactly two human confirmation points in the default pipeline.

#### CONFIRM 1
After design audit, before any artifact construction or delta-driven implementation begins.

The operator is approving the **direction**.

#### CONFIRM 2
After `executor_plan` compilation and validation, before execution.

The operator is approving the **operation**.

Everything else runs internally and is only shown through trace visibility unless the pipeline blocks.

### 3.7 Failure classification is deterministic first, AI second

Most failures should be classified by hardcoded rules.

Only unmatched residual cases go to AI.

That gives:

- predictability
- testability
- lower retry noise
- cleaner ownership of repairs

### 3.8 Learning is stratified, not global

Fluxbee will not use one generic cookbook.

It will maintain separate cookbooks for:

- design
- artifact generation
- plan compilation
- repair

### 3.9 Persistence stays on existing infrastructure

No new storage platform is introduced.

Persist using:

- the same SQLite/LanceDB-backed host persistence already used by Archi sessions/operations
- filesystem/blob-root for intermediate artifacts and cookbook files
- DB references to artifact files

---

## 4. Canonical pipeline

### 4.1 End-to-end flow

```text
human need
  -> SY.architect host
  -> designer
  -> solution_manifest
  -> design_auditor
  -> host summary to operator
  -> CONFIRM 1
  -> deterministic reconciler/compiler
       -> actual_state snapshot
       -> delta_report
       -> build_task_packets if required
  -> artifact loop (0..N times)
       -> real_programmer
       -> artifact_bundle
       -> artifact_auditor
       -> repair_packet if needed
  -> delta refresh if artifacts changed readiness
  -> plan_compiler AI
       -> executor_plan
  -> plan validator
  -> deterministic failure classifier
  -> residual audit if needed
  -> host summary to operator
  -> CONFIRM 2
  -> executor in SY.admin
  -> execution_report
  -> final verifier / verification_auditor
  -> cookbook writes for verified successes
```

### 4.2 Stage boundaries

Each stage emits an artifact or report.

No stage should pass only raw prompt text to the next stage when a typed artifact can exist instead.

---

## 5. Canonical roles and renamed responsibilities

### 5.1 Host (`SY.architect`)

#### Responsibilities

- converse with operator
- maintain session state and trace visibility
- orchestrate stage progression
- persist artifacts and pipeline run state
- enforce confirmation points
- route failure handling to the correct stage
- render summaries and traces

#### Non-responsibilities

- not the executor
- not the deterministic compiler
- not the artifact generator
- not the final classifier for all errors

### 5.2 Designer

#### Purpose
Produce or revise the `solution_manifest`.

#### Responsibilities

- define topology, hives, runtimes, nodes, routing, policy, identity
- extend existing solutions through manifest revisions
- reason in desired state terms
- reference known patterns and constraints

#### Allowed tools

- `query_hive` read-only inventory/state discovery
- `get_manifest_current` to load the current solution manifest when extending an existing solution
- handbook / knowledge pack
- design cookbook

#### Forbidden tools

- `get_admin_action_help`
- admin mutating tools
- execution tools

#### Non-responsibilities

- no `executor_plan`
- no package/source generation
- no operational payload construction

### 5.3 Design Auditor

#### Purpose
Validate the `solution_manifest` as design.

#### Responsibilities

- detect missing topology pieces
- detect ownership ambiguity
- detect policy/routing/runtime mismatches
- challenge weak assumptions
- score or classify design completeness
- produce actionable feedback for the designer

#### Inputs

- `solution_manifest.desired_state`
- `solution_manifest.advisory`
- optional current manifest version
- optional current environment snapshot summary

#### Outputs

- `design_audit_verdict`
- optional `design_repair_notes`

### 5.4 Deterministic Reconciler / Manifest Compiler

#### Purpose
Transform desired state and actual state into a deterministic `delta_report`.

#### Nature
Rust code. Not an AI actor.

#### Responsibilities

- load current actual state snapshot
- compare against `desired_state`
- classify each resource into create/update/delete/no-op/conflict
- enforce ownership semantics
- identify missing artifacts required for convergence
- emit deterministic resource operations and gaps
- emit exact delta reasons

#### Outputs

- `actual_state_snapshot`
- `delta_report`
- `build_task_packets[]` when artifacts are required

#### Non-responsibilities

- no creative interpretation
- no software generation
- no free-form plan writing
- no direct admin mutations

### 5.5 Real Programmer

#### Purpose
Generate technical artifacts needed by the delta.

#### Responsibilities

- create runtime packages
- create workflow definitions
- create rego sources
- generate prompts/assets/config bundles
- patch or regenerate artifacts after repair instructions

#### Inputs

- `build_task_packet`
- relevant cookbook entries
- handbook / packaging conventions
- prior failed artifact and `repair_packet` when retrying

#### Outputs

- `artifact_bundle`

#### Non-responsibilities

- no design decisions beyond task scope
- no runtime deployment
- no admin action planning

### 5.6 Artifact Auditor

#### Purpose
Validate generated artifacts before they can influence planning or execution.

#### Responsibilities

- validate structural correctness
- validate package/runtime type conventions
- validate required files and references
- validate compiler-facing contracts
- classify artifact failures
- emit `repair_packet` for reattempts

#### Outputs

- `artifact_audit_verdict`
- optional `repair_packet`

### 5.7 Plan Compiler

#### Purpose
Convert deterministic deltas and approved artifacts into an `executor_plan`.

This is the component currently closest to the old `fluxbee_programmer` behavior.

#### Responsibilities

- map deterministic operations to exact admin action sequences
- preserve lifecycle order
- use real admin action names and fields
- avoid undeclared steps
- use cookbook support for recurrent plan shapes
- produce operator-facing summary

#### Allowed tools

- `get_admin_action_help`
- `query_hive` read-only
- plan-compile cookbook
- handbook
- resolved artifact references
- deterministic `delta_report`

#### Forbidden tools

- no design revision
- no package/source generation
- no undeclared mutating calls

### 5.8 Plan Validator

#### Purpose
Validate that the compiled plan is structurally and contractually acceptable before operator confirmation.

#### Nature
Mostly deterministic code.

#### Responsibilities

- validate `executor_plan` schema
- validate action names against executor-visible catalog
- validate arg shape against action schema
- validate lifecycle ordering constraints
- validate references to approved artifacts

### 5.9 Failure Classifier

#### Purpose
Classify failures precisely so the correct repair loop is triggered.

#### Nature
Two-tier system:

1. deterministic classifier in code
2. residual AI classifier for unmatched cases

### 5.10 Executor (`SY.admin`)

Unchanged in principle.

#### Responsibilities

- execute steps in order
- use exact admin functions
- use help when allowed
- stop on failure or ambiguity
- emit structured events and final report

### 5.11 Verification Auditor

#### Purpose
Decide whether the pipeline outcome is truly successful and whether knowledge should be retained.

#### Responsibilities

- inspect `execution_report`
- inspect post-run verification checks
- decide whether result is verified success, partial success, or failed success
- allow cookbook writes only for verified reusable wins

---

## 6. Canonical artifacts

All stage boundaries should be explicit typed artifacts.

### 6.1 `solution_manifest`

Canonical design artifact.

#### Top-level shape

```json
{
  "manifest_version": "1.0",
  "solution": { ... },
  "desired_state": { ... },
  "advisory": { ... }
}
```

#### Rule

- everything the compiler needs for reconciliation lives in `desired_state`
- design metadata, sizing notes, commentary, optional constraints, and reasoning notes live in `advisory`

#### Compiler rule

The deterministic compiler reads **only** `desired_state`.

#### Auditor rule

The design auditor reads both `desired_state` and `advisory`.

### 6.2 `actual_state_snapshot`

Deterministic snapshot of the current environment.

Contains only the resource surfaces needed for reconciliation.

Example sections:

- tenants
- hives
- vpns
- runtimes
- nodes
- routes
- policy state
- identity state

### 6.3 `delta_report`

The critical new artifact in v2.

#### Purpose
Represent the deterministic difference between desired state and actual state.

#### Example shape

```json
{
  "delta_report_version": "0.1",
  "manifest_ref": "manifest://acme-support/2.1.0",
  "actual_state_ref": "snapshot://run-482/current",
  "summary": {
    "creates": 4,
    "updates": 3,
    "deletes": 1,
    "noops": 12,
    "artifact_tasks": 2
  },
  "operations": [
    {
      "op_id": "op-01",
      "resource_type": "runtime",
      "resource_key": "ai.support.demo",
      "change_type": "create",
      "ownership": "solution",
      "desired_ref": "desired_state.runtimes.packages[2]",
      "actual_ref": null,
      "compiler_class": "publish_runtime_required",
      "artifact_requirement": "artifact://task/runtime/ai.support.demo",
      "blocking": true,
      "notes": []
    }
  ]
}
```

#### Allowed change classes

At minimum:

- `create`
- `update`
- `delete`
- `noop`
- `conflict`
- `blocked`

### 6.4 `build_task_packet`

Artifact-generation request emitted by the deterministic compiler.

#### Purpose
Tell the real programmer exactly what technical artifact is missing.

#### Example

```json
{
  "task_packet_version": "0.1",
  "task_id": "art-01",
  "artifact_kind": "runtime_package",
  "source_delta_op": "op-01",
  "runtime_name": "ai.support.demo",
  "target_kind": "inline_package",
  "requirements": {
    "package_type": "config_only",
    "runtime_base": "AI.common",
    "required_files": [
      "package.json",
      "config/default-config.json",
      "assets/prompts/system.txt"
    ]
  },
  "constraints": {
    "must_be_publishable": true,
    "must_match_naming_policy": true
  },
  "context_refs": [
    "manifest://acme-support/2.1.0#desired_state.runtimes.packages[2]"
  ]
}
```

### 6.5 `artifact_bundle`

Output of the real programmer.

Contains:

- artifact metadata
- file map or blob refs
- declared type/runtime_base/version
- provenance and generator metadata

### 6.6 `artifact_audit_verdict`

Artifact validation result.

Possible statuses:

- `approved`
- `repairable`
- `rejected`
- `blocked`

### 6.7 `repair_packet`

Explicit repair instruction for retry loops.

Contains:

- failing stage
- failure class
- deterministic evidence
- exact correction goals
- retry policy metadata

### 6.8 `executor_plan`

Unchanged in core meaning.

Operational artifact executed by `SY.admin`.

### 6.9 `execution_report`

Executor output containing:

- step events
- final status
- completed steps
- failed step
- error data
- optional verification pointers

### 6.10 `cookbook_entry_v2`

Layer-specific reusable knowledge entry.

The exact shape varies by cookbook type, but all entries should contain:

- `kind`
- `pattern_key`
- `trigger`
- `inputs_signature`
- `successful_shape`
- `constraints`
- `recorded_from_run`
- `verification_status`

---

## 7. `solution_manifest` v2 guidance

### 7.1 Principle

The manifest must stop mixing reconciliable state with narrative design notes.

### 7.2 Required split

#### `desired_state`
Compiler-facing state only.

This includes:

- tenant
- topology
- runtimes
- nodes
- routing
- policy
- identity
- ownership semantics
- explicit configuration mode where needed

#### `advisory`
Design-facing metadata only.

This includes:

- sizing notes
- observations
- rationale
- recommendations
- non-binding constraints
- operator notes

### 7.3 Ownership semantics

Every resource class that can be reconciled must have explicit ownership semantics when ambiguity is possible:

- `system`
- `solution`
- `external`

This is required to prevent the deterministic compiler from deleting or rewriting resources it does not own.

### 7.4 Compiler-only rule

If a field is needed to reconcile, it belongs in `desired_state`.

If a field is only explanatory or optional to design review, it belongs in `advisory`.

---

## 8. Deterministic reconciler/compiler design

### 8.1 Why deterministic

Reconciliation has trivial logic compared to generative tasks:

- presence/absence
- version drift
- owned vs external
- equality/inequality
- required create/update/delete ordering

This is exactly the sort of work best done in code.

Using AI here would create unnecessary failure modes:

- incorrect diffs
- silent deletes
- drift hallucination
- unstable retries
- hard-to-test edge cases

### 8.2 Inputs

- current `solution_manifest`
- current `actual_state_snapshot`
- compiler config
- optional approved artifact registry

### 8.3 Outputs

- `delta_report`
- `build_task_packets[]`
- compiler diagnostics

### 8.4 Required compiler logic

At minimum the compiler must implement deterministic comparison for:

- tenant existence and metadata
- hive topology
- VPN topology
- published runtimes and versions
- node existence and key config hashes
- route existence and key fields
- policy presence/version
- identity vocabulary and initial entries

### 8.5 Artifact gap detection

The compiler must identify when desired state refers to artifacts that do not yet exist or are not yet approved for publication/execution.

Examples:

- runtime package declared but no approved package artifact exists
- workflow runtime declared but no workflow definition artifact exists
- config-only runtime references missing generated prompt/config assets

### 8.6 Compiler acceptance requirements

The compiler must be unit-testable without AI involvement.

That means:

- manifest fixtures in
- actual state fixtures in
- deterministic `delta_report` out

---

## 9. Artifact loop

### 9.1 Purpose

The artifact loop exists to materialize technical assets required by the deterministic delta.

### 9.2 Canonical loop

```text
build_task_packet
  -> real_programmer
  -> artifact_bundle
  -> artifact_auditor
     -> approved     -> done
     -> repairable   -> repair_packet -> retry
     -> rejected     -> escalate to host/designer
     -> blocked      -> stop pipeline
```

### 9.3 Retry rules

Artifact retries are bounded.

Default policy:

- max attempts per task packet: 3
- if repeated same failure signature occurs twice, stop early
- if failure class changes across attempts, record both signatures

### 9.4 Repair packet principle

The repair loop must never say only “fix it.”

It must provide structured correction guidance.

Example:

```json
{
  "repair_packet_version": "0.1",
  "repair_id": "rep-01",
  "source_task_id": "art-01",
  "failure_class": "ARTIFACT_CONTRACT_INVALID",
  "evidence": {
    "missing_files": ["package.json"],
    "invalid_field": "runtime_base"
  },
  "required_corrections": [
    "add package.json",
    "set runtime_base to AI.common"
  ],
  "retry_allowed": true
}
```

### 9.5 Artifact auditor requirements

The artifact auditor must be able to validate at least:

- package layout
- naming conventions
- runtime type/runtime_base coherence
- required file presence
- manifest/compiler-facing references
- compatibility with publication path

### 9.6 Artifact outputs are not auto-executed

Approved artifacts are inputs to planning.

They do not directly mutate the system.

---

## 10. Plan compiler

### 10.1 Role clarification

The old `fluxbee_programmer` behavior maps here.

It should be renamed conceptually and eventually in code to something like:

- `fluxbee_plan_compiler`
- or `manifest_to_plan_compiler`

Because it is not a real software programmer.

### 10.2 Inputs

- deterministic `delta_report`
- approved artifacts
- admin action help
- read-only hive queries
- plan cookbook
- handbook

### 10.3 Output

- `executor_plan`
- `human_summary`
- optional `plan_compile_trace`
- optional cookbook candidate

### 10.4 Hard constraints

- no action invention
- no field alias invention
- exact admin action names
- exact admin arg names
- preserve lifecycle order
- no hidden mutating pre-steps

### 10.5 Lifecycle ordering rule

The plan compiler must obey canonical Fluxbee lifecycle ordering, including:

1. publish/materialize runtime or package when required
2. optional `sync_hint` / `update`
3. `run_node` / `restart_node`
4. config operations
5. routing operations

### 10.6 Plan compiler tools

Allowed:

- `get_admin_action_help`
- `query_hive`
- handbook
- plan compile cookbook

Not allowed:

- artifact generation
- design editing
- undeclared mutation

---

## 11. Human-in-the-loop contract

### 11.1 CONFIRM 1 — design approval

Occurs after:

- designer output
- design audit
- host summary

The operator is approving the direction of the solution.

What the operator sees:

- concise solution summary
- topology summary
- main runtimes/nodes/routes/policy direction
- main auditor warnings

What does **not** happen yet:

- artifact generation
- plan execution

### 11.2 Internal loop between CONFIRM 1 and CONFIRM 2

The following stages run without interrupting the operator unless blocked:

- deterministic compilation/reconciliation
- artifact loop
- plan compilation
- plan pre-validation
- deterministic failure classification
- residual audit if needed

The operator sees progress only through a trace panel or host updates.

### 11.3 CONFIRM 2 — execution approval

Occurs after:

- `executor_plan` exists
- plan validation passed
- execution summary is ready

The operator is approving the exact operational change.

What the operator sees:

- plan summary
- creates/updates/deletes summary
- destructive actions called out explicitly
- key risks or blockers

### 11.4 Blocked pipeline behavior

If any internal loop exceeds retry limits or reaches a blocked state, the host pauses and presents:

- what stage failed
- failure class
- what was attempted
- what options exist next

---

## 12. Failure classification system

### 12.1 Two-layer classifier

#### Layer 1 — deterministic rules in code

This classifier must handle the common cases.

#### Layer 2 — residual AI classifier

Used only when no deterministic rule matches.

### 12.2 Canonical failure domains

At minimum:

- `DESIGN_INVALID`
- `DESIGN_INCOMPLETE`
- `ARTIFACT_CONTRACT_INVALID`
- `ARTIFACT_BUILD_FAILED`
- `PLAN_INVALID`
- `PLAN_CONTRACT_MISMATCH`
- `EXECUTION_ENVIRONMENT_MISSING`
- `EXECUTION_ADMIN_FAILURE`
- `EXECUTION_TIMEOUT`
- `TRANSIENT_INFRA_FAILURE`
- `UNCLASSIFIED`

### 12.3 Deterministic examples

Examples that should be hardcoded first:

- `runtime not materialized` -> `EXECUTION_ENVIRONMENT_MISSING`
- `unknown action` -> `PLAN_INVALID`
- `missing required field X` during plan validation -> `PLAN_INVALID`
- `missing required field X` during artifact audit -> `ARTIFACT_CONTRACT_INVALID`
- package layout invalid -> `ARTIFACT_CONTRACT_INVALID`
- node naming policy failure -> `ARTIFACT_CONTRACT_INVALID` or `DESIGN_INVALID` depending on stage

### 12.4 Why deterministic first

This ensures:

- reproducible retries
- easy unit tests
- less noisy loops
- fewer hallucinated remediations

### 12.5 Residual AI classifier inputs

Only when no deterministic rule matches:

- stage context
- error payload
- recent trace
- artifact refs / plan refs / delta refs

---

## 13. Cookbook architecture

### 13.1 No global cookbook

Cookbooks are split by layer.

### 13.2 Canonical cookbooks

#### `design_cookbook`
Reusable solution/topology patterns.

Examples:

- support system with dedicated worker
- basic AI + WF + IO support topology
- single-tenant worker isolation pattern

#### `artifact_cookbook`
Reusable package/workflow/prompt/config generation patterns.

Examples:

- `config_only` AI runtime package layout
- workflow package layout
- common prompt asset structure

#### `plan_compile_cookbook`
Reusable delta-to-plan patterns.

Examples:

- publish -> run_node -> add_route
- publish workflow -> restart existing WF node
- package update -> sync/update -> restart

#### `repair_cookbook`
Verified repair patterns by failure signature.

Examples:

- missing `package.json`
- invalid `runtime_base`
- missing workflow definition field

### 13.3 Seed policy

Cookbooks must be manually seeded now.

Do not wait for the new pipeline to learn from zero.

Minimum seed plan:

- plan compile cookbook: seed from 3-4 successful current `fluxbee_programmer` cases
- design cookbook: seed from 2-3 recurring topology patterns
- artifact cookbook: seed from known working runtime/package layouts
- repair cookbook: seed from known repeated failure/correction pairs where available

### 13.4 Write policy

Cookbook entries are written only when:

- the run reached verified success
- the verification auditor marks the pattern reusable

---

## 14. Tooling by actor

### 14.1 Designer

Allowed:

- `query_hive`
- `get_manifest_current`
- handbook / design pack
- design cookbook

Forbidden:

- `get_admin_action_help`
- admin mutation surface
- artifact tools

### 14.2 Real Programmer

Allowed:

- artifact task packet
- artifact cookbook
- runtime packaging handbook/specs
- relevant file templates
- optional repository/file context supplied explicitly by host

Forbidden:

- design mutation
- admin mutation
- executor tools

### 14.3 Artifact Auditor

Allowed:

- artifact bundle
- artifact validation rules/specs
- packaging conventions
- prior repair packet when re-auditing

### 14.4 Plan Compiler

Allowed:

- `get_admin_action_help`
- `query_hive`
- `delta_report`
- approved artifacts
- plan compile cookbook
- handbook

### 14.5 Executor

Allowed:

- exact admin-mapped functions
- `get_admin_action_help`
- auxiliary safe read/help calls under current executor rules

---

## 15. Persistence and recovery

### 15.1 Storage principle

Use the current host persistence base.

Do not introduce a new storage service.

### 15.2 `pipeline_runs` table

Add a pipeline-level run table to the existing persistence system.

Suggested fields:

- `run_id`
- `session_id`
- `solution_id`
- `status`
- `current_stage`
- `current_attempt`
- `state_json`
- `created_at_ms`
- `updated_at_ms`
- `interrupted_at_ms`

### 15.3 Artifact storage

Intermediate artifacts live in filesystem/blob-root.

Examples:

- manifests
- actual state snapshots
- delta reports
- task packets
- artifact bundles
- repair packets
- plans
- execution reports
- cookbook files

The DB stores references to these files.

### 15.4 Recovery behavior

On startup, any pipeline run still marked `in_progress` becomes `interrupted`.

When the related session is reopened, the host presents:

- interrupted stage
- last known status
- available restart/resume/discard options

### 15.5 Trace persistence

Each stage should emit structured trace entries linked to the pipeline run.

---

## 16. Module and naming changes

### 16.1 Preserve

Keep as-is conceptually:

- executor in `SY.admin`
- `executor_plan`
- host rendering of execution traces
- publish/materialize/spawn responsibility split between `SY.admin` and `SY.orchestrator`

### 16.2 Rename

The current `fluxbee_programmer` should be renamed conceptually and eventually in code.

Target:

- `fluxbee_plan_compiler`

Related renames:

- `programmer_trace` -> `plan_compile_trace`
- `programmer_pending` -> `plan_compile_pending`
- `programmer cookbook` -> `plan_compile_cookbook`

### 16.3 New modules/components

Create:

- `designer`
- `design_auditor`
- deterministic `manifest_compiler` / `reconciler`
- `real_programmer`
- `artifact_auditor`
- `failure_classifier`
- `verification_auditor`

### 16.4 Keep boundaries explicit in code

Do not let one module drift into another through convenience helpers.

Examples:

- the reconciler should not call OpenAI
- the plan compiler should not generate packages
- the real programmer should not produce `executor_plan`

---

## 17. Migration plan

### Phase 0 — freeze the working executor boundary

Goal:

- stop changing the executor contract casually
- treat `executor_plan -> executor` as stable core

Tasks:

- freeze executor plan schema changes unless required
- freeze 1:1 admin action mapping as canonical
- keep chat execution rendering stable

### Phase 1 — rename and isolate the current programmer

Goal:

- stop conceptual confusion immediately

Tasks:

- relabel current `fluxbee_programmer` as plan compiler in docs and UI traces
- split its cookbook into `plan_compile_cookbook`
- stop presenting it as the software programmer

Acceptance:

- no architecture doc uses `programmer` ambiguously anymore

### Phase 2 — formalize `solution_manifest` v2 shape

Goal:

- separate `desired_state` from `advisory`

Tasks:

- update manifest spec
- update design prompts and validation rules
- add ownership semantics where required

Acceptance:

- compiler can read only `desired_state`
- design auditor can read both sections

### Phase 3 — implement deterministic reconciler/compiler

Goal:

- produce `delta_report` in Rust

Tasks:

- define `actual_state_snapshot`
- define `delta_report`
- implement deterministic comparison logic
- implement artifact gap detection
- unit-test with fixtures

Acceptance:

- same inputs always yield same delta
- no AI involvement required to compute deltas

### Phase 4 — implement artifact loop

Goal:

- introduce the real programmer only together with the artifact auditor

Tasks:

- define `build_task_packet`
- define `artifact_bundle`
- define `artifact_audit_verdict`
- define `repair_packet`
- implement bounded retry rules
- seed artifact cookbook

Acceptance:

- generated artifacts cannot advance without audit approval

### Phase 5 — connect plan compiler to deterministic delta

Goal:

- remove manifest interpretation from the plan compiler

Tasks:

- make plan compiler consume `delta_report`
- keep admin help + read-only query tools
- add plan validation and cookbook seeding

Acceptance:

- plan compiler converts delta to plan, not design to plan

### Phase 6 — implement failure classifier

Goal:

- make retries stage-aware and predictable

Tasks:

- deterministic matcher library
- residual AI classifier
- route failures to correct repair loops

Acceptance:

- common failures classify without AI

### Phase 7 — implement confirmation checkpoints and pipeline persistence

Goal:

- stabilize operator control and recovery

Tasks:

- CONFIRM 1 flow
- CONFIRM 2 flow
- `pipeline_runs` persistence
- interrupted run recovery

Acceptance:

- operator sees only two approval checkpoints
- interrupted runs are recoverable

### Phase 8 — verification and cookbook writes

Goal:

- ensure learning only from verified wins

Tasks:

- verification auditor
- cookbook write policy
- manual seed import

Acceptance:

- no cookbook receives unverified garbage

---

## 18. Testing strategy

### 18.1 Deterministic compiler tests

Must be the largest test surface above execution.

Required:

- manifest fixture + actual state fixture -> exact `delta_report`
- ownership edge cases
- no-op/idempotency cases
- delete classification cases
- artifact gap cases

### 18.2 Artifact loop tests

Required:

- valid artifact passes
- invalid artifact yields repairable verdict
- repeated same failure stops early
- approved artifact becomes available to planning

### 18.3 Plan compiler tests

Required:

- delta -> valid `executor_plan`
- order constraints preserved
- unknown action rejected
- schema mismatch rejected

### 18.4 Failure classifier tests

Required:

- deterministic mapping for common error strings/payloads
- residual path only for unknown signatures

### 18.5 End-to-end tests

Required:

1. design approval -> artifact loop -> plan -> execute success
2. artifact repair loop success after one correction
3. plan invalid -> corrected without returning to designer
4. missing environment/runtime materialization -> correct classification
5. interrupted run recovery

---

## 19. What this architecture explicitly rejects

The system must not drift back into any of the following.

### 19.1 AI diffing of desired vs actual state

Rejected. Diffing is deterministic compiler work.

### 19.2 One “super programmer” agent

Rejected. Design, artifact generation, and plan compilation are different concerns.

### 19.3 Artifact generation without artifact audit

Rejected. The artifact loop is inseparable.

### 19.4 A single generic cookbook

Rejected. Knowledge must be layer-specific.

### 19.5 More than two human confirmation points by default

Rejected. Internal loops should not spam the operator.

### 19.6 Free-form failure reasoning as the primary classifier

Rejected. Deterministic rules come first.

### 19.7 New storage infrastructure for pipeline persistence

Rejected. Extend current host persistence.

---

## 20. Success criteria

This rearchitecture is successful when all of the following are true:

1. `solution_manifest` is cleanly split into `desired_state` and `advisory`
2. deterministic Rust code can compute `delta_report` from manifest + actual state
3. missing artifacts are emitted as `build_task_packets`
4. the artifact loop can produce approved artifacts or stop with structured failure
5. the plan compiler consumes `delta_report`, not raw design intent
6. the operator sees only two confirmation points
7. common failures are classified deterministically
8. the working executor boundary remains unchanged and stable
9. cookbook writes occur only after verified success
10. interrupted pipeline runs can be recovered visibly

---

## 21. Bottom line

The right next architecture for Fluxbee is not to keep stretching the pilot upward.

It is to do three things cleanly:

1. **keep the executor boundary that already works**
2. **move reconciliation into deterministic code**
3. **split design, artifact generation, and plan compilation into separate typed loops**

The decisive formula is:

> `solution_manifest` = declarative design source  
> `delta_report` = deterministic reconciliation result  
> `artifact_loop` = bounded technical generation/repair  
> `executor_plan` = exact operational program  
> `executor` = exact admin operator

That is the architecture that can scale from pilot to full Fluxbee without freezing the system in the accidental shape of its first successful experiments.
