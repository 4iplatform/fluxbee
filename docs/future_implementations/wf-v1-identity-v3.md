# Fluxbee — WF (Workflow Nodes) v1

**Status:** v1.0 draft
**Date:** 2026-04-13
**Audience:** AI designers (Archi), workflow authors, runtime developers, ops/SRE

---

## Table of Contents

**Part I — Conceptual foundation**
1. Why WF exists
2. What WF is and what WF is not
3. The four properties of a WF task
4. How WF fits with OPA and AI nodes
5. How a WF is triggered
6. WF as a system toolbox

**Part II — Model and runtime**
7. Node architecture
8. Workflow definition format
9. States, transitions, and guards
10. Actions (the primitive vocabulary)
11. Timers in WF
12. Soft abort
13. Persistence
14. Runtime lifecycle
15. WF_HELP and self-description

**Part III — Operational surface**
16. SY.admin read-only surface
17. Authorization (via OPA and claims)
18. Error codes

**Part IV — Canonical example**
19. Case study: `WF.invoice` (billing)

**Part V — Reference**
20. Decisions and scope
21. v1 limitations (explicit)
22. References

---

# Part I — Conceptual Foundation

## 1. Why WF exists

Fluxbee is built on the premise that an AI can operate a complete system. But this premise is only true if the AI has tools to delegate deterministic work to something that is actually deterministic.

LLMs are probabilistic by construction. They compress their input context into an implicit representation and decide from there, which makes them phenomenally good at things that tolerate variability in the result — interpretation, synthesis, natural conversation, judgment. But a real system has tasks that do not tolerate variability. An invoice is issued exactly once, with exactly the right items, in exactly the right order relative to payment. Not "roughly right". Not "usually correct". Exactly.

If these deterministic tasks live inside the probabilistic layer, the system cannot be trusted to operate critical processes. The LLM will occasionally get it wrong, not because it is defective, but because that is what probabilistic systems do. The only way to build a trustworthy AI-operated system is to give the AI a place to delegate the parts that must be deterministic, and trust that those parts will execute the same way every time.

**WF is that place.** It is the layer where determinism lives in a system whose primary layer is probabilistic. It exists so that an AI can say: "this, specifically this, must happen exactly this way, and I don't trust any LLM to do it repeatedly well, so I delegate it to an executor that is deterministic by construction".

Without WF, Fluxbee would be an AI orchestration framework that cannot be entrusted with real business operations. With WF, it becomes a substrate on which AI can be trusted to build and run complete systems, because the determinism-sensitive parts are handled by a component whose entire purpose is determinism.

This is not a feature. It is the mechanism that makes the whole premise of Fluxbee viable.

## 2. What WF is and what WF is not

**WF is** a family of system nodes that execute deterministic multi-step processes, maintaining state between steps, coordinating other nodes through messages, and guaranteeing that the process completes according to a fixed definition even if it takes hours or days.

**WF is not:**

- A router with history. Routing single messages is what OPA does. WF coordinates processes that span many messages in time.
- A middleware that intercepts messages between other nodes. WF is a destination like any other node. The router routes a message to a WF node if OPA decides that message should go there; WF never sits between other nodes.
- A lenguaje general de programación. WF is a state machine with a small, fixed vocabulary of actions. If you find yourself wanting to write imperative code inside a WF, you are probably using the wrong tool.
- An AI with deterministic wrapping. WF does not reason. It evaluates. If reasoning is required, reasoning lives in an AI node that receives a question from the WF and returns a structured answer that the WF evaluates deterministically.
- A place where ambiguity is resolved. If data is ambiguous, the WF delegates the ambiguity to an AI node, which returns something structured that the WF can evaluate. The WF itself never handles ambiguity directly.

### 2.1 WF vs OPA vs AI — the clean separation

| Layer | What it does | Stateful? | When decisions are made |
|---|---|---|---|
| **OPA** | Decides where a single message should go, based on message + system state snapshot | No | Per-message, sub-millisecond, inline in the router |
| **AI node** | Handles ambiguous tasks: interpretation, conversation, judgment, natural language generation | Sometimes (via thread memory) | Per-request, probabilistic, responds with content |
| **WF** | Orchestrates a multi-step process with memory, evaluating deterministic transitions on events | Yes (per instance) | Per-event, deterministic, across many messages in time |

The three layers are complementary, not competing. A typical business flow uses all three: OPA routes messages, AI handles ambiguous parts, WF coordinates the deterministic process that holds it all together.

## 3. The four properties of a WF task

A task belongs in WF if and only if it has **all four** of the following properties. If any is missing, the task is probably not a WF.

**Multi-step.** The task involves more than one thing happening in a coordinated sequence. A single-step task is a capability or a message, not a workflow.

**Memory between steps.** The steps depend on each other. Step 2 needs to know what happened in step 1. Step 4 may need data gathered in step 1 and modified in step 3. This memory persists across the lifetime of the task.

**Deterministic.** The result of each step does not tolerate variability. Either the step succeeds exactly or it fails in a predictable way. A step whose result is "whatever the LLM thinks is reasonable" is not deterministic and does not belong in a WF.

**Lives in time.** The task may span seconds, minutes, hours, or days. It survives node restarts. It waits for external events. It may sleep for long periods between steps.

### 3.1 Examples that are WFs

- **Invoice issuance.** Multi-step (validate data, create invoice, send to customer, confirm), memory (data collected builds up), deterministic (exactness required), lives in time (waits for customer data, waits for external system response).
- **Customer onboarding.** Multi-step (collect info, create tenant, associate ILKs, configure, welcome), memory (progress tracked), deterministic (mandatory steps in order), lives in time (waits for user input, waits for approvals).
- **Support escalation with SLA.** Multi-step (wait, check response, escalate L1→L2→human), memory (elapsed time, tries), deterministic (SLA rules are rules), lives in time (hours or days of waiting).
- **End-of-month close.** Multi-step (gather data, compute, validate, publish), memory (intermediate results), deterministic (must be exact), lives in time (takes real time to run).
- **Multi-party approval.** Multi-step (request each approver, collect responses), memory (who approved), deterministic (rules for "enough approvals"), lives in time (waits for humans).

### 3.2 Examples that are not WFs

- **Classify the intent of a message.** Single-step, no memory across invocations, and the output is not strictly deterministic (it is a classification, inherently probabilistic). This is an AI node.
- **Route this message to the right node.** Single-step, stateless, deterministic but trivially so. This is OPA.
- **Generate a friendly response for the user.** Single-step and probabilistic. This is an AI node.
- **Fetch data from an external API.** Single-step (from the caller's perspective). This is a capability / IO node.
- **Maintain a conversation with a customer.** Multi-step and lives in time, but not deterministic. Conversations are inherently probabilistic. This is an AI node, possibly with memory help from SY.cognition.

If you are unsure whether something is a WF, ask yourself: "If this gets it wrong once in a hundred attempts, is that acceptable?" If yes, it is probably an AI node. If no, it is probably a WF.

## 4. How WF fits with OPA and AI nodes

The key insight: **WF is a pure executor**, in the same spirit as OPA. A WF transition is a pure function:

```
(current_state, event, workflow_definition) → (next_state, [actions])
```

No I/O during evaluation. No calls to other nodes during evaluation. No consulting external data at decision time. If the WF needs data to decide, that data must arrive as part of the event, placed there by whoever emitted the event.

This mirrors how OPA works: OPA does not fetch data while evaluating; the caller passes the input with everything OPA needs to know. OPA decides and returns. WF does the same, but over a longer-lived process.

The consequence is powerful: **every WF decision is replayable**. Given the same initial state and the same sequence of events, a WF always reaches the same final state. This property is the foundation of determinism and is what makes WF trustworthy for critical processes.

Actions are not evaluation. Actions are effects that the runtime publishes **after** the transition has been decided. `send_message`, `schedule_timer`, `set_variable` — these are side effects of the decision, not parts of the decision itself. The decision is pure; the effects come next.

### 4.1 What happens when ambiguity arises

If a WF step encounters ambiguous data ("are these customer details sufficient to invoice?"), the WF **does not try to resolve the ambiguity itself**. It delegates to an AI node by emitting a message, then waits for the AI's response as an incoming event, then evaluates the response deterministically.

```
WF state: awaiting_data_review
  ↓ send_message to AI.billing-validator with customer data
  ↓ (wait for response)
  ↓ receive event: {complete: true} or {complete: false, missing: [...]}
  ↓ evaluate guard
  ↓ transition to next state based on structured response
```

The ambiguity lived for a moment inside the AI node and came back resolved into a structured form. The WF never touched the ambiguity directly. This pattern is how WF and AI cooperate cleanly: WF asks, AI answers in a form evaluable by a deterministic guard.

## 5. How a WF is triggered

A WF instance is born when a message arrives at the WF node that matches the WF's declared input schema. The WF never self-triggers. It is always a response to something that happened in the system.

### 5.1 Possible triggers

The following are the canonical sources from which WFs are triggered:

**From an AI node.** The most common case. An AI is conversing with a user, realizes the conversation requires a deterministic process, and delegates to a WF by emitting a message. Example: customer says "I need an invoice", AI understands, AI emits message to WF.invoice with the initial data.

**From an IO node with a webhook or external event.** No reasoning needed — the external event directly maps to a process to run. Example: a payment processor webhook arrives at IO.payments, and OPA routes it directly to WF.payment-reconciliation without passing through any AI.

**From SY.timer.** Periodic or scheduled processes. Example: a recurring cron at end-of-month triggers `TIMER_FIRED` to WF.monthly-close. See section 11 for how timers distinguish "start new instance" from "event for existing instance".

**From another WF.** A WF, as part of its actions, can send a message to another WF. The receiving WF treats it as any other trigger. Example: `WF.tenant-onboarding` in its final step sends a message to `WF.invoice` to create the first billing cycle.

**From SY.cognition.** A cognitive analysis detects a condition that requires a deterministic response. Example: cognition notices a support conversation has been stalled for 48 hours and emits a message to WF.escalation.

### 5.2 What cannot trigger a WF

- **The WF itself** cannot spontaneously create new instances. Instances exist only as responses to incoming messages.
- **Archi** does not trigger WFs at runtime. Archi designs WFs and deploys them as code; once deployed, Archi has no privileged trigger path.
- **External systems directly**. External systems talk to IO nodes, which then emit Fluxbee-native messages that OPA routes.

### 5.3 Deterministic triggers vs probabilistic triggers

Triggers can be deterministic (timers, webhooks) or probabilistic (an AI deciding to delegate). **This is fine, as long as the distinction is explicit.**

The key property is that **once triggered, the WF is deterministic regardless of how the trigger happened**. The AI may be wrong to trigger the WF, but if it does, the WF behaves exactly as defined. If the trigger was a mistake, the WF either completes its process correctly (just on wrong data) or fails deterministically (e.g., the input schema rejects the payload). The damage is bounded because the execution is deterministic.

**The boundary of determinism is the edge of the WF.** Outside, there can be all the probabilistic reasoning you want. Inside, there is none.

## 6. WF as a system toolbox

WF nodes are not private components of specific flows. They are infrastructure — services with public interfaces declared via `WF_HELP`, available to any node in the hive that has authorization via OPA.

### 6.1 The hive-wide live catalog

At any moment, the hive has a catalog of capabilities available. This catalog is the aggregation of all `HELP` responses from all active nodes, including all WF nodes. `SY.admin` provides this catalog on demand — there is no separate registry.

When an AI node wants to know "what workflows are available right now?", it asks `SY.admin`, which fans out `HELP` queries to all WF nodes (or uses a short cache), aggregates, and returns. The AI uses this as its toolbox: it sees what WFs exist, what tasks they offer, and what input schemas they expect. Archi does the same when designing new flows.

This is self-description by pull, not registration by push. Each node is the authoritative source on what it can do. If a new WF is deployed, it becomes visible to the rest of the system the next time someone queries the catalog.

### 6.2 Authorization via claims, not via the catalog

Visibility and authorization are different. The catalog shows what exists; OPA decides who can use what. An AI can see in the catalog that `WF.refund` exists, but whether it is allowed to trigger it depends on OPA rules, which in v3 can consume identity claims (see section 17).

---

# Part II — Model and Runtime

## 7. Node architecture

Each `WF.*` node is an independent Go process that:

- Executes exactly one workflow type per node instance (e.g., `WF.invoice@motherbee` runs the invoice workflow; `WF.onboarding@motherbee` runs a different one).
- Can hold many concurrent instances of its workflow.
- Is monolithic with respect to its own state — it does not share state with other WF nodes, even same-type ones on other hives.
- Persists state in its own SQLite file under the managed node directory.
- Connects to the local router via the standard `fluxbee-go-sdk` lifecycle.
- Uses `cel-go` for guard evaluation.
- Uses `SY.timer` for all workflow timers (see section 11).

```
┌──────────────────────────────────────────────────────────┐
│                 WF.invoice@motherbee                     │
│                                                          │
│  ┌───────────────┐    ┌───────────────────────────────┐  │
│  │  L2 Dispatch  │    │     Instance Manager          │  │
│  │               │───▶│                               │  │
│  │ - recibe msgs │    │  - load instance by id        │  │
│  │ - route to    │    │  - eval transitions           │  │
│  │   instance or │    │  - execute actions            │  │
│  │   new inst    │    │  - persist state              │  │
│  └───────────────┘    │                               │  │
│                       │  per-instance mutex           │  │
│                       └───────────────────────────────┘  │
│                                   │                      │
│                                   ▼                      │
│  ┌────────────────────────────────────────────────────┐  │
│  │             SQLite (wf_instances.db)               │  │
│  │                                                    │  │
│  │  table: wf_definitions   (frozen workflow code)    │  │
│  │  table: wf_instances     (current state per inst)  │  │
│  │  table: wf_instance_log  (FIFO action log)         │  │
│  └────────────────────────────────────────────────────┘  │
│                                                          │
└──────────────────────────────────────────────────────────┘
```

### 7.1 Runtime dependencies

- **`fluxbee-go-sdk`** — router connection, message envelope, identity resolution, timer client, admin command helpers.
- **`cel-go`** — guard expression evaluation.
- **`modernc.org/sqlite` or `mattn/go-sqlite3`** — persistence.
- **SY.timer** — all scheduled events lasting ≥ 60 seconds.
- **Go native `time`, `context`** — internal runtime plumbing timeouts (sub-60s internal to the node, never part of the workflow logic).

### 7.2 File layout

```
/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/
├── config.json       # orchestrator-managed
├── state.json        # optional runtime state
├── secrets.json      # if applicable
├── wf_instances.db   # SQLite, all instance state
└── status_version
```

## 8. Workflow definition format

A workflow is a JSON document with a fixed, validated structure. The format is:

- **Declarative** — describes states and transitions, never imperative flow.
- **Verbose and regular** — every element has the same shape; no shortcuts, no optional abbreviations.
- **Strictly validated at load time** — the runtime compiles guards against the declared input schema, verifies all state references exist, and validates all action types. If anything is wrong, the workflow fails to load with a specific error.
- **Archi-first** — optimized for LLM generation and consumption. Human editability is secondary.
- **Versioned by `wf_schema_version`** — allows forward evolution without breaking existing workflows.

### 8.1 Top-level structure

```json
{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "Issues an invoice, collects missing data if needed, delivers to customer.",
  "input_schema": {
    "type": "object",
    "required": ["customer_id", "items"],
    "properties": {
      "customer_id": { "type": "string" },
      "items": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "sku": { "type": "string" },
            "qty": { "type": "integer" },
            "unit_price": { "type": "number" }
          },
          "required": ["sku", "qty", "unit_price"]
        }
      },
      "currency": { "type": "string" }
    }
  },
  "initial_state": "collecting_data",
  "terminal_states": ["completed", "failed", "cancelled"],
  "states": [
    { ... state definitions ... }
  ]
}
```

Fields:

| Field | Description |
|---|---|
| `wf_schema_version` | Version of the workflow schema format. Current: `"1"`. |
| `workflow_type` | Unique name for this workflow type. Appears in logs and `WF_HELP`. |
| `description` | Human-readable description for introspection. |
| `input_schema` | JSON Schema for the initial event that creates an instance. Compiled to CEL types at load. |
| `initial_state` | Name of the state where new instances begin. |
| `terminal_states` | List of state names that end the instance when reached. |
| `states` | Array of state definitions. |

### 8.2 State definition

```json
{
  "name": "collecting_data",
  "description": "Verifying customer data is complete for invoice emission.",
  "entry_actions": [
    { "type": "send_message", "target": "AI.billing-validator@motherbee", "payload": { ... } }
  ],
  "exit_actions": [],
  "transitions": [
    { ... transition definitions ... }
  ]
}
```

Every state has exactly these fields, even if empty:

| Field | Description |
|---|---|
| `name` | Unique within the workflow. Used for references and logs. |
| `description` | Human-readable. |
| `entry_actions` | Array of actions executed when the state is entered. Empty array if none. |
| `exit_actions` | Array of actions executed when the state is exited. Empty array if none. |
| `transitions` | Array of possible transitions from this state. |

### 8.3 Transition definition

```json
{
  "event_match": {
    "msg": "DATA_VALIDATION_RESPONSE"
  },
  "guard": "event.payload.complete == true",
  "target_state": "creating_invoice",
  "actions": [
    { "type": "set_variable", "name": "validated_at", "value": "now()" }
  ]
}
```

Fields:

| Field | Description |
|---|---|
| `event_match` | Criteria for which incoming events trigger this transition. Matches on `meta.msg` and optionally `meta.type`. |
| `guard` | CEL expression evaluated against `input` (original trigger), `state` (current variables), `event` (incoming event). Must return boolean. |
| `target_state` | Name of the state to transition to. |
| `actions` | Array of actions executed as part of this transition, before entering the target state. |

Within a single state, transitions are evaluated in the order they are declared. The first one whose `event_match` matches and whose `guard` evaluates to `true` is taken. If none match, the event is logged and ignored.

### 8.4 Input schema compilation to CEL

The `input_schema` is compiled at workflow load time into a CEL type environment. Three implicit variables are available in every guard:

- `input` — the original event that created the instance. Typed according to the input schema. Immutable for the lifetime of the instance.
- `state` — the current workflow variables. Typed as a dynamic map; individual field types are assigned as `set_variable` actions run. Guards referencing fields that have not been set yet evaluate to an error (which fails the guard).
- `event` — the incoming event that triggered this evaluation. Typed as `Message` (meta, payload, routing).

A built-in function `now()` returns the current UTC timestamp, obtained via `SY.timer`. This makes guards replayable: for deterministic replay, the runtime can inject a historical timestamp.

### 8.5 Load-time validation

When a WF node loads its workflow definition, it runs these checks:

1. JSON structure valid per `wf_schema_version`.
2. `input_schema` is valid JSON Schema.
3. `initial_state` exists in `states`.
4. All `terminal_states` exist in `states`.
5. Every `target_state` in every transition exists in `states`.
6. Every guard CEL expression compiles successfully against the typed environment.
7. Every action has a known `type` and valid parameters.
8. For `send_message` actions, the `target` is syntactically a valid L2 name.
9. For `schedule_timer` actions, the duration is ≥ 60 seconds (see section 11).
10. Every `set_variable` uses a valid variable name.

Any failure returns a load error with a specific path to the offending element. The node does not accept events until the workflow loads successfully.

## 9. States, transitions, and guards

### 9.1 State semantics

A state is a named stable point in the workflow. An instance is always in exactly one state. Multiple instances of the same workflow can be in different states simultaneously; they do not interact.

Entry actions run when the instance enters the state (including from `initial_state` on creation). Exit actions run when leaving the state. Actions on a transition run after the source state's exit actions and before the target state's entry actions:

```
[source state: exit_actions]
  → [transition: actions]
    → [target state: entry_actions]
```

If an action fails (e.g., a `send_message` whose target is unreachable), the failure is logged but does not halt the transition — the workflow continues. This is deliberate: WF is not responsible for retry semantics of external systems.

### 9.2 Transition evaluation

When an event arrives at an instance (resolved via `instance_id` in the event, or created as a new instance for initial trigger events), the runtime:

1. Acquires the per-instance mutex.
2. Loads the current state.
3. Iterates transitions of the current state in declaration order.
4. For each, evaluates `event_match` against the event's metadata.
5. If matched, evaluates `guard` in the CEL environment.
6. If guard evaluates to `true`, this transition is taken. Stop evaluation.
7. If no transition matches, logs the event as "unhandled" and ignores it.

Evaluation is strictly synchronous per instance. Concurrent events for the same instance are serialized by the mutex.

### 9.3 Guard timeout

Each guard evaluation is bounded to **10 milliseconds** of CEL execution time. If a guard takes longer (typically indicates an infinite loop or malformed expression), it is treated as a failed guard (returns false for that transition).

This is a hard limit to prevent malicious or buggy guards from stalling the runtime. In practice, well-formed CEL guards execute in microseconds.

### 9.4 State variables

Variables in `state` are set by `set_variable` actions. They persist for the lifetime of the instance (until terminal state). There is no separate "context" object — `state` is the context.

Variables have no declared types. They are whatever was last assigned. Guards referencing untyped or missing variables cause the guard to fail silently (returns false), which is the same as the transition not applying.

Variables are not globally visible across instances. Each instance has its own `state`.

## 10. Actions (the primitive vocabulary)

Actions are the side effects available to a workflow. The set is **fixed and small in v1**. A workflow cannot define custom actions. New action types require a runtime release.

### 10.1 `send_message`

Emits an L2 unicast message to a target.

```json
{
  "type": "send_message",
  "target": "IO.quickbooks@motherbee",
  "meta": {
    "msg": "INVOICE_CREATE_REQUEST"
  },
  "payload": {
    "customer_id": "state.customer_id",
    "items": "state.items",
    "currency": "state.currency"
  }
}
```

Fields:

| Field | Description |
|---|---|
| `target` | L2 name of the target node. Validated syntactically at load. |
| `meta.msg` | The `meta.msg` field of the outgoing message. |
| `meta.type` | Defaults to `"system"`. Can be overridden to `"user"` for user-facing messages. |
| `payload` | JSON object. String values in the form `"state.foo"` or `"input.foo"` or `"event.payload.foo"` are substituted at action execution time. |

Substitution happens via a simple path expression: `state.customer_id` reads the `customer_id` variable from the instance state at the moment of execution. This is not CEL; it is a literal path lookup. If the path does not resolve, the field is omitted from the outgoing payload.

The action emits the message and moves on. It does not wait for a response. Responses arrive as separate incoming events, processed by normal transition evaluation.

### 10.2 `schedule_timer`

Programs a timer via `SY.timer`. See section 11 for full semantics.

```json
{
  "type": "schedule_timer",
  "timer_key": "invoice_data_timeout",
  "fire_in": "30m",
  "missed_policy": "fire"
}
```

Fields:

| Field | Description |
|---|---|
| `timer_key` | A stable identifier for this timer within the instance, used to cancel or reschedule it later. Not a timer UUID — the runtime maps it to one. |
| `fire_in` | Duration string (`"30m"`, `"2h"`, `"1d"`). Minimum 60s. |
| `fire_at` | Alternative to `fire_in`. ISO 8601 absolute UTC timestamp. |
| `missed_policy` | Forwarded to SY.timer. `"fire"`, `"drop"`, or `"fire_if_within"` with `missed_within_ms`. |

The timer is registered with `target = this WF node` and `payload = { instance_id: ..., timer_key: ... }`. When it fires, the runtime routes the `TIMER_FIRED` event to the correct instance.

### 10.3 `cancel_timer`

Cancels a timer previously scheduled by this instance.

```json
{
  "type": "cancel_timer",
  "timer_key": "invoice_data_timeout"
}
```

Looks up the timer UUID by `(instance_id, timer_key)` and calls `TIMER_CANCEL` on SY.timer. If the timer has already fired or was never scheduled, the cancel is a no-op.

### 10.4 `reschedule_timer`

Changes the fire time of an existing scheduled timer.

```json
{
  "type": "reschedule_timer",
  "timer_key": "invoice_data_timeout",
  "fire_in": "30m"
}
```

Useful for SLAs that reset when the customer responds (the heartbeat pattern). If the timer has already fired, returns no-op.

### 10.5 `set_variable`

Modifies a variable in the instance `state`.

```json
{
  "type": "set_variable",
  "name": "validated_at",
  "value": "now()"
}
```

The `value` is a CEL expression evaluated in the same environment as guards. Common forms:

- Literal: `"pending"`, `42`, `true`
- Reference: `"event.payload.invoice_id"`, `"input.customer_id"`
- Expression: `"state.retry_count + 1"`, `"now()"`

The result replaces the variable. If the variable did not exist, it is created. There is no type checking across assignments.

### 10.6 What is NOT in v1

The following action types are intentionally excluded from v1 and reserved for future versions:

- `emit_internal_event` — generate a synthetic event to process in the same instance
- `call_capability` — higher-level wrapper for request/response with a node
- `compensate` — sagas and rollback
- `spawn_subworkflow` — child workflows
- `http_request` — direct HTTP calls
- `fetch_data` — data retrieval from external sources

Anything these would enable, v1 expresses through combinations of `send_message` and transitions on the response events. This is more verbose but keeps the runtime small and the semantics pure.

## 11. Timers in WF

### 11.1 Two kinds of time

WF has two distinct kinds of time, handled differently:

**Workflow time** — timers that are part of the workflow logic. "Wait 30 minutes for customer response". "Escalate in 2 hours". "Close at end of month". These are always handled via `SY.timer` and go through the `schedule_timer` / `cancel_timer` / `reschedule_timer` actions. Minimum duration: 60 seconds, enforced by SY.timer.

**Runtime time** — timers internal to the node's own implementation. Socket read timeouts, debounce delays, internal task timeouts. These are handled with Go native primitives (`time.After`, `context.WithTimeout`, `time.Ticker`) and never touch SY.timer. They are invisible to workflow authors and do not participate in the workflow logic.

The rule is clean: if a timer is visible in the workflow definition, it goes through SY.timer. If a timer exists only inside the runtime's own implementation for its own housekeeping, it uses Go native primitives.

### 11.2 How TIMER_FIRED is routed

When `schedule_timer` fires a timer via SY.timer, the runtime passes a payload containing the `instance_id`:

```json
{
  "target_l2_name": "WF.invoice@motherbee",
  "payload": {
    "instance_id": "wfi:550e...",
    "timer_key": "invoice_data_timeout",
    "reason": "customer did not respond within 30 minutes"
  }
}
```

When SY.timer fires, the `TIMER_FIRED` event arrives at the WF node with this payload inside `user_payload`. The WF runtime inspects `user_payload.instance_id`:

- **If present and an instance exists** — deliver the event to that instance. The instance processes it as a normal event in its transition evaluation. Transitions can match on `meta.msg == "TIMER_FIRED"` and guard on `event.user_payload.timer_key`.

- **If present but the instance is already completed/cancelled** — the timer is orphaned. Log and discard.

- **If not present** — treat the event as a potential new trigger. If the payload also matches the workflow's `input_schema`, create a new instance with this event as the initial trigger. This supports the "recurring cron triggers new instance" pattern, where a timer on SY.timer has `payload = { task: ..., ...initial data }` and the WF node treats its arrival as a start.

### 11.3 Timer cleanup on instance termination

When an instance enters a terminal state, the runtime automatically iterates its registered timers and calls `TIMER_CANCEL` on each one, to avoid orphaned timer fires. This is part of the built-in cleanup and does not require a workflow to do it explicitly.

## 12. Soft abort

WF v1 supports soft abort of an instance. A soft abort means: "stop processing new transitions for this instance, mark it cancelled, but do not reverse any side effects that have already happened".

### 12.1 Triggering abort

An authorized node (determined by OPA policy — typically the AI that originally triggered the WF, or `SY.admin`) sends a system message to the WF:

```json
{
  "meta": { "type": "system", "msg": "WF_CANCEL_INSTANCE" },
  "payload": {
    "instance_id": "wfi:550e...",
    "reason": "user changed their mind"
  }
}
```

### 12.2 Semantics

On receiving `WF_CANCEL_INSTANCE`:

1. The runtime acquires the instance mutex.
2. If the instance is already in a terminal state, responds with `INSTANCE_ALREADY_TERMINATED`.
3. Otherwise, marks the instance as `cancelling`, logs the cancellation reason.
4. On the next event that would trigger a transition for this instance, instead of evaluating the transition, the runtime transitions to the terminal state `cancelled` directly.
5. Entry actions of `cancelled` run as normal (typical: a cleanup `send_message` to notify the originator).
6. All timers for this instance are cancelled as part of cleanup.

### 12.3 What is NOT undone

- Messages already sent by previous transitions are not retracted.
- External side effects (invoice already created, customer already notified) are not reversed.
- Previous state transitions are not rolled back.

**WF v1 does not do compensation or rollback.** If you need to undo an invoice, that is a separate business process (perhaps another workflow: `WF.invoice-cancellation`) and it must be designed explicitly.

### 12.4 Races

If `WF_CANCEL_INSTANCE` arrives while the instance is in the middle of a transition (the mutex is held by the evaluator), the cancel waits on the mutex and applies once the current transition completes. The next event after that will see the `cancelling` flag and terminate.

## 13. Persistence

### 13.1 What is stored

Per WF node, a single SQLite file `wf_instances.db` contains three tables:

**`wf_definitions`** — frozen workflow definitions.

When a workflow is loaded (at node startup or via a hot-reload mechanism, v1 only at startup), the definition JSON is stored here with a hash. Each instance references the definition by hash, so instances continue using the workflow version they were born with even if the definition changes later.

```sql
CREATE TABLE wf_definitions (
    definition_hash TEXT PRIMARY KEY,
    workflow_type TEXT NOT NULL,
    schema_version TEXT NOT NULL,
    definition_json TEXT NOT NULL,
    loaded_at INTEGER NOT NULL
);
```

**`wf_instances`** — current state of each instance.

```sql
CREATE TABLE wf_instances (
    instance_id TEXT PRIMARY KEY,
    definition_hash TEXT NOT NULL REFERENCES wf_definitions(definition_hash),
    current_state TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running', 'cancelling', 'completed', 'cancelled', 'failed')),
    input_json TEXT NOT NULL,
    state_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    terminated_at INTEGER,
    triggered_by TEXT NOT NULL
);

CREATE INDEX idx_instances_status ON wf_instances(status);
CREATE INDEX idx_instances_created ON wf_instances(created_at);
```

`state_json` is a serialized JSON object of all workflow variables. `input_json` is the original trigger event's payload (frozen for replay purposes).

**`wf_instance_log`** — flat FIFO log of actions executed per instance.

```sql
CREATE TABLE wf_instance_log (
    log_id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    executed_at INTEGER NOT NULL,
    action_type TEXT NOT NULL,
    action_summary TEXT NOT NULL,
    from_state TEXT,
    to_state TEXT,
    ok INTEGER NOT NULL
);

CREATE INDEX idx_log_instance ON wf_instance_log(instance_id, log_id DESC);
```

Each log entry is one action execution: type (`send_message`, `set_variable`, etc.), a short text summary, the state transition context, and whether it succeeded.

Cap per instance: 100 entries, FIFO. Older entries are pruned when adding new ones. When the instance terminates, its log is preserved until the instance itself is GC'd (retention default 7 days, configurable).

### 13.2 What is NOT stored

- Full transition history with CEL evaluation details — too verbose, not needed for the documented introspection use cases.
- Intermediate snapshots of `state` per transition — only the current `state_json` is kept.
- Raw incoming messages — only the derivative log entries.
- Outgoing messages beyond their log entry — the message itself is not saved.

If deep debugging is needed during development, the debug log at the process level (journal) has more detail. The persisted data is the minimum for operational introspection and replay.

### 13.3 Replay semantics

WF v1 supports replay in a limited sense: given the `input_json` and the sequence of events that arrived (if external events are also persisted somewhere — currently they are not at the WF level), a replay would reproduce the state.

In v1, true event-sourced replay is **not** implemented — only the current state is kept. This is sufficient for "look at an instance and see what it did", which is the documented use case. True replay for debugging is deferred to v2, where an event log table may be added.

### 13.4 Restart behavior

When a WF node restarts:

1. Opens `wf_instances.db`.
2. Loads all `wf_definitions` into memory.
3. Loads all instances with `status IN ('running', 'cancelling')` into memory.
4. For each instance, checks its registered timers in SY.timer (via `TIMER_LIST` filtered by owner) and reconciles: expired timers that are fire-policy get processed now, valid timers stay valid, cancelled timers are ignored.
5. Enters the receive loop.

Completed / cancelled / failed instances remain in the database for read-only introspection but are not loaded into memory.

## 14. Runtime lifecycle

### 14.1 Node startup

1. Connect to local router via `fluxbee-go-sdk`.
2. Register identity via `ILK_REGISTER` (orchestrator-mediated at spawn time).
3. Load workflow definition from config (the workflow definition JSON for this node type).
4. Compile against CEL environment. Fail startup if validation fails.
5. Open `wf_instances.db`, restore running instances.
6. Start receive loop.
7. Start periodic GC task (sweeps terminated instances older than retention).

### 14.2 Event processing loop

```
loop {
    msg := router.receive()
    handler := dispatch(msg.meta.msg)
    handler(msg)
}
```

Handlers:

- `NODE_STATUS_GET` — standard, responds with health.
- `WF_HELP` — returns the workflow HELP descriptor.
- `WF_CANCEL_INSTANCE` — initiates soft abort.
- `WF_GET_INSTANCE` — returns instance info (state, status, variables, recent log).
- `WF_LIST_INSTANCES` — returns instances matching filter.
- `WF_GET_CODE` — returns the current workflow definition.
- `TIMER_FIRED` — routes to instance by `user_payload.instance_id`, or treats as new trigger.
- Any other `meta.msg` — attempts to match against an existing instance by `meta.thread_id` or similar correlation, or if it matches the `input_schema`, creates a new instance.

### 14.3 Instance creation

When an incoming event looks like a trigger (no `instance_id` correlation and payload matches the `input_schema`):

1. Validate payload against `input_schema`.
2. If validation fails, respond with `INVALID_INPUT` error.
3. Generate a new `instance_id` (`wfi:<uuid>`).
4. Create instance row with `current_state = initial_state`, `status = "running"`, `state_json = {}`, `input_json = <payload>`.
5. Acquire the per-instance mutex.
6. Run entry actions of the initial state.
7. Persist.
8. Release mutex.

## 15. WF_HELP and self-description

Every WF node exposes `WF_HELP` as part of the platform's self-description protocol.

```json
{
  "meta": { "type": "system", "msg": "WF_HELP" },
  "payload": {}
}
```

Response:

```json
{
  "meta": { "type": "system", "msg": "WF_HELP_RESPONSE" },
  "payload": {
    "ok": true,
    "node_family": "WF",
    "node_kind": "WF.invoice",
    "workflow_type": "invoice",
    "description": "Issues an invoice, collects missing data, delivers to customer.",
    "version": "1.0",
    "input_schema": { ... full JSON schema ... },
    "trigger_pattern": {
      "meta.msg": "INVOICE_REQUEST",
      "required_claims": [
        { "facet": "function", "key": "can_invoice" }
      ]
    },
    "states": [
      {
        "name": "collecting_data",
        "description": "..."
      },
      ...
    ],
    "terminal_states": ["completed", "failed", "cancelled"],
    "operations": [
      {
        "verb": "WF_CANCEL_INSTANCE",
        "description": "Request soft cancellation of a running instance."
      },
      {
        "verb": "WF_GET_INSTANCE",
        "description": "Get current state and recent actions of an instance."
      },
      {
        "verb": "WF_LIST_INSTANCES",
        "description": "List instances, optionally filtered by status."
      },
      {
        "verb": "WF_GET_CODE",
        "description": "Get the workflow definition this node executes."
      }
    ]
  }
}
```

Archi consumes this to understand what WFs are available, what they need, what claims authorize triggering them, and how to interact with them.

---

# Part III — Operational Surface

## 16. SY.admin read-only surface

Consistent with the pattern established by SY.timer, `SY.admin` exposes read-only operations for WF nodes. Mutations always go through direct SDK calls from authorized nodes.

| Admin action | Maps to | Purpose |
|---|---|---|
| `wf_help` | `WF_HELP` | Get workflow contract |
| `wf_list_instances` | `WF_LIST_INSTANCES` | List instances with filters |
| `wf_get_instance` | `WF_GET_INSTANCE` | Get one instance full detail |
| `wf_get_code` | `WF_GET_CODE` | Get workflow definition code |

No admin-initiated mutations in v1. Specifically, there is **no** `wf_cancel_instance` in admin — cancellation goes through the SDK from authorized nodes, and is subject to OPA policy.

### 16.1 Instance listing and retrieval

`WF_LIST_INSTANCES` accepts filters:

```json
{
  "meta": { "type": "system", "msg": "WF_LIST_INSTANCES" },
  "payload": {
    "status_filter": "running",
    "limit": 100,
    "created_after_ms": 1775000000000
  }
}
```

Returns compact rows:

```json
{
  "payload": {
    "ok": true,
    "count": 3,
    "instances": [
      {
        "instance_id": "wfi:...",
        "status": "running",
        "current_state": "awaiting_invoice_creation",
        "created_at_ms": 1775577000000,
        "updated_at_ms": 1775578000000,
        "triggered_by": "ilk:ai-billing-..."
      }
    ]
  }
}
```

`WF_GET_INSTANCE` returns full detail:

```json
{
  "payload": {
    "ok": true,
    "instance": {
      "instance_id": "wfi:...",
      "status": "running",
      "current_state": "awaiting_invoice_creation",
      "input": { ... original trigger payload ... },
      "state_variables": { ... current variables ... },
      "recent_log": [
        {
          "executed_at_ms": 1775577100000,
          "action_type": "send_message",
          "action_summary": "send_message to IO.quickbooks",
          "from_state": "collecting_data",
          "to_state": "awaiting_invoice_creation",
          "ok": true
        }
      ]
    }
  }
}
```

Read-only. No ability to modify state or force transitions.

## 17. Authorization (via OPA and claims)

### 17.1 The model

Authorization for triggering a WF lives entirely in OPA. The WF node itself does not authorize — it trusts the router, which trusts OPA, which reads identity claims.

When a message arrives at the router destined for a WF node, OPA evaluates a policy that checks:

- The source node type (IO, AI, SY, etc.) — via L2 name.
- The source ILK's claims — via `data.identity[src_ilk].claims_index`.
- Any business-specific rules (tenant, time of day, etc.).

### 17.2 Example policy

Authorize `WF.invoice` triggers based on canonical `function/can_invoice` claim:

```rego
allow {
    input.routing.dst_type == "WF.invoice"
    input.meta.msg == "INVOICE_REQUEST"
    has_canonical_claim(input.meta.src_ilk, "function", "can_invoice")
}

has_canonical_claim(ilk, facet, key) {
    c := data.identity[ilk].claims_index[sprintf("%s/%s", [facet, key])]
    c.canonical == true
}
```

### 17.3 Where claims come from

Per identity v3 (see `10-identity-v3.md`):

- `AI.billing` nodes are born with `function/can_invoice` as a default claim (via the node defaults pattern matching `AI.billing`).
- Human ILKs can receive `function/can_invoice` from frontdesk if the registration process determines they are authorized.
- System nodes can have claims declared explicitly.

The WF node sees none of this. It receives a message, processes it, and executes deterministically. All authorization happened upstream.

### 17.4 What the WF does NOT do

- The WF does not check claims itself.
- The WF does not maintain its own authorization rules.
- The WF does not know about OPA at runtime (except transitively through the router).

This separation keeps the WF simple and keeps all authorization logic in one place: OPA policies + identity claims.

## 18. Error codes

| Code | Description |
|---|---|
| `INVALID_INPUT` | Trigger event payload does not match `input_schema`. |
| `WORKFLOW_LOAD_FAILED` | Workflow definition failed load-time validation. Includes path to offending element. |
| `GUARD_COMPILE_FAILED` | A CEL guard expression failed to compile. |
| `GUARD_TIMEOUT` | A guard exceeded the 10ms evaluation limit. |
| `ACTION_TYPE_UNKNOWN` | An action uses an unknown `type`. |
| `INVALID_TARGET_STATE` | A transition references a state that does not exist. |
| `INVALID_TARGET_L2_NAME` | A `send_message` target is not a valid L2 name. |
| `TIMER_DURATION_BELOW_MINIMUM` | A `schedule_timer` action specifies < 60s. |
| `INSTANCE_NOT_FOUND` | Referenced `instance_id` does not exist. |
| `INSTANCE_ALREADY_TERMINATED` | Operation on a terminated instance. |
| `INSTANCE_CANCELLING` | Operation on an instance that is already in cancelling state. |
| `STORAGE_ERROR` | SQLite persistence error. |
| `INTERNAL_ERROR` | Unclassified runtime error. |

---

# Part IV — Canonical Example

## 19. Case study: `WF.invoice`

This is the reference example used throughout the document. It demonstrates all the core properties of a WF in one concrete workflow.

### 19.1 Business description

The task: when a customer (or an AI agent on their behalf) requests an invoice, the workflow must:

1. Validate that the customer data is complete enough to invoice.
2. If incomplete, request missing data and wait for it.
3. Once complete, invoke an external invoicing system (IO.quickbooks) to create the invoice.
4. Wait for confirmation of invoice creation.
5. Deliver the invoice to the customer via their original channel, using an AI node to generate a friendly message.
6. Complete.

Duration: minutes to hours. Deterministic in every decision. Side effects are real and visible externally (invoice exists in QuickBooks, message delivered). Must survive node restarts.

### 19.2 Workflow definition (abridged JSON)

```json
{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "Issues an invoice, collects missing data if needed, delivers to customer.",
  "input_schema": {
    "type": "object",
    "required": ["customer_id", "items", "originator_channel"],
    "properties": {
      "customer_id": { "type": "string" },
      "items": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["sku", "qty", "unit_price"],
          "properties": {
            "sku": { "type": "string" },
            "qty": { "type": "integer" },
            "unit_price": { "type": "number" }
          }
        }
      },
      "currency": { "type": "string" },
      "originator_channel": { "type": "string" },
      "originator_ilk": { "type": "string" }
    }
  },
  "initial_state": "validating_data",
  "terminal_states": ["completed", "failed", "cancelled"],
  "states": [
    {
      "name": "validating_data",
      "description": "Asking AI.billing-validator if the customer data is complete for invoicing.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing-validator@motherbee",
          "meta": { "msg": "VALIDATE_INVOICE_DATA" },
          "payload": {
            "customer_id": "input.customer_id",
            "items": "input.items",
            "currency": "input.currency"
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "validation_timeout",
          "fire_in": "10m",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "validation_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA_RESPONSE" },
          "guard": "event.payload.complete == true",
          "target_state": "creating_invoice",
          "actions": [
            { "type": "set_variable", "name": "validated_at", "value": "now()" }
          ]
        },
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA_RESPONSE" },
          "guard": "event.payload.complete == false",
          "target_state": "requesting_missing_data",
          "actions": [
            { "type": "set_variable", "name": "missing_fields", "value": "event.payload.missing" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'validation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'validator timeout'" }
          ]
        }
      ]
    },

    {
      "name": "requesting_missing_data",
      "description": "Asking the customer for the missing fields, waiting for their response.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing-responder@motherbee",
          "meta": { "msg": "REQUEST_MISSING_INVOICE_DATA" },
          "payload": {
            "originator_channel": "input.originator_channel",
            "originator_ilk": "input.originator_ilk",
            "missing_fields": "state.missing_fields"
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "customer_response_timeout",
          "fire_in": "24h",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "customer_response_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "CUSTOMER_RESPONSE" },
          "guard": "has(event.payload.missing_data)",
          "target_state": "validating_data",
          "actions": [
            { "type": "set_variable", "name": "items", "value": "event.payload.missing_data.items" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'customer_response_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'customer did not respond'" }
          ]
        }
      ]
    },

    {
      "name": "creating_invoice",
      "description": "Invoking IO.quickbooks to create the invoice.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "IO.quickbooks@motherbee",
          "meta": { "msg": "INVOICE_CREATE_REQUEST" },
          "payload": {
            "customer_id": "input.customer_id",
            "items": "input.items",
            "currency": "input.currency"
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "qb_creation_timeout",
          "fire_in": "5m",
          "missed_policy": "drop"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "qb_creation_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "INVOICE_CREATE_RESPONSE" },
          "guard": "event.payload.ok == true",
          "target_state": "delivering_to_customer",
          "actions": [
            { "type": "set_variable", "name": "invoice_id", "value": "event.payload.invoice_id" },
            { "type": "set_variable", "name": "invoice_link", "value": "event.payload.link" }
          ]
        },
        {
          "event_match": { "msg": "INVOICE_CREATE_RESPONSE" },
          "guard": "event.payload.ok == false",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'quickbooks error: ' + event.payload.error" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'qb_creation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'quickbooks timeout'" }
          ]
        }
      ]
    },

    {
      "name": "delivering_to_customer",
      "description": "Asking AI.billing-responder to compose and send a friendly delivery message.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing-responder@motherbee",
          "meta": { "msg": "DELIVER_INVOICE_TO_CUSTOMER" },
          "payload": {
            "originator_channel": "input.originator_channel",
            "originator_ilk": "input.originator_ilk",
            "invoice_id": "state.invoice_id",
            "invoice_link": "state.invoice_link"
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "delivery_timeout",
          "fire_in": "5m",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "delivery_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "DELIVER_INVOICE_RESPONSE" },
          "guard": "event.payload.delivered == true",
          "target_state": "completed",
          "actions": []
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'delivery_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'delivery timeout'" }
          ]
        }
      ]
    },

    {
      "name": "completed",
      "description": "Invoice issued and delivered successfully.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },

    {
      "name": "failed",
      "description": "Terminal state due to failure. Variable 'failure_reason' holds the cause.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },

    {
      "name": "cancelled",
      "description": "Terminal state due to explicit cancellation via WF_CANCEL_INSTANCE.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    }
  ]
}
```

### 19.3 What this example demonstrates

- **All four WF properties present:** multi-step (validation → creation → delivery), memory between steps (state variables accumulating), deterministic (every transition is a guard-based decision, no ambiguity), lives in time (timers of minutes to 24 hours).
- **Clean separation AI/WF/IO:** the WF delegates ambiguous work (validating data sufficiency, composing friendly messages) to AI nodes, and the structured work (creating the invoice) to an IO node. The WF itself does no reasoning.
- **Timer usage:** SLA-style timeouts at every wait state, cancelled on exit. This is the standard pattern.
- **Failure handling:** every wait state has a timer-driven path to `failed`, with the reason stored as a variable for post-mortem.
- **No recovery or rollback:** if the invoice creation fails, the workflow goes to `failed`. It does not try to roll back whatever happened. Recovery is a business decision made outside this workflow.

### 19.4 Authorization in OPA for this example

```rego
allow {
    input.routing.dst == "WF.invoice@motherbee"
    input.meta.msg == "INVOICE_REQUEST"
    has_canonical_claim(input.meta.src_ilk, "function", "can_invoice")
}
```

With this rule, only ILKs that have the `function/can_invoice` canonical claim can trigger this workflow. Per identity v3 defaults, `AI.billing.*` nodes inherit this claim automatically, so AI agents in the billing family can initiate invoicing flows.

### 19.5 What Archi sees when designing new workflows

When Archi wants to design a new workflow that needs to trigger `WF.invoice` as part of its flow, it asks `SY.admin` for the hive catalog, gets the `WF_HELP` of `WF.invoice`, sees the input schema, sees that `function/can_invoice` is required, and can generate a compliant `send_message` action with the right payload and know that the source node has the claim (because the source node was also designed by Archi with the right defaults).

This is the Archi-first loop in action: self-describing nodes, declarative authorization via claims, deterministic execution.

---

# Part V — Reference

## 20. Decisions and scope

### 20.1 Design decisions

| Decision | Reason |
|---|---|
| WF is a pure executor, not a router or a reasoner | Mirrors OPA model; keeps determinism airtight |
| State machine declarative JSON, not code | Archi can generate and edit as data |
| CEL for guards | Deterministic, typed, sandboxed, 10ms ceiling |
| `cel-go` as implementation | Canonical, production-grade, well-maintained |
| Fixed action vocabulary (`send_message`, timers, `set_variable`) | Simplicity and safety |
| Workflow timers via SY.timer | No duplicated timer infrastructure; minimum 60s enforced |
| Runtime timers via Go native | Sub-60s plumbing stays invisible to workflow authors |
| SQLite per node, monolithic | Same pattern as SY.timer, consistent with Fluxbee |
| Soft abort only in v1 | Rollback and compensation are complex, deferred |
| No override from admin (read-only) | If a WF is stuck, fix the code and redeploy; never patch runtime state |
| Claims-based authorization via OPA | Centralized policy, identity v3 foundation |
| Archi-first format | LLM consumers dominate the author base; humans are observers |
| Each WF node runs exactly one workflow type | Monolithic instances, clean contract, easier to reason about |
| Frozen workflow definition per instance | Instances outlive workflow updates; new version for new instances only |

### 20.2 What is explicitly NOT in v1

This list is intentional. Each item was considered and deferred:

- **Subworkflows.** A WF cannot spawn a child workflow. If coordination between workflows is needed, they communicate via messages as any other nodes.
- **Rollback / compensation / sagas.** No automatic undo. Failure is a terminal state, nothing is reversed.
- **Explicit parallel branches.** A transition can emit multiple actions in parallel (e.g., multiple `send_message` actions from one state), but the workflow cannot fork into multiple state tracks simultaneously. If parallelism is needed, it lives outside: the WF sends multiple messages and waits for multiple responses, merging in a subsequent state.
- **Loops over collections.** No `for_each` action. If a workflow needs to iterate over items, it models the iteration as states.
- **Hot reload of workflow definitions.** v1 only loads definitions at startup. Changing the definition requires restarting the node. New instances will use the new version; old instances complete with the version they were born with.
- **Event sourcing with full replay.** Only current state is persisted. Future v2 may add an event log.
- **Cross-instance queries.** "Give me all instances waiting on customer X" is not supported directly. Can be emulated by indexing state variables, but v1 does not provide this.
- **Direct admin mutations.** No `wf_cancel_instance` or similar in admin. Cancellation goes through the SDK from an authorized node.
- **Scheduled starts.** A workflow is always triggered by a message. If a time-based start is needed, it is a timer on SY.timer whose target is the WF node.

## 21. v1 limitations (explicit)

Workflow authors should be aware of these hard limits:

- **Max 32 states per workflow.** Beyond this, consider breaking into multiple workflows.
- **Max 16 transitions per state.** Same reasoning.
- **Max 100 actions in one transition's action list.** Unusual to approach.
- **Guard evaluation limit: 10ms CEL execution.**
- **Max 64 variables in instance state.** Keep state focused.
- **Max 1000 concurrent running instances per WF node.** Beyond this, scale horizontally with more nodes.
- **Workflow definition size: 512KB JSON.** Should be very difficult to exceed for reasonable workflows.
- **Log retention: 7 days for terminated instances (default, configurable).**
- **Minimum timer duration: 60 seconds** (inherited from SY.timer).

These are not arbitrary — they reflect what a sane workflow should stay within. Hitting them is a signal to reconsider the design.

## 22. References

| Topic | Document |
|---|---|
| Identity v3 and claims | `10-identity-v3.md` |
| System timer | `sy-timer.md` |
| Architecture | `01-arquitectura.md` |
| Protocol | `02-protocolo.md` |
| Routing / OPA | `04-routing.md` |
| Control plane (CONFIG_GET/SET) | `node-config-control-plane-spec.md` |
| Conceptual model for claims | `fluxbee_identity_claims_normative_model.md` |

---

## Appendix: Glossary

| Term | Meaning |
|---|---|
| **WF** | Workflow node family. Deterministic multi-step process executor. |
| **Instance** | A single running execution of a workflow, identified by `instance_id`. |
| **Workflow definition** | The JSON document describing states, transitions, actions. Static and frozen per instance. |
| **State** | A named stable point within a workflow where an instance waits for events. |
| **Transition** | A rule for moving from one state to another when a specific event arrives and a guard passes. |
| **Guard** | A CEL expression that decides whether a transition applies. |
| **Action** | A side effect executed by the runtime as part of a transition or state entry/exit. |
| **Trigger** | An incoming message that starts a new instance. |
| **Event** | Any incoming message processed by an existing instance. |
| **`state_json`** | The serialized current variables of an instance. |
| **Soft abort** | Cancellation that stops future processing but does not reverse past side effects. |
| **Terminal state** | A state declared as ending the instance. No transitions out. |
| **Trigger pattern** | The combination of `meta.msg` and required claims that cause a message to create a new instance. |
