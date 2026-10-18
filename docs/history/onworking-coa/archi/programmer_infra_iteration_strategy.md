# SY.architect — Programmer / Infrastructure Iteration Strategy

Date: 2026-04-23
Scope: pilot architecture review based on `programmer_agent_tasks.md`, `handbook_fluxbee.md`, `runtime_package_publish_tasks.md`, and `sy_architect.rs`

## 1. Main conclusion

The current implementation already has a viable base for a planner/executor loop, but it is not yet a good strategy for a true code-writing programmer loop.

The key correction is:

- keep **planner** and **coder** as separate roles
- keep **infrastructure executor** as the only authority for publish / spawn / config / route execution
- add a small **auditor** between programmer output and the next retry
- store only **verified successful patterns** in the cookbook

## 2. What exists today

### 2.1 Planner path exists

Current `fluxbee_programmer` is not really a software programmer. It is a planner that:

- reads live admin actions
- reads cookbook examples
- calls an OpenAI function loop
- submits an `executor_plan`
- validates the plan shape
- stores a pending plan for later confirmation/execution

### 2.2 Live-state and help lookup already exist

The programmer prompt and tools already push the agent to:

- query live state with `query_hive`
- read exact action schemas with `get_admin_action_help`
- submit exactly one final plan through `submit_executor_plan`

This is good and should stay.

### 2.3 Execution and package publication are already separated correctly

Publication and install belong to `SY.admin`; activation/spawn belongs to `SY.orchestrator`.
That separation is healthy and should not be collapsed into the programmer.

## 3. The current mismatch

The main architectural mismatch is that the current component named `fluxbee_programmer` is a **plan synthesizer**, while the new need is a **code-writing programmer** that creates package contents or runtime software which the infrastructure path later publishes and runs.

If both responsibilities stay inside one agent, retries become noisy and failure attribution becomes weak.

## 4. Recommended role split

### 4.1 Archi host

Responsibilities:

- understand operator intent
- choose whether the task is planning-only, coding, infra execution, or mixed
- assemble the task packet
- maintain run state, attempt count, and chat visibility
- decide when to stop

### 4.2 Programmer (coder)

Responsibilities:

- write or modify package contents
- produce structured artifacts, not raw prose
- output a bounded result:
  - files
  - package metadata
  - declared assumptions
  - declared expected runtime base
  - declared acceptance checks

The coder should **not** publish, spawn, or route directly.

### 4.3 Infrastructure executor

Responsibilities:

- materialize package source
- call `publish_runtime_package`
- perform `sync/update/run_node/restart_node/add_route`
- run verification checks
- return structured failures

The executor should remain the only component that touches Fluxbee admin/orchestrator mutations.

### 4.4 Auditor

Responsibilities:

- classify the failure
- decide if retry is useful
- shrink feedback before it goes back to the coder
- reject loops that are just repeating the same broken idea

The auditor should be small and strict.

## 5. The loop that should exist

Use this fixed loop:

1. Archi creates a **task packet**
2. Programmer returns a **candidate artifact**
3. Auditor performs a **static review**
4. Infrastructure executes the artifact in a controlled way
5. Auditor classifies the result
6. If retryable, Archi sends a compact **repair packet** back to programmer
7. Stop after bounded attempts

## 6. Task packet shape

The packet sent to the programmer should be explicit:

```json
{
  "task_id": "...",
  "goal": "build a WF node that ...",
  "target_hive": "motherbee",
  "artifact_kind": "runtime_package_source",
  "constraints": {
    "runtime_type": "config_only | full_runtime | workflow",
    "runtime_base": "AI.common",
    "naming_rules": true,
    "allowed_actions": ["publish_runtime_package", "run_node", "add_route"]
  },
  "acceptance": {
    "must_publish": true,
    "must_spawn": true,
    "must_route": false
  },
  "known_context": {
    "runtimes": [],
    "running_nodes": [],
    "routes": []
  },
  "attempt": 1,
  "max_attempts": 3,
  "cookbook_context": []
}
```

This keeps the coder bounded and makes retries comparable.

## 7. Programmer output shape

The coder should return a structured artifact, for example:

```json
{
  "status": "artifact_ready",
  "artifact_kind": "runtime_package_source",
  "summary": "...",
  "package": {
    "source": {
      "kind": "inline_package",
      "runtime_name": "wf.router",
      "version": "0.1.0",
      "files": {
        "package.json": "...",
        "workflow.json": "..."
      }
    }
  },
  "assumptions": ["AI.common exists on motherbee"],
  "verification_hints": ["spawn WF.router@motherbee", "check route prefix ..."]
}
```

Avoid free text as the main payload.

## 8. Auditor decision model

The auditor should classify outcomes into a small set:

- `STATIC_INVALID`
  - malformed package
  - missing required file
  - wrong schema
  - invalid node/runtime naming
- `CONTRACT_INVALID`
  - admin help or runtime contract mismatch
  - wrong action args
- `ENVIRONMENT_MISSING`
  - runtime base missing
  - node dependency missing
  - wrong hive target
- `EXECUTION_TRANSIENT`
  - timeout
  - temporary unavailable service
- `LOGIC_BUG`
  - package publishes but behavior is wrong
  - node starts but does not satisfy acceptance checks
- `NON_RETRYABLE`
  - repeated same failure
  - task contradicted by constraints

Only the first five should produce retry feedback. The last should stop the loop.

## 9. Repair packet shape

Do not send raw logs back to the coder.
Send a compact repair packet:

```json
{
  "task_id": "...",
  "attempt": 2,
  "previous_artifact_digest": "sha256:...",
  "failure_class": "STATIC_INVALID",
  "failing_stage": "publish_runtime_package",
  "error_summary": "package missing package.json",
  "error_details": [
    "publish_runtime_package rejected source.kind=inline_package because package.json was absent"
  ],
  "do_not_repeat": [
    "do not omit package.json",
    "preserve runtime_name wf.router"
  ]
}
```

That is much better than “programmer gets feedback from infrastructure” in raw form.

## 10. Retry policy

Recommended default:

- max 3 coder attempts per task
- max 1 retry for the same failure class unless artifact digest changes substantially
- if the same failure class happens twice with near-identical artifact, stop
- transient infra failures may retry executor without asking coder to rewrite code

This is critical. Otherwise the system burns cycles rewriting the same broken package.

## 11. Cookbook policy

The cookbook should store only **verified successes**, not just “plan executed without immediate error”.

A cookbook entry should require:

1. publish success
2. spawn success if required
3. post-run verification success
4. no auditor objections

### 11.1 Cookbook entry should include more context

Current shape is too weak for robust reuse. Add:

- `artifact_kind`
- `runtime_type`
- `runtime_base`
- `failure_classes_avoided`
- `acceptance_signature`
- `environment_fingerprint`

### 11.2 Cookbook retrieval should be gated

Do not inject all recent entries.
Filter by:

- task family
- runtime type
- node prefix family (`AI`, `WF`, `IO`)
- whether publish is needed
- whether task is single-hive or multi-hive

## 12. Concrete issues found in the current implementation

1. **Planner vs programmer confusion**
   - the current `fluxbee_programmer` is a planner, not a code writer
2. **Spec/code mismatch for cookbook storage**
   - task doc says `{blob_root}/cookbook/programmer-v1.json`
   - code writes to `state_dir/programmer/cookbook-v1.json`
3. **Spec/code mismatch for tool registration**
   - task doc says `ProgrammerSubmitTool` as only registered function
   - code also registers `get_admin_action_help` and `query_hive`
4. **Implicit hidden retry exists already**
   - the first plan is pre-validated with `executor_validate_plan`
   - on failure, code retries programmer once with feedback
5. **Cookbook success criterion is too weak**
   - current write happens after executor success, but there is no separate acceptance verifier/auditor gate yet

## 13. Best next step

Do not try to make one giant smart programmer.

Implement this next:

### Phase 1
- freeze role split: `planner`, `coder`, `infrastructure executor`, `auditor`
- rename the current `fluxbee_programmer` conceptually to `planner` in docs, even if code name stays for now

### Phase 2
- define coder artifact contract
- define repair packet contract
- define auditor classification contract

### Phase 3
- add attempt ledger in Archi state
- persist task_id, attempt_no, failure_class, artifact_digest, final_outcome

### Phase 4
- gate cookbook writes on audited success
- add cookbook retrieval filter

### Phase 5
- expose one combined chat timeline:
  - programmer/planner activity
  - infrastructure execution
  - auditor verdict
  - retry count

## 14. Bottom line

The system will start behaving coherently when it follows this rule:

- **programmer writes artifacts**
- **infrastructure executes reality**
- **auditor decides what the failure means**
- **cookbook stores only verified winners**

Without that split, the loop becomes noisy and the agent learns from accidental “green” cases that were not actually robust.
