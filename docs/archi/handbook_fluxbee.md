# Fluxbee Pipeline Handbook

**Audience:** internal pipeline agents used by Archi: Designer, RealProgrammer, PlanCompiler, and Archi as coordinator  
**Status:** v3  
**Date:** 2026-04-29

---

## 1. Quick Decision Guide

Use this section first. If the task is clear here, do not overcomplicate it.

### 1.1 Which tool family should solve the problem?

| Situation | Correct path |
| --- | --- |
| The operator wants to inspect state, logs, inventory, runtimes, routes, workflows, or node status | `fluxbee_system_get` / read-only tools |
| The operator wants a clear mutation on existing platform primitives | `fluxbee_plan_compiler` |
| The operator wants broad desired-state design across topology, runtimes, nodes, routing, workflow deployment, or OPA deployment | `fluxbee_start_pipeline` |

### 1.2 Workflow vs routing vs taps vs OPA

| Need | Prefer |
| --- | --- |
| Deterministic choreography between known nodes | `wf_deployments` / `wf_rules_compile_apply` |
| Prefix- or hive-based forwarding between known destinations | `routing` / `add_route` |
| Secondary copy of unicast traffic to an additional node | `router taps` / `add_tap` |
| Policy-based target resolution when destination is not explicit | `opa` (one user policy for every hive) / `opa_compile_apply` |

Rules:
- If the operator already names the participating nodes, do **not** default to OPA.
- If the operator wants fan-out or ordered business flow that depends on message content, think **workflow first**, then routing.
- Use OPA when the business wants policy-driven target selection, not when the path is already explicit.

### 1.3 IO, AI, WF, SY boundaries

| Prefix | Role | What it is for | What it is not for |
| --- | --- | --- | --- |
| `AI.*` | conversational / reasoning nodes | language behavior, agent work, interpretation | external integrations |
| `WF.*` | orchestration / deterministic process | workflow steps, branching, stateful orchestration | generic chat nodes |
| `IO.*` | integration boundary | HTTP, Slack, WhatsApp, email, external ingress/egress | default internal relay/router |
| `SY.*` | infrastructure | admin, storage, routes config, identity, policy plumbing | normal application-level business nodes |

Rules:
- Do not invent a new `IO.*` node as an internal relay unless the operator explicitly wants a dedicated integration/relay runtime.
- Do not propose creating or modifying `SY.*` nodes for ordinary operator work.
- For internal message choreography, prefer `WF.*` or routing before proposing new `IO.*` or `SY.*`.

### 1.4 Mutation path rule

Archi does not mutate the system directly.

- For mutations, Archi should end at `fluxbee_plan_compiler` or `fluxbee_start_pipeline`.
- The emitted executor plan is what eventually calls admin actions.
- SCMD is user-directed system control and is not part of the pipeline reasoning model.

---

## 2. Core Mental Model

Fluxbee is a distributed node platform. Each **hive** is a host machine. Each hive runs one or more managed **nodes** created from published **runtimes**.

```text
human / client
      |
      v
  [IO node] <-> [AI node] <-> [WF node]
                                  |
                              [SY infra]
```

### 2.1 Runtime vs node

| Concept | Meaning | Analogy |
| --- | --- | --- |
| Runtime | published package/template | Docker image |
| Node | managed instance created from a runtime | running container |

Rules:
- Publishing a runtime does not create a node.
- A node can exist as a persisted managed instance even if the process is stopped.
- A live process is not the same thing as a persisted managed instance.

### 2.2 Naming

#### Node names

Pattern: `TYPE.name@hive`

Examples:
- `AI.sales@motherbee`
- `WF.invoice@motherbee`
- `IO.api.support@motherbee`
- `SY.admin@motherbee`

#### Runtime names

Runtime/package names are **lowercase** and do **not** include `@hive`.

Examples:
- `ai.generic`
- `wf.engine`
- `io.api`
- `io.slack`
- `io.wapp`

Rules:
- Do not create new mixed-case runtime names.
- Node identity and runtime key are different concepts.
- `AI.sales@motherbee` is a node (an instance of `ai.generic`).
- `ai.generic` is a runtime.
- `SY.*` nodes are core services, not runtimes: never `run_node` them.

### 2.3 Creating an Internet-facing `IO.api`

`IO.api` is an instanced runtime, not a singleton service and not a direct HTTP listener. Each
instance owns one ICH and publishes `/e/<ich>` through the configured `SY.edge`.

Required sequence:

1. Choose the tenant the API serves (never the hive's root tenant
   `tnt:00000000-0000-0000-0000-000000000001`, see §8.2), a unique node name, `api_channel_id`,
   fixed `dst_node` and Edge node.
2. Spawn `runtime=io.api` through `run_node`; Orchestrator injects the instance ILK/tenant.
3. Apply the Edge-native config without credentials. IO.api self-externalizes and `SY.admin` mints
   and stores the Edge bearer through the canonical ingress flow.
4. Capture `runtime.publication.entry_token` from the successful `CONFIG_SET` response when present;
   it is a one-time delivery and must not be logged or requested through Vault.
5. Read node `CONFIG_GET` until `runtime.publication.status=published`; return its `ich`/`url` and
   the separately captured bearer to the operator.

Do not generate `listen`, `auth.api_keys`, `integrations`, webhook, multipart or attachment config.
Do not place tenant or `dst_node` in the public request. The full contract and the operational
flow are in `docs/io/io-api-node-spec.md` (§10).

---

## 3. Placement, Hive, and Scope

### 3.1 How to choose a hive

Resolve hive in this order:
1. If the operator names a hive explicitly, use it.
2. If the operator names an existing node with `@hive`, use that hive.
3. If the system has only one hive, use that hive without asking.
4. If several hives exist and the target is genuinely ambiguous, ask one concise clarification.

### 3.2 When to use `motherbee` vs `worker-*`

| Situation | Prefer |
| --- | --- |
| Shared general-purpose node | `motherbee` |
| Heavy compute or special hardware | `worker-*` |
| Tenant isolation by host | dedicated `worker-*` |
| LAN-only deployment needs outbound internet (LLM APIs, registries) | `egress` hive (NAT gateway) |

If a worker hive does not exist yet, include topology/VPN creation in desired state. If it already exists, do not recreate topology.

**Egress hives.** When internal hives have no internet access and need outbound HTTPS, provision a hive with `add_hive role=egress`. It runs a minimal profile (only `SY.config.routes` plus the implicit `RT.gateway`/`SY.orchestrator`) and applies OS-level NAT so internal hives can reach the internet through it. Egress provisioning requires host-specific params in the `add_hive` body under `egress` (`lan_cidr`, `wan_iface`, `lan_iface`; optional `edge_ip`/`ipv6`) — query `get_admin_action_help add_hive` for the field contract. If the deployment already has a physical router for internet, do **not** create an egress hive; instead workers receive the gateway route automatically once motherbee declares `egress.gateway_ip`.

---

## 4. Node Family Guidance

### 4.1 AI nodes

Use when the work is conversational, interpretive, or agentic.

Every AI node is an instance of the `ai.generic` runtime; there is no other AI runtime, and no AI
node boots with the base install. To make one:
- `run_node` with `runtime: ai.generic` and the node's tenant;
- `CONFIG_SET` its operational config: `behavior.kind: ai_chat`. Optional:
  - `behavior.provider`, `openai` or `anthropic`, and `behavior.model`: without them the node runs
    on the hive's `ai.default_provider` and on that provider's model
    (`ai.providers.<provider>.model` in hive.yaml). Name them only when the agent needs its own,
    and a model always of the node's provider. Naming a provider other than the hive's default
    needs its `behavior.model` too, unless that hive's hive.yaml lists a model for it: a worker
    added by `add_hive` lists only the default provider's. `CONFIG_SET` refuses a config whose
    model it cannot resolve (`invalid_config`); its answer and `CONFIG_GET` show both in effect,
    in `ai_provider` and `ai_model`;
  - `behavior.multimodal`, whether the model receives images (true when absent;
    `behavior.capabilities` is refused).

  It carries no prompt: `CONFIG_SET` refuses `instructions`;
- give it its behavior through its ILK definition: a role, skills, handbooks and optionally a
  personality, created as agent assets and set with `set_ilk_definition` on its ILK (§8.1.2). The
  ILK definition is the agent's only prompt source.

The provider is the node's: two agents of one hive may run on different providers. The node does
not name a Vault key: it uses the key SY.vault gives it for its provider (resource_type `openai` or
`anthropic`): one it owns, one assigned to it, or its tenant's general one (no owner, no assigned
node). The root tenant's keys are the core's; a tenant's node never falls back to them. So a node
runs on a provider only when its tenant has that provider's key (`vault_list` shows the metadata).
The SY nodes (Archi, cognition, the frontdesk) keep the hive's provider.

### 4.1.1 Agent tools

What an AI agent can do beyond answering is its **tools**: the functions its model calls while it
works. Every AI agent loads the same **base tools**, built into `ai.generic`: no config adds or
removes one, and you never choose tools per agent. Each tool has a card: what it does, when to use
it, its limits, when it is present (some need the message to name its interlocutor, or the node to
have its own identity), its input and output, and its errors.

- **The catalog:** read it first with `fluxbee_system_get` on `/hives/motherbee/runtimes/ai.generic`
  (`get_runtime`), which answers it as `tool_catalog`: the cards the installed version ships, even
  before any AI agent runs. On a running AI node, `/hives/{hive}/nodes/{node_name}/help`
  (`node_help`) answers the same cards in `help.tools`. Any other node (only `ai.generic` describes
  itself) answers `help: null` at once, with the reason, and so do a stopped AI node and, after
  3 s, a busy one. The same catalog is `docs/reference/agent-tools.md`, generated from the code
  with each runtime version.
- **Read it before you design a role, a skill or a handbook,** and design against it. Never write
  one that promises a tool the catalog does not list.
- **Roles stay reusable:** a role says what the agent does, never through which channel or tool, so
  one "sales" role serves an agent on WhatsApp and another on email. Skills and handbooks may name
  base tools, since every agent has them.

### 4.2 WF nodes and workflows

Use a workflow when the behavior is deterministic, step-based, branched, or must coordinate
several nodes. That covers business choreography, fan-out, ordered delivery, retries, dispatch,
and every conversation where several agents take part. Whenever a solution has several agents, a
WF sits at the IO node's exit.

This section is the reference for writing a WF; docs/wf-rules-quickstart.md covers deploying and
operating one.

- **Declaring it:** in `wf_deployments`, deployed with `wf_rules_compile_apply`. SY.wf-rules
  packages it, then creates and runs its `WF.<workflow_name>@<hive>` node. Never declare that node
  in `nodes`, and never `run_node` it: a manifest that declares a `WF.*` node is refused.
- **Wiring it:** each IO node whose messages the WF takes sends them to the WF: its config's
  `io.dst_node` is `WF.<workflow_name>@<hive>`. That covers the nodes of `start_from`, and the
  supervisors' io.slack of a consultation. Without it, the IO node keeps sending where it did, and
  the WF never hears from it.
- **Where it runs:** where SY.wf-rules runs, on the motherbee or a worker. Never on an ingress or
  an egress hive.
- **A complete example:** the seeds `wf_node_with_workflow` and `workflow_definition`. An agent
  answers through a menu or consults the supervisors, and the catch acts on its answers wherever
  they arrive.

#### 4.2.1 The definition

SY.wf-rules checks it (`go/pkg/wfcel`), and an error names the exact path.

- **Its fields:** `wf_schema_version` (`"1"`), `workflow_type`, `description`, `start_from`,
  `input_schema` (the JSON Schema of the first message's payload), `initial_state`,
  `terminal_states`, `states` and, optionally, `catch`. Any other field is refused. The language
  no longer has `meta.type`, `target_ref` or `thread_id_ref`.
- **`start_from`** names the nodes whose messages start an instance: IO nodes or WFs, each with its
  hive. Never an `AI.*` node, because an agent only answers, and never a `SY.*` or `RT.*` node.
- **One instance per conversation.** An instance lives on the thread of the message that started
  it.
  - Only the nodes the definition names drive the instance: those of `start_from`, those of an
    `event_match.src`, and the targets of its sends, the catch's included. SY.timer drives it too,
    for its own timers. Anyone else is dropped.
  - A `start_from` node's message on a thread that no instance holds starts a new instance.
- **A state** has `name`, `description`, `entry_actions`, `exit_actions` and `transitions`. A state
  named `failed` must be terminal: an action that fails ends the instance there.
- **A transition:**
  - `event_match`: a `msg`, a `src`, or both;
  - `guard`: CEL;
  - `target_state`: without it, the transition is internal and stays in its state, with no exit
    or entry actions;
  - `actions`.

  A transition with a `target_state` runs the state's exit actions, then its own actions, then the
  target's entry actions. With both a `msg` and a `src`, both must match. The first transition
  whose match and guard pass is taken. A message that no transition of the
  state takes goes to the catch (§4.2.5); it is never queued for a later state.

#### 4.2.2 Guards (CEL)

- **Variables:**
  - `input`: the payload of the message that started the instance;
  - `state`: the variables `set_variable` writes;
  - `event`: the message that arrived. It has `msg`, `type`, `src` (as the router stamps it),
    `thread_id`, `trace_id`, `payload` and `context`, and on a `TIMER_FIRED` also `timer_key`.
- **Functions:**
  - CEL's standard library: `has`, `size`, `in`, `contains`, `startsWith`, `matches`;
  - the string extensions: `split`, `join`, `format` and the rest;
  - `now()`: the node's clock, in Unix milliseconds;
  - `json_parse(text)`: 64 KiB at most.
- **Numbers:** a JSON integer is an `int`, and any other number a `double`. Comparisons mix them.
  Arithmetic does not: write `double(state.n) * 1.1` or `int(state.n) % 2`.
- **Limits:** a guard runs for 10 ms at most. A guard that is stopped, or that errs, is false.

Common guards:
- `event.timer_key == 'consult_timeout'`
- `event.context.evaluation.option == 'consult'`
- `event.payload.type == 'error'`
- `state.retry_count < 3 && has(event.payload.ok)`

#### 4.2.3 Actions

**What every action has in common:**
- **`$ref`.** Values like `{"$ref": "state.order"}` are resolved when the action runs. They work in
  payloads, `meta.context`, `vars` and timer times.
  - A `$ref` is a dot path rooted at `input.`, `state.` or `event.`.
  - An object whose `$ref` holds `#`, `/` or `:` (a JSON Schema reference) is a literal.
  - A missing field fails the action, and a `null` field gives `null`.
  - `{"$ref": "event.context", "default": {}}` gives the literal default for a missing or `null`
    field. A `$ref` holds only `$ref` and `default`.
- **`on_error`.** `fail` is the default for sends, keeps, sets and emits: the transition halts, and
  the instance ends in `failed`. `continue` is the default for timer actions.

**The actions:**
- **`send_message`** sends a message the WF writes. Its fields are `target`, `meta.msg`,
  `meta.context`, `payload`, `vars` and `menu`.
  - `target` is a node name written in the definition, never a `SY.*` or `RT.*` node. To choose
    among destinations, write one transition per destination, each with its guard.
  - Every message a WF sends is a `user` message, and carries the instance's thread.
  - A message the WF writes carries no person's context. When an agent's answer to it must reach a
    person, write the person's `io` context, kept in `state` from their message, in
    `meta.context.io`.
- **`forward`** sends the event's message, or the one kept under `message`, as it was received. Its
  fields are `target`, `message`, `meta.context`, `vars` and `menu`.
  - The payload goes byte for byte. The meta goes too, the person's ILK included, but not
    `memory_package`.
  - The WF changes only the destination, the thread, the `meta.context` keys it writes, and its
    own block.
  - A forward has no `meta.msg` and no `payload`. It fails when there is no message: a
    `TIMER_FIRED`, an internal event, or a name not kept yet.
- **`keep_message`** (`name`) keeps the event's message, whole, for a later forward. A later keep
  under the same name replaces it. The kept message survives a restart, and goes when the instance
  ends.

  On a transition matched on `TIMER_FIRED`, a `keep_message` and a `forward` without `message` are
  refused.
- **`set_variable`** (`name`, `value`) writes a variable. `value` is a CEL expression written as a
  JSON string: `"'pending'"`, `"42"`, `"state.n + 1"`, `"event.payload.result"`.
  - A bare JSON number or boolean is refused.
  - The state keeps plain JSON.
  - A value has a cost limit; past it, the action fails.
- **`emit_internal_event`** (`meta.msg`, `payload`) queues an event for the same instance. It runs
  right after the transition. At most 100 follow one event; one more fails the instance.
- **Timer actions:** `schedule_timer` (`timer_key`, `fire_in` or `fire_at`, `missed_policy`),
  `cancel_timer` and `reschedule_timer`, all through SY.timer.
  - The minimum is 60 s. A time may be a `$ref`; the minimum is then checked when the timer is set.
  - A timeout is a `TIMER_FIRED` transition, guarded by `event.timer_key == '<key>'`. Every wait for
    an answer needs one.
  - `missed_policy` is `fire`, `drop` or `fire_if_within` (with `missed_within_ms`). It covers
    only a fire that SY.timer itself delivers late.
- **The WF's block.** Every send and forward carries `meta.context.wf`. An agent shows it to its
  model, and never sends it back. A send's `meta.context` never writes `wf`. The block holds:
  - `trail`: the nodes the message went through;
  - `vars`: the action's `vars`;
  - `menu`: the action's menu.

#### 4.2.4 Asking an agent: the menu

A WF never puts routing rules in an agent's role. It hands the agent, with each message, a menu of
what the agent may answer, and the role keeps only the agent's trade.

- **`menu`** goes on a send or a forward to an `AI.*` node, and only to one:

  ```json
  "menu": {
    "help": "The person is buying licenses. A discount above 10% needs a supervisor's approval.",
    "options": [
      {"name": "answer", "description": "You answer the person yourself."},
      {"name": "invoice", "description": "They accepted the quote and ask for the invoice.",
       "params": {"quantity": {"type": "integer"}, "plan": {"type": "string", "enum": ["basic", "pro"]}}},
      {"name": "consult", "description": "A supervisor must approve something before you answer.",
       "params": {"question": {"type": "string", "description": "What you ask, with the data to decide."}}}
    ]
  }
  ```

- **Its limits** (characters are Unicode code points):
  - `help` is optional, up to 600 characters. Use it to give the choice context.
  - 1 to 8 options. A name matches `^[a-z][a-z0-9_]{0,31}$`, is unique, and is never `none`. A
    description holds 1 to 200 characters.
  - Up to 8 params per option. A param's name matches the same pattern. Its type is `string`,
    `boolean`, `integer` or `number`, and its optional description holds up to 200 characters.
  - `enum` is for strings only: 1 to 20 values, each non-empty and unique.
  - The menu is literal (no `$ref`), and a field it does not define is refused.
- **`none`:** the agent node adds it to every menu, with the param `reason`.
- **The answer:**
  - it comes back with the request's `msg`. A forwarded message has none, so match the agent by
    `event_match.src`;
  - the text for the person is in `event.payload.content`;
  - `event.context.evaluation` is `{reply, option, params}`. Read
    `event.context.evaluation.option` and `event.context.evaluation.params.question`. Its `reply`
    is the text the person gets: the model's, or the line that names the agent's files and the
    links of its pages;
  - a guard, a `set_variable` or a `$ref` that names an option or a param that no menu of the
    definition has is refused, and so is a field of the evaluation other than those three. A WF
    reads only the answers to its own menus: to pass a choice on to another WF, send it in a
    payload built from `$ref`s.
- **Params are the person's words passed on by a model.** Guard them before acting on them, such as
  a price below the list price.
- **Errors:** an answer off the menu comes back as a payload of type `error`, code
  `invalid_structured_output`, with no retry. Off the menu means an unknown option, a missing or
  mistyped param, or no JSON. A timeout and a provider failure are errors too. An error's `msg` is
  `<msg>.error` only when the request had a `msg`.
- **Delivering:**
  - to give the person the agent's reply, forward the agent's answer to the IO node. A reply may be
    empty: guard that forward with `event.context.evaluation.reply != ''`;
  - to hand the conversation to another agent, forward the person's kept message.

  An io.slack finds a conversation's place from its thread. Another IO node may need the person's
  `io` context written back (§4.2.3).
- **`meta.context.response_envelope` is refused in a WF:** a WF asks an agent with a menu.

#### 4.2.5 The catch, the default and the cap

- **`catch`** is a top-level list of transitions, as in .NET's try/catch.
  - It is tried in any state, when no transition of the current state takes an event. A catch
    transition with a `target_state` runs as a state's; without one, the instance stays.
  - Its `event_match` may be empty, which matches every event.
  - A state can take `none`, or an error, with its own transitions first.
  - Its actions are sends, forwards, `keep_message`, `set_variable` and `emit_internal_event`.
    Timers belong to states.
  - A `keep_message`, or a forward of the event, needs an `event_match.src`.
- **What reaches the catch:** an agent's `none`, an agent's error, and any event no transition
  took.
- **The default:** without a catch, or when the catch takes nothing, the event is logged locally,
  in the instance's log and the node's log, as `unhandled event`. The instance stays where it was.
- **The cap:** at most 8 messages go to agents between two triggers. A trigger is a message from an
  IO node or another WF, or a timer fire.
  - Past the cap, the send is not made.
  - The instance gets one `AGENT_CALL_CAP` event (a payload of type `error`, code `agent_call_cap`),
    for its transitions or its catch.
  - The instance does not fail.

#### 4.2.6 Patterns

- **Keep a conversation bounded:** a handback delivers the agent's answer, then waits for the
  person's next message. Never ask an agent again with the same message.
- **Give control back to the first agent** in two ways, and recommend both: the agent hands back
  (an option), or an inactivity timer fires.
- **A consultation:**
  1. The agent answers `consult`.
  2. The WF forwards the agent's reply to the person, when it is not empty.
  3. It writes the question (a `$ref` to the param) to the supervisors' io.slack, and waits with a
     timer. Only io.slack maps a reply in the thread of its post back to the instance's thread.
  4. Each supervisor's message in that thread reaches the instance. The WF forwards it to the agent
     with a menu of two options: the message decides the question (`decided`), or decides nothing
     yet (`wait`). Only a decision reaches the person.
- **Answers in any state:** a person who writes twice gets two answers, and a decision may cross an
  answer to the person. The catch acts on an agent's answers wherever they arrive.
- **Late events:** handle a message where it can arrive. Use an internal transition when it must not
  leave the state, and guard the catch with a state variable when it must not move the instance.

### 4.3 IO nodes

Use for external ingress/egress only:
- HTTP/API
- Slack
- WhatsApp
- email
- webhooks

Do not assume an `IO.*` node should do:
- internal routing
- workflow branching
- policy resolution
- generic relay of business traffic

### 4.3.1 Public generated artifacts (`/public/<key>`)

Use this circuit when an `AI.*`, `IO.*`, or `WF.*` producer must expose an already-generated file
through the public edge, including a self-contained interactive HTML report.

The ownership and call path are fixed:

1. The producer writes and promotes the content with the Blob SDK and obtains a `BlobRef`.
2. The same producer calls `SY.admin@<hive>` with
   `RouterDispatcher::send_admin_rpc(AdminCommandRequest { action: "publish_artifact", target: None, ... })`.
3. The router-stamped producer identity is authoritative. Admin resolves its tenant, authorizes and
   records the publication, `IO.blob` curates the bytes, and `SY.edge` serves `/public/<key>`.
4. To revoke the link before it expires, an `AI.*` producer calls `unpublish_artifact` on its own
   publication, with the returned `publication_id` or the key of the link (`/public/<key>`). Over
   the mesh only `AI.*` may: an `IO.*` or `WF.*` publication ends at its expiry or by the operator.

Rules for Archi:

- Publication is behavior implemented inside the producer runtime. It is **not** an HTTP/SCMD
  Admin operation and must not be emitted as an Admin executor step.
- When designing or packaging such a producer, require the runtime to use the existing Blob SDK and
  generic `send_admin_rpc` path. Do not invent an `IO.*` relay, a second public server, or a new edge API.
- `publish_artifact` accepts only `blob_ref`, optional `presentation` (`inline` or `attachment`), and
  optional `expires_in_secs`. It does not accept `tenant_id`, publisher identity, paths, public keys,
  raw headers, SHA-256 filenames, or CSP.
- Query `get_admin_action_help publish_artifact` and `get_admin_action_help unpublish_artifact` for
  the live request and response contracts before specifying implementation details.
- The v1 URL is a bearer capability: anyone with the link can read it until expiry or unpublish.
  There is no public listing, upload endpoint, login, or tenant/session check on each read yet.
- Inline HTML may contain self-contained JavaScript for local interaction. It cannot call network
  APIs; edge enforces the fixed sandbox/CSP policy. Multi-file sites and service workers are out of scope.
- If expiration is not specified, Admin defaults to 24 hours and clamps requested lifetimes to the
  supported range. Do not build independent expiry policy into the producer.

### 4.4 SY nodes

These are system infrastructure:
- `SY.admin`
- `SY.storage`
- `SY.identity`
- `SY.cognition`
- `SY.vault`
- `SY.config.routes`
- `SY.opa.rules`

Normal operator requests should not create or redesign these.

### 4.4.1 Node teardown semantics

- `purge_instance=true` is a real tabula-rasa for orchestrator-owned node state: instance directory, timers, `node_ilk_map`, ILK, and dedicated vault secrets are removed. Routes, VPNs, and taps are not auto-deleted; teardown returns `routing_references` so the operator can clean those deliberately.

### 4.5 Secrets and vault (Model D')

`SY.vault` is the canonical secret backend. Secrets live **entirely** in vault — nodes never receive plaintext secrets through `CONFIG_SET` and never persist them locally. Consumers discover their secret at boot/refresh by querying vault for a `resource_type` (e.g. `openai`, `postgres`, `slack`).

How consumers find their secret:

- Each consumer calls `resolve_resource(resource_type, my_tenant_id)` against vault. Vault matches in this order: secret dedicated to the caller's ILK → secret in the caller's tenant pool → secret in the hive's root tenant pool (universal for system callers).
- "Pool" means a secret stored without an explicit `ilk` — every consumer of that tenant reads the same value. Useful when several SY services share one Postgres or one OpenAI key.
- "Dedicated" means a secret stored with `ilk` set to a specific ILK. Only that exact ILK reads it. Useful when one tenant or one specific node needs its own credential.
- Every `tenant_id` in Model D' follows the canonical `tnt:<uuid>` form. The hive's root tenant (`tnt:00000000-0000-0000-0000-000000000001`, alias `fluxbee`) holds infrastructure-wide secrets (shared Postgres, shared OpenAI key, etc.); client tenants hold their own client-scoped secrets.

Rules for archi when asked about secrets:

- For inspection of who has what: prefer `vault_list` (returns metadata only, no plaintext) and `vault_get_metadata` (full metadata for one key).
- Use `vault_get` only when the operator explicitly asks to reveal or use the plaintext value.
- For writes, use admin actions exposed by `get_admin_action_help`; do not invent vault payload shape. The contract changed in Model D' — read the live contract instead of remembering older payload shapes.
- **Never** suggest storing `vault://<key>` references in node config: that path was removed. Node configs no longer carry any secret-bearing field.
- **Never** suggest `metadata.owner_ilk` in `vault_put`: it is rejected. Use `metadata.owner_node` (a friendly L2 name like `SY.architect`) and admin will resolve the ILK; or omit it entirely to publish the secret to the pool.
- `metadata.resource_type` is mandatory on every `vault_put` (it's the discovery key). `metadata.tenant_id` is optional and defaults to the hive's root tenant when omitted (infrastructure-wide secret).
- If a node reports `missing_secret`, inspect its live `CONFIG_GET` (`/control/config-get`) — look at `contract.resources[]` to see which `resource_type` each consumer is waiting for. SY.vault broadcasts `VAULT_SECRET_CHANGED` on every put/rotate/delete; storage and identity react with `exit(0)` (systemd restart), admin/architect/cognition refresh in-memory. If a consumer stays missing after a vault_put, check the broadcast was received (vault logs `vault secret changed broadcast sent`).
- Supported `resource_type` values: `postgres`, `mysql`, `redis`, `mongodb`, `openai`, `anthropic`, `gemini`, `mistral`, `cohere`, `perplexity`, `google_calendar`, `gmail`, `google_drive`, `google_sheets`, `google_docs`, `google_slides`, `google_cloud`, `microsoft_graph`, `outlook_email`, `outlook_calendar`, `teams`, `sharepoint`, `slack`, `discord`, `hubspot`, `salesforce`, `linked_helper`, `github`, `gitlab`, `jira`, `linear`, `notion`, `stripe`, `twilio`, `sendgrid`, `smtp`, `imap`, `aws`, `azure`, `s3`, `webhook`, `bearer_token`, `api_key`, `oauth_bundle`, plus free-form custom strings (lowercase, snake_case, max 64 chars).
- Postgres value contract: store **credentials + host only** (no `dbname`) — each consumer applies its own dbname (`fluxbee_storage`, `fluxbee_identity`, etc.). Example value: `{"postgres_url": "postgresql://user:pass@host:5432"}`.

---

## 5. Choosing Between Routing, Workflow, Taps, and OPA

This is the section Archi was missing most often.

### 5.1 Use routing when

- the destination rule is simple and deterministic
- forwarding is based on prefix or next-hop hive
- you are connecting already-known traffic patterns between nodes/hives

Examples:
- forward `AI.specialist.*` traffic from `motherbee` to `worker-220`
- send a prefix to another hive

### 5.2 Use workflow when

- a message should trigger several downstream effects
- one inbound interaction should fan out to multiple nodes
- ordering matters
- one node response should trigger another side effect
- the operator is describing a business flow, not just a network path

Examples:
- inbound from `IO.api.support@motherbee` goes to `AI.support@motherbee`
- the same interaction is mirrored to `IO.slack.support@motherbee`
- AI response should also be echoed to Slack

That is usually **workflow-orchestration territory**, not OPA-first.

### 5.3 Use a tap when

- a copy of a unicast delivery should also reach an additional node
- the secondary copy carries no business decision (no branching, no transformation, no state)
- the primary path should not change

Examples:
- audit copy of traffic between two named nodes to a third
- observer node that receives a copy of every message without participating in the flow

Taps are router-level (`add_tap`); the copy is fire-and-forget and carries `meta.via_tap=true` so it never cascades.

### 5.4 Use OPA when

- destination is policy-driven
- tenant/identity/rule evaluation determines target
- the request is about governance or decision policy rather than message choreography

Examples:
- route by tenant policy
- route by identity restriction
- route by role/capability decision when `dst` is not explicit

### 5.5 Anti-patterns

Do not:
- use OPA as a substitute for deterministic workflow orchestration
- invent a new `IO.echo` node by default when workflow/routing/taps already model the problem
- route normal application behavior through `SY.config.routes` as if it were a business node

---

## 6. Pipeline Roles

### 6.1 Designer

Role: produce a `solution_manifest` v2 describing desired state.

Valid `desired_state` sections:
- `topology`: `hives`, and `vpns`, each an SY.config.routes VPN rule `{hive, pattern, match_kind,
  vpn_id, ownership}` (`vpn_id` a number, `match_kind` EXACT, PREFIX or GLOB, PREFIX when omitted;
  optional `priority`, 100 when omitted), one rule per pattern on a hive. A rule puts the nodes of
  its hive whose names match into that VPN, and nodes reach only nodes of their own VPN (system
  nodes excepted): declare VPN rules only to isolate a group, with the same `vpn_id` for every
  member on each hive where it runs
- `runtimes`
- `nodes`
- `routing`
- `wf_deployments`
- `opa`: one user OPA policy for every hive (`rego_source`, `entrypoint`, `ownership`), no hive. If several solutions declare one, the last one applied wins.

Not supported here:
- `policy`
- `identity`

Ownership rules:
- `solution` means the pipeline creates and updates it
- `external` means the reconciler must not touch it
- omitted defaults to conservative external behavior
- a manifest version owns what it declares only once a plan made from it has run

A manifest is refused when it breaks what the system nodes accept:
- routes: `action` FORWARD (with `next_hop_hive`) or DROP, case-sensitive; one route per prefix on
  a hive
- nodes: never `WF.*` (SY.wf-rules creates them) nor core nodes (`SY.*`, `RT.*`); runtime names
  are lowercase letters and digits in dot-separated parts (`ai.generic`, `ai.support`)

The reconcile never removes a node, route or VPN rule a solution stops declaring: SY.orchestrator
and SY.config.routes record no owner for them. It removes a solution's workflows and user policy,
because Archi knows which solution deployed them. Remove an extra node, route or VPN rule through
SY.admin's API (`kill_node`, `delete_route`, `delete_vpn`).

### 6.2 RealProgrammer

Role: materialize exactly one artifact bundle per `build_task_packet`.

It builds one artifact kind, `runtime_package`; a packet of any other kind is refused. A workflow is
not a programmer artifact: its definition is the manifest's `wf_deployments`, deployed with
`wf_rules_compile_apply`; the user policy is the manifest's `opa`, deployed with
`opa_compile_apply`.

Important:
- `package.json.name` is lowercase
- `runtime_base` must exist in known context

### 6.3 PlanCompiler

Role: translate a `delta_report` into a static `executor_plan`.

Rules:
- all step args are static
- reads happen during plan generation via `query_hive`
- call `get_admin_action_help` for each action used
- do not invent actions outside compiler-class mappings
- the args of `opa_compile_apply`, `wf_rules_compile_apply`, `wf_rules_delete`, `add_route`,
  `add_vpn` and a replace's `delete_route` / `delete_vpn` are pinned to the manifest afterwards:
  write them exactly as the declared resource

### 6.4 Archi

Role: coordinate, choose the right path, ask at most one necessary clarification, and present the result.

Archi should:
- use reads for inspection
- use `fluxbee_plan_compiler` for clear mutations
- use `fluxbee_start_pipeline` for broad desired-state design
- not mutate directly

### 6.5 Archi UI sections (rail + Messages)

The Archi web UI is a single-page app served by `SY.architect@<hive>`. Two top-level sections are reachable via the left rail (icon column on the far left of the page); navigation is hash-based:

| Hash | Section | Purpose |
| --- | --- | --- |
| `#/archi` (default) | Archi chat | The original operator/impersonation chat workspace, `Publish Software` panel, and chat history. |
| `#/messages` | Messages | Read-only viewer of the system-wide ILK message log persisted by storage in `storage_inbox`. |

Rules for the rail:

- Click a rail icon to navigate; clicking the already-active icon for `messages` forces a refresh of that section without reloading the page.
- Each section owns its own internal layout. Switching sections does not unmount the other; it only toggles `[hidden]`. SSE streams in inactive sections are stopped on deactivate.
- Adding new sections later (e.g. cognition log) is one rail entry plus one `<section data-section="...">` block; do not introduce parallel layout systems.

**Messages section requirements:**

- archi must have `config.storage.messages_db_url` configured (`SCMD POST /architect/control/config-set`). Until then the panel shows a centered "messages_db_url not configured" card with copy-pasteable SCMD examples.
- The connection string must point to storage's database (`fluxbee_storage`), not the base `fluxbee`. archi only runs `SELECT` against `storage_inbox`, so a separate read-only role is optional.
- Filters (`Window`, `Only errors`) are client-side wrappers around `GET /api/messages` query params. The SSE tail (`GET /api/messages/stream`) is unfiltered — incoming rows that do not match the active filters are simply not appended.

---

## 7. Common Mutation Patterns

### 7.1 New node from existing runtime

Use when runtime already exists and is materialized.

Pre-read:
- `query_hive(list_runtimes)`
- optionally `query_hive(list_nodes)` or node status

Then:
- `run_node`

### 7.2 Publish runtime then create node

Use when runtime does not yet exist or is not materialized.

Then:
1. `publish_runtime_package`
2. `run_node`

### 7.3 Update node config

Use:
- `node_control_config_get` first when contract/version matters
- then `node_control_config_set`
- then restart only if required

Restart bias:
- `AI.*` and `IO.*`: usually hot apply
- `WF.*` and `SY.*`: usually restart unless contract says otherwise

### 7.4 Routing change

Use:
- `add_route`
- `delete_route`
- `delete_route` then `add_route` for replace

### 7.5 Workflow deployment

Use:
- `wf_rules_compile_apply`

Use this when the operator is describing:
- branching
- fan-out
- echo/mirror
- chained side effects
- deterministic multi-node behavior

### 7.6 OPA deployment

Use:
- `opa_compile_apply`

Use this only for policy-driven routing/enforcement needs.

---

## 8. Important Args and Conventions

### 8.1 `run_node`

Canonical required shape:

```json
{
  "hive": "motherbee",
  "node_name": "AI.coa@motherbee",
  "runtime": "ai.generic",
  "runtime_version": "current",
  "tenant_id": "tnt:<uuid>"
}
```

Notes:
- `node_name` uses type prefix + `@hive`
- `runtime` is lowercase and has no `@hive`
- `tenant_id` is root-level and required for `AI.*` / `IO.*` first spawn; do not bury it inside `config`
- an `IO.*` node other than `IO.cloud`, `IO.blob` and `IO.web` (the core IO nodes) never runs in the hive's root tenant `tnt:00000000-0000-0000-0000-000000000001`: `run_node` with that tenant fails with `TENANT_ROOT_NOT_ALLOWED` (§8.2)
- For `WF.*`, do not use `runtime: "wf.engine"` in `run_node`; first publish/apply the workflow through `wf_rules_compile_apply` and spawn the resulting concrete runtime (`wf.<workflow_name>`).

### 8.1.1 Tenant discovery before first spawn

When the operator says things like:
- "use the same tenant as `AI.sales@motherbee`"
- "associate this tenant to that sponsor"
- "create the client under the same sponsor as another tenant"

do **not** infer tenant data from:
- inventory
- `list_nodes`
- runtime names
- channel names

Use the identity tenant read surface first:
- `list_tenants`
- `get_tenant`
- `create_tenant`
- `set_tenant_sponsor`

Rules:
- executor plans use flat `step.args`; do not wrap tenant mutation fields inside `body`
- use `list_tenants` to discover candidate root/default tenants, sponsors, or client tenants
- use `get_tenant` when one exact tenant id is already known or when you need the resolved sponsor record
- use `create_tenant` to create a new admin/company tenant or a new client tenant
- create an admin/company tenant as a root tenant with no sponsor
- create a client tenant with `sponsor_tenant_id` pointing to the admin/company tenant
- when the client tenant depends on a just-created admin/company tenant in the same executor plan, use a formal output reference: `"$steps.s1.payload.tenant_id"`
- never use non-executable placeholders like `<tenant_id_from_s1>`
- in tenant reads, treat `is_root=true` as a root/default tenant candidate and `is_sponsor=true` as a tenant that currently sponsors child tenants
- if the task says "same tenant as an existing node", prefer reading the node config/live config to find the exact `tenant_id`, then validate that tenant with `get_tenant`
- run client-facing `AI.*` / `IO.*` nodes with the client `tenant_id`, not the sponsor/admin tenant, unless the operator explicitly asks for an internal admin node, which runs in the admin/company tenant
- run an `IO.*` node in the tenant the operator names, or in the one you read from an existing node (as above); if there is none, block with `missing_fields:["tenant_id"]`. Never pick the hive's root tenant `tnt:00000000-0000-0000-0000-000000000001` for it, even when it is the only `is_root=true` tenant: only the core IO nodes `IO.cloud`, `IO.blob` and `IO.web` run there
- if no reliable tenant can be found, block and ask for exactly one missing tenant clarification

### 8.1.2 Agent cognitive definition

`ai.generic` agents boot with operational config first and receive their cognitive "alma" later through identity.

Use:
- `list_agent_assets` to inspect existing immutable role/skill/handbook/personality assets
- `get_agent_asset` to read the real JSON content of an asset by hash
- `create_agent_role_asset` to create the role asset
- `create_agent_skill_asset` to create each skill asset
- `create_agent_handbook_asset` to create each handbook/reference asset
- `create_agent_personality_asset` to create the personality asset (nationality, languages, timezone, biography). At most one per agent.
- `delete_agent_asset` to delete an unused local asset by hash when the operator explicitly asks
- `get_ilk` to resolve the target agent ILK
- `set_ilk_definition` to apply role/skill/handbook (and optionally personality) hashes
- `node_control_config_get` to verify the running agent loaded or rejected the definition

Rules:
- design roles, skills and handbooks against the agent tools (§4.1.1): read their catalog first; a role names no channel and no tool, and no asset promises a tool the catalog does not list
- when the operator asks to inspect or show an asset, read it with `get_agent_asset`; do not reconstruct it from chat memory
- asset builder tools only write content-addressed files under `blob://agent-assets/<hash>.json`
- deleting an asset does not update ILK definitions; first remove or replace the hash from any active definition if the asset is still referenced
- `set_ilk_definition` updates identity only; it does not create blob assets
- role, skill, handbook, and personality assets must already exist in `blob://agent-assets/<hash>.json`
- identity stores hashes only; never put prompt text, skill instructions, handbook content, or personality biography directly in `set_ilk_definition`
- OPA/routing can match `role_hash`, `skill_hashes`, `handbook_hashes`, and `personality_hash`. Matching is hash-only — OPA cannot read blob contents. A flat projection of personality `system_fields` (timezone/country_code/primary_language) is *not* exposed in `data.identity[ilk]` today; route by the hash itself when matching personality.
- when an operator asks to make an agent "Argentinian", "Spanish-speaking", "based in Mendoza", or describes nationality/timezone/language traits, route through `create_agent_personality_asset` (or reuse an existing personality hash from the catalog). Do **not** stuff these traits into the role asset — the role describes function, the personality describes person.
- do not use legacy `roles` or `capabilities`; they are retired

### 8.2 IO tenant naming

An IO channel node (`io.api`, `io.slack`, `io.wapp`, `io.linkedhelper`, ...) is launched by the
tenant that needs it, or by an operator for that tenant, and runs with that tenant. None runs in the
hive's root tenant `tnt:00000000-0000-0000-0000-000000000001`: only the core IO nodes run there,
`IO.cloud`, `IO.blob` and `IO.web`. The base install starts `IO.cloud` and `IO.blob` (operator
decision 2026-10-02); `IO.web@motherbee` starts only when the operator launches it. The
orchestrator refuses any other IO node in the root tenant with `TENANT_ROOT_NOT_ALLOWED`. Every
binding is its own instance of its runtime:

- `IO.slack.T126@motherbee` — one Slack binding, named after its tenant or workspace token
- `IO.wapp.acme@motherbee` — one WhatsApp number of the tenant `acme`

Use the same short tenant token consistently across related nodes.

---

## 9. Compiler-Class Mapping

PlanCompiler should map delta operations like this:

| compiler_class | Required steps |
| --- | --- |
| `NODE_RUN_MISSING` | `run_node` |
| `NODE_CONFIG_APPLY_HOT` | `node_control_config_set` |
| `NODE_CONFIG_APPLY_RESTART` | `node_control_config_set` -> `restart_node` |
| `NODE_RESTART_ONLY` | `restart_node` |
| `NODE_KILL` | `kill_node` |
| `NODE_RECREATE` | `kill_node` -> `run_node` |
| `RUNTIME_PUBLISH_ONLY` | `publish_runtime_package` |
| `RUNTIME_PUBLISH_AND_DISTRIBUTE` | `publish_runtime_package` |
| `RUNTIME_DELETE_VERSION` | `remove_runtime_version` |
| `HIVE_CREATE` | `add_hive` |
| `VPN_CREATE` | `add_vpn` |
| `VPN_DELETE` | `delete_vpn` |
| `VPN_REPLACE` | `delete_vpn` -> `add_vpn` |
| `ROUTE_ADD` | `add_route` |
| `ROUTE_DELETE` | `delete_route` |
| `ROUTE_REPLACE` | `delete_route` -> `add_route` |
| `WF_DEPLOY_APPLY` | `wf_rules_compile_apply` |
| `WF_DEPLOY_RESTART` | `wf_rules_compile_apply` -> `restart_node` |
| `WF_REMOVE` | `wf_rules_delete` |
| `OPA_APPLY` | `opa_compile_apply` |
| `OPA_REMOVE` | `opa_clear` |

Blocked classes: do not emit executor steps for `NOOP`, `SCOPE_NOT_SUPPORTED`, or `BLOCKED_*`.

---

## 10. Good Usage Rules

### 10.1 Ask less, infer more

Ask one short clarification only when a critical input cannot be inferred safely.

For read-only diagnostics, inspect before asking:
- If the operator names a family, count, role, or partial description instead of exact resource names, read inventory/list endpoints and resolve the concrete targets yourself.
- If the resulting candidates are clear, continue with persisted config, live config, status, runtime, route, or vault metadata reads as needed in the same turn.
- When state looks unexpected (a node changed, appeared, or broke "on its own"), read `list_recent_commands` first — the admin command audit log answers "what was done to this backend lately, by whom" (redacted params, all origins) before you reverse-engineer it from infra.
- Do not ask for permission to read non-secret system state. Ask only when the live state still leaves multiple unsafe interpretations or no viable candidate.

Examples of acceptable one-question clarifications:
- which hive?
- which tenant?
- is this policy-driven or deterministic workflow behavior?

Before asking "which tenant?", try these reads first when applicable:
- `get_node_config` or `node_control_config_get` on an existing tenant-scoped node
- `list_tenants`
- `get_tenant`

### 10.2 Prefer existing platform primitives

Prefer:
- existing runtimes
- existing nodes
- workflow deployment
- routing

Before inventing:
- new relay nodes
- new OPA bundles
- new infra nodes

### 10.3 Distinguish clear mutation from broad design

Use `fluxbee_plan_compiler` when the operator clearly knows what should change.

Use `fluxbee_start_pipeline` when the operator is effectively asking:
- what should the topology be?
- how should this distributed behavior be designed?
- what resources are needed to realize a higher-level solution?

### 10.4 Recover a Blocked Pipeline

If a pipeline is blocked, do not start a second pipeline blindly.

Use the blocked run recovery actions:

- `retry` — return the blocked run to the last confirmation checkpoint. This is useful after execution or verify failures where the plan already exists.
- `restart_from_design` — close the blocked run and redesign from scratch using the same task. This is the correct default for design/schema failures because there is no useful execution checkpoint to re-enter.
- `discard` — close the blocked run without retrying.

When `fluxbee_start_pipeline` reports `blocked_run_pending`, the actionable tool is `fluxbee_pipeline_action`. The structured response includes `next_tool` and `allowed_actions`.

---

## 11. What Not to Do

- Do not treat SCMD as part of pipeline reasoning.
- Do not create new `SY.*` nodes as part of ordinary app work.
- Do not use OPA as first choice for deterministic business routing.
- Do not use `IO.*` as default internal relay nodes.
- Do not create new mixed-case runtime names.
- Do not assume `motherbee` if hive is known to be something else.
- Do not publish a runtime when the operator only asked for inspection.

---

## 12. Source References

This handbook is the concise operational layer for Archi. Broader platform detail lives in:

- [reference/architecture.md](../reference/architecture.md)
- [reference/routing-and-policy.md](../reference/routing-and-policy.md)
- [reference/identity.md](../reference/identity.md)
- [nodes/](../nodes/): one page per system node; [nodes/sy-admin.md](../nodes/sy-admin.md) lists every
  admin action
- [io-blob-spec-v1.md](../io-blob-spec-v1.md)

If this handbook conflicts with live action contracts, the live action contracts from `get_admin_action_help(...)` win for request shape, and this handbook wins for high-level planning intent.
