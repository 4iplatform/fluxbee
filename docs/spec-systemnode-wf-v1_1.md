# Fluxbee — WF (Workflow Nodes) v1

**Status:** v1.0 draft
**Date:** 2026-04-13
**Audience:** AI designers (Archi), workflow authors, runtime developers, ops/SRE
**Identity dependency:** builds on `SY.identity v2` (`10-identity-v2.md`). A future iteration of WF may adopt the claims model from `10-identity-v3.md` (currently archived as conceptual draft) when that identity evolution matures.

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
17. Authorization (via OPA and identity v2)
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
  ↓ send_message to AI.billing-validator: a text with the customer data, and a
    meta.context.response_envelope naming the fields of the answer
  ↓ (wait for the answer: the request's own msg, on the instance's thread)
  ↓ receive event: a text holding {complete: true} or {complete: false, missing: [...]}
  ↓ evaluate guard: json_parse(event.payload.content).complete == true
  ↓ transition to next state based on structured response
```

The ambiguity lived for a moment inside the AI node and came back resolved into a structured form. The WF never touched the ambiguity directly. This pattern is how WF and AI cooperate cleanly: WF asks, AI answers in a form evaluable by a deterministic guard.

## 5. How a WF is triggered

A WF instance is born when a message arrives at the WF node from a node its definition's `start_from` names, on a thread no running instance holds, and its payload matches the WF's declared input schema (§14.5). The WF never self-triggers. It is always a response to something that happened in the system.

An instance is a conversation, since 0.1.91: it lives on the thread of the message that started it (§14.3), and every message it sends carries that thread, so the agents it talks to keep one memory per conversation and their answers come back to it. A person who writes again on that thread reaches the running instance; after it ended, a new one starts, at the initial state.

### 5.1 Possible triggers

Only a node the definition names in `start_from` starts an instance (§8.1, §14.5). OPA decides, besides, which nodes may send the WF anything (§17); it does not decide alone. The canonical sources:

**From an IO node.** The most common case: a person's message, or an external event, arrives at an IO node, which hands it to the WF. A WF at an IO node's exit holds the person's conversation, one instance per conversation, and hands it among agents (`docs/wf-orchestration-v1.md` §5.2). Example: a payment processor webhook arrives at IO.payments, which sends it to WF.payment-reconciliation without passing through any AI.

**From another WF.** A WF, as part of its actions, can send a message to another WF, which takes it as any other trigger when it names the sender in `start_from`. Example: `WF.tenant-onboarding` in its final step sends a message to `WF.invoice` to create the first billing cycle.

**Never from an agent.** An agent only answers (`docs/wf-orchestration-v1.md` §1): it never starts an instance, and `start_from` refuses an `AI.*` node, whose late answer would otherwise start one (a WF↔AI loop). A WF asks agents, and branches on their answers.

**Never from a system node.** `SY.cognition`, `SY.admin` or any other `SY.*` or `RT.*` node never starts an instance: `start_from` refuses them.

**From SY.timer: not built.** A `TIMER_FIRED` reaches only the running instance whose timer it is, and never starts one (§11.2); a periodic or scheduled start (an end-of-month close, say) is not built (`docs/wf-orchestration-v1.md` §4.4).

### 5.2 What cannot trigger a WF

- **The WF itself** cannot spontaneously create new instances. Instances exist only as responses to incoming messages.
- **A node outside `start_from`**, an agent and a system node included. Its message reaches a running instance on its thread when the definition names it (§14.5), and starts none.
- **Archi** does not trigger WFs at runtime. Archi designs WFs and deploys them as code; once deployed, Archi has no privileged trigger path.
- **External systems directly**. External systems talk to IO nodes, which then emit Fluxbee-native messages that OPA routes.

### 5.3 Deterministic triggers vs probabilistic triggers

Triggers can be deterministic (a webhook, another WF) or human (a person's message, through an IO node, whatever it says). **This is fine, as long as the distinction is explicit.**

The key property is that **once triggered, the WF is deterministic regardless of how the trigger happened**. The trigger may be a mistake, but if it comes, the WF behaves exactly as defined. If the trigger was a mistake, the WF either completes its process correctly (just on wrong data) or fails deterministically (e.g., the input schema rejects the payload). The damage is bounded because the execution is deterministic.

**The boundary of determinism is the edge of the WF.** Outside, there can be all the probabilistic reasoning you want. Inside, there is none.

## 6. WF as a system toolbox

WF nodes are not private components of specific flows. They are infrastructure: any node its definition names in `start_from`, whose message OPA lets through, can start an instance. Their self-description, `WF_HELP`, is not open to every node: like every `WF_*` control verb, a WF answers it only to `SY.admin@motherbee` and `SY.wf-rules` of its hive (since 0.1.90, §15).

### 6.1 The hive-wide live catalog

At any moment, the hive has a catalog of capabilities available. This catalog is the aggregation of all `HELP` responses from all active nodes, including all WF nodes. `SY.admin` provides this catalog on demand — there is no separate registry.

When an AI node wants to know "what workflows are available right now?", it asks `SY.admin`, which fans out `HELP` queries to all WF nodes (or uses a short cache), aggregates, and returns. The AI uses this as its toolbox: it sees what WFs exist, what tasks they offer, and what input schemas they expect. Archi does the same when designing new flows. **Not built for WF nodes:** SY.admin has no WF action yet (§16), so neither an AI node nor Archi reads a WF's `WF_HELP` today.

This is self-description by pull, not registration by push. Each node is the authoritative source on what it can do. If a new WF is deployed, it becomes visible to the rest of the system the next time someone queries the catalog.

### 6.2 Authorization via OPA, not via the catalog

Visibility and authorization are different. The catalog shows what exists. Who starts an instance is the definition's `start_from`; OPA decides, besides, which nodes may send the WF anything, with rules that in identity v2 evaluate against `ilk_type`, `registration_status`, and the L2 name of the source node (see section 17). An AI can see in the catalog that `WF.refund` exists, but it never starts it: an agent only answers, and `start_from` names IO nodes and WFs.

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
- **SY.timer v1.1 or later** — all scheduled events lasting ≥ 60 seconds. WF v1 requires SY.timer v1.1, which extends v1.0 with `client_ref`-based lookup for `TIMER_CANCEL`, `TIMER_RESCHEDULE`, and `TIMER_GET`. See section 10.2 for why.
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

A sketch (`...` stands for the states, §8.2; §19.2 is a complete definition):

```
{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "Issues an invoice, collects missing data if needed, delivers to customer.",
  "start_from": ["IO.api.billing@motherbee"],
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
| `start_from` | Required. The node names whose messages start an instance, each with its hive (`IO.api.sales@worker1`): IO nodes and WFs, never an `AI.*` node (an agent only answers, §5.1) nor a `SY.*` or `RT.*` node. A message from any other node starts none (§14.5). |
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
| `event_match` | Which incoming events trigger this transition: by `msg` (`meta.msg`), by `src` (the sender's name as the router stamps it, `event.src`: a node name with its hive, never a `SY.*` or `RT.*` node), or by both, and then both must match; at least one is required. A person's message and an agent's answer to it have no `msg`, so they are matched by their sender: `{"src": "AI.sales@worker1"}`. Every message a WF takes is a `user` message, but a `TIMER_FIRED` (§14.2); `event_match.type` left the language in 0.1.90 and is refused, by its path. |
| `guard` | CEL expression evaluated against `input` (original trigger), `state` (current variables), `event` (incoming event). Must return boolean. |
| `target_state` | Name of the state to transition to. Without it the transition is **internal**: it runs its actions and stays in its state, with no exit or entry actions (§9.1). |
| `actions` | Array of actions executed as part of this transition, before entering the target state. |

Within a single state, transitions are evaluated in the order they are declared. The first one whose `event_match` matches and whose `guard` evaluates to `true` is taken. If none match, the event is logged and ignored.

### 8.4 Input schema compilation to CEL

The `input_schema` validates the payload that creates an instance (JSON Schema); it gives CEL no types. Three implicit variables are available in every guard, each a map of dynamic values:

- `input` — the payload of the event that created the instance, as the `input_schema` accepted it. Immutable for the lifetime of the instance.
- `state` — the current workflow variables. Typed as a dynamic map; individual field types are assigned as `set_variable` actions run. Guards referencing fields that have not been set yet evaluate to an error (which fails the guard).
- `event` — the incoming event that triggered this evaluation: a flat map, not the whole message. Its keys are `msg`, `type`, `thread_id`, `trace_id`, `src` (the sender's L2 name, as the router stamps it), `context` (the parsed `meta.context`) and `payload`; a `TIMER_FIRED` also has `timer_key`, the key the timer was scheduled with (from the timer's `client_ref`, `wf:<instance_id>::<timer_key>`). An internal event (§10.6) has no `src`, and the instance's thread. The same map is the `event` root of `$ref`.

A built-in function `now()` returns the WF node's clock as Unix milliseconds (UTC); it is not SY.timer's. `json_parse(text)` decodes a JSON string. Numbers have one type everywhere, live and after a restart: a JSON integer is a CEL `int` and any other number a `double`, in `input`, `state`, `event` and `json_parse` alike, and in the definition's own literals (`3` goes out as `3`, `1.0` as `1.0`). A comparison or an equality between an `int` and a `double` works when a side comes from `input`, `state`, `event` or `json_parse` (`state.score > 7.5` with an `int` score); between two literals (`1 < 2.0`) it does not compile. Arithmetic that mixes them, and `%` on a `double`, fails (a guard that fails is false, a `set_variable` that fails fails its action): convert first, `double(state.n) * 1.1`, `int(state.n) % 2`.

### 8.5 Load-time validation

When a WF node loads its workflow definition, it runs these checks:

1. JSON structure valid per `wf_schema_version` (`"1"`); an unknown field is refused.
1a. `start_from` names at least one node, each a node name with its hive, never a `SY.*`, `RT.*` or `AI.*` node: an agent only answers, and one that starts instances and that the WF asks would have its late answer start a new instance, which asks it again (a WF↔AI loop).
2. `input_schema` is valid JSON Schema.
3. `initial_state` exists in `states`.
4. All `terminal_states` exist in `states`; a state named `failed` is one of them.
5. Every `target_state` in every transition exists in `states`; a transition without one is internal.
6. Every transition has a guard, and an `event_match` with a `msg`, a `src` or both, a `src` being a node name with its hive, never a `SY.*` or `RT.*` node; and every guard CEL expression compiles.
7. Every action has a known `type` and valid parameters, and only the fields of its type: a field of another type is refused, by its path (a `forward` with a `name` instead of a `message` would forward the event).
8. For `send_message` and `forward` actions, the `target` is syntactically a valid L2 name, without spaces around it, and not a `SY.*` or `RT.*` node. A `forward` has no `meta.msg` and no `payload` (it keeps the message's own), and the `message` it names is one a `keep_message` of the definition keeps.
8a. No action has `meta.type`, `target_ref` or `thread_id_ref`, and no transition has `event_match.type`: they left the language in 0.1.90 and 0.1.91 (every message a WF sends is a `user` message, to a node name written in the definition, on the instance's thread; every message it takes is a `user` message, but a `TIMER_FIRED`), and a definition that still has one is refused, by its path.
8b. A `send_message`'s or a `forward`'s `meta.context` is a JSON object (or a `$ref`) that does not write `wf`, the WF's own block (§10.9), and its `vars` is a JSON object.
8c. A `meta.context.response_envelope` written in the definition (not a `$ref`) follows the rules the agent reads it with (`response_contract_to_output_schema_spec` in the AI SDK): `kind` `json_object_v1`; `required`, a non-empty list of names among the `properties`; `properties`, a non-empty object of properties each of `type` `string`, `boolean`, `integer` or `number`; an `enum`, a list of strings, only on a string; a `text_field`, when there is one, a string listed in `required` whose property is a string. An agent refuses a contract that breaks one, and the instance would wait for an answer that never comes.
8d. On a transition matched on `msg` `TIMER_FIRED`, no `keep_message` and no `forward` without `message`, among its actions, its state's exit actions and its target's entry actions: a `TIMER_FIRED` is no node's message.
9. For `schedule_timer` and `reschedule_timer` actions, exactly one of `fire_in` and `fire_at`, a literal or a `$ref`: a literal duration is ≥ 60 seconds (see section 11), and so is a `$ref`'s default; a `$ref` is checked when the action runs. A duration in days is at most `106751d`, the most a duration holds.
10. Every `set_variable` uses a valid variable name, and its `value` is a CEL expression, written as a JSON string, that compiles; every `keep_message` keeps under a valid name.
11. Every `$ref` that is a dot path (no `#`, `/` or `:`) is rooted at `input.`, `state.` or `event.` (a typo such as `inptu.x` is refused), has no empty segment, holds only `$ref` and `default` (`a $ref holds only $ref and default`), and its `default` is a literal, with no `$ref` inside (§10.1). A `$ref` that is not a string is refused.

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

An **internal transition** (one without a `target_state`) runs its own actions only and stays in its state: no exit or entry actions run, so the state's timers keep running. An event that arrives early is noted without leaving the state (a due date that fires while an agent is answering, say), and the next state acts on it. A transition whose `target_state` is its own state leaves the state and enters it again.

If an action fails, its `on_error` decides: `fail` halts the transition and routes the instance to its terminal `failed` state (running that state's entry actions, best-effort, when the definition declares it); `continue` logs the failure and goes on. The default is `fail` for `send_message`, `forward`, `keep_message`, `set_variable` and `emit_internal_event` (a lost message, a message not kept or an unset variable leaves the instance inconsistent), `continue` for the timer actions. A send to a node that is not connected is not a failure the WF sees: the router's `UNREACHABLE` comes back as a notice, which the WF does not answer.

### 9.2 Transition evaluation

When an event arrives at an instance (resolved by its `meta.thread_id`, the instance's thread, or created as a new instance for a trigger, §14.5), the runtime:

1. Acquires the per-instance mutex.
2. Loads the current state.
3. Iterates transitions of the current state in declaration order.
4. For each, evaluates `event_match` against the event: its `meta.msg` and its router-stamped sender.
5. If matched, evaluates `guard` in the CEL environment.
6. If guard evaluates to `true`, this transition is taken. Stop evaluation.
7. If no transition matches, logs the event as "unhandled" and ignores it.

Evaluation is strictly synchronous per instance. Concurrent events for the same instance are serialized by the mutex.

### 9.3 Guard timeout

Each guard evaluation is bounded to **10 milliseconds**. A guard runs on the node's receive loop and is stopped at its timeout (CEL checks for it at every step of a comprehension); a guard that is stopped, errs or panics is false for that transition. Before 0.1.90 a guard past its timeout went on running in a goroutine of its own, reading the instance's state while the next transition wrote it: a fatal error, which the panic guard does not catch, and, with the event journaled, again at every start (FINDINGS A-118).

This is a hard limit to prevent malicious or buggy guards from stalling the runtime. In practice, well-formed CEL guards execute in microseconds. A `set_variable` value has its own bound (§10.5).

### 9.4 State variables

Variables in `state` are set by `set_variable` actions. They persist for the lifetime of the instance (until terminal state). There is no separate "context" object — `state` is the context.

Variables have no declared types. They are whatever was last assigned. Guards referencing untyped or missing variables cause the guard to fail silently (returns false), which is the same as the transition not applying.

Variables are not globally visible across instances. Each instance has its own `state`.

## 10. Actions (the primitive vocabulary)

Actions are the side effects available to a workflow. The set is **fixed and small in v1**: `send_message`, `forward`, `keep_message`, the timer actions, `set_variable` and `emit_internal_event`. A workflow cannot define custom actions. New action types require a runtime release.

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
    "customer_id": { "$ref": "state.customer_id" },
    "items": { "$ref": "state.items" },
    "currency": { "$ref": "state.currency" },
    "literal_note": "Invoice created by automated workflow"
  }
}
```

Fields:

| Field | Description |
|---|---|
| `target` | L2 name of the target node, written in the definition. Validated at load: a valid L2 name, never a `SY.*` or `RT.*` node. |
| `meta.msg` | The `meta.msg` field of the outgoing message. |
| `meta.context` | Optional. A JSON object, resolved like the payload, and sent as the message's `meta.context` (to ask an agent for a structured answer: `{"response_envelope": {...}}`, which, written in the definition, is checked at load by the agent's rules, §8.5). It never writes `wf`, the WF's own block (§10.9). |
| `vars` | Optional. A JSON object, resolved like the payload, sent in the WF's block as `meta.context.wf.vars`: the deterministic context the WF gives the receiver (§10.9). |
| `payload` | JSON object. See payload substitution rules below. |

Every message a WF sends is a `user` message: there is no `meta.type` (before 0.1.90 it defaulted to `system`, and an agent answered a system message from its control plane, so the instance waited forever). A WF that chooses among destinations writes one transition per destination, each with its guard and its `send_message` to a name written there; no destination is taken from data (`target_ref` left the language in 0.1.90).

Every message an instance sends, a `send_message` or a `forward` (§10.7), carries the instance's thread (§14.3): the thread of the message that started it, the conversation's, or the instance's own id when that message had none. An agent keeps one memory per conversation, and its answer comes back on that thread, to the instance. `thread_id_ref`, which replaced the thread with one taken from data, left the language in 0.1.91, and is refused, by its path.

**Payload substitution rules.**

The payload object is a regular JSON object with one special construct: an object whose `$ref` is a dot path (a string without `#`, `/` or `:`) is a path lookup, not a literal value. Its root must be `input`, `state` or `event`, so a typo such as `inptu.x` is refused at load. It holds only `$ref` and, optionally, `default`: any other key is refused at load (`a $ref holds only $ref and default`), so a typo such as `defualt` is never sent as a literal. Everything else is a literal, an object whose `$ref` holds `#`, `/` or `:` included (a JSON Schema's own `"$ref": "#/definitions/item"` goes out as written).

The `$ref` value is a path expression of the form `<root>.<field>[.<subfield>...]` where `<root>` is one of:

- `input` — the original event that created the instance
- `state` — the current workflow variables
- `event` — the incoming event being processed in the current transition

Examples:

```json
{
  "$ref": "input.customer_id"
}
```
Resolves to the value of `customer_id` from the original trigger event.

```json
{
  "$ref": "state.items"
}
```
Resolves to the array of items stored in the instance state. The resolved value can be of any JSON type — string, number, object, or array. The substitution preserves the type of the source.

```json
{
  "$ref": "event.payload.invoice_id"
}
```
Resolves a nested field from the current event's payload.

If the path does not resolve (a key is absent, or a value on the way is not an object), the action **fails**, and with it, by its default `on_error`, the instance; a JSON `null` at the path is a value, and `{"$ref": path}` gives `null`. For a field that may be missing, give a default: `{"$ref": "event.context", "default": {}}` takes the literal `{}` when the path does not resolve or leads to `null` (as an IO node writes a field it does not have). The default is a literal: a `$ref` inside it is refused at load.

The `$ref` form is **not** CEL. It is a literal path lookup, processed by simple field traversal at action execution time. CEL is used only in guards and in `set_variable` values. The two paths are intentionally separate.

Literal values that happen to look like paths (`"state.foo"` as a literal string) are unambiguous in this scheme: a string is always a literal string. Only the `{"$ref": ...}` object form triggers substitution.

The action emits the message and moves on. It does not wait for a response. Responses arrive as separate incoming events, processed by normal transition evaluation.

### 10.2 `schedule_timer`

Programs a timer via `SY.timer`. See section 11 for full semantics of how timers fit into WF.

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
| `timer_key` | A stable identifier for this timer within the instance. Must be unique within an instance. Used to cancel or reschedule it later. |
| `fire_in` | Duration string (`"30m"`, `"2h"`, `"1d"`), or a `$ref` to one. Minimum 60s. |
| `fire_at` | Alternative to `fire_in`. RFC 3339 absolute UTC timestamp, or a `$ref` to one, 60 s ahead at least. |
| `missed_policy` | Forwarded to SY.timer. `"fire"`, `"drop"`, or `"fire_if_within"` with `missed_within_ms`. |

**A time from state.** `fire_in` and `fire_at` take a `$ref` (with an optional literal `default`, §10.1), resolved when the action runs: the time can come from the instance's state or from an agent's answer, as when an invoice's due date sets the timer that checks its payment (`"fire_in": {"$ref": "state.invoice.due_in"}`). The value must then be a string that parses, and the 60 s floor is checked then; one that does not fails the action, under its `on_error` (a timer action continues by default, so a time an agent gives is worth `"on_error": "fail"`). A literal is checked at load.

**Identification through `client_ref`.**

When the WF runtime sends `TIMER_SCHEDULE` to SY.timer, it constructs a stable `client_ref` of the form:

```
wf:<instance_id>::<timer_key>
```

For example: `wf:wfi:abc-123::invoice_data_timeout`.

This `client_ref` is the canonical identifier of the timer for all subsequent operations from the WF — `cancel_timer` and `reschedule_timer` both look up the timer by `client_ref`, not by the SY.timer-generated UUID. This is important because:

- `client_ref` is **deterministic** at scheduling time — the WF knows it before SY.timer responds.
- Subsequent operations (cancel/reschedule) can be issued **immediately** without waiting for the SY.timer response with the UUID. There is no race window where the timer is scheduled but not yet known to the WF.
- After a crash and restart, the WF can recover its mapping just by knowing the `(instance_id, timer_key)` pair, without consulting any local index of UUIDs.
- If a re-execution after crash schedules the same timer twice, SY.timer detects the duplicate `client_ref` and treats the second call as idempotent (returns the existing timer instead of creating a duplicate).

This dependency on `client_ref` semantics requires `SY.timer v1.1` to support lookup operations (`TIMER_CANCEL`, `TIMER_RESCHEDULE`, `TIMER_GET`) by `client_ref` in addition to `timer_uuid`. WF v1 will not work against SY.timer v1.0 without this extension.

The runtime does not block waiting for the `TIMER_SCHEDULE_RESPONSE`. It logs the schedule action immediately as ok=true and continues with the next action. The response arrives later as a reply on no instance's thread, and is dropped (§14.5): the `client_ref` is sufficient for all subsequent operations. A schedule sent while SY.timer does not answer is not lost: the timer stays in the WF's index, and the next reconciliation schedules it again (§11.4).

### 10.3 `cancel_timer`

Cancels a timer previously scheduled by this instance.

```json
{
  "type": "cancel_timer",
  "timer_key": "invoice_data_timeout"
}
```

The runtime constructs the `client_ref` as `wf:<instance_id>::<timer_key>` and sends `TIMER_CANCEL` to SY.timer using that `client_ref`. SY.timer resolves it to the underlying timer and cancels it. If the timer has already fired, the cancel is a no-op. If no timer with that `client_ref` exists, the cancel is also a no-op (logged as warning, not an error).

### 10.4 `reschedule_timer`

Changes the fire time of an existing scheduled timer.

```json
{
  "type": "reschedule_timer",
  "timer_key": "invoice_data_timeout",
  "fire_in": "30m"
}
```

Looks up by `client_ref` as in `cancel_timer`. Useful for SLAs that reset when the customer responds (the heartbeat pattern). If the timer has already fired, returns no-op. Its `fire_in` or `fire_at` is a literal or a `$ref`, as in `schedule_timer`.

### 10.5 `set_variable`

Modifies a variable in the instance `state`.

```json
{
  "type": "set_variable",
  "name": "validated_at",
  "value": "now()"
}
```

The `value` is a CEL expression, written as a JSON string, evaluated in the same environment as guards. Common forms:

- Literal: `"'pending'"`, `"42"`, `"true"` (a JSON number or boolean, `42` or `true`, is refused at load: it is no CEL expression)
- Reference: `"event.payload.invoice_id"`, `"input.customer_id"`
- Expression: `"state.retry_count + 1"`, `"now()"`, `"'Customer %s'.format([input.customer_id])"`

Its evaluation has a deterministic bound, 1,000,000 CEL cost units: past it the action fails, under its `on_error`.

The result replaces the variable. If the variable did not exist, it is created. There is no type checking across assignments. The state holds plain JSON: a CEL list or a map with string keys is kept as a JSON array or object, a timestamp or a duration as its CEL string; a value JSON cannot hold (bytes, a map with other keys, NaN) fails the action.

### 10.6 `emit_internal_event`

Queues a synthetic event for the **same instance**. This is a local runtime primitive: it does not send a router message, does not require `SY.timer`, and does not create a new instance.

```json
{
  "type": "emit_internal_event",
  "meta": {
    "msg": "INTERNAL_COMPLETE"
  },
  "payload": {
    "result": { "$ref": "state.result" }
  }
}
```

Fields:

| Field | Description |
|---|---|
| `meta.msg` | The `meta.msg` field of the internal synthetic event. Required. |
| `payload` | JSON object, using the same `$ref` substitution rules as `send_message`. |

An internal event's `type` is `user`, as every message a WF makes.

**Not a timer, not a router message.**

`emit_internal_event` is neither workflow time nor runtime housekeeping time. It is an immediate causal step inside the workflow runtime. The event is bound to the current instance at creation time and bypasses normal router correlation entirely.

**Durable queue semantics.**

When `emit_internal_event` executes:

1. The runtime resolves the payload against the current `(input, state, event)`.
2. It materializes an internal event object in memory.
3. When the transition commits, the internal event is persisted into the instance's internal event queue atomically with the new instance state.
4. After that commit succeeds, the runtime drains the queue for that instance FIFO and processes each queued internal event as a normal event against the same transition engine.

This ordering is intentional. The runtime must never process an internal event that was not durably committed, and must never lose a committed internal event across restart. An internal event a crash left queued counts its attempts at each start (§14.4).

**Visibility to guards and actions.**

When the queued internal event is processed, it is presented through the normal `event` object available to guards and `set_variable` expressions:

- `event.msg`
- `event.type`
- `event.payload`
- `event.trace_id`

From the point of view of the workflow definition, an internal event behaves like any other event. The only difference is where it comes from.

**Trace semantics.**

An internal event inherits the `trace_id` of the currently-processing event. If the runtime crashes after committing the internal event but before draining it, recovery resumes with the same `trace_id`.

**Terminal behavior.**

If an instance reaches a terminal state, any queued internal events for that instance are discarded as part of terminal cleanup.

**Bound.**

At most 100 internal events may follow one inbound event (its own transition's and the ones they lead to). One more fails the instance, with the cause `internal_event_cap`, instead of an `emit_internal_event` that emits itself again holding the node's receive loop forever.

### 10.7 `forward`

Sends a message on **as it was received** (since 0.1.91): the event's message, or one kept earlier with `keep_message` (§10.8). A WF that sits at an IO node's exit uses it to hand a person's message to the agent that holds the conversation, and the agent's answer back to the person, without touching the message's content.

```json
{
  "type": "forward",
  "target": "AI.invoice@worker1",
  "message": "customer",
  "meta": {
    "context": {
      "response_envelope": { "kind": "json_object_v1", "strict": true, "text_field": "reply", "required": ["reply"], "properties": { "reply": { "type": "string" } } }
    }
  },
  "vars": { "order": { "$ref": "state.order" } }
}
```

Fields:

| Field | Description |
|---|---|
| `target` | L2 name of the target node, written in the definition, as for `send_message`. |
| `message` | Optional. The name a `keep_message` kept a message under; without it, the event's message. |
| `meta.context` | Optional. The keys the action writes over the message's own `meta.context`, resolved like a payload (`response_envelope`, say). It never writes `wf`. |
| `vars` | Optional. As for `send_message`: `meta.context.wf.vars` (§10.9). |

What goes out:

- **The payload, byte for byte**, never decoded: its text, its attachments, the order of its keys and its numbers as they came (`1.10` stays `1.10`, `<b>` stays `<b>`).
- **The meta is the message's**: every field of it but `memory_package` (its type, `msg`, `src_ilk`, `dst_ilk`, `ich`, `scope`, `priority`, `context` and any other), and the WF sets the thread and the context. The person's ILK travels intact, for the agent's memory and the IO node's delivery.
- **What the WF changes is metadata**: the destination, `target`; the thread, which becomes the instance's (§10.1); the context keys the action writes; and the WF's block, whose trail is the message's own and this node.

A `forward` fails when there is no message to forward: the event is a `TIMER_FIRED`, an internal event or a control message (no node sent it as a workflow message), or no message is kept under `message` yet. Its `on_error` defaults to `fail`, as for `send_message`. It has no `meta.msg` and no `payload`: a definition that writes either is refused at load, and so is a `forward` without `message` on a transition matched on `TIMER_FIRED` (§8.5).

### 10.8 `keep_message`

Keeps the event's message, whole and as received, under a name, for a later `forward` (since 0.1.91):

```json
{ "type": "keep_message", "name": "customer" }
```

- A later `keep_message` under the same name replaces it.
- Kept messages are committed with the transition (§13.1, `wf_kept_messages`), survive a restart, and go when the instance ends.
- It fails when the event is no message a node sent (a `TIMER_FIRED`, an internal event); its `on_error` defaults to `fail`. On a transition matched on `TIMER_FIRED` it is refused at load (§8.5).

The pattern of a conversation: the initial state keeps the person's message (`customer`), and each state that hands the conversation to an agent forwards `customer` to it; a listening state keeps the person's next message under the same name.

### 10.9 The WF's block: `meta.context.wf`

Every message a WF sends, a `send_message` or a `forward`, carries the WF's block in `meta.context.wf` (since 0.1.91): deterministic context for the receiver, which an agent shows its model before the person's text.

```json
{ "wf": { "trail": ["IO.api.sales@worker1", "WF.sales@worker1"], "vars": { "order": { "product": "Pro", "quantity": 10, "unit_price": 100 } } } }
```

- **`trail`** lists the nodes the message went through. A `forward` takes the message's own trail (or its sender's name, when it has none) and adds the WF's name; a `send_message` starts it with the WF's name. An agent's answer never carries the request's `wf` back, so a forwarded answer's trail starts at the agent.
- **`vars`** holds what the action's `vars` writes, `$ref`-resolved; it is left out when the action has none.
- An action's `meta.context` never writes `wf`: validation refuses it, and a `$ref` that brings one is overwritten.

### 10.10 What is NOT in v1

The following action types are intentionally excluded from v1 and reserved for future versions:

- `call_capability` — higher-level wrapper for request/response with a node
- `compensate` — sagas and rollback
- `spawn_subworkflow` — child workflows
- `http_request` — direct HTTP calls
- `fetch_data` — data retrieval from external sources

Anything these would enable, v1 expresses through combinations of `send_message`, `forward` and transitions on the response events. This is more verbose but keeps the runtime small and the semantics pure.

## 11. Timers in WF

### 11.1 Two kinds of time

WF has two distinct kinds of time, handled differently:

**Workflow time** — timers that are part of the workflow logic. "Wait 30 minutes for customer response". "Escalate in 2 hours". "Close at end of month". These are always handled via `SY.timer` and go through the `schedule_timer` / `cancel_timer` / `reschedule_timer` actions. Minimum duration: 60 seconds, enforced by SY.timer.

**Runtime time** — timers internal to the node's own implementation. Socket read timeouts, debounce delays, internal task timeouts. These are handled with Go native primitives (`time.After`, `context.WithTimeout`, `time.Ticker`) and never touch SY.timer. They are invisible to workflow authors and do not participate in the workflow logic.

**Internal events are not time.** `emit_internal_event` does not wait and does not schedule anything. It appends a durable synthetic event to the current instance's internal queue, and the runtime drains that queue immediately after the current transition commits (or at recovery if the node crashed before draining it).

The rule is clean: if a timer is visible in the workflow definition, it goes through SY.timer. If a timer exists only inside the runtime's own implementation for its own housekeeping, it uses Go native primitives.

### 11.2 How TIMER_FIRED is routed

When `schedule_timer` schedules a timer with SY.timer, the runtime gives it the timer's `client_ref` (`wf:<instance_id>::<timer_key>`) and this payload:

```json
{
  "target_l2_name": "WF.invoice@motherbee",
  "payload": {
    "instance_id": "wfi:550e...",
    "timer_key": "invoice_data_timeout"
  }
}
```

When SY.timer fires, the `TIMER_FIRED` event arrives at the WF node with the `client_ref`, the timer's owner (`owner_l2_name`) and this payload inside `payload.user_payload`. The WF takes a `TIMER_FIRED` only from SY.timer of its own hive (the router stamps the sender), and only for a timer it scheduled itself (`owner_l2_name` is its own name): anyone else could fire an instance's timer by its `client_ref`, and SY.timer lets any node schedule a timer whose fire goes to another node, under SY.timer's own name (FINDINGS A-119). A refused fire is dropped, never answered. The runtime finds the instance by the `client_ref` alone (`user_payload` is not read):

- **If an instance exists and runs** — deliver the event to that instance. The instance processes it as a normal event in its transition evaluation. Transitions match on `meta.msg == "TIMER_FIRED"` and guard on `event.timer_key`, the key the engine exposes for a fired timer (`event.payload` stays as SY.timer sent it).

- **Otherwise** (the instance ended, or the fire names none) — the timer is orphaned. Log and discard. A `TIMER_FIRED` never starts an instance; a schedule that starts a workflow is not built (wf-orchestration-v1 §4.4).

When the fire is taken, the timer's index row goes in the same commit that saves the event (§14.4), so a restart does not fire it again.

### 11.3 Timer cleanup on instance termination

When an instance enters a terminal state, the runtime automatically iterates its registered timers and calls `TIMER_CANCEL` on each one (by `client_ref`), to avoid orphaned timer fires. This is part of the built-in cleanup and does not require a workflow to do it explicitly.

### 11.4 Recovery and timer reconciliation after restart

When the WF node restarts, it must reconcile its view of timers with SY.timer's view, because there is a window where SY.timer may have fired timers while the WF was down, or where the WF's local state about scheduled timers may have drifted from reality.

The recovery flow is:

1. The WF loads its persisted running instances, each on the definition it was born with (§13.1).
2. It asks `SY.timer` for its pending timers (`TIMER_LIST` filtered by `owner = this WF node`, at most 1000). If SY.timer does not answer, the node starts anyway, reports itself `DEGRADED` (`timers_not_reconciled`), and asks again every 30 s; the reconciliation below runs when it answers.
3. The WF cross-references SY.timer's timers (by `client_ref`) against its own index of the timers its instances scheduled. It goes through the index in the order the timers fire, and reads each row again before it acts on it: a fire it gives an instance may cancel or move a later timer, which then does not fire (before 0.1.90 a timer an earlier fire had cancelled fired anyway). For a running instance's timer:
   - **Pending in SY.timer, `fire_at` ahead**: nothing to do.
   - **Pending in SY.timer, `fire_at` past**: the WF gives the instance a synthetic `TIMER_FIRED` now, as SY.timer's would go (§14.4), then cancels the pending one in SY.timer so it does not fire again, unless the fire scheduled the same key again.
   - **In the index but not in SY.timer, `fire_at` past**: it fired while the WF was down, and SY.timer dropped the fire. The WF gives the instance a synthetic `TIMER_FIRED`.
   - **In the index but not in SY.timer, `fire_at` ahead**: SY.timer never got it (it was scheduled while SY.timer did not answer) or lost it. The WF schedules it again, under the same `client_ref`, at its `fire_at` (60 s ahead at least), with `missed_policy` `fire` (the index does not keep the policy). Before 0.1.90 the index row went, and an instance waiting on that timer waited forever.
   - **Scheduled after the WF asked SY.timer for its list**: left alone, since the list could not have it.

   A timer of an instance that no longer runs leaves the index, and a cancelling instance's timers are left alone. A timer **in SY.timer but not in the index** is an orphan; the WF cancels it.

A timer's `missed_policy` is forwarded to SY.timer, and SY.timer applies it to its own late fires; the WF's synthetic fires at recovery do not apply it.

This recovery flow gives the WF authority over its own timer state across restarts. The reason is that the WF needs to update its instance state when a timer fires, and only the WF can do that — SY.timer firing a timer in the WF's absence is fire-and-forget, the WF would never see it.

## 12. Soft abort

WF v1 supports soft abort of an instance. A soft abort means: "stop processing new transitions for this instance, mark it cancelled, but do not reverse any side effects that have already happened".

### 12.1 Triggering abort

`SY.admin@motherbee` or `SY.wf-rules` of the WF's hive sends a system message to the WF (the WF answers its control verbs to those two only, since 0.1.90; anyone else gets `UNAUTHORIZED`):

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
3. Otherwise, logs the cancellation reason and forces an immediate transition to the terminal state `cancelled`.
4. The transition order is the normal one: `exit_actions` of the current state run first, then `entry_actions` of `cancelled`.
5. All timers for this instance are cancelled as part of terminal cleanup.
6. The terminal state is persisted before the `WF_CANCEL_INSTANCE_RESPONSE` is sent.

### 12.3 What is NOT undone

- Messages already sent by previous transitions are not retracted.
- External side effects (invoice already created, customer already notified) are not reversed.
- Previous state transitions are not rolled back.

**WF v1 does not do compensation or rollback.** If you need to undo an invoice, that is a separate business process (perhaps another workflow: `WF.invoice-cancellation`) and it must be designed explicitly.

### 12.4 Races

If `WF_CANCEL_INSTANCE` arrives while the instance is in the middle of a transition (the mutex is held by the evaluator), the cancel waits on the mutex and applies once the current transition completes. Once it acquires the mutex, it immediately performs the forced transition to `cancelled`.

## 13. Persistence

### 13.1 What is stored

Per WF node, a single SQLite file `wf_instances.db` (schema version 4, since 0.1.91) contains the tables below. It runs in WAL mode with `synchronous` `FULL`, as SY.timer's store does: a commit is on disk when it returns, so a power loss does not take back a transition whose messages went out (before 0.1.90 it was `NORMAL`).

**`wf_definitions`** — workflow definitions, kept by hash.

At startup the node keeps the definition it runs under its hash (the SHA-256 of its bytes). Each instance records the hash of the definition it was born with, and runs on it until it ends, even if the definition changes later: at startup each running instance is bound back to its own. A definition goes once it is neither the current one nor used by a running instance. An instance whose definition is not kept, or no longer loads, fails at startup (`definition_unavailable`).

```sql
CREATE TABLE wf_definitions (
    hash            TEXT PRIMARY KEY,
    workflow_type   TEXT NOT NULL,
    definition_json TEXT NOT NULL,
    loaded_at_ms    INTEGER NOT NULL
);
```

**`wf_instances`** — current state of each instance.

```sql
CREATE TABLE wf_instances (
    instance_id       TEXT PRIMARY KEY,
    workflow_type     TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'running'
                      CHECK(status IN ('running','cancelling','completed','cancelled','failed')),
    current_state     TEXT NOT NULL,
    input_json        TEXT NOT NULL DEFAULT '{}',
    state_json        TEXT NOT NULL DEFAULT '{}',
    current_trace_id  TEXT NOT NULL DEFAULT '',
    created_at_ms     INTEGER NOT NULL,
    updated_at_ms     INTEGER NOT NULL,
    terminated_at_ms  INTEGER,
    definition_hash   TEXT NOT NULL DEFAULT '',
    thread_id         TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_wfi_thread ON wf_instances(thread_id);
```

`state_json` is a serialized JSON object of all workflow variables. `input_json` is the original trigger event's payload. A double keeps its decimal point in both (`7.0`), so a value has the same type after a restart. `thread_id` is the instance's thread (§14.3), indexed: a message on it reaches the instance.

**`wf_instance_log`** — flat FIFO log of what the runtime did per instance: one entry per action, transition and unhandled event (`action_type`, `summary`, `ok`, `error_detail`). A failure the engine causes on its own starts its `error_detail` with the cause: `transition_panic`, `internal_event_cap`, `definition_unavailable`, `state_not_in_definition`, `replay_exhausted`.

Cap per instance: 100 entries, FIFO. Older entries are pruned when adding new ones. When the instance terminates, its log is preserved until the instance itself is GC'd (retention default 7 days, configurable).

**`wf_instance_timers`** — the index of the timers each instance has scheduled (`instance_id`, `timer_key`, `scheduled_at_ms`, `fire_at_ms`), reconciled with SY.timer at startup (§11.4).

**`wf_internal_events`** — durable FIFO queue of internal synthetic events emitted by `emit_internal_event` and not yet consumed (`event_id`, `instance_id`, `msg_type`, `msg_name`, `payload_json`, `trace_id`, `created_at_ms`, `attempts`). These rows are transient runtime state, not history. They exist only until the corresponding internal event is consumed or the instance terminates.

**`wf_kept_messages`** — the messages `keep_message` kept (§10.8), by instance and name, whole and as received (`message_json`); committed with the transition, and dropped when the instance ends.

```sql
CREATE TABLE wf_kept_messages (
    instance_id   TEXT NOT NULL,
    name          TEXT NOT NULL,
    message_json  TEXT NOT NULL,
    kept_at_ms    INTEGER NOT NULL,
    PRIMARY KEY (instance_id, name)
);
```

A store of schema version 3 migrates at the node's start: each instance takes its own id as its thread, which is what its sends carried.

**The upgrade to 0.1.91.** A definition written before 0.1.91 no longer loads: it has no `start_from`, and may have `thread_id_ref`. Its node is `FAILED_CONFIG` (`invalid_definition`) until the workflow is redeployed with `start_from`, and the running instances born with such a definition fail at the next start (`definition_unavailable`).

**`wf_inbound_events`** — the inbound journal: an event the runtime took and has not finished, with the starts that ran it again (`attempts`, §14.4).

```sql
CREATE TABLE wf_inbound_events (
    event_id        INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id     TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK(kind IN ('create','event')),
    message_json    TEXT NOT NULL,
    received_at_ms  INTEGER NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0
);
```

### 13.2 What is NOT stored

- Full transition history with CEL evaluation details — too verbose, not needed for the documented introspection use cases.
- Intermediate snapshots of `state` per transition — only the current `state_json` is kept.
- Raw incoming messages — only the derivative log entries. An inbound event stays in `wf_inbound_events` only until its transition commits, and a message `keep_message` kept stays in `wf_kept_messages` only until its instance ends.
- Outgoing messages beyond their log entry — the message itself is not saved.
- Consumed internal events — only pending internal events remain in `wf_internal_events`; once consumed, they are deleted.

If deep debugging is needed during development, the debug log at the process level (journal) has more detail. The persisted data is the minimum for operational introspection and replay.

### 13.3 Replay semantics

WF v1 supports replay in a limited sense: given the `input_json` and the sequence of events that arrived (if external events are also persisted somewhere — currently they are not at the WF level), a replay would reproduce the state.

In v1, true event-sourced replay is **not** implemented — only the current state is kept. This is sufficient for "look at an instance and see what it did", which is the documented use case. True replay for debugging is deferred to v2, where an event log table may be added. What v1 does run again is an event a crash left unfinished (§14.4).

### 13.4 Restart behavior

When a WF node restarts:

1. Opens `wf_instances.db` and keeps the definition it runs (by its hash).
2. Loads all instances with `status IN ('running', 'cancelling')`, each on the definition it was born with, on its thread, with the messages it kept; fails those whose definition is not available (`definition_unavailable`) or does not have their current state (`state_not_in_definition`).
3. Drains any pending internal events for each recovered instance, in FIFO order, until each queue is empty or the instance becomes terminal, each one counting its attempt first (§14.4).
4. Runs again every inbound event left in `wf_inbound_events`, in the order they came: an event of an instance, or a creation under the `instance_id` it had (when the current definition still starts from its sender), each one counting its attempt first; one already run at 3 starts is not run again (§14.4).
5. Reconciles the timers with SY.timer (§11.4); when SY.timer does not answer, it starts `DEGRADED` and reconciles from the receive loop.
6. Drops the definitions no running instance uses, and enters the receive loop.

Completed / cancelled / failed instances remain in the database for read-only introspection but are not loaded into memory.

## 14. Runtime lifecycle

### 14.1 Node startup

The WF node runs on the managed control plane of the Go SDK (`go/fluxbee-go-sdk/managed_control_plane.go`), the same model as the AI and IO nodes (`docs/wf-standard-path-v1.md`, since 0.1.70):

1. Read `config.json` (`nodes/WF/<node@hive>/config.json`, from `FLUXBEE_NODE_NAME`). Its identity was registered by the orchestrator at spawn (`ILK_REGISTER`).
2. Validate it: the operator's config (unknown keys are refused), made concrete with what `_system` says (the package, the hive), and the workflow definition, loaded from `_system.package_path/flow/definition.json` and compiled against the CEL environment. The node reads `_system` leniently and never rewrites it, so a key the orchestrator adds never stops it.
3. Connect to the local router via `fluxbee-go-sdk`, **whatever the result**.
4. With a config in use (`CONFIGURED`): open `wf_instances.db`, restore running instances, start the periodic GC task (it sweeps terminated instances older than retention), and enter the receive loop.
5. With a config the node cannot use (`FAILED_CONFIG`: `invalid_config`, or `invalid_definition` for a missing or broken definition), it stays up and answers control messages only:
   - `NODE_STATUS_GET`: `DEGRADED` plus `last_error`;
   - `CONFIG_GET`: the state and the error;
   - `CONFIG_SET`: validated and persisted;
   - `WF_*` requests: `node_not_configured`;
   - workflow traffic: logged and dropped.

   A restart applies a corrected config. A redeploy through SY.wf-rules does that: it rebinds the node and restarts it. Before 0.1.70 the node exited and systemd relaunched it every 5 s, so the reason reached only journald.

Opening the store, and a store that fails during recovery, are still fatal at startup; SY.timer not answering is not (§11.4).

### 14.2 Event processing loop

```
loop {
    msg := router.receive()
    handler := dispatch(msg.meta.msg)
    handler(msg)
}
```

Handlers:

- `NODE_STATUS_GET` — the health of its config state: `HEALTHY` when `CONFIGURED`, `DEGRADED` with `last_error` otherwise (the orchestrator's `get_node_status` reports both); also `DEGRADED`, with `timers_not_reconciled`, while its timers wait for SY.timer.
- `WF_HELP` — returns the workflow HELP descriptor.
- `WF_CANCEL_INSTANCE` — initiates soft abort.
- `WF_GET_INSTANCE` — returns instance info (state, status, variables, recent log).
- `WF_LIST_INSTANCES` — returns instances matching filter.
- `WF_GET_CODE` — returns the current workflow definition.
- `TIMER_FIRED` — from SY.timer of its own hive only, for a timer this node scheduled: routes to the instance its `client_ref` names (§11.2).
- Any other `system` message — dropped, logged and never answered: a system message is control, never a workflow event, but a `TIMER_FIRED` (since 0.1.90; the AI and IO nodes take the system kind only as control too).
- Any other message — taken only from a node the definition names; matched against the running instance on its `meta.thread_id`, or, from a `start_from` node, if it is no reply and it matches the `input_schema`, creates a new instance on its thread (§14.5).

The `WF_*` control verbs answer only `SY.admin@motherbee` and `SY.wf-rules` of the WF's hive; anyone else gets `UNAUTHORIZED` (FINDINGS A-119). Before any of this, a notice is dropped and never answered: a message the router made (no `routing.src_l2_name`), `HELLO`, `ANNOUNCE`, `WITHDRAW`, `ECHO`, `ECHO_REPLY`, `UNREACHABLE`, `TTL_EXCEEDED`, `TIMER_HELP`, or a system message without a name — SY.timer's rule and the AI nodes'.

Internal events emitted by `emit_internal_event` do **not** enter through this inbound loop. They are already bound to a specific instance and are drained from that instance's durable internal queue by the runtime itself.

### 14.3 Instance creation

When an incoming event looks like a trigger (it comes from a `start_from` node, no running instance holds its thread, it is no reply, and its payload matches the `input_schema`):

1. Validate payload against `input_schema`.
2. If validation fails, respond with `INVALID_INPUT` error — only to a named request: a message without a name, a reply and a notice are never answered (§14.5).
3. Generate a new `instance_id` (`wfi:<uuid>`), and save the event in the inbound journal under it. The instance's thread is the event's `meta.thread_id`, the conversation's, or the `instance_id` when the event has none; every message the instance sends carries it.
4. Acquire the per-instance mutex.
5. Run entry actions of the initial state.
6. In one commit: create the instance row (`current_state = initial_state`, or terminal; `input_json = <payload>`; its definition's hash; its thread), enqueue its internal events, write the messages it kept, and delete the journaled event. A crash before this commit leaves no instance at all, and the journaled creation for recovery, which runs it again under the same `instance_id`.
7. Drain the instance's internal event queue until empty or terminal.
8. Release mutex.

### 14.4 Execution model and crash semantics: at-least-once

The WF runtime executes transitions following an **act-then-persist** model with **at-least-once** delivery guarantees on actions. This is a deliberate design choice. The implications matter for anyone building nodes that are targets of a WF:

The execution order within a transition is: save the inbound event in the journal (`wf_inbound_events`) → run exit_actions of source state → run transition actions → run entry_actions of target state → in one commit, persist the new state and any emitted internal events and delete the journaled event → drain the internal queue. If the runtime crashes in the middle of this sequence (after some actions executed but before the new state is persisted), the next restart recovers the instance in its **previous** state, finds the event still in the journal, and re-executes the transition from the beginning with it (a creation is covered the same way, §14.3). Some actions may have already executed before the crash; they will execute again. (Before 0.1.90 nothing was journaled, and a crash lost the event in progress.)

**A poison event does not keep the node down.** Each start counts the attempt of an event it runs again (a journaled one, or an internal event a crash left queued) in a commit of its own, before it runs it. An event already run at 3 starts without finishing (one that takes the node down beyond the panic guard: a fatal error, an out-of-memory) is not run again: its instance fails, with no action run (`replay_exhausted`), and a creation whose instance never committed is dropped, logged. An event taken live counts no attempt.

**A commit consumes every earlier event of its instance.** A commit that failed (a database error) leaves its event journaled while the live instance holds what that event did; the instance's next commit writes that too, so it deletes every journaled event of the instance up to its own. (Before 0.1.90 it deleted only its own, and the next start ran the first event again on a state that already held it.)

This means **side effects from a transition can occur more than once** in the case of a crash. Specifically:

- A `send_message` action may send the same message twice. The destination node will see two messages with the same content and the same `meta.trace_id` (the runtime preserves the trace_id across re-executions for exactly this reason).
- A `schedule_timer` action may attempt to schedule the same timer twice. SY.timer detects the duplicate via the deterministic `client_ref` (`wf:<instance_id>::<timer_key>`) and returns the existing timer instead of creating a new one. This is naturally idempotent.
- A `cancel_timer` action is idempotent by design (cancelling an already-cancelled or non-existent timer is a no-op).
- A `set_variable` action has no external side effect; re-execution simply re-assigns the same value.
- An `emit_internal_event` action is not external I/O. The internal event becomes visible for execution only after it is durably committed with the instance state. If the crash happens after commit but before drain, recovery resumes from the queued internal event; if the crash happens before commit, the internal event does not exist yet.

**The contract for nodes that receive messages from a WF is: be idempotent.** When a WF sends `INVOICE_CREATE_REQUEST` to `IO.quickbooks`, the IO node should use `meta.trace_id` as an idempotency key — if it sees two requests with the same trace_id, it must treat the second as a no-op and return the same response as the first.

This is the same pattern that applies to other distributed systems with at-least-once delivery (Kafka consumers, AWS SQS, etc.). The runtime guarantees the action will execute **at least once**; the destination's responsibility is to make repeated execution safe.

The alternative — guaranteeing exactly-once through write-ahead logging of pending actions — was considered and rejected for v1 as too complex for the value it provides. If a target node truly cannot tolerate duplicate messages, it should implement deduplication via trace_id at its own boundary. The runtime is explicit and honest about its semantics rather than promising guarantees it cannot keep.

### 14.5 Event correlation: which instance does this event belong to?

When an event arrives at the WF node, the runtime must decide whether it is for an existing instance, a new instance, or neither. An instance is a conversation: it lives on the thread of the message that started it (§14.3). Only the nodes written in the definition drive an instance (FINDINGS A-123): those of its `start_from`, of an `event_match.src`, and the targets of its `send_message` and `forward` actions, plus SY.timer for the WF's own timers. The resolution order is strict (since 0.1.91):

1. **If `meta.msg == "TIMER_FIRED"`**: taken only from SY.timer of the WF's own hive, and only for a timer this node scheduled (`owner_l2_name`). The event is for the instance its `client_ref` names. If the instance does not run (terminated, expired, never existed), log "orphaned timer" and discard.

2. **If the sender is no node the definition involves**: log and discard, never answered. On the thread of a running instance, that instance's own definition decides (it may be older than the current one, §13.1); otherwise the current definition.

3. **If `meta.thread_id` is the thread of a running instance**: the event is for that instance. This is how a person's next message reaches their conversation's instance, and how the answer of an AI / IO node the WF contacted comes back: the WF sets `meta.thread_id` to the instance's thread on every message it sends, and target nodes are expected to copy this value to the `meta.thread_id` of their response. An event the instance's current state has no transition for is logged and dropped (§8.3): it is never queued for a later state.

4. **If the sender is a `start_from` node and the message is no reply** (a reply is any `*_RESPONSE`, an `INVALID_INPUT`, or an agent's error named `<msg>.error`): a new instance on its thread (§14.3), as the `input_schema` allows. If the schema refuses it, respond with `INVALID_INPUT`, unless the message has no name: a reply, a notice and a message without a name are never answered, so two WFs cannot bounce an `INVALID_INPUT` (FINDINGS A-119).

5. **Otherwise** — an answer to an instance that ended (its timer fired, it was cancelled or failed, or a first copy of the answer finished it), say: log and discard, never answered. An agent answers with the request's own `msg` and thread; as a new instance it would ask the agent again, a WF↔AI loop.

So a person who writes again after their conversation's instance ended starts a new one on the same thread, at the initial state. A late answer from a node outside `start_from` (an agent's) is dropped only while no instance runs on its thread: once the conversation's next instance runs there, the late answer reaches it (step 3), like any message on its thread from a node the definition names. A `system` message other than a `TIMER_FIRED` never reaches this order: it is dropped before (§14.2).

`emit_internal_event` is outside this correlation order. Once committed, it is already attached to a specific instance in `wf_internal_events` and is consumed from there directly by the runtime.

This order is fixed in v1 and not configurable. It reflects the reality of how messages flow in Fluxbee:

- Timer events have explicit instance correlation in their `client_ref`.
- A conversation keeps its thread, from the IO node through the WF to the agents and back.
- A trigger comes from a node the definition names in `start_from`, on a thread no instance holds.

**Contract for nodes responding to a WF**: when you receive a message from a WF (a `send_message` or a `forward`), copy `meta.thread_id` into the `meta.thread_id` of your response. This is how the WF correlates your response with the instance that asked the question. If you don't preserve thread_id, your response reaches no instance and is dropped.

This contract is implicit in many systems (HTTP correlation IDs, RPC call IDs, etc.) but here it is explicit and load-bearing. The WF cannot work without it.

## 15. WF_HELP and self-description

Every WF node answers `WF_HELP` with its workflow's contract. Like every `WF_*` control verb, it answers only `SY.admin@motherbee` and `SY.wf-rules` of the WF's hive; anyone else gets `UNAUTHORIZED` (since 0.1.90, FINDINGS A-119).

```json
{
  "meta": { "type": "system", "msg": "WF_HELP" },
  "payload": {}
}
```

Response (for §19.2's workflow):

```json
{
  "meta": { "type": "system", "msg": "WF_HELP_RESPONSE" },
  "payload": {
    "ok": true,
    "workflow_type": "invoice",
    "description": "Issues an invoice the billing system asks for: has the customer's data checked, creates the invoice and has the billing agent deliver it; tells the billing agent what is missing.",
    "initial_state": "validating_data",
    "states": ["validating_data", "missing_data_requested", "creating_invoice", "delivering_to_customer", "completed", "failed", "cancelled"],
    "terminal_states": ["completed", "missing_data_requested", "failed", "cancelled"],
    "input_schema": { "type": "object", "required": ["customer_id", "items"], "properties": { "customer_id": { "type": "string" } } }
  }
}
```

`states` lists the state names, and `input_schema` is the definition's, as written (abridged above). The descriptor has no version, trigger pattern, source hints or list of operations: an earlier design had them, and they were never built. Who may start an instance is the definition's `start_from` (§8.1), among the senders OPA lets through (§17). `WF_GET_CODE` answers the whole definition (`{"ok": true, "workflow_type": ..., "definition_json": "<the definition, as a string>"}`), to the same two senders.

Archi does not read `WF_HELP` today (§19.5).

---

# Part III — Operational Surface

## 16. SY.admin read-only surface

**Not built.** SY.admin has none of the actions below yet: exposing instances through SY.admin is a later case (`docs/wf-orchestration-v1.md` §4.8, Case 4). Today the WF node answers its `WF_*` verbs to `SY.admin@motherbee` and `SY.wf-rules` of its hive only; an operator can send one through SY.admin's `send_node_message`, whose answer is not shown. The design follows.

Consistent with the pattern established by SY.timer, `SY.admin` would expose read-only operations for WF nodes:

| Admin action | Maps to | Purpose |
|---|---|---|
| `wf_help` | `WF_HELP` | Get workflow contract |
| `wf_list_instances` | `WF_LIST_INSTANCES` | List instances with filters |
| `wf_get_instance` | `WF_GET_INSTANCE` | Get one instance full detail |
| `wf_get_code` | `WF_GET_CODE` | Get workflow definition code |

No admin action mutates an instance in v1: there is **no** `wf_cancel_instance` in admin. A cancel is a `WF_CANCEL_INSTANCE` from `SY.admin@motherbee` or `SY.wf-rules` of the WF's hive, the only senders a WF takes it from (§12.1); an operator sends it through SY.admin's `send_node_message` (`POST /hives/{hive}/nodes/{node}/messages`), whose answer is not shown.

### 16.1 Instance listing and retrieval

`WF_LIST_INSTANCES` takes optional filters: `status` (`running`, `cancelling`, `completed`, `cancelled` or `failed`; absent, all), `limit` (newest first; `0` or absent, all) and `created_after_ms`:

```json
{
  "meta": { "type": "system", "msg": "WF_LIST_INSTANCES" },
  "payload": {
    "status": "running",
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
    "count": 1,
    "instances": [
      {
        "instance_id": "wfi:550e8400-e29b-41d4-a716-446655440000",
        "thread_id": "conv-7c1e",
        "workflow_type": "invoice",
        "definition_hash": "4f1c9a0e7d2b...",
        "status": "running",
        "current_state": "creating_invoice",
        "created_at_ms": 1775577000000,
        "updated_at_ms": 1775578000000,
        "terminated_at_ms": null
      }
    ]
  }
}
```

SY.wf-rules reads `ok` and `count` of it (`{"status": "running", "limit": 0}`) for a workflow's active instances.

`WF_GET_INSTANCE` takes `instance_id` (and `log_limit`, 20 by default) and returns the same fields, the instance's input and variables as the store keeps them, and its most recent log entries, newest first:

```json
{
  "payload": {
    "ok": true,
    "instance": {
      "instance_id": "wfi:550e8400-e29b-41d4-a716-446655440000",
      "thread_id": "conv-7c1e",
      "workflow_type": "invoice",
      "definition_hash": "4f1c9a0e7d2b...",
      "status": "running",
      "current_state": "creating_invoice",
      "created_at_ms": 1775577000000,
      "updated_at_ms": 1775578000000,
      "terminated_at_ms": null,
      "input": { "customer_id": "c1", "items": [{ "sku": "A", "qty": 2, "unit_price": 9.5 }] },
      "state_variables": { "validation_request": "Customer c1 asks for an invoice of these items: ...", "validated_at": 1775577100000 },
      "recent_log": [
        {
          "logged_at_ms": 1775577100000,
          "action_type": "send_message",
          "summary": "send_message to IO.quickbooks@motherbee",
          "ok": true,
          "error_detail": ""
        }
      ]
    }
  }
}
```

The keys are these, in snake_case, since 0.1.90 (before, the instances went out with the store's Go field names); `thread_id`, the instance's thread (§14.3), since 0.1.91. A refusal is `{"ok": false, "error": "<code>", "detail": "..."}` (`INVALID_REQUEST`, `INSTANCE_NOT_FOUND`, `STORE_ERROR`).

Read-only. No ability to modify state or force transitions.

## 17. Authorization (via OPA and identity v2)

### 17.1 The model

Authorization for triggering a WF lives entirely in OPA. The WF node itself does not authorize — it trusts the router, which trusts OPA, which reads identity data from the SHM region written by `SY.identity`. What the WF does check is deterministic and written in its definition (since 0.1.91): it takes workflow traffic only from the nodes the definition names, and starts an instance only from those of its `start_from` (§14.5).

For v1 of WF, authorization is expressed using the primitives that identity v2 already provides:

- **`ilk_type`** — `human`, `agent`, `system`.
- **`registration_status`** — `temporary`, `partial`, `complete`.
- **`tenant_id`** — the tenant the source ILK belongs to.
- **Source L2 node name** — matched by exact name, prefix, or family (`AI.*`, `IO.*`, `SY.*`, `WF.*`). The router exposes `routing.src_l2_name` as part of the normalized input to OPA.

This is sufficient for the vast majority of real authorization cases. More granular per-ILK permissions (e.g. "this specific customer can trigger refunds") are deferred to a later identity evolution, and are not needed for WF v1.

### 17.2 Example: authorize `WF.invoice`

Typical rule: only the billing system's IO node may ask for an invoice, and only on behalf of a fully-registered human. Who starts an instance is, besides, the definition's `start_from` (§14.5): OPA lets a message through, and the definition decides what it does.

```rego
# the billing system's IO node asks for invoices
allow {
    input.routing.dst == "WF.invoice@motherbee"
    input.meta.msg == "INVOICE_REQUEST"
    input.routing.src_l2_name == "IO.api.billing@motherbee"
    src := data.identity[input.meta.src_ilk]
    src.ilk_type == "human"
    src.registration_status == "complete"
}
```

Anyone else — other IO nodes, temporary humans, or system nodes not explicitly authorized — is rejected. An agent never asks: it only answers (§5.1).

### 17.3 Example: authorize `WF.payment-reconciliation` (webhook-only)

Some workflows should only be triggered by IO nodes, never by AI or humans directly. For example, a reconciliation flow driven by a payment processor webhook.

```rego
allow {
    input.routing.dst == "WF.payment-reconciliation@motherbee"
    input.meta.msg == "PAYMENT_RECEIVED"
    startswith(input.routing.src_l2_name, "IO.")
}
```

This cleanly enforces "only mechanical integrations can start this" without needing any identity metadata beyond what v2 already provides.

### 17.4 Example: authorize by tenant

A WF may need to restrict triggering to ILKs belonging to specific tenants, for example during a pilot rollout.

```rego
allow {
    input.routing.dst == "WF.invoice@motherbee"
    input.meta.msg == "INVOICE_REQUEST"
    src := data.identity[input.meta.src_ilk]
    src.tenant_id == "tnt:pilot-customer-uuid"
    src.ilk_type == "human"
}
```

### 17.5 What the WF does NOT do

- The WF does not inspect identity data itself.
- The WF does not maintain its own authorization rules or allowlists.
- The WF does not validate the source of workflow traffic beyond the input schema and the nodes its definition names (`start_from`, `event_match.src`, the targets of its sends; §14.5). It does check the sender of its control verbs (SY.admin@motherbee and SY.wf-rules of its hive) and of a `TIMER_FIRED` (SY.timer of its hive, for a timer the WF scheduled), which the router does not protect.

All authorization is upstream, in OPA, reading identity v2 data. The WF simply receives messages that passed the gate and executes deterministically. If a WF instance receives a message, it trusts that the router (through OPA) approved it.

### 17.6 Note on future identity evolution

A future iteration of identity may introduce a claims model that allows per-ILK granular authorization (see the archived draft `10-identity-v3.md`). When that evolution matures, OPA rules can be rewritten to consume claims without changing anything inside the WF node itself — the authorization surface is OPA, and the WF is agnostic to how OPA evaluates. This means WF v1 will not need to be modified when identity evolves.

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
| `UNAUTHORIZED` | A `WF_*` control verb from a sender other than `SY.admin@motherbee` or `SY.wf-rules` of the WF's hive (since 0.1.90). |

What the engine answers today: `INVALID_INPUT`, `INVALID_REQUEST`, `INSTANCE_NOT_FOUND`, `INSTANCE_ALREADY_TERMINATED`, `STORE_ERROR` (not `STORAGE_ERROR`), `INTERNAL_ERROR`, `UNAUTHORIZED` and `node_not_configured`. The load-time codes above are not returned as such: a definition that fails validation is `invalid_definition` at the node (`FAILED_CONFIG`) and `COMPILE_ERROR` at SY.wf-rules, with the path; `GUARD_TIMEOUT` and `INSTANCE_CANCELLING` are never returned (a guard that times out is false).

---

# Part IV — Canonical Example

## 19. Case study: `WF.invoice`

This is the reference example used throughout the document. It demonstrates all the core properties of a WF in one concrete workflow.

### 19.1 Business description

The task: when the billing system asks for an invoice for a customer, through its IO node (`IO.api.billing@motherbee`), the workflow must:

1. Have an AI node check that the customer data is complete enough to invoice, for a structured answer.
2. If incomplete, tell the billing agent (the AI node in the conversation with the customer) what is missing, and end. The agent asks the customer, who answers it through the IO node of their channel, never this instance; the billing system asks for the invoice again once it has the data.
3. Once complete, invoke an external invoicing system (IO.quickbooks) to create the invoice.
4. Wait for confirmation of invoice creation.
5. Have the billing agent deliver the invoice to the customer, for a structured answer.
6. Complete.

Duration: minutes to hours. Deterministic in every decision. Side effects are real and visible externally (invoice exists in QuickBooks, message delivered). Must survive node restarts.

### 19.2 Workflow definition

```json
{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "Issues an invoice the billing system asks for: has the customer's data checked, creates the invoice and has the billing agent deliver it; tells the billing agent what is missing.",
  "start_from": ["IO.api.billing@motherbee"],
  "input_schema": {
    "type": "object",
    "required": ["customer_id", "items"],
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
      "currency": { "type": "string" }
    }
  },
  "initial_state": "validating_data",
  "terminal_states": ["completed", "missing_data_requested", "failed", "cancelled"],
  "states": [
    {
      "name": "validating_data",
      "description": "Asking AI.billing-validator, for a structured answer, whether the customer's data is complete.",
      "entry_actions": [
        {
          "type": "set_variable",
          "name": "validation_request",
          "value": "'Customer %s asks for an invoice of these items: %s.'.format([input.customer_id, input.items])"
        },
        {
          "type": "send_message",
          "target": "AI.billing-validator@motherbee",
          "meta": {
            "msg": "VALIDATE_INVOICE_DATA",
            "context": {
              "response_envelope": {
                "kind": "json_object_v1",
                "strict": true,
                "required": ["complete", "missing"],
                "properties": {
                  "complete": { "type": "boolean" },
                  "missing": { "type": "string" }
                }
              }
            }
          },
          "payload": { "type": "text", "content": { "$ref": "state.validation_request" } }
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
          "event_match": { "msg": "VALIDATE_INVOICE_DATA" },
          "guard": "json_parse(event.payload.content).complete == true",
          "target_state": "creating_invoice",
          "actions": [
            { "type": "set_variable", "name": "validated_at", "value": "now()" }
          ]
        },
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA" },
          "guard": "json_parse(event.payload.content).complete == false",
          "target_state": "missing_data_requested",
          "actions": [
            {
              "type": "set_variable",
              "name": "missing_data_request",
              "value": "'The invoice for customer %s cannot be issued yet: it needs %s. Ask the customer for it.'.format([input.customer_id, json_parse(event.payload.content).missing])"
            }
          ]
        },
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA.error" },
          "guard": "true",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'validator error: ' + event.payload.code" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.timer_key == 'validation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'validator timeout'" }
          ]
        }
      ]
    },
    {
      "name": "missing_data_requested",
      "description": "Terminal: tells the billing agent what is missing, and ends. The agent asks the customer, who answers it, never this instance; the billing system asks for the invoice again once it has the data.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing@motherbee",
          "meta": { "msg": "INVOICE_DATA_MISSING" },
          "payload": { "type": "text", "content": { "$ref": "state.missing_data_request" } }
        }
      ],
      "exit_actions": [],
      "transitions": []
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
            "customer_id": { "$ref": "input.customer_id" },
            "items": { "$ref": "input.items" },
            "currency": { "$ref": "input.currency", "default": "USD" }
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
            {
              "type": "set_variable",
              "name": "delivery_request",
              "value": "'Send customer %s the invoice %s: %s'.format([input.customer_id, event.payload.invoice_id, event.payload.link])"
            }
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
          "guard": "event.timer_key == 'qb_creation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'quickbooks timeout'" }
          ]
        }
      ]
    },
    {
      "name": "delivering_to_customer",
      "description": "Asking the billing agent, for a structured answer, to send the invoice to the customer.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing@motherbee",
          "meta": {
            "msg": "DELIVER_INVOICE",
            "context": {
              "response_envelope": {
                "kind": "json_object_v1",
                "strict": true,
                "required": ["delivered"],
                "properties": { "delivered": { "type": "boolean" } }
              }
            }
          },
          "payload": { "type": "text", "content": { "$ref": "state.delivery_request" } }
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
          "event_match": { "msg": "DELIVER_INVOICE" },
          "guard": "json_parse(event.payload.content).delivered == true",
          "target_state": "completed",
          "actions": []
        },
        {
          "event_match": { "msg": "DELIVER_INVOICE" },
          "guard": "true",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'the agent did not deliver the invoice'" }
          ]
        },
        {
          "event_match": { "msg": "DELIVER_INVOICE.error" },
          "guard": "true",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'delivery error: ' + event.payload.code" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.timer_key == 'delivery_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'delivery timeout'" }
          ]
        }
      ]
    },
    {
      "name": "completed",
      "description": "Invoice issued and delivered.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "failed",
      "description": "Terminal failure. The variable 'failure_reason' holds the cause.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "cancelled",
      "description": "Cancelled through WF_CANCEL_INSTANCE.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    }
  ]
}
```

### 19.3 What this example demonstrates

- **All four WF properties present:** multi-step (validation → creation → delivery), memory between steps (state variables accumulating), deterministic (every transition is a guard-based decision, no ambiguity), lives in time (timers of minutes to 24 hours).
- **Clean separation AI/WF/IO:** the WF delegates ambiguous work (validating data sufficiency, delivering the invoice to a person) to AI nodes, and the structured work (creating the invoice) to an IO node. The WF itself does no reasoning.
- **Asking an agent:** the WF writes the agent a text (`set_variable` builds it with CEL's `format`, and `$ref` carries it), asks for a structured answer (`meta.context.response_envelope`, whose fields are scalars: the agent takes a string, boolean, integer or number), and branches on its fields with `json_parse(event.payload.content)`. The agent answers with the request's own `msg` (`VALIDATE_INVOICE_DATA`), never a `*_RESPONSE`; its error answer is a payload of type `error`, named `<msg>.error` because the request has a `msg`, and has a transition of its own.
- **Who starts it:** `IO.api.billing@motherbee`, the billing system's IO node, the only node in `start_from`. The instance lives on the thread of its request, and every message the instance sends carries that thread. The agents are targets: they answer, and start nothing.
- **No wait for a person:** a person answers the agent of their conversation, never a WF instance, so `missing_data_requested` tells the agent and ends. The agent's acknowledgement then comes from a node outside `start_from`, on a thread no instance holds: it is dropped (§14.5), instead of starting a new instance.
- **A field that may be missing:** `currency` is optional, so its `$ref` gives a default (`USD`); a `$ref` without one fails the action when the field is not there.
- **Timer usage:** SLA-style timeouts at every wait state, cancelled on exit. This is the standard pattern.
- **Failure handling:** every wait state has a timer-driven path to `failed`, with the reason stored as a variable for post-mortem.
- **No recovery or rollback:** if the invoice creation fails, the workflow goes to `failed`. It does not try to roll back whatever happened. Recovery is a business decision made outside this workflow.

### 19.4 Authorization in OPA for this example

Who starts an instance is the definition's `start_from`: `IO.api.billing@motherbee`. With identity v2, OPA decides, besides, which messages reach `WF.invoice` at all, by source L2 name and ILK type:

```rego
# the billing system's IO node, for fully-registered humans
allow {
    input.routing.dst == "WF.invoice@motherbee"
    input.routing.src_l2_name == "IO.api.billing@motherbee"
    src := data.identity[input.meta.src_ilk]
    src.ilk_type == "human"
    src.registration_status == "complete"
}

# the nodes the workflow asks, answering
answering := {"AI.billing-validator@motherbee", "AI.billing@motherbee", "IO.quickbooks@motherbee"}

allow {
    input.routing.dst == "WF.invoice@motherbee"
    answering[input.routing.src_l2_name]
}
```

The first rule lets the billing system ask for an invoice on behalf of a human ILK that has completed registration; the second lets the answers through. Neither starts anything by itself: an answer reaches only the running instance on its thread, and only a `start_from` node starts one (§14.5).

### 19.5 What Archi sees when designing new workflows

Archi cannot read `WF_HELP` today: a WF answers it only to `SY.admin@motherbee` and `SY.wf-rules` of its hive (§15), and SY.admin has no WF action that asks it and shows the answer (§16). What Archi reads is the definition, through SY.wf-rules (`wf_rules_get_workflow`: the current definition, its version and hash), and the input schema and `start_from` in it. If Archi adds a source of invoice requests (another IO node, or another WF), it names it in the definition's `start_from`, and a redeploy applies it; OPA has to let its messages through too. An agent is never one: it only answers.

This is the Archi-first loop: definitions as data, simple OPA rules using identity primitives, deterministic execution.

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
| `$ref` wrapper for payload substitution | Avoids ambiguity between literal strings and path lookups; supports any JSON type |
| `client_ref` deterministic timer identification | Eliminates race window between schedule and operation; requires SY.timer v1.1 |
| At-least-once with downstream idempotency | Honest contract; alternative (exactly-once with WAL) too complex for v1 |
| Strict event correlation order | The WF's own timer first (by its `client_ref`), then a sender the definition does not name dropped, then a running instance's thread, then a new instance from a `start_from` node, and anything else dropped (§14.5) — predictable, no ambiguity |
| One instance per conversation | An instance lives on the thread of the message that started it, so the agents keep one memory per conversation and a person's next message reaches it (since 0.1.91) |
| WF owns timer recovery on restart | The WF is the only one that can update instance state when a timer fires; reconciliation is its responsibility |
| No override from admin (read-only) | If a WF is stuck, fix the code and redeploy; never patch runtime state |
| Who may send a WF anything is OPA's, consuming identity v2 primitives (`ilk_type`, `registration_status`, L2 name patterns) | Centralized policy, agnostic to future identity evolution; the WF itself checks what its definition writes (the nodes that start and drive an instance, §14.5) and what the router does not protect: the senders of its control verbs and of a `TIMER_FIRED`, and the owner of a fired timer (§17.5) |
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
- **Direct admin mutations.** No `wf_cancel_instance` or similar in admin. A cancel is a `WF_CANCEL_INSTANCE` from `SY.admin@motherbee` or `SY.wf-rules` of the WF's hive; an operator sends it through SY.admin's `send_node_message` (§16).
- **Scheduled starts.** An instance is started only by a message from a `start_from` node that is no reply, never a `system` one (§14.2, §14.5). A `TIMER_FIRED` reaches only the running instance whose timer it is, so a timer on SY.timer whose target is the WF node starts nothing; a time-based start is not built (`docs/wf-orchestration-v1.md` §4.4).

## 21. v1 limitations (explicit)

Workflow authors should be aware of these limits.

**Enforced:**

- **Guard evaluation: 10 ms.** A guard is stopped at its timeout, and counts as false (§9.3).
- **`set_variable` evaluation: 1,000,000 CEL cost units.** Past it the action fails (§10.5).
- **Minimum timer duration: 60 seconds** (SY.timer's own minimum), checked at load for a literal, and when the action runs for a `$ref` (§10.2).
- **100 log entries per instance**, FIFO.
- **Retention: 7 days for terminated instances** (default, configurable).
- **At most 100 internal events after one event** (`internal_event_cap`).
- **At most 3 starts for an event a crash left unfinished** (`replay_exhausted`, §14.4).
- **At most 1000 timers read back from SY.timer** at a reconciliation.

**Guidance, not enforced:**

- **Max 32 states per workflow.** Beyond this, consider breaking into multiple workflows.
- **Max 16 transitions per state.** Same reasoning.
- **Max 100 actions in one transition's action list.** Unusual to approach.
- **Max 64 variables in instance state.** Keep state focused.
- **Max 1000 concurrent running instances per WF node.** Beyond this, scale horizontally with more nodes.
- **Workflow definition size: 512KB JSON.** Should be very difficult to exceed for reasonable workflows.

These are not arbitrary — they reflect what a sane workflow should stay within. Hitting them is a signal to reconsider the design.

## 22. References

| Topic | Document |
|---|---|
| Identity (current) | `10-identity-v2.md` |
| Identity (future conceptual draft, archived) | `10-identity-v3.md` |
| System timer | `sy-timer.md` |
| Architecture | `01-arquitectura.md` |
| Protocol | `02-protocolo.md` |
| Routing / OPA | `04-routing.md` |
| Control plane (CONFIG_GET/SET) | `node-config-control-plane-spec.md` |
| Conceptual model for claims (future) | `fluxbee_identity_claims_normative_model.md` |

Operational note: for managed `WF.*` nodes, `GET /hives/{hive}/nodes/{node_name}/config` reads the persisted effective `config.json`, while `POST /hives/{hive}/nodes/{node_name}/control/config-get` / `config-set` is the live node-owned control-plane path. In `WF v1`, `CONFIG_SET` is boot-time only: it validates and persists the config and answers `restart_required: true`; `CONFIG_GET` shows it under `pending_restart` until a restart applies it. Errors are `{code, message}` with the IO family's codes (`invalid_config`, `invalid_definition`, `stale_config_version`, `unsupported_apply_mode`, `managed_package_path_locked`, `config_persist_error`); a `config_version` must be newer than the node's, `0` meaning the next one.

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
| **Trigger pattern** | What starts a new instance: a message from a node in the definition's `start_from`, on a thread no running instance holds, that is no `system` one and no reply, whose payload the `input_schema` accepts (§14.5). OPA decides, besides, who may send the WF anything. |
