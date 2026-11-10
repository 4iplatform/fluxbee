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

**A join over a password asks the operator one thing first** (FINDINGS A-165). A join that logs in
with `ssh_password` revokes its own access at its end, and nothing else closes password logins on
the box. So with `ssh_password` and no `ssh_key`, `add_hive` requires `harden_ssh`, and refuses the
request without it:
- `true` turns password logins off at the join's end. Afterwards only the hypervisor's console,
  or a key of the operator's own, gets in.
- `false` leaves them on, and the join's answer warns when it logged in over the password.

Ask the operator which, with that consequence, and never choose for them. A join with `ssh_key`
needs no such answer: next to it, a password only feeds `sudo`. A join re-run after one that failed
late finds the worker running and touches no SSH: its answer says so, and `harden_ssh` is not
applied.

---

## 4. Node Family Guidance

### 4.1 AI nodes

Use when the work is conversational, interpretive, or agentic.

Every AI node is an instance of the `ai.generic` runtime; there is no other AI runtime, and no AI
node boots with the base install. To make one:
- `run_node` with `runtime: ai.generic` and the node's tenant;
- `CONFIG_SET` its operational config: `behavior.kind: ai_chat`. Optional:
  - `behavior.provider`, `openai` or `anthropic`, and `behavior.model`: without them the node runs
    on the installation's `ai.default_provider` and on that provider's model
    (`ai.providers.<provider>.model` in the installation file, the same on every hive). Name them
    only when the agent needs its own, and a model always of the node's provider. Naming a
    provider other than the default needs its `behavior.model` too, unless the installation file
    lists a model for that provider. `CONFIG_SET` refuses a config whose
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
The SY nodes (Archi, cognition, the frontdesk) keep the hive's provider. The installation's provider
and models change only with `set_installation_ai` (§4.4.3).

When an agent's provider gives no answer (no key for it, an error status after the AI SDK's
retries, no connection, none in time, an answer with no text, or the model's refusal), the person
gets one fixed text: *"There is a problem with the AI provider this agent uses, so it could not
answer. Please contact your administrator."* A person who reports it points to the agent's key or
its provider: the node's log has the error, `AI runtime request failed`. `missing AI api key` is
a tenant with no key for the agent's provider (`vault_list`); otherwise `provider_status` says
which (401 or 403: the key; 429: its rate or quota; 5xx: the provider). A WF that asks the
agent with a menu gets the error payload instead, with its code, and the same text in `reply`: its
catch sends it to the person (§4.2.4, atom 6). A program that asks under a response envelope gets
the same payload.

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
- **The atoms:** §4.2.6 has each mechanism as a small definition. The seeds `wf_node_with_workflow`
  and `workflow_definition` hold the basic loop, atom 1 with a catch, and §4.2.7 says how to check,
  deploy and operate a WF.

#### 4.2.1 The definition

SY.wf-rules checks it (`go/pkg/wfcel`), and an error names the exact path.

- **Its fields:** `wf_schema_version` (`"1"`), `workflow_type`, `description`, `start_from`,
  `input_schema` (the JSON Schema of the first message's payload), `initial_state`,
  `terminal_states`, `states` and, optionally, `catch` and `idle_timeout`. Any other field is
  refused. The language no longer has `meta.type`, `target_ref` or `thread_id_ref`.
- **`idle_timeout`** closes a conversation that went quiet: no event for that long, and no timer
  pending. A duration from `1h` to `7d`; `4d` when the definition leaves it out. It closes as a
  cancel does: the current state's exit actions, then the `cancelled` state's entry actions, if
  the definition has that state. The next message on the thread starts a new instance.
- **`start_from`** names the nodes whose messages start an instance: IO nodes or WFs, each with its
  hive. Never an `AI.*` node, because an agent only answers, and never a `SY.*` or `RT.*` node.
- **One instance per conversation.** An instance lives on the thread of the message that started
  it.
  - Only the nodes the definition names drive the instance: those of `start_from`, those of an
    `event_match.src`, and the targets of its sends, the catch's included. SY.timer drives it too,
    for its own timers. Anyone else is dropped.
  - A `start_from` node's message on a thread that no instance holds starts a new instance. A
    message with no thread starts nothing: a WF never makes up a thread.
- **Nothing from an IO node ends in silence.** An IO node's message that nothing takes gets
  `NOT_TAKEN`, with its reason. io.slack shows it to whoever wrote; the other IO nodes ignore it
  for now. The reasons:
  - `no_conversation`: no instance holds its thread, and it starts none;
  - `no_thread`: it has no thread;
  - `invalid_input`: `input_schema` refuses it;
  - `not_expected`: the instance took nothing, neither a transition nor the catch;
  - `unavailable`: the WF node cannot use its instance store, and takes no message until it can.
    Its status says why (§4.2.7).

  Another sender gets it only for a named request. A sender the definition does not name gets
  nothing.
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
  `meta.context`, `payload`, `vars`, `menu` and `subject`.
  - `target` is a node name written in the definition, never a `SY.*` or `RT.*` node. To choose
    among destinations, write one transition per destination, each with its guard.
  - Every message a WF sends is a `user` message, and carries the instance's thread.
  - A message the WF writes to an agent or a WF carries the instance's person: the `src_ilk` and
    `ich` that the messages of the person's IO node share: the IO node the person wrote from. On a thread several people write on,
    it carries none. Its writer is the WF (below): the agent reads it as the WF's, not as the
    person's words.
  - A message the WF writes to an IO node never names the person as its sender: the router's stamp
    names the WF. To the person's own IO node (the one whose message started the instance; another IO node of
    `start_from`, the team's say, is not the person's), it names the person as
    `dst_ilk`, and the person's channel as `ich`.
  - It carries no `io` context: when an agent's answer to it must reach a person, write the
    person's `io` context, kept in `state` from their message, in `meta.context.io`.
- **`forward`** sends the event's message, or the one kept under `message`, as it was received. Its
  fields are `target`, `message`, `meta.context`, `vars`, `menu` and `subject`.
  - The payload goes byte for byte. The meta goes too, the `src_ilk` of whoever speaks included,
    but not `memory_package`.
  - The WF changes only the destination, the thread, the `meta.context` keys it writes, its own
    block, and, toward an IO node, whom the message names:
    - to the person's own IO node it names the person, as `dst_ilk` and `ich`, whoever the
      message answered: an agent's answer to a supervisor reaches the person as the person's;
    - to any other IO node, the team's say, it names no one, no `dst_ilk` and no `ich`, as a
      send: whoever the message was said to is not there. Cognition takes such a line as said to
      the last person who wrote through that IO node: sales' answer to a team member, sent back
      to the team, reads as said to that team member.
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
  - A definition schedules at most 10 timer keys: the WF keeps one timer per conversation and key,
    so its keys are the most timers a conversation holds at once. A logic that needs more is two
    WFs.
  - A timeout is a `TIMER_FIRED` transition, guarded by `event.timer_key == '<key>'`. Every wait for
    an answer needs one.
  - `missed_policy` is `fire`, `drop` or `fire_if_within` (with `missed_within_ms`). It covers
    only a fire that SY.timer itself delivers late.
- **`subject`,** on a send or a forward to an `IO.*` node only, tells the IO node where the message
  stands. The WF knows the subject; the IO node knows its places and places the message.
  - `new`: the message opens a subject;
  - `same`: it continues the subject of the WF's last message on this conversation to that node;
  - `end`: it ends that subject.

  io.slack keeps a subject in one Slack thread: `new` opens a post, `same` goes into its thread, and
  `end` closes the post's buttons and marks it closed. Without `subject`, the IO node does as always.
- **The WF's block.** Every send and forward carries `meta.context.wf`. An agent shows it to its
  model, and never sends it back. A send's `meta.context` never writes `wf`. The block holds:
  - `vars`: the action's `vars`, the facts the WF gives the receiver. io.slack shows them as a line
    under the post;
  - `menu`: the action's menu;
  - `subject`: the action's subject.
- **The message's trace.** Every message of a solution carries `meta.context.trace`: one entry for
  each node it went through in its cycle, from where it started (a person's message, a timer) to
  where it ends, with the time, the source, the destination and what the node did. Each node writes
  its own entry. The WF's entry names the actions of its step, the one that sent the message first
  (`["send_message", "schedule_timer"]`). An agent reads the trace before the message. It is for
  debugging and the log: `turns` keeps every message with it.
- **Who wrote it.** Every message also names its writer, `meta.context.writer`: the person a text
  of theirs comes from, an agent for its answer, the WF for its own `send_message`. A `forward`
  keeps the writer of what it passes on. An agent's memory names each line by its writer, and
  its cognition analyzes only what a person wrote, or what the agent answered a person. A
  definition never writes `writer`.

#### 4.2.4 Asking an agent or a person: the menu

A WF never puts routing rules in an agent's role. It hands the agent, with each message, a menu of
what the agent may answer, and the role keeps only the agent's trade. A person gets the same menu
through their IO node.

- **`menu`** goes on a send or a forward to an `AI.*` node, or to an `IO.*` node for the people
  there:

  ```json
  "menu": {
    "help": "The person is buying licenses. A discount above 10% needs a supervisor's approval.",
    "options": [
      {"name": "answer"},
      {"name": "invoice", "description": "They accepted the quote and ask for the invoice.",
       "params": {"quantity": {"type": "integer"}, "plan": {"type": "string", "enum": ["basic", "pro"]}}},
      {"name": "consult"}
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
- **Known options.** Write a known option by its name alone, `{"name": "consult"}`, and the compiler
  writes its description and params. A known name written with any other field is refused. An
  option of the definition's own, such as `invoice`, is written whole. What an option does is
  always the definition's: its transitions.

The known options:

<!-- known-options:start -->
| Name | Description | Params |
|---|---|---|
| `answer` | Your reply goes to the person. | |
| `consult` | A supervisor must approve something before you answer. Your reply tells the person you will check. | `question` (string): What you ask the supervisor, with the data to decide. |
| `decided` | The message decides the question: your reply tells the person the decision. | |
| `wait` | The message decides nothing yet, such as a remark among supervisors: your reply is empty. | |
| `answer_team` | The message asks you something: your reply goes to the team, not to the person. | |
| `handover` | A person of the team must take the conversation. Your reply tells the person; the summary goes to the team. | `summary` (string): What the team needs to take over: the person by name, what they want, what was done. No internal ids. |
| `handback` | Hand the conversation back to the agent. | `note` (string): What the agent must know: what was done and agreed. |
| `done` | Your part is done: your reply goes to the person, and the conversation goes back to the agent that had it. | |
<!-- known-options:end -->

- **`none`:** the agent node adds it to every menu, with the param `reason`.
- **A menu for people,** sent to an IO node: io.slack shows each option as a button on the post, and
  an option's params as a form, all of them required.
  - A click comes back as a message from whoever clicked, on the conversation's thread, with
    `event.context.evaluation` = `{option, params}`, and no `reply`.
  - Its text is the option's name, then each param as `name: value`, one per line. Forward it to an
    agent to pass it on in the person's own words.
  - Match a click by its IO node, `event_match.src`, and tell it from a text with
    `has(event.context.evaluation)`. An agent's answer has an evaluation too, and a known option
    may be on both menus.
  - A post's buttons stay until a new menu or a new post (`subject: new`) on the conversation, or
    an `end`, closes them. A click after that gets "This is no longer open." from io.slack.
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
  mistyped param, or no JSON. A timeout and a provider failure are errors too. A provider failure's
  error also carries `reply`, the operator's text for the person (§4.1): the catch sends it with
  `{"$ref": "event.payload.reply", "default": "<the WF's own text>"}` (atom 6). An error's `msg`
  is `<msg>.error` only when the request had a `msg`.
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
  An IO node's message also gets `NOT_TAKEN` (`not_expected`, §4.2.1).
- **The cap:** at most 8 messages go to agents between two triggers. A trigger is a message from an
  IO node or another WF, or a timer fire.
  - Past the cap, the send is not made.
  - The instance gets one `AGENT_CALL_CAP` event (a payload of type `error`, code `agent_call_cap`),
    for its transitions or its catch.
  - The instance does not fail.

#### 4.2.6 The atoms

A solution is built from atoms. Each atom is one mechanism, written as the smallest definition that
runs it, and each compiles as it stands: a test compiles every definition in this handbook.

- **Combine them.** Take the states and transitions a mechanism needs, under the solution's names.
- **Never copy a whole case.** A case combines atoms: Case 4's handover is atom 4 with a second
  timer, "nobody took it".
- **The names are examples:** `IO.api.shop@worker1` is the person's IO node, `AI.sales@worker1` an
  agent, and `IO.slack.team@motherbee` the team's channel.

**Idioms the atoms share:**
- **A `start` state.** The message that starts an instance is the event of the initial state's entry
  actions. A state that a timer can enter never forwards the event on entry: a forward of a
  `TIMER_FIRED` is refused. So the first message is forwarded in `start`, which moves on with an
  internal event, and later messages are forwarded in transitions.
- **A reply may be empty.** Every forward of an agent's reply is guarded with
  `event.context.evaluation.reply != ''`. An option that changes the state has two transitions: one
  with a reply, one without.
- **A send to an agent whose answer must reach the person** writes the person's `io` context, kept
  in `state.io` from their first message.
- **Every instance ends.** An `idle` timer closes a conversation nobody writes in, as soon as the
  solution needs. Without one, the WF closes a conversation that went quiet, with no timer pending,
  after its `idle_timeout`.

**Writing texts:**
- A text never repeats a value the definition holds elsewhere. Say "a while", never "30 minutes":
  the timer may change, and the text would not.
- What goes to the team names the person by name, never by an internal id (an ILK, an ICH, a node's
  name).

**Atom 1, a person talks with an agent.** For one agent that serves the person.

```json
{
  "wf_schema_version": "1",
  "workflow_type": "atom.answer",
  "description": "A person talks with one agent: each message of the person goes to AI.sales with a menu, and sales' reply goes to the person. A while with no message closes the conversation.",
  "start_from": ["IO.api.shop@worker1"],
  "input_schema": {"type": "object", "required": ["type"], "properties": {"type": {"type": "string"}}},
  "initial_state": "talking",
  "terminal_states": ["closed", "failed"],
  "states": [
    {
      "name": "talking", "description": "The person's messages go to sales, and its replies to the person.",
      "entry_actions": [
        {"type": "forward", "target": "AI.sales@worker1", "menu": {"options": [{"name": "answer"}]}},
        {"type": "schedule_timer", "timer_key": "idle", "fire_in": "30m"}
      ],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "idle"}],
      "transitions": [
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.sales@worker1", "menu": {"options": [{"name": "answer"}]}}, {"type": "reschedule_timer", "timer_key": "idle", "fire_in": "30m"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'idle'", "target_state": "closed", "actions": []}
      ]
    },
    {"name": "closed", "description": "The conversation ended.", "entry_actions": [], "exit_actions": [], "transitions": []},
    {"name": "failed", "description": "An action failed.", "entry_actions": [], "exit_actions": [], "transitions": []}
  ]
}
```

**Atom 2, the conversation goes to another agent and comes back.** For a part of the conversation
that is another agent's trade. The agent that holds it gets every message of the person, with the
facts it needs in `vars`. It gives the conversation back with `done`, and an `idle` timer gives it
back too.

```json
{
  "wf_schema_version": "1",
  "workflow_type": "atom.redirect",
  "description": "Sales holds the conversation and may hand it to invoice, which holds it until it says done or a while passes with no message. Then sales holds it again.",
  "start_from": ["IO.api.shop@worker1"],
  "input_schema": {"type": "object", "required": ["type"], "properties": {"type": {"type": "string"}}},
  "initial_state": "start",
  "terminal_states": ["closed", "failed"],
  "states": [
    {
      "name": "start", "description": "The first message is kept and goes to sales.",
      "entry_actions": [
        {"type": "keep_message", "name": "person"},
        {"type": "forward", "target": "AI.sales@worker1", "menu": {"options": [{"name": "answer"}, {"name": "invoice", "description": "They accepted a quote and ask for the invoice.", "params": {"order": {"type": "string", "description": "What they buy, how many, and at what price."}}}]}},
        {"type": "emit_internal_event", "meta": {"msg": "STARTED"}}
      ],
      "exit_actions": [],
      "transitions": [
        {"event_match": {"msg": "STARTED"}, "guard": "true", "target_state": "with_sales", "actions": []}
      ]
    },
    {
      "name": "with_sales", "description": "Sales holds the conversation. Its reply goes to the person; invoice hands the conversation to invoice, with the order and the person's last message.",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "idle", "fire_in": "30m"}],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "idle"}],
      "transitions": [
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "keep_message", "name": "person"}, {"type": "forward", "target": "AI.sales@worker1", "menu": {"options": [{"name": "answer"}, {"name": "invoice", "description": "They accepted a quote and ask for the invoice.", "params": {"order": {"type": "string", "description": "What they buy, how many, and at what price."}}}]}}, {"type": "reschedule_timer", "timer_key": "idle", "fire_in": "30m"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'invoice' && event.context.evaluation.reply != ''", "target_state": "with_invoice", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}, {"type": "set_variable", "name": "order", "value": "event.context.evaluation.params.order"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'invoice'", "target_state": "with_invoice", "actions": [{"type": "set_variable", "name": "order", "value": "event.context.evaluation.params.order"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"src": "AI.invoice@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'idle'", "target_state": "closed", "actions": []}
      ]
    },
    {
      "name": "with_invoice", "description": "Invoice holds the conversation. Its reply goes to the person; done gives the conversation back to sales, and so does a while with no message.",
      "entry_actions": [
        {"type": "forward", "target": "AI.invoice@worker1", "message": "person", "vars": {"order": {"$ref": "state.order"}}, "menu": {"help": "You hold the person's conversation about the order in vars.order. Say done when their business with invoicing is.", "options": [{"name": "answer"}, {"name": "done"}]}},
        {"type": "schedule_timer", "timer_key": "idle", "fire_in": "10m"}
      ],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "idle"}],
      "transitions": [
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "keep_message", "name": "person"}, {"type": "forward", "target": "AI.invoice@worker1", "message": "person", "vars": {"order": {"$ref": "state.order"}}, "menu": {"help": "You hold the person's conversation about the order in vars.order. Say done when their business with invoicing is.", "options": [{"name": "answer"}, {"name": "done"}]}}, {"type": "reschedule_timer", "timer_key": "idle", "fire_in": "10m"}]},
        {"event_match": {"src": "AI.invoice@worker1"}, "guard": "event.context.evaluation.option == 'done' && event.context.evaluation.reply != ''", "target_state": "with_sales", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"src": "AI.invoice@worker1"}, "guard": "event.context.evaluation.option == 'done'", "target_state": "with_sales", "actions": []},
        {"event_match": {"src": "AI.invoice@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'idle'", "target_state": "with_sales", "actions": []}
      ]
    },
    {"name": "closed", "description": "The conversation ended.", "entry_actions": [], "exit_actions": [], "transitions": []},
    {"name": "failed", "description": "An action failed.", "entry_actions": [], "exit_actions": [], "transitions": []}
  ]
}
```

**Atom 3, escalating a question.** For an agent that needs a person's decision, such as a
supervisor's approval, before it answers. The agent stays the person's voice. Only io.slack maps a
reply in its post's thread back to the instance's thread.

```json
{
  "wf_schema_version": "1",
  "workflow_type": "atom.consult",
  "description": "Sales may consult the team before it answers: its question goes to the team's channel, and each message of the team in that thread goes to sales, which says whether it decides the question. Sales stays the person's voice. When no decision comes, sales hears it and tells the person.",
  "start_from": ["IO.api.shop@worker1"],
  "input_schema": {"type": "object", "required": ["type"], "properties": {"type": {"type": "string"}}},
  "initial_state": "start",
  "terminal_states": ["closed", "failed"],
  "states": [
    {
      "name": "start", "description": "Keeps the person's io context; the first message goes to sales.",
      "entry_actions": [
        {"type": "set_variable", "name": "io", "value": "event.context.io"},
        {"type": "forward", "target": "AI.sales@worker1", "menu": {"help": "A discount above 10% needs the team's approval.", "options": [{"name": "answer"}, {"name": "consult"}]}},
        {"type": "emit_internal_event", "meta": {"msg": "STARTED"}}
      ],
      "exit_actions": [],
      "transitions": [
        {"event_match": {"msg": "STARTED"}, "guard": "true", "target_state": "talking", "actions": []}
      ]
    },
    {
      "name": "talking", "description": "Sales holds the conversation. Its reply goes to the person; consult tells the person that sales will check, and writes sales' question to the team.",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "idle", "fire_in": "30m"}],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "idle"}],
      "transitions": [
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.sales@worker1", "menu": {"help": "A discount above 10% needs the team's approval.", "options": [{"name": "answer"}, {"name": "consult"}]}}, {"type": "reschedule_timer", "timer_key": "idle", "fire_in": "30m"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'consult' && event.context.evaluation.reply != ''", "target_state": "consulting", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}, {"type": "send_message", "target": "IO.slack.team@motherbee", "subject": "new", "meta": {"msg": "QUESTION"}, "payload": {"type": "text", "content": {"$ref": "event.context.evaluation.params.question"}}}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'consult'", "target_state": "consulting", "actions": [{"type": "send_message", "target": "IO.slack.team@motherbee", "subject": "new", "meta": {"msg": "QUESTION"}, "payload": {"type": "text", "content": {"$ref": "event.context.evaluation.params.question"}}}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'idle'", "target_state": "closed", "actions": []}
      ]
    },
    {
      "name": "consulting", "description": "The team has sales' question. Each message of the team goes to sales: decided goes to the person and ends the consultation, wait waits on, answer_team goes to the team in the question's thread. The person's messages still go to sales.",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "no_decision", "fire_in": "30m"}],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "no_decision"}],
      "transitions": [
        {"event_match": {"src": "IO.slack.team@motherbee"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.sales@worker1", "menu": {"options": [{"name": "decided"}, {"name": "wait"}, {"name": "answer_team"}]}}]},
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.sales@worker1", "menu": {"options": [{"name": "answer"}]}}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'decided'", "target_state": "talking", "actions": [{"type": "forward", "target": "IO.api.shop@worker1", "meta": {"context": {"io": {"$ref": "state.io"}}}}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'wait'", "actions": []},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'answer_team'", "actions": [{"type": "forward", "target": "IO.slack.team@motherbee", "subject": "same"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1", "meta": {"context": {"io": {"$ref": "state.io"}}}}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'no_decision'", "target_state": "talking", "actions": [{"type": "send_message", "target": "AI.sales@worker1", "meta": {"msg": "NO_DECISION", "context": {"io": {"$ref": "state.io"}}}, "menu": {"options": [{"name": "answer"}]}, "payload": {"type": "text", "content": "Nobody of the team decided your question. Tell the person that you could not confirm it yet."}}]}
      ]
    },
    {"name": "closed", "description": "The conversation ended.", "entry_actions": [], "exit_actions": [], "transitions": []},
    {"name": "failed", "description": "An action failed.", "entry_actions": [], "exit_actions": [], "transitions": []}
  ]
}
```

- Sales' answer to the team's message carries the team's `io` context, since an agent answers
  where it was asked. So everything that goes from sales to the person while the team is consulted
  writes the person's, `meta.context.io` from `state.io`. Without it, an IO node that places a
  reply by its `reply_target` (io.wapp) drops the decision.

**Atom 4, escalating the conversation.** For a person of the team who must take the conversation.
- The team's io.slack shows the menu as a button. A click comes back with
  `event.context.evaluation`, and a text without it.
- While the team holds the conversation, an agent's late answer is not delivered.

```json
{
  "wf_schema_version": "1",
  "workflow_type": "atom.escalate",
  "description": "Sales may hand the conversation to the team: its summary opens a post in the team's channel with a handback button, and the team and the person talk through the WF until the team hands it back or it goes quiet. Then sales holds it again and tells the person.",
  "start_from": ["IO.api.shop@worker1"],
  "input_schema": {"type": "object", "required": ["type"], "properties": {"type": {"type": "string"}}},
  "initial_state": "start",
  "terminal_states": ["closed", "failed"],
  "states": [
    {
      "name": "start", "description": "Keeps the person's io context; the first message goes to sales.",
      "entry_actions": [
        {"type": "set_variable", "name": "io", "value": "event.context.io"},
        {"type": "forward", "target": "AI.sales@worker1", "menu": {"help": "A credit note needs a person of the team.", "options": [{"name": "answer"}, {"name": "handover"}]}},
        {"type": "emit_internal_event", "meta": {"msg": "STARTED"}}
      ],
      "exit_actions": [],
      "transitions": [
        {"event_match": {"msg": "STARTED"}, "guard": "true", "target_state": "talking", "actions": []}
      ]
    },
    {
      "name": "talking", "description": "Sales holds the conversation. Its reply goes to the person; handover tells the person, and hands the conversation to the team.",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "idle", "fire_in": "30m"}],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "idle"}],
      "transitions": [
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.sales@worker1", "menu": {"help": "A credit note needs a person of the team.", "options": [{"name": "answer"}, {"name": "handover"}]}}, {"type": "reschedule_timer", "timer_key": "idle", "fire_in": "30m"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'handover' && event.context.evaluation.reply != ''", "target_state": "with_team", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'handover'", "target_state": "with_team", "actions": []},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'idle'", "target_state": "closed", "actions": []}
      ]
    },
    {
      "name": "with_team", "description": "The team holds the conversation. Sales' summary opens a post with a handback button; the team's messages go to the person, and the person's to the post's thread. A handback, or a while with no message, gives the conversation back to sales, and sales' late answers are not delivered meanwhile.",
      "entry_actions": [
        {"type": "send_message", "target": "IO.slack.team@motherbee", "subject": "new", "meta": {"msg": "HANDOVER"}, "menu": {"help": "When you are done, hand the conversation back to sales with a note.", "options": [{"name": "handback"}]}, "payload": {"type": "text", "content": {"$ref": "event.context.evaluation.params.summary"}}},
        {"type": "schedule_timer", "timer_key": "quiet", "fire_in": "30m"}
      ],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "quiet"}],
      "transitions": [
        {"event_match": {"src": "IO.slack.team@motherbee"}, "guard": "!has(event.context.evaluation)", "actions": [{"type": "forward", "target": "IO.api.shop@worker1", "meta": {"context": {"io": {"$ref": "state.io"}}}}, {"type": "reschedule_timer", "timer_key": "quiet", "fire_in": "30m"}]},
        {"event_match": {"src": "IO.slack.team@motherbee"}, "guard": "event.context.evaluation.option == 'handback'", "target_state": "talking", "actions": [{"type": "send_message", "target": "IO.slack.team@motherbee", "subject": "end", "meta": {"msg": "TEAM_END"}, "payload": {"type": "text", "content": "Handed back to the agent."}}, {"type": "forward", "target": "AI.sales@worker1", "meta": {"context": {"io": {"$ref": "state.io"}}}, "menu": {"help": "The team handed the conversation back to you, with this note. Tell the person what was done.", "options": [{"name": "answer"}]}}]},
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "IO.slack.team@motherbee", "subject": "same"}, {"type": "reschedule_timer", "timer_key": "quiet", "fire_in": "30m"}]},
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "true", "actions": []},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'quiet'", "target_state": "talking", "actions": [{"type": "send_message", "target": "IO.slack.team@motherbee", "subject": "end", "meta": {"msg": "TEAM_END"}, "payload": {"type": "text", "content": "The case went quiet; it went back to the agent."}}, {"type": "send_message", "target": "AI.sales@worker1", "meta": {"msg": "TEAM_QUIET", "context": {"io": {"$ref": "state.io"}}}, "menu": {"options": [{"name": "answer"}]}, "payload": {"type": "text", "content": "The conversation with the team went quiet, and it is yours again. Tell the person, and help them as you can."}}]}
      ]
    },
    {"name": "closed", "description": "The conversation ended.", "entry_actions": [], "exit_actions": [], "transitions": []},
    {"name": "failed", "description": "An action failed.", "entry_actions": [], "exit_actions": [], "transitions": []}
  ]
}
```

**Atom 5, a timer whose time an agent's answer gives.** For a time that an answer sets, such as a due
date or a reminder. Guard the time's shape before setting the timer. The minimum is 60 s.

```json
{
  "wf_schema_version": "1",
  "workflow_type": "atom.due_timer",
  "description": "Invoice issues an invoice and says when it falls due; the WF sets a timer at that time, and when it fires, payments writes the person.",
  "start_from": ["IO.api.shop@worker1"],
  "input_schema": {"type": "object", "required": ["type"], "properties": {"type": {"type": "string"}}},
  "initial_state": "start",
  "terminal_states": ["closed", "failed"],
  "states": [
    {
      "name": "start", "description": "Keeps the person's io context; the first message goes to invoice.",
      "entry_actions": [
        {"type": "set_variable", "name": "io", "value": "event.context.io"},
        {"type": "forward", "target": "AI.invoice@worker1", "menu": {"options": [{"name": "answer"}, {"name": "issued", "description": "You issued the invoice.", "params": {"invoice_number": {"type": "string"}, "due_in": {"type": "string", "description": "When it falls due, such as 2m, 1h or 30d."}}}]}},
        {"type": "emit_internal_event", "meta": {"msg": "STARTED"}}
      ],
      "exit_actions": [],
      "transitions": [
        {"event_match": {"msg": "STARTED"}, "guard": "true", "target_state": "talking", "actions": []}
      ]
    },
    {
      "name": "talking", "description": "Invoice holds the conversation. An invoice issued with a due time goes to the person and sets the timer; its other replies go to the person.",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "idle", "fire_in": "30m"}],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "idle"}],
      "transitions": [
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.invoice@worker1", "menu": {"options": [{"name": "answer"}, {"name": "issued", "description": "You issued the invoice.", "params": {"invoice_number": {"type": "string"}, "due_in": {"type": "string", "description": "When it falls due, such as 2m, 1h or 30d."}}}]}}, {"type": "reschedule_timer", "timer_key": "idle", "fire_in": "30m"}]},
        {"event_match": {"src": "AI.invoice@worker1"}, "guard": "event.context.evaluation.option == 'issued' && event.context.evaluation.params.due_in.matches('^[1-9][0-9]*[mhd]$')", "target_state": "due_pending", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}, {"type": "set_variable", "name": "invoice_number", "value": "event.context.evaluation.params.invoice_number"}, {"type": "set_variable", "name": "due_in", "value": "event.context.evaluation.params.due_in"}]},
        {"event_match": {"src": "AI.invoice@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'idle'", "target_state": "closed", "actions": []}
      ]
    },
    {
      "name": "due_pending", "description": "The invoice is pending: the person's messages still go to invoice. When the timer fires, payments writes the person.",
      "entry_actions": [
        {"type": "schedule_timer", "timer_key": "payment_due", "fire_in": {"$ref": "state.due_in"}}
      ],
      "exit_actions": [],
      "transitions": [
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.invoice@worker1", "menu": {"options": [{"name": "answer"}, {"name": "issued", "description": "You issued the invoice.", "params": {"invoice_number": {"type": "string"}, "due_in": {"type": "string", "description": "When it falls due, such as 2m, 1h or 30d."}}}]}}]},
        {"event_match": {"src": "AI.invoice@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'payment_due'", "target_state": "notifying", "actions": [{"type": "send_message", "target": "AI.payments@worker1", "meta": {"msg": "PAYMENT_DUE", "context": {"io": {"$ref": "state.io"}}}, "vars": {"invoice_number": {"$ref": "state.invoice_number"}}, "menu": {"options": [{"name": "answer"}]}, "payload": {"type": "text", "content": "The invoice in vars.invoice_number fell due and was not paid. Tell the person."}}]}
      ]
    },
    {
      "name": "notifying", "description": "Payments' reply goes to the person, and the conversation ends.",
      "entry_actions": [
        {"type": "schedule_timer", "timer_key": "payments_timeout", "fire_in": "120s"}
      ],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "payments_timeout"}],
      "transitions": [
        {"event_match": {"src": "AI.payments@worker1"}, "guard": "event.context.evaluation.reply != ''", "target_state": "closed", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"src": "AI.payments@worker1"}, "guard": "true", "target_state": "closed", "actions": []},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'payments_timeout'", "target_state": "closed", "actions": []}
      ]
    },
    {"name": "closed", "description": "The conversation ended.", "entry_actions": [], "exit_actions": [], "transitions": []},
    {"name": "failed", "description": "An action failed.", "entry_actions": [], "exit_actions": [], "transitions": []}
  ]
}
```

**Atom 6, the catch.** For every definition with more than one state. It takes an agent's `none`,
its errors, and the answers that arrive after their wait. An error of the agent also tells the
person: when its provider gave no answer, the error carries the operator's text for the person in
`reply` (§4.1), and a `$ref` with a `default` sends the WF's own text for any other error.

```json
{
  "wf_schema_version": "1",
  "workflow_type": "atom.catch",
  "description": "The basic loop with a wait for sales' answer and a catch: an agent's none and any error are noted in last_error, an error of sales' also tells the person, and sales' answer that comes after its wait still reaches the person.",
  "start_from": ["IO.api.shop@worker1"],
  "input_schema": {"type": "object", "required": ["type"], "properties": {"type": {"type": "string"}}},
  "initial_state": "thinking",
  "terminal_states": ["closed", "failed"],
  "states": [
    {
      "name": "thinking", "description": "The person's message goes to sales, which has a while to answer.",
      "entry_actions": [
        {"type": "forward", "target": "AI.sales@worker1", "menu": {"options": [{"name": "answer"}]}},
        {"type": "schedule_timer", "timer_key": "sales_timeout", "fire_in": "120s"}
      ],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "sales_timeout"}],
      "transitions": [
        {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'answer' && event.context.evaluation.reply != ''", "target_state": "listening", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]},
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "actions": [{"type": "forward", "target": "AI.sales@worker1", "menu": {"options": [{"name": "answer"}]}}]},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'sales_timeout'", "target_state": "listening", "actions": [{"type": "set_variable", "name": "last_error", "value": "'sales did not answer in time'"}]}
      ]
    },
    {
      "name": "listening", "description": "Waits for the person's next message.",
      "entry_actions": [{"type": "schedule_timer", "timer_key": "idle", "fire_in": "30m"}],
      "exit_actions": [{"type": "cancel_timer", "timer_key": "idle"}],
      "transitions": [
        {"event_match": {"src": "IO.api.shop@worker1"}, "guard": "true", "target_state": "thinking", "actions": []},
        {"event_match": {"msg": "TIMER_FIRED"}, "guard": "event.timer_key == 'idle'", "target_state": "closed", "actions": []}
      ]
    },
    {"name": "closed", "description": "The conversation ended.", "entry_actions": [], "exit_actions": [], "transitions": []},
    {"name": "failed", "description": "An action failed.", "entry_actions": [], "exit_actions": [], "transitions": []}
  ],
  "catch": [
    {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.option == 'none'", "actions": [{"type": "set_variable", "name": "last_error", "value": "'sales answered none'"}]},
    {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.payload.type == 'error'", "actions": [{"type": "set_variable", "name": "last_error", "value": "'error: ' + event.payload.code"}, {"type": "send_message", "target": "IO.api.shop@worker1", "meta": {"msg": "SALES_ERROR", "context": {"io": {"$ref": "event.context.io", "default": {}}}}, "payload": {"type": "text", "content": {"$ref": "event.payload.reply", "default": "Sorry, something went wrong on our side. Could you write that again?"}}}]},
    {"event_match": {}, "guard": "event.payload.type == 'error'", "actions": [{"type": "set_variable", "name": "last_error", "value": "'error: ' + event.payload.code"}]},
    {"event_match": {"src": "AI.sales@worker1"}, "guard": "event.context.evaluation.reply != ''", "actions": [{"type": "forward", "target": "IO.api.shop@worker1"}]}
  ]
}
```

**Rules across atoms:**
- **Keep a conversation bounded:** `done` delivers the agent's answer, then waits for the person's
  next message. Never ask an agent again with the same message.
- **The catalog only grows:** a known option keeps its name and its params. A meaning that changes
  takes a new name.
- **Answers in any state:** a person who writes twice gets two answers, and a decision may cross an
  answer to the person. Handle an agent's answers wherever they can arrive, or in the catch.
- **Late events:** handle a message where it can arrive. Use an internal transition when it must
  not leave the state, and guard the catch with a state variable when it must not move the
  instance.
- **A handover between agents carries nothing by itself.** The agent that takes a conversation
  sees its own turns with the person, and what people said to each other while it held the
  conversation or before any agent did; never another agent's turns, so no agent takes over
  another's role.
- **What people said to each other reaches the agent; its role decides whether to use it.** Each
  such line names its writer and whom it was to. When the agent must not use it (what two people
  said in private, say), write that in its role. When it must
  know something to go on (a state, a result, an order's number), the workflow passes it on in the
  message it sends, as Case 4's `handback` carries its `note`. Otherwise the person tells it, or it
  asks.

#### 4.2.7 Checking, deploying and operating a WF

- **Check, show, deploy.** `wf_rules_check` validates a definition without staging or applying it,
  and names each error's exact path.
  - In chat, check every definition, show the operator what it does, and only then deploy.
  - In the pipeline, the designer's workflows are checked before the plan, and a refusal comes back
    to the designer.
- **A solution runs on one hive. This is a rule, not a preference.** A workflow and every agent
  (`AI.*`) and workflow (`WF.*`) its definition names run on the same hive: deploy the workflow on
  its agents' hive, and create a new agent on the hive of the workflows that will use it.
  - SY.wf-rules refuses any other definition, at the check and at the deploy: `COMPILE_ERROR`,
    `a solution's WFs and agents run on one hive, this one (<hive>)`, with each path it found.
  - IO nodes are the doors, and may run on any hive.
  - Why: cognition, on the agents' hive, takes the turns the workflow relays between people, so
    the agent that takes a conversation back sees what was said meanwhile.
- **A workflow's name** is lowercase letters and digits, in parts joined by dots, the first
  starting with a letter: `sales`, `sales.gate0195`, never `sales-gate`. Its runtime is
  `wf.<workflow_name>`, and the runtime naming policy has no hyphen; the check refuses one.
- **The manifest:** `desired_state.wf_deployments[i]` holds `hive`, `workflow_name`, `definition`
  and `ownership`, and `tenant_id` for a workflow its hive does not have yet. The pipeline deploys
  with `auto_spawn` and that tenant, so a node that does not exist is spawned.
- **In chat,** a first deploy is `wf_rules_compile_apply` with `auto_spawn: true` and `tenant_id`.
- **A deploy whose node does not run the definition is refused,** `WF_NODE_NOT_DEPLOYED`: its
  `wf_node` says why (a spawn or a restart that failed, a tenant that does not exist). The
  definition is current anyway: fix the cause, and deploy again.
- **An update** starts from the deployed definition: read it with `GET
  /hives/{hive}/wf-rules?workflow_name=<name>` (`wf_rules_get_workflow`: its definition, version and
  hash), change only what the operator asked, and check it. Never rebuild it from memory. The
  update is one `wf_rules_compile_apply`, and the WF node restarts once.
  - A running instance keeps the definition it was born with, and a new conversation takes the new
    one.
  - `wf_rules_rollback` goes back to the version before.
- **A removal** ends the conversations still running. The pipeline's plan says how many there are,
  and the operator's confirmation removes the workflow with `force`. To let them finish, wait; to
  end them one by one, cancel them.
- **Operating instances** (the node is `WF.<workflow_name>`):
  - `wf_list_instances` (`hive`, `node_name`, `status`, `limit`): each instance's state, and its
    last change (`updated_at_ms`), which is not when it entered its state: that is its last
    `transition <from> → <state>` entry in `wf_get_instance`'s log;
  - `wf_get_instance` (`hive`, `node_name`, `instance_id`, `log_limit`): its input, its state
    variables (`waiting_for` and `last_error`, when the definition keeps them), and its last log
    entries;
  - `wf_cancel_instance` (`hive`, `node_name`, `instance_id`, `reason`): it ends the instance. Ask
    the operator first.

  An instance that waits long in one state is stuck when no timer of that state can fire: read its
  log.
- **A WF node that cannot use its instance store stays up** and says why in `get_node_status`:
  `DEGRADED` with `last_error` `store_unavailable` (a corrupt file, a full disk, a file system
  remounted read-only, a failed recovery), or `invalid_config` naming its `db_path` when that path
  cannot hold the store (a directory, a place it cannot write, a journal file `<db>-wal` or
  `<db>-shm` it cannot write), its config `FAILED_CONFIG` meanwhile. It takes no message (`NOT_TAKEN` `unavailable`), the instance
  verbs answer `STORE_ERROR` (`node_not_configured` for a bad `db_path`), and it opens the store
  again every 30 s, ready as soon as it opens. A `db_path` the operator changes is a config change,
  applied at the node's restart.

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

**Taking a page down is the operator's,** whoever published it (FINDINGS A-120):

- Stage `unpublish_artifact` with `fluxbee_plan_compiler`: one step, with exactly one of
  `publication_id` (`pub:<uuid>`) or `key`, the last segment of the page's link. Of
  `https://<host>/public/<key>`, pass only `<key>`. The step takes no `hive`.
- The operator confirms the plan, and SY.admin's executor takes the page down as the operator. The
  link stops working at once, for everyone who has it.
- A page already down or expired answers `unpublished: false`; a link or id no publication has,
  `NOT_FOUND`.
- No action lists the pages: the operator names one by its link, or by the `publication_id` its
  producer reported.

Rules for Archi:

- Publishing is behavior implemented inside the producer runtime: `publish_artifact` is **not** an
  HTTP/SCMD Admin operation and must not be emitted as an Admin executor step. Taking a page down
  is one (above).
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

### 4.4.2 Clearing a hive's cognition

`cognition_clear` makes one hive's `SY.cognition` drop everything it keeps, in memory, in its
file and in its memory region: every communication (contexts, reasons, scopes, memories,
feelings and last turns) and every episode (FINDINGS A-160). It cannot be undone: the hive's
agents lose the memory of every conversation.

- Use it only when the operator asks to forget a hive's conversations. Tell the operator which
  hive and what goes, then stage it with `fluxbee_plan_compiler`: one step per hive, whose only
  arg is `hive`.
- The operator confirms the plan, and SY.admin's executor runs it as the operator. It is closed to
  the mesh: never send it as an `ADMIN_COMMAND` of your own, nor as SCMD.
- Only the motherbee and the workers run a cognition; an ingress or an egress has none.
- The answer says what it removed: `removed.file` and `removed.memory`, each with
  `communications` and `episodes`. The cognition keeps running, empty: a thread's next turn starts
  its communication again.
- Each hive keeps its own copy of each episode: clearing one hive leaves the other hives' copies.
- The factory reset clears every hive's cognition itself.

### 4.4.3 The installation's AI settings

The installation's `ai` is its default provider and each provider's model. The SY nodes run on
it (you, SY.admin's executor, SY.cognition, SY.frontdesk.gov), and so does every AI node that names
no provider or model of its own. It is one installation file, the same version on every hive.
Only the motherbee's orchestrator writes it, and only through `set_installation_ai`. It holds no
credential: each provider's key is in SY.vault (`resource_type` `openai` or `anthropic`).

- **Read it** with `get_installation` (`GET /installation`, a read):
  - `motherbee.ai`, the values now, and `motherbee.state`;
  - `expected_version`, the version a change names;
  - `versions`, the newest 20 versions with their values and their `source` (`migration`,
    `operator`), oldest first; `versions_total` says how many there are, and while older ones
    remain `versions_next_before` is what lists them: `GET /installation?versions_before=<it>`
    (`versions_limit`, 1 to 100, asks another count);
  - `hives`, each hive's copy: `state` (`current`, `behind`, `pending`, `edited_by_hand`,
    `unknown`), `version`, and `applied`, the version it applied last and what it restarted.

  `GET /versions` holds the same, in each hive's `config` (the motherbee's lists `versions`).
- **Change it** only when the operator asks. Tell them what changes and what restarts (below),
  then stage it with `fluxbee_plan_compiler`: one `set_installation_ai` step, args `ai` and
  `expected_version`.
  - `ai` is the whole new `ai`: `motherbee.ai` from `get_installation` with what changes, every
    provider listed with its model, the default provider among them.
  - `expected_version` is `get_installation`'s.
  - The operator confirms the plan, and SY.admin's executor runs it as the operator. It is closed
    to the mesh: never send it as an `ADMIN_COMMAND` of your own, nor as SCMD.
  - Switching the default provider: every reader of the default needs a key of the new provider,
    and SY.admin refuses the change when one has none (`PROVIDER_KEYS_MISSING`, below). Check it
    with the operator before staging: `vault_list` with that `resource_type` in the root tenant
    (you, the executor, SY.cognition and SY.frontdesk.gov read theirs there) and in the tenant of
    each AI node that names no provider of its own.
- **Undo a change** by setting a previous version's values: its `ai` from `versions`, with the
  current `expected_version`. It becomes a new version. A core rollback never touches the file.
- **The refusals** write nothing:
  - `NODES_LEFT_WITHOUT_ENGINE` (A-11): the change switches the default provider or drops a
    provider, and the nodes it names would lose their engine. A node that names its model and not
    its provider would ask the new default provider for a model of the old one; a node that names
    its provider and not its model loses that model when the provider is dropped. Fix each first,
    as the operator decides: give it `behavior.provider` and `behavior.model` with
    `set_node_config`, the ones it runs now. For your own `config.ai.model`: set it to null before
    the switch, and to a model of the new provider after it.
  - `HIVES_NOT_CHECKED`: a hive's AI nodes could not be read for that check. Retry once it
    answers.
  - `PROVIDER_KEYS_MISSING`: the change switches the default provider, and the readers it names
    (`missing`: each node, its tenant and the `resource_type`) would find no key of the new
    provider in SY.vault: none of their own, none assigned to them, no general one in their
    tenant. Tell the operator: they put the key in SY.vault (only the operator handles a key's
    value), or give that AI node its own `behavior.provider` and `behavior.model` with
    `set_node_config`. Then stage the change again.
  - `KEYS_NOT_CHECKED`: SY.vault did not list a tenant's keys for that check. Retry once it
    answers.
  - `JOIN_IN_PROGRESS`: a hive is joining or being removed. Change it once that ends.
  - `INSTALLATION_EDITED_BY_HAND`: someone edited the motherbee's file by hand (`motherbee.state`
    is `edited_by_hand`). Tell the operator: they restore it, or choose to replace the hand edit
    with this change. Only on that choice add `replace_hand_edit: true`; never choose it yourself.
  - `VERSION_CONFLICT`: the file changed since you read it. Read `get_installation` again, show the
    operator what changed, and decide again with them.
  - `INSTALLATION_UNCHANGED`: the current version holds these values already.
  - `INVALID_AI_SETTINGS`: a provider listed without its model, the default provider not listed,
    or a field `ai` does not take.
  - `INSTALLATION_NOT_WRITTEN`: the motherbee holds no file yet. The operator fixes `ai` in the
    motherbee's hive.yaml and restarts `sy-orchestrator`, which writes version 1.
  - `TIMEOUT` (or `TRANSPORT_ERROR`): the write did not answer within 30 s. It is no refusal: the
    write may still land, since the motherbee's orchestrator takes its messages one at a time.
    Read `get_installation`: when its `expected_version` is past the one you sent, the version
    was written and applied on no hive, so finish each hive with `update` (below); when it is the
    same, nothing was written, and you may stage the change again.
- **What restarts where:** only the nodes that read `ai` at their start, and only when their
  engine, the default provider with its model, changed: SY.cognition and SY.frontdesk.gov on the
  motherbee, SY.cognition on each worker, nothing on an ingress or an egress. AI nodes take it at
  their next message. You and the executor take it at your next use, never restarted, so the
  change's answer reaches you. Another provider's model restarts nothing, except where the hive
  cannot vouch for what a reader runs: the first change a hive takes (nothing recorded yet), and a
  reader restarted since the hive's last change. Those restart once.
- **The answer:** `version` and each hive's `status`. `ok` on every hive is done; `partial` names
  the hives that are not ok, each with a `next` that says what finishes it:
  - `sync_pending`: the file had not reached it within 90 s;
  - `not_answered`: it did not answer, and it may hold the version already;
  - `error`: its `error_code` says why. A hive on a release before 0.1.122 answers
    `MANIFEST_INVALID` (it takes no category `config`): its core update comes first.
    `INSTALLATION_HASH_MISMATCH`: its copy holds the version with another content (edited by
    hand): the operator restores it, then `update` that hive;
  - `left_to_task`: another task of the installation's update (§4.4.4) held it until too little of
    the change's time was left to apply it, named in `detail`, so the change sent it nothing. Once
    that task ends, `update_installation` finishes it.

  None of these means "not applied". Check with `GET /versions` (each hive's `config`), and finish
  a hive with `update` for that hive: `category: config`, `manifest_version` and `manifest_hash`
  the new version's (`get_installation`). The run is not verified until every hive applied it.
  `skipped` names each hive whose join did not finish: no change reaches it. Tell the operator:
  finish its join or remove it, then `update` it the same way.
- **The change opens the installation's update** (§4.4.4) with the new version as its target's
  file and the motherbee's core as its core: `update` in the answer is its `{id, state}`.
  - A hive that did not answer stays pending in it, and SY.admin finishes it once by itself when
    the hive answers again: its core too, when it comes back behind the motherbee's release.
  - It replaces an open update (another target): the new one reads every hive again.
  - A hive that joins after the change gets the version at the next `update_installation`, which
    restarts its readers once.
  - The change is refused while an update run is under way (`INSTALLATION_UPDATE_RUNNING`), and
    when SY.admin cannot read its registry of hives (`HIVE_REGISTRY_UNREADABLE`, nothing written:
    the operator checks it, then you stage the change again). When the motherbee's own core is
    rolled back, no update opens (`update.not_opened` says why; the version is still applied).

### 4.4.4 The installation's update

One update brings every hive to what the motherbee holds: its core (the release installed on the
motherbee) and its installation file (§4.4.3). SY.admin keeps it on disk, one at a time, until
every hive is on it.

- **Run it** (`update_installation`) only when the operator asks: after a release is installed on
  the motherbee, to bring every hive to it, or to finish an open update. Stage it with
  `fluxbee_plan_compiler`: one `update_installation` step, args `reason` (a few words) and, only on
  the operator's choice, `include_rolled_back` (below). The operator confirms the plan, and
  SY.admin's executor runs it as the operator. It is closed to the mesh: never send it as an
  `ADMIN_COMMAND` of your own, nor as SCMD.
  - Each hive gets its core first (its services restart onto the release), then its config.
  - The answer comes once every core update answered: `ok` when every hive is on the target
    (`done`), or when the update is `held` (below); `partial` while the update stays open, with
    `pending` naming each hive not on it. A core that answered `restarting` is normal: SY.admin
    follows it to its settle and then applies its config.
  - After the plan runs, its verification follows the update by itself, up to 10 minutes, while its
    hives only move (a core restarting or settling, its config after it, a hive not read yet). The
    run is verified once the update is `done` or `held`: tell the operator which hives a held
    update leaves on the core they rolled back to. A hive in error, without an answer,
    `sync_pending` at its bound or left as it is keeps the run unverified: name each one with its
    `next`.
  - One run at a time: a second is refused with `INSTALLATION_UPDATE_RUNNING`, naming the one under
    way (wait for it; `get_installation_update` follows it). `HIVE_REGISTRY_UNREADABLE`: SY.admin
    cannot read its registry of hives, and changed nothing; the operator checks it, then runs it
    again. `MOTHERBEE_ROLLED_BACK`: the motherbee's own core is rolled back, so it names no release
    to bring the hives to: the operator installs a release on the motherbee first.
- **The installation's rollback:** the operator installs the previous release on the motherbee,
  then runs the update. It moves every hive back to the motherbee's release. An update left open
  for another release says `stale_target`: nothing acts on it until the next run replaces it.
- **Read it** with `get_installation_update` (`GET /installation/update`, a read; `history: true`
  adds the closed ones):
  - `update`: the open update, or the last one closed: its `id`, `state` (`open`; `held`, every
    hive on the target but those the operator rolled back, named in `held`; `done`; `superseded`),
    `target` (`core.version`, `config.version`), `stale_target`, and `hives`;
  - each hive's `core` and `config`: `state` (`on_target`, `pending`, `in_flight`, `excluded` with
    its `exclusion`, `unknown`), `reason`, `outcome` (`status`: `sent`, `ok`, `restarting`,
    `settled`, `sync_pending`, `error` with its `error_code`, `not_answered`, `excluded`; `by`:
    `explicit`, `automatic` or `background`; `seen: true` when a read found the hive on the target
    and no attempt moved it there), and `next`, what moves it;
  - `automatic`: the automatic step's one attempt on the hive, when it made one.

  `GET /versions` and `get_installation` carry its summary (`installation_update`, `update`).
  `not_answered` never means "not applied": the hive may hold the target already.
- **A held update** is A-5's one allowed divergence: every hive is on the target but those the
  operator rolled back (`core_rollback`), each with its config on the target. It needs nothing
  from you: the operator's per-hive `update` that moves such a hive back closes it `done`.
- **The exclusions**, each with its reason and the per-hive action that moves it, which only the
  operator decides:
  - **A hive the operator rolled back** (`core_rollback`): the update keeps its core as it is, the
    one divergence the operator chose; its config is still applied. To move its core too: a plan
    with `include_rolled_back: ["<hive>"]`, or `update` for that hive with `category: core`.
  - **A copy edited by hand** (`config.state` `edited_by_hand`): no update applies config to it.
    The operator restores it; then the update, or `update` with `category: config`.
  - **A hive before 0.1.122** takes no category `config`: its core goes first.
  - **The motherbee's file edited by hand** gives the update no config to apply, on any hive:
    `target.config_note` says so. The operator restores it, or makes it a version (§4.4.3).
- **The automatic step**, SY.admin's own while an update is open: a hive that did not answer
  (`not_answered`) and answers again gets the update finished, once per hive and target, core then
  config, with the same waits. A core it finds already under way is only followed, and the step
  stays for later. It never acts on a hive rolled back by the operator, one ahead of the motherbee
  (on a newer release), or one with a copy edited by hand: each is recorded `excluded`. It never
  tries a hive twice: an error, or a copy that did not arrive in time, is left for the operator
  with its `next`. It opens nothing: only a run opens an update.
- **What SY.admin reads by itself** while an update is open: a hive in flight every 3 s; a hive
  that did not answer, only once it shows up connected again (checked every 30 s, without asking
  it); a hive left as it is, or in error, every 10 minutes. The operator's per-hive `update` or
  `core_rollback`, a join and a removal make it look at that hive at once, and so does your read
  of `get_installation_update`. A core that did not settle stays pending until its record changes:
  run the update again, or the per-hive `update`.

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
  write them exactly as the declared resource. A new workflow's `auto_spawn` and `tenant_id`, and a
  removal's `force`, are pinned too

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

### 7.7 A base runtime back on its package's version

A base runtime is one the installed Fluxbee package brought (`ai.generic`, `wf.engine`, the IO
runtimes). `list_runtimes` and `get_runtime` show the version the package brought as
`package_version`. A `current` that differs is a version published by hand, and it stays current
through every upgrade while its directory is on disk (FINDINGS A-122).

- When the operator asks to bring it back, or you see a `current` that differs from
  `package_version` and the operator confirms it is not wanted, tell them which runtime, its
  `current` and its `package_version`, and that every node following `current` moves onto the
  package's version (the WF and config-only packages built on it with them).
- Stage it with `fluxbee_plan_compiler`: one `reset_base_runtime` step, args `hive: "motherbee"`
  and `runtime`. The operator confirms the plan, and SY.admin's executor runs it as the operator.
  It is closed to the mesh: never send it as an `ADMIN_COMMAND` of your own, nor as SCMD.
- Each hive's orchestrator moves the running nodes within its next reconcile pass (60 s); a node
  pinned to a version stays.
- The hand-published version stays available. If the operator wants it gone,
  `remove_runtime_version` deletes it once no node runs it. A base that packages are built on
  (`wf.engine` with deployed workflows, `ai.generic` with config-only packages) refuses that with
  `RUNTIME_HAS_DEPENDENTS`: the version then stays, unused, and costs only its disk.
- The package's own version is never removed (`RUNTIME_PACKAGE_VERSION`): it is the way back.

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
| `WF_DEPLOY_RESTART` | `wf_rules_compile_apply` (it restarts the WF node itself) |
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

### 10.1.1 Long lists come in pages

An answer reaches you in one mesh frame, at most 128 KiB. The lists that grow with the
installation answer in pages:

- **Which**: `/inventory`, `/inventory/{hive}`, `/hives/{hive}/nodes`, `/versions` (a page of hives),
  `/hives/{hive}/identity/ilks`, `/hives/{hive}/identity/tenants`,
  `/hives/{hive}/vault/secrets` and `/hives/{hive}/commands` (SY.admin's command log) take
  `?limit=&cursor=`.
- **How**: read without them first. Most lists then give every row, as always; two give one page
  even so: `/hives/{hive}/vault/secrets` 200 secrets and `/hives/{hive}/commands` 50 commands.
  When an answer has `next_cursor`, more rows follow: read the same path again with
  `?cursor=<next_cursor>` (and the same `limit`), until an answer has none. `total` says how many
  rows there are in all. Pass the cursor exactly as it came: a cursor of another list is refused
  with `INVALID_CURSOR`.
- **A list is no snapshot.** SY.admin gathers a long list page by page, so a row written or removed
  while it reads may be in it or not, though never twice. A list that would take longer than one
  request answers `TIMEOUT`: read it in pages.
- **`REPLY_TOO_LARGE`** means the answer did not fit one frame, so a small error came in its place.
  It is not a failure of the system and nothing is wrong with the data:
  - for a list, `error_detail.paging` names the params and `error_detail.suggested_limit` how
    many rows fit: read it again with `?limit=<suggested_limit>` and follow `next_cursor`;
  - for another read, say so to the operator: when `error_detail.http` names a route, the answer
    is readable over HTTP on the motherbee;
  - for a change, `error_detail.action_ran` is `true` and `error_detail.outcome` holds its status
    and ids: the change ran. Never run it again to see its answer; read what it changed back.
- **The installation's versions**: `/installation` and `/hives/{hive}/versions` list the newest 20;
  `?versions_before=<versions_next_before>` lists older ones.
- **A plan's answer** is cut only as far as the frame needs. First the largest step outputs are
  cut: such an event says `output_truncated`, with `output_bytes`, its size before, and keeps its
  status and small fields whole. A larger answer drops every step's output, keeping each step's
  status, its ids and what you read of it (a deploy's `wf_node`, a change's `version` and
  `pending`, an installation update's `id`), and then keeps only each step's last event.
  `payload.outputs_bounded` says how it was cut. Read what a step changed back with the read that
  shows it.
- **A plan whose answer is still too large** comes back as `REPLY_TOO_LARGE` with
  `action_ran: true`: the plan ran. Say so, never that it failed: name its `execution_id` and its
  `plan_status` from `error_detail.outcome`, and read each step's outcome in the command log
  (`/hives/motherbee/commands?limit=`, the steps with origin `executor`); the run's full log is on
  the motherbee at `outcome.log_path`. Never run the plan again to see its answer.

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
