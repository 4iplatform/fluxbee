# Fluxbee Executor Plan Pilot Spec

**Status:** draft for implementation  
**Date:** 2026-04-19  
**Audience:** `SY.architect`, `SY.admin`, executor/runtime developers  
**Purpose:** define the pilot implementation of the **last execution stage** of Archi using an AI executor that lives inside `SY.admin`, OpenAI function calling, and a minimal execution-oriented plan.

---

## 1. Goal

The goal of this pilot is to implement and test the **final execution stage** of Archi without waiting for the full architecture/design pipeline to be complete.

This pilot focuses only on:

- receiving an execution-oriented plan from outside Archi
- having `SY.architect` read and validate it
- invoking the admin-resident AI executor
- exposing **one OpenAI function per real Fluxbee admin action**
- executing steps in sequence
- using admin help to remove ambiguity when needed
- showing the execution sequence and results in the chat

This pilot does **not** attempt to solve the full design problem.
It solves the **execution problem**.

Important terminology for this pilot:

- **manifest** = the declarative source artifact for solution design
- **execution plan** = the ordered executable artifact derived from a manifest or provided externally

This document defines the **execution plan** layer, not the canonical solution manifest.

---

## 2. Core Decision

The executor will operate against the **real Fluxbee admin surface**, not against a macro layer, not against a generic DSL, and not through renamed helper abstractions.

### 2.1 What this means

- each admin action is exposed as an OpenAI function tool
- the function name is exactly the same as the admin action name
- each function has its own JSON schema
- each function schema must match the real `SY.admin` contract exactly
- the executor calls those functions directly
- `get_admin_action_help` is also exposed as a function
- the executor may use help to clarify contracts before executing

### 2.2 What this explicitly rejects

- no macro framework
- no execution DSL
- no generic tool like `execute_admin(path, method, body)` exposed to the model
- no renamed simplified functions for the executor layer
- no `CONFIRM` pause in the middle of execution for this pilot mode

---

## 3. Why This Direction

The main problem to solve is not architectural creativity.
The main problem is operational correctness.

The executor must be a highly specialized operator that:

- knows how to work with Fluxbee admin actions
- knows how to read help and contracts
- knows how to build the correct JSON for each function call
- does not invent action names
- does not invent payload shapes
- stops when a step cannot be executed safely

The purpose of the AI layer here is **not** to invent a command language.
The purpose is to resolve the last-mile structured data needed to invoke the real admin actions correctly.

---

## 4. Responsibility Split

### 4.1 `SY.architect`

`SY.architect` is the host.
It is responsible for:

- receiving the execution plan
- validating the execution plan schema
- extracting the execution block
- creating an execution session
- sending the execution request to `SY.admin`
- tracking step-by-step progress
- rendering execution events in the chat
- returning the final execution summary

`SY.architect` is the orchestrator of the pilot.
It is **not** the specialist executor.

### 4.2 `SY.admin` Executor Mode

`SY.admin` hosts the executor mode for this pilot.
That executor mode is responsible for:

- reading the provided execution steps
- executing them in order
- calling the exact admin-mapped functions
- using admin help when needed
- stopping on ambiguity, contract mismatch, or execution failure
- reporting progress and results per step

Why `SY.admin` is the natural home:

- the action registry already lives there
- the help system already lives there
- request contracts already originate there
- execution errors are born there
- it avoids duplicating the admin action catalog inside `SY.architect`

The executor does **not**:

- redesign the solution
- add extra steps on its own
- reorder steps
- rename actions
- invent missing fields silently
- bypass the declared function schemas

---

## 5. Function Calling Model

## 5.1 One function per admin action

For this pilot, the function catalog exposed to the executor should be built 1:1 from the admin action surface and hosted by `SY.admin`.

Examples:

- `list_admin_actions`
- `get_admin_action_help`
- `get_versions`
- `get_runtime`
- `list_nodes`
- `add_hive`
- `remove_hive`
- `add_route`
- `delete_route`
- `add_vpn`
- and so on

The exact names must match admin exactly.

## 5.2 No generic path/body executor tool

The executor must **not** receive a generic function such as:

```json
{
  "name": "execute_admin",
  "parameters": {
    "path": "...",
    "method": "...",
    "body": {"...": "..."}
  }
}
```

That would reintroduce ambiguity and move the burden of contract expression back onto the model.

## 5.3 Each function has its own strict schema

Each function must have:

- explicit parameter names
- explicit required fields
- explicit field types
- `additionalProperties: false`
- enums where the admin contract is finite
- exact argument names from `SY.admin`
- no executor-only aliases such as `target` when the real admin contract uses `hive`, `runtime`, `node_name`, etc.

The purpose is to make incorrect invocation structurally harder.

## 5.4 Catalog maintenance goal

The programmer-facing goal is:

- when a new admin action becomes executor-visible, a matching OpenAI function is added in `SY.admin`
- the function should be generated from admin metadata whenever possible
- the team should avoid hand-maintaining multiple divergent schema sources

Preferred long-term flow:

1. define/update the admin action in `SY.admin`
2. expose a machine-usable request contract there
3. generate or register the executor-visible OpenAI function inside `SY.admin` from that contract
4. add/update focused tests for the action mapping

What changes in each node:

- `SY.admin`
  - gains executor mode
  - gains OpenAI function catalog generation from admin actions
  - gains executor prompt/runtime
  - may return richer execution errors and explanations because it is operating next to the real action contracts

- `SY.architect`
  - stays the host/UI
  - no longer owns the executor function catalog
  - sends plans to `SY.admin`
  - renders progress/results returned by `SY.admin`

---

## 6. Role of Help

Help is part of the execution model, but it is **supporting**, not primary.

### 6.1 Help is used for

- checking whether an action exists
- confirming required fields
- understanding argument meaning
- resolving uncertainty before a call
- confirming example payload shape

### 6.2 Help is not used for

- replacing the function schemas
- dynamically generating a new action surface
- allowing arbitrary free-form admin requests

### 6.3 Practical rule

If the executor cannot confidently build the call from:

- the current step
- the function schema
- its execution rules

then it may call:

- `get_admin_action_help`

If the step still cannot be completed after that, execution stops.

---

## 7. Execution Plan Scope for This Pilot

This pilot does **not** require a full solution manifest.
It requires only a minimal execution-oriented plan.

The plan is not a macro language.
It is just a structured sequential list of admin actions to run.

### 7.1 Minimal execution plan shape

```json
{
  "plan_version": "0.1",
  "kind": "executor_plan",
  "metadata": {
    "name": "pilot_route_setup",
    "target_hive": "motherbee"
  },
  "execution": {
    "strict": true,
    "stop_on_error": true,
    "allow_help_lookup": true,
    "steps": [
      {
        "id": "s1",
        "action": "get_runtime",
        "args": {
          "hive": "motherbee",
          "runtime": "AI.chat"
        }
      },
      {
        "id": "s2",
        "action": "add_route",
        "args": {
          "hive": "motherbee",
          "prefix": "tenant.demo.",
          "action": "next_hop_hive",
          "next_hop_hive": "southbee"
        },
        "executor_fill": {
          "allowed": [],
          "notes": []
        }
      }
    ]
  }
}
```

### 7.2 Meaning of the fields

#### `plan_version`
Version of this execution plan format.

#### `kind`
Must be `executor_plan` for this pilot mode.

#### `metadata`
Descriptive metadata for the execution session.

#### `execution.strict`
When `true`, the executor must not improvise missing data.

#### `execution.stop_on_error`
When `true`, any failed step ends the execution session.

#### `execution.allow_help_lookup`
When `true`, the executor may call `get_admin_action_help` when needed.

#### `execution.steps`
Ordered list of admin actions to execute.

Each step contains:

- `id`: unique step identifier
- `action`: exact admin action name
- `args`: structured JSON arguments matching the real admin function schema exactly
- `executor_fill`: optional declaration of fields that may be derived from infrastructure knowledge instead of design intent

### 7.3 `args` is the primary contract

For this pilot, the plan should provide the exact executor call shape as much as possible.

That means:

- if the admin function expects `hive`, the plan uses `hive`
- if the admin function expects `runtime`, the plan uses `runtime`
- if the admin function expects `node_name`, the plan uses `node_name`

The plan must not introduce a second aliasing layer.

### 7.4 `executor_fill` is only for infrastructure-known refinement

The executor may complete only values that are:

- operationally obvious from current infrastructure
- derivable from admin help or read-only state
- not architectural decisions

Allowed examples:

- using the current hive when the plan explicitly allows it
- applying a documented optional default from the admin contract
- filling a mechanically derivable field from current infrastructure

Forbidden examples:

- choosing a route destination
- inventing a tenant id
- deciding merge vs replace unless the plan already declared it
- inventing runtime names, workflow names, or node names

---

## 8. Execution Rules

The executor must follow these rules strictly.

### 8.1 Sequence rule

Steps execute in order.
No reordering.
No parallelization in this pilot.

### 8.2 Action rule

The executor must call the function whose name matches `step.action` exactly.

### 8.3 Argument rule

The executor must use `step.args` as the base call data.
It may only refine the call when:

- the field is explicitly allowed by `step.executor_fill`
- the value is derivable from infrastructure knowledge or admin help
- the value is not a design decision

### 8.4 No invention rule

The executor must not silently invent:

- action names
- extra steps
- missing required values
- replacement semantics not present in the step
- architectural intent

### 8.5 Stop rule

Execution stops when:

- a step references an unknown action
- required data is missing
- help still leaves the step ambiguous
- function validation fails
- the admin action returns failure and `stop_on_error=true`
- `SY.admin` is unavailable or times out

### 8.6 Help lookup rule

If allowed, the executor may call `get_admin_action_help(step.action)` before executing a step when there is uncertainty.

### 8.7 Auxiliary read rule

The executor may insert **auxiliary read/help calls** only when needed to execute a declared step safely.

Allowed auxiliary calls:

- `get_admin_action_help`
- read-only admin actions needed to resolve infrastructure-known values

Not allowed as auxiliary calls:

- undeclared mutating actions
- destructive side effects outside the declared step list

### 8.8 Reporting rule

Every step must produce visible execution events in the chat.

---

## 9. No Human Confirmation in Pilot Mode

This pilot intentionally removes interactive human confirmation inside the execution flow.

### 9.1 Why

The goal is to test the end-to-end last-mile execution mechanism.
A `CONFIRM` checkpoint would block the pilot and prevent full execution testing.

### 9.2 Consequence

In this mode:

- write actions execute directly
- there is no staged write waiting for user confirmation
- safety is enforced by strict schemas, action-level help, and stop-on-error behavior
- if this breaks, it is acceptable for the pilot

### 9.3 Important note

This is a pilot-only execution mode.
It should remain clearly separable from any future production mode that may require interactive confirmation or policy gates.

---

## 10. Chat Visibility Requirements

One of the core requirements of this pilot is that execution must be visible in the chat.
The user must be able to see:

- which step is running
- which function was called
- whether help lookup happened
- the result of the call
- where execution stopped if there was an error

### 10.1 Event states

Each step should emit visible states such as:

- `queued`
- `running`
- `help_lookup`
- `done`
- `failed`
- `stopped`

### 10.2 Minimum event payload

For each event, the host should be able to render at least:

- `execution_id`
- `step_id`
- `step_index`
- `step_action`
- `status`
- `timestamp`
- `summary`
- `tool_name`
- `tool_args_preview`
- `result_preview`
- `error_message` if any
- optional `error_code`
- optional `error_source`

### 10.3 Event source

For the pilot, the host should be the event source of record.

Recommended MVP behavior:

- `SY.architect` emits `queued`, `running`, `done`, `failed`, `stopped`
- the executor produces structured tool results
- the host converts those results into visible chat events

This avoids introducing a second event transport subsystem in the pilot.

### 10.4 Example chat rendering

```text
Execution: pilot_route_setup

[1/2] get_runtime
status: running

[1/2] get_runtime
status: done
result: runtime AI.chat found on motherbee

[2/2] add_route
status: running
args: {"hive":"motherbee","prefix":"tenant.demo.","action":"next_hop_hive","next_hop_hive":"southbee"}

[2/2] add_route
status: done
result: route added
```

### 10.5 Example with help lookup

```text
[2/2] add_route
status: help_lookup
help_action: add_route

[2/2] add_route
status: running

[2/2] add_route
status: done
result: route added
```

### 10.6 Example with failure

```text
[2/2] add_route
status: failed
error: missing required field 'prefix'
execution stopped
```

---

## 11. Host-Executor Runtime Flow

The runtime flow of this pilot should be:

```text
external execution plan
  -> SY.architect
  -> schema validation
  -> execution session creation
  -> execution request to SY.admin
  -> SY.admin executor reads steps in order
  -> SY.admin executor calls admin-mapped functions
  -> optional help lookup
  -> step event stream emitted back to host
  -> chat rendering updated
  -> final summary returned
```

### 11.1 Detailed loop

For each step:

1. mark step as `queued`
2. mark step as `running`
3. optionally call `get_admin_action_help`
4. validate the step against the function schema
5. call the function named by `step.action`
6. capture raw result
7. emit `done` or `failed`
8. if failed and `stop_on_error=true`, emit `stopped` for the plan

---

## 12. Recommended Executor Prompt Contract

The executor prompt should be narrow and operational.
It should encode rules such as:

- you are the Fluxbee executor specialist
- you execute only the provided execution plan steps
- you must call the exact admin-mapped function for each step
- do not rename actions
- do not add or reorder steps
- do not invent missing required values
- you may only complete infrastructure-obvious values explicitly allowed by the plan
- use `get_admin_action_help` when the contract is unclear and help lookup is allowed
- if needed, you may use auxiliary read/help calls, but never undisclosed mutating calls
- if a step remains ambiguous or incomplete, stop and report failure
- emit progress clearly for each step
- keep execution deterministic and exact

The prompt should **not** try to contain the full admin manual.
The tool schemas and help system are the source of operational truth.

This executor prompt belongs in `SY.admin`, not in `SY.architect`.

---

## 13. Function Catalog Generation Strategy

The function catalog should be generated from the admin action registry whenever possible.

### 13.1 Desired behavior

For each admin action:

- expose one OpenAI function
- use the exact action name
- derive or define its JSON schema
- include a concise description
- bind it to the existing admin dispatcher implementation

The catalog should prefer **generation from admin metadata** over hand-written duplicate schemas whenever possible.

If some actions cannot yet be generated cleanly, they may be registered manually for the pilot, but the long-term target remains:

- one admin action
- one machine-usable contract source
- one executor-visible OpenAI function

### 13.2 Internal implementation note

Even if all actions are exposed as separate OpenAI functions, the backend may still route them through a shared admin dispatcher internally.

That is acceptable.
The important constraint is the **model-visible surface**, not the internal transport code.

For this pilot, that catalog should live in `SY.admin`, because that is where:

- the action registry already exists
- action help already exists
- request contract drift can be controlled at the source
- richer operation-specific errors can be produced most naturally

### 13.3 Pilot bootstrap and discovery in `SY.admin`

For implementation and operator bootstrap, the executor configuration in `SY.admin` should use the node's standard control-plane surface:

- `CONFIG_GET`
- `CONFIG_SET`

through the existing admin/operator path:

- `POST /hives/{hive}/nodes/SY.admin@{hive}/control/config-get`
- `POST /hives/{hive}/nodes/SY.admin@{hive}/control/config-set`

This bootstrap surface is responsible for:

- loading the OpenAI key in local node secrets
- setting executor-local model/catalog config

Executor function discovery may still use a read-only admin HTTP path such as:

- `GET /admin/executor/functions`

because that is catalog discovery, not node secret/config mutation.

The execution path remains:

- `SY.architect` hosts the session
- `SY.architect` sends an execution request to `SY.admin`
- `SY.admin` runs the executor against the real admin action surface

Pilot host ingress in `SY.architect` may use a dedicated API path such as:

- `POST /api/executor/plan`

That path is separate from:

- normal operator chat
- SCMD
- attachment upload

Its purpose is only to ingest an external `executor_plan`, create or reuse a session, invoke `SY.admin`, and persist/render the execution result.

Pilot implementation may also persist a redacted execution log on the `SY.admin` side containing:

- incoming execution request
- step events
- final summary

If present, the final summary may expose a `log_path` for operator/debug use.

Current implementation direction for the pilot:

- internal request contract lives in `SY.admin` as executor-specific internal admin commands
- `executor_validate_plan`
- `executor_execute_plan`

These are internal host-to-admin execution contracts.
They are **not** part of the normal operator-facing admin action catalog.

---

## 14. MVP Boundaries

This pilot intentionally excludes:

- full solution design
- multi-specialist planning
- reconciler logic
- diff generation
- automatic architecture inference
- parallel step scheduling
- rollback orchestration
- interactive confirmation inside execution
- macro languages
- renamed helper abstractions

This pilot also excludes:

- generic planner intelligence inside the executor
- hidden mutating pre-steps invented by the executor
- use of `SY.timer` for execution progress; MVP progress should be host-side code

The pilot includes only the last-mile execution mechanism.

---

## 15. Success Criteria

This pilot is successful if all of the following are true:

1. `SY.architect` can receive an external execution plan
2. the plan can be validated and parsed
3. the executor can be invoked with an admin-backed function catalog sufficient for the pilot scope
4. the executor can call real admin actions by exact name
5. help can be used to resolve contract uncertainty
6. the execution sequence is visible in chat step by step
7. failures stop the flow clearly and visibly
8. the entire loop can be tested end to end without manual `CONFIRM`

---

## 16. Immediate Implementation Plan

### Phase 1

- define `executor_plan` schema
- add a dedicated executor mode in `SY.admin`
- disable staged confirmation in this mode

### Phase 2

- expose the full admin action catalog as OpenAI functions in `SY.admin`
- keep function names identical to admin names
- add `get_admin_action_help`
- ensure function argument names match admin exactly
- ensure schemas use `additionalProperties: false`

### Phase 3

- implement the executor prompt in `SY.admin`
- wire execution plan requests from `SY.architect` to `SY.admin`
- add stop-on-error handling
- add support for executor-safe auxiliary read/help calls

### Phase 4

- emit step-by-step execution events from `SY.admin`
- render events and results in chat from `SY.architect`
- return final execution summary

---

## 17. Bottom Line

The correct pilot for this stage is:

- not a macro system
- not a generic admin request tool
- not a redesign of the full manifest architecture

It is:

- a minimal execution plan
- `SY.architect` as host/UI
- `SY.admin` as the AI executor home
- one strict OpenAI function per real admin action
- help lookup as a supporting constraint mechanism
- direct execution without interactive confirmation
- visible step-by-step execution in the chat

This gives a complete, testable implementation of the **last part of Archi** while keeping the surface exact, auditable, and close to the real Fluxbee operational model.
