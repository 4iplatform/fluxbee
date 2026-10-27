# Agent tools v1: what an AI agent can do, declared once

**Status:**

- The direction was agreed with the operator on 2026-10-26.
- The first release, 0.1.86, is validated on 8.x (2026-10-27; ledger 0.1.86). It covers §3.1–§3.4:
  the cards for the 14 existing base tools, the catalog, `NODE_HELP`, and T8, T9 and A-95.
- Iteration 1 of §6, 0.1.87, is validated on 8.x (2026-10-27).
- 0.1.88 fixed what that gate found (FINDINGS A-109, with A-101 and A-102): running agents move onto
  a new runtime by themselves. Validated on 8.x (2026-10-27).
- Next: iteration 2 (§6.2), 0.1.89.

This is the single place for the track; the next iteration starts here.

---

## 1. What exists (verified in the code, 2026-10-26)

- **Every `ai.generic` agent gets the same tools,** fixed in the runtime's code
  (`register_common_tools` in nodes/ai/ai-generic):
  - `thread_state_get`, `thread_state_put` and `thread_state_delete`: state per interlocutor,
    present only when the message names one (`src_ilk`);
  - ten file generators, `generate_{csv,text,json,markdown,html,pdf,xlsx,docx,png,jpeg}_artifact`;
  - `publish_html_page`, present only when the node has an identity.
- **The model learns them** only from each tool's definition: a name, one to three lines, and the
  input's JSON schema. Nothing says what a tool returns or what its limits are. For example,
  nothing tells the agent that a published link expires in 24 h.
- **The agent's definition** (role, skills, handbooks, personality; identity addendum v2.1) is
  config: Archi writes it, and it goes into the prompt. It does not mention tools.
- **Archi does not know which tools an agent has.** Its handbook does not list them, so a role can
  promise something no tool does.
- **A precedent:** SY.timer's "Self-Description (HELP)" principle (docs/sy-timer.md §3): *every
  node must be able to describe its own capabilities*, so that *a cognitive agent can ask a node
  "what can you do?"*. Only SY.timer implements it (`TIMER_HELP`; SY.admin's `timer_help`).
- **The word "capabilities"** already means six other things (FINDINGS A-94).

## 2. Decisions (operator, 2026-10-26)

| ID | Decision |
|---|---|
| T1 | **Two families.** The *base tools* live in the AI runtime: the ones of §1, plus the ones added later. *Fluxbee tools* act on the platform through SY.admin (an admin tool, for instance). They come later, one at a time. |
| T2 | **Every tool declares one card in its code, in a fixed format:** what it does, when to use it, its limits, when it is present, its input and output as JSON schemas, and its errors. The format is extensible: a new tool is a new card. |
| T3 | **Three things come from the cards:** the definition the model receives; a catalog generated with each runtime version; and a HELP that every agent answers. The catalog follows SY.timer's principle. |
| T4 | **Archi reads the catalog to design solutions.** It is information, not something Archi operates. A role never promises a tool that does not exist. |
| T5 | **Every AI agent loads every base tool.** Archi does not choose tools per agent: they are base tools. |
| T6 | **Roles stay reusable and independent of channels and tools.** A "sales" role serves an agent on WhatsApp and one on email. Skills and handbooks may name base tools, since every agent has them. The definition model (role, skills, handbooks, personality) does not change. |
| T7 | **The names:** what an agent can do is called **tools**, and the generated document is **the tool catalog**. The word "capabilities" is not used for this (A-94). |

**Also decided, the same day:**

| ID | Decision |
|---|---|
| T8 | **The AI provider is per node (A-93).** The AI key stays restricted to the node's tenant, but not to the hive's provider. One agent may run on Claude and another on OpenAI. |
| T9 | **In the AI node, "capabilities" goes; "tools" is its only word for what an agent can do (A-94).** Infra nodes keep their own uses (the router's WAN HELLO, SY.timer's HELP, the security sense of a capability link, Cloud's catalog). |
| T10 | **The frontdesk (`SY.frontdesk.gov`) is a SY node with its own needs.** Its tools are its own, outside the base-tool catalog. |

**T8 in the code (checked 2026-10-26).** The SDK's clients take the provider on every call, both
protocols exist, and the usage records keep the provider and the model of each call. Two nodes on
the same code with different providers share nothing. What changes:

- `behavior.provider` is accepted; without it, the node uses the hive's `ai.default_provider`;
- the vault lookup follows the node's provider;
- `behavior.base_url`, when set, must be that provider's host. Today either host passes with
  either provider, which is a latent mismatch;
- the docs and Archi learn the field.

The SY nodes (cognition, Archi, the frontdesk) keep the hive's provider.

**T9 in the code:** the AI node's only "capabilities" is `behavior.capabilities.multimodal`,
whether the model receives images. It is about the model, not a tool.

- It becomes `behavior.multimodal`.
- The old key is refused. PROD's AI configs are converted once, and the conversion is reported.
- The default role's "Do not invent platform capabilities" becomes "Do not offer tools you do not
  have".

## 3. The details (technical, proposed by Claude 2026-10-26; the operator may veto)

1. **The card.** `FunctionToolDefinition` grows into a card with these fields:
   - `name`, `summary` (one line), `when_to_use`, `limits` (a list) and `present_when`, the
     conditions. For example, the state tools need an interlocutor, and `publish_html_page` needs
     an identity;
   - `input` and `output` as JSON schemas;
   - `errors`, as `{code, meaning}`.

   The model's definition is rendered from it: the summary, when to use it, the limits, and one
   line on what it returns, with the input schema. Model APIs take only a name, a description and
   the input.
2. **The catalog** is generated from the cards into the repo:
   - `docs/reference/agent-tools.md`, plus its JSON;
   - a CI check fails when the catalog and the code disagree, as the admin catalog's parity check
     does;
   - the package ships the JSON next to the runtime (`dist/runtimes/ai.generic/<version>/tools.json`),
     which is the copy Archi reads. `get_runtime` answers it as `tool_catalog`, so Archi can
     design agents before any of them runs.
3. **The HELP** is one generic verb, `NODE_HELP`, which any node with a self-description answers.
   The contract (decided 2026-10-26):
   - **One card:**
     `{name, summary, when_to_use, limits: [..], present_when, input: <JSON schema>,
     output: <JSON schema>, errors: [{code, meaning}]}`.
   - **The catalog file:** `{runtime, runtime_version, tools: [card, ..]}`, every base tool.
   - **`NODE_HELP_RESPONSE` from the node:** `{status: "ok", node_name, runtime,
     runtime_version, tools: [card, ..]}`, the same cards as the catalog. `present_when` says
     when each tool is there.
   - **The route:** `NODE_HELP` travels as `NODE_STATUS_GET` does: SY.admin → the motherbee's
     orchestrator → the node's hive orchestrator → the node. SY.admin addresses only its own
     hive's orchestrator.
   - **A node that does not describe itself:** `node_help` answers 200 with `help: null` and the
     reason.
     - The orchestrator answers at once, without asking, for a node whose runtime (or runtime
       base) is not `ai.generic`.
     - An AI node that does not answer within 3 s gets a neutral reason.
     - The wait runs in its own task, so it never holds the orchestrator.
   - `ai.generic` answers it with its catalog.
   - SY.timer keeps `TIMER_HELP`, and may answer `NODE_HELP` later.
   - SY.admin exposes it as `node_help`, for the operator and Archi, because SY.admin mediates.
4. **The first release (proposed 0.1.86):**
   - the cards for the 14 tools that exist;
   - the catalog and its CI check;
   - `NODE_HELP` and `node_help`;
   - Archi reading the catalog;
   - T8, T9 and A-95.

   No new tool is in it.
5. **The new base tools come after, one per iteration, each with its own rule:**
   - the current time;
   - timers (SY.timer);
   - a message to another node (SY.admin mediates every action);
   - reading from the blob.

   Fluxbee tools (T1) come later.

## 4. Related

- FINDINGS A-93: an AI node cannot choose its provider (pending).
- FINDINGS A-94: the cleanup of "capabilities".
- FINDINGS A-95: `ai.generic` logs the full link of a page it publishes.

## 5. Every model tool in the code today (inventory, 2026-10-27)

The starting point for consolidating them one by one (the operator, 2026-10-27).

**A. The AI agents' base tools (`ai.generic`).** All of them have cards since 0.1.86.

| Tool | What it does |
|---|---|
| `thread_state_get`, `thread_state_put`, `thread_state_delete` (AI SDK) | One saved record per person, across that person's conversations with the agent. |
| `generate_{csv,text,json,markdown,html,pdf,xlsx,docx,png,jpeg}_artifact` | A file attached to the reply. Since 0.1.87, up to 8 a reply, and the bytes never go back to the model. |
| `get_current_time` (0.1.87) | The time now, in UTC or an IANA zone, from SY.timer on the agent's hive. |
| `publish_html_page` | A page at a public link that expires in 24 h (SY.admin's `publish_artifact`). |

**B. The frontdesk's own tools (`SY.frontdesk.gov`, T10):** `ilk_register`, plus
`thread_state_get` and `thread_state_put`.

**C. Archi's tools (`src/bin/sy_architect.rs`, 19, plain definitions, no cards):**

| Where | Tools |
|---|---|
| The operator's chat | `fluxbee_system_get` (SY.admin's reads), `list_agent_assets`, `get_agent_asset`, `delete_agent_asset`, `create_agent_{role,skill,handbook,personality}_asset`, `fluxbee_start_pipeline`, `fluxbee_pipeline_action`, `fluxbee_plan_compiler` |
| The designer | `assess_layer`, `get_manifest_current`, `query_hive`, `submit_solution_manifest` |
| The design auditor | `submit_design_audit_verdict` |
| The programmer | `submit_artifact_bundle` |
| The plan compiler | `query_hive`, `get_admin_action_help`, `submit_executor_plan` |

**D. SY.admin's executor (runs Archi's plans, `src/bin/sy_admin.rs`):**

- one tool per plan step, named after its admin action and built from the action registry;
- `get_admin_action_help` and `report_step_issue`.

**E. Programmed elsewhere, with no agent tool yet** (the candidates of §3.5):

| Candidate | What already exists |
|---|---|
| The current time | SY.timer: `TIMER_NOW`, `TIMER_NOW_IN`, `TIMER_CONVERT`, `TIMER_PARSE`, `TIMER_FORMAT`. Any node may call SY.timer directly. |
| Timers | SY.timer: `TIMER_SCHEDULE`, `TIMER_SCHEDULE_RECURRING`, `TIMER_GET`, `TIMER_LIST`, `TIMER_CANCEL`, `TIMER_RESCHEDULE`, `TIMER_PURGE_OWNER`, `TIMER_HELP`. |
| A message to another node | Every node sends messages through the SDK's node client; SY.admin's `send_node_message` is the operator's. |
| Reading from the blob | The SDK's `BlobToolkit`. Agents write blobs (the generators) and receive attachments, but cannot read one on demand. |
| Taking a page down | SY.admin's `unpublish_artifact` already admits an AI node on its own publications. |

**Overlaps found:**

- `get_admin_action_help` exists twice, in Archi's plan compiler and in SY.admin's executor.
- `query_hive` serves both the designer and the plan compiler.
- Archi's and the executor's tools have no cards: T2 covers the agents' tools only.

## 6. Consolidating A, one iteration at a time (the operator, 2026-10-27)

**The criterion:**
- only A (`ai.generic`) gets an ordered toolbox; it is the node that grows with the product;
- B, C and D keep their own tools: they are specific, and integrating them buys nothing;
- the mechanism of §3 is shared, and the toolboxes are not;
- the overlaps of §5 get aligned.

**The order:** least risk first.
1. The four defects of A's tools.
2. The current time.
3. Reading from the blob.
4. Timers.
5. A message to another node.
6. Taking a page down.
7. The overlaps.

### 6.1 Iteration 1 (0.1.87): the defects, the time, the overlaps

**Validated on 8.x (2026-10-27; ledger 0.1.87).** The gate found A-109: a runtime-only release
leaves running agents on the old binary.

1. **A-97: the PDF writes Spanish.**
   - The font declares `WinAnsiEncoding`, and the text is encoded into it, so accents, ñ, ¿ and ¡
     print. A character outside that set becomes `?`.
   - Lines wrap to the page width, and the text continues on new pages.
2. **A-98 and A-99: a generated file stays out of the model's context, and the model's own reply
   is kept.**
   - A generator keeps the file aside, for the reply, and answers the model only
     `{status, filename, mime, size_bytes}`.
   - The reply is the model's own final text, carrying every file the turn made, not only the
     last one, up to a cap.
   - The generators' `text` input goes away: the model writes its reply as it always does. The
     publish card no longer has to tell it where to put the link.
3. **A-104: the model defaults to the hive's.**
   - Without `behavior.model`, the node uses the hive's model for its provider
     (`hive.yaml ai.providers.<provider>.model`).
   - CONFIG_GET shows the model and where it came from.
   - SY.admin's and Archi's examples name neither the provider nor the model, unless a node needs
     one of its own.
4. **The time:** a base tool, `get_current_time`.
   - Input: an IANA time zone, optional; UTC by default.
   - It answers through SY.timer on the agent's own hive (`TIMER_NOW_IN`, the SDK's
     `TimerClient`).
   - It is present when the hive runs SY.timer: the motherbee and the workers.
5. **The overlaps:**
   - the executor's `get_admin_action_help` takes `action_name`, as Archi's does; both already
     call the same SY.admin action;
   - `query_hive` is one tool registered in two of Archi's sub-agents, which is no overlap.

### 6.2 Iteration 2 (0.1.89): decided by the operator, 2026-10-27

Items 3–6 act beyond the agent's own reply. The operator decided their rules:
*"la carga de programación decidila vos, lo que no quiero es dejar cosas escondidas, registrá si
no se hace"*.

- **`read_blob`:** text formats, the whole hive (*"hoy prefiero que lea todo lo del hive"*).
- **A reminder:** for the agent itself. It returns to the same conversation, whose destination
  gets the answer.
- **`send_message`:** to a destination in the agent's own tenant, *"un mensaje con destino"*; the
  destination can answer; OPA decides.
- **`unpublish_html_page`.**

The details below were checked in the code on 2026-10-27 and proposed by Claude. Three facts
differ from what the decisions assumed:
- **OPA does not see a message that has a destination.**
  - The user policy only picks a destination for a message without one.
  - PROD has no user policy.
  - So "OPA decides" has no place to run today (FINDINGS A-113).
- **No IO node can write to a person first.**
  - An IO node answers only inside the conversation it brought in: the Slack thread, the
    WhatsApp customer.
  - Nothing maps a person to a channel (FINDINGS A-114).
- **A fired timer never reaches the model, and it carries no conversation.**
  - The AI node's control plane refuses `TIMER_FIRED`.
  - SY.timer returns only the payload it was given.

#### 6.2.1 The shared piece: answering a conversation later

A fired reminder and an answer to a `send_message` both start a turn whose output does not go to
what triggered it. It goes to the conversation the agent was in.

- **The conversation's address** holds what a reply in that conversation needs:
  - the node that brought the conversation in (an IO node or an agent), by the name the router
    stamped;
  - the `thread_id`;
  - the counterpart's ILK;
  - `ich`;
  - `context.io`, with its `reply_target`.

  The runner takes the address from the message of an ordinary turn and hands it to the tools.
- **A turn for a reminder or an answer:**
  - its memory and state are the conversation's (keyed by the counterpart, as today);
  - its input says what happened: *"Reminder you set on <date>: <note>"*, or *"Answer from
    AI.sales@worker1 to your message: <text>"*;
  - its output is a new message to the conversation's address;
  - what triggered the turn gets no reply.
- **The node can send a message that is not a reply.** The runtime stamps it as it stamps a
  reply: the node's UUID and its ILK.
- **Whether a person gets it depends on the IO node:**
  - io.slack posts it in the thread;
  - io.wapp sends it to the customer;
  - io.api drops it, because no request is waiting for it (FINDINGS A-114).

#### 6.2.2 `read_blob`

- **Input:**
  - `blob_name`, as an attachment shows it (`<stem>_<hash16>.<ext>`);
  - `offset` in bytes, optional.
- **Text only:**
  - txt, csv, json, md and html, by the name's extension;
  - bytes that are not UTF-8 are refused.
- **What it reads:**
  - its hive's store, which the motherbee and the workers share through Syncthing (on in PROD);
  - across tenants (A-106);
  - it waits up to 20 s for a blob still on its way from another hive.
- **Limits:**
  - files up to 10 MiB, the attachment cap;
  - each call returns at most 64 KiB of text from `offset`, cut on a character boundary;
  - with `size_bytes`, and `next_offset` (null at the end).
- **Errors:** `invalid_blob_name`, `not_text`, `not_found`, `too_large`.
- **The SDK reads a blob by its name:** without following links, and checking the hash in the
  name.
- **Where the model gets names to read:**
  - an attachment now shows its blob name;
  - an attached text cut at 100,000 characters says so and names its blob.
- **With it (FINDINGS A-111):** a CSV or HTML attachment becomes text, as txt, md and json are.
  - Today it reaches the model as a document.
  - Claude accepts only PDF documents, so the whole message fails.

#### 6.2.3 `unpublish_html_page`

- **Input:** `publication_id` (`pub:<uuid>`) or the page's `url`.
  - The link is what a person has.
  - The agent keeps no list of its pages. It serves many people, and a list would show one
    person another's pages.
- **SY.admin's `unpublish_artifact`:**
  - also takes the link's key;
  - checks the owner as today: the exact node that published;
  - answers `NOT_FOUND` for a publication that does not exist, where today it answers
    `INVALID_REQUEST`.
- **Answers:**
  - `unpublished: true`;
  - `false` when the page was already down or had expired.
- **Errors:**
  - `not_found`;
  - `not_yours`;
  - `unpublish_failed`, retryable: unpublishing twice is harmless.
- **Present** wherever `publish_html_page` is. The publish card no longer says a page cannot be
  taken down early.
- **Fixed with it:**
  - a page taken down in the same turn is not added to the reply;
  - when SY.admin returns no public address, the tool tells the model the page was not
    published. Today that page stays live until it expires. Now the tool takes it down.

#### 6.2.4 `set_reminder`, `list_reminders`, `cancel_reminder`

- **`set_reminder`'s input:**
  - `note`, up to 500 characters, written to stand alone;
  - exactly one of `in_minutes` or `at` (ISO 8601 with its offset).
- **Limits:**
  - at least 1 minute (SY.timer's minimum), at most 30 days;
  - at most 20 pending reminders per agent, counted at SY.timer.
- **Mechanism:** `TIMER_SCHEDULE` on the agent's hive.
  - `client_ref` is `reminder:<uuid>`.
  - The payload holds the note, when it was set, and the conversation's address.
  - `missed_policy` is `fire`, so a reminder due while SY.timer was down fires when it returns.
- **When it fires:**
  - the runner accepts `TIMER_FIRED` only from `SY.timer@<its hive>`, and only for a reminder it
    knows;
  - it runs the turn of §6.2.1.
- **A missed fire:**
  - SY.timer does not retry, so a fire is lost while the agent is down: on a hive reboot (SY.timer
    starts before the agents), or during a relaunch.
  - The agent keeps a record of the reminders it handled. At start it asks SY.timer for its fired
    reminders. Those it missed in the last 24 h it handles, saying they are late; older ones it
    drops with a log line.
  - The record also drops a duplicate fire.
- **`list_reminders` and `cancel_reminder`** see only the current conversation's reminders. An
  agent serves many people.
- **Purge:** a purged node's reminders go with it (SY.orchestrator's `TIMER_PURGE_OWNER`).
- **Visibility:** SY.timer shows every timer, payload included, to the nodes of its hive and
  through SY.admin. A reminder is not hidden from the operator (FINDINGS A-115).

#### 6.2.5 `send_message`

- **Input:**
  - `to`: an agent or a WF by its full name (`AI.<name>@<hive>`, `WF.<name>@<hive>`);
  - `text`.
- **Refused:**
  - any other kind of node: SY, RT and IO (a person cannot be addressed yet, A-114);
  - the agent itself;
  - a destination outside the agent's tenant, as the operator decided. The tool reads the
    destination's tenant through its ILK in the identity shared memory, and it refuses a node
    without one.

  [ilk-tenant-model-v1](ilk-tenant-model-v1.md) calls a tenant a cost center, not a
  permission. This is a rule of the tool, not of routing.
- **The send:**
  - a user message in the current conversation's thread;
  - the tool records a pending send, for 24 h, across restarts;
  - it waits up to 2 s for the router's `UNREACHABLE`, then answers `not_delivered` or `sent`.
- **The answer:**
  - a message from that destination, with the send's `trace_id` or in that thread, is the
    answer;
  - it starts the turn of §6.2.1, so the agent tells its own conversation, not the other agent;
  - an agent answers another agent's `send_message` with an ordinary reply, and that reply
    returns to the sender's conversation.

  That is what keeps two agents from answering each other forever.
- **The cap:**
  - in one thread, an agent sends at most 10 messages to agents or WFs, its replies included;
  - the count restarts when a person writes in that thread, or after an hour without such
    messages;
  - past it, the tool answers `limit_reached`, and the agent stops replying to agents and WFs in
    that thread, with one log line;
  - answers to its own sends still reach its conversation.
- **A WF:**
  - a message starts a WF only if the WF's `input_schema` accepts the agent's text;
  - the WF answers in the agent's conversation only if its definition replies on the caller's
    thread (`thread_id_ref`) to the caller (`target_ref`).

  Archi designs that (FINDINGS A-114).
- **OPA** is not in the path; the router's VPN check is (A-113).
- **The name:** `send_message` is also the WF action's name and the router's `action_class`
  label, with the same meaning. It stays.

#### 6.2.6 Fixed on the way

- **FINDINGS A-112:** a router notice that reaches the AI node's control plane is answered.
  - The notices are `UNREACHABLE` and `TTL_EXCEEDED`; the answer is a `CONFIG_RESPONSE` to the
    router.
  - This most likely loops while a reply cannot be delivered. The build confirms it with a test.
  - The node now never answers a router notice: it logs it, and `send_message` reads it as its
    failure.
- **FINDINGS A-110:** the publish script writes every file of a version first and moves the
  manifest last. The catalog's source is a parameter.
- **The docs this iteration relies on match the code again:**
  - docs/sy-timer.md (FINDINGS A-115);
  - docs/blob-annex-spec.md;
  - the unpublish messages in docs/nodes/sy-admin.md.

**Pending, by the operator's choice: `update_html_page`, the same link with new content**
(FINDINGS A-107).
- A link is content-addressed (`public/<sha256>`), and the edge refuses to rebind a key to other
  facts. So updating today means publishing again and taking the old page down.
- A stable link would need an explicit edge operation from SY.admin, for the publisher's own
  publications.
