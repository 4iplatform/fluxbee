# Agent tools v1: what an AI agent can do, declared once

**Status:**

- The direction was agreed with the operator on 2026-10-26.
- The first release, 0.1.86, is validated on 8.x (2026-10-27; ledger 0.1.86). It covers §3.1–§3.4:
  the cards for the 14 existing base tools, the catalog, `NODE_HELP`, and T8, T9 and A-95.
- Iteration 1 of §6, 0.1.87, is validated on 8.x (2026-10-27).
- 0.1.88 fixed what that gate found (FINDINGS A-109, with A-101 and A-102): running agents move onto
  a new runtime by themselves. Validated on 8.x (2026-10-27).
- Next: iteration 2 (§6.2), 0.1.89: `read_blob`, `unpublish_html_page` and fixes. The operator
  moved reminders and messages to others to WFs (§6.3).

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

### 6.2 Iteration 2 (0.1.89): decided by the operator, 2026-10-27, in two steps

**First,** the operator decided four tools, each acting beyond the agent's own reply:
- `read_blob`, with text formats, from the whole hive (*"hoy prefiero que lea todo lo del hive"*);
- a reminder for the agent itself;
- `send_message`, to a destination in the agent's own tenant;
- `unpublish_html_page`.

The rule for all four: *"la carga de programación decidila vos, lo que no quiero es dejar cosas
escondidas, registrá si no se hace"*.

**Then, once the code was checked, the operator decided that the AI node stays a pure
responder.**
- **An agent has two kinds of event:** it answers a message, which is reactive, and a timer
  fires, which is active.
- **A destination other than the sender belongs to a WF.** The operator: *"si queremos hacer el
  timer se pone un WF, se programa ahí el routeo y se lanza un timer… el WF cuando se despierta
  le manda al nodo AI el mensaje, luego el nodo AI evalúa y responde al WF, no cambia nada en el
  nodo AI, queda clara la programación del routeo"*.
  - The alternative was a timer that carries a destination named in the agent's prompt. It lost:
    *"la 2da es la forma más ortodoxa de hacerlo, más visible también"*.
  - Archi orchestrates with WFs: *"el WF es la interconexión inevitable determinística hecha con
    workflows… no queda otra"*.
- **OPA stays for rules of substance:** security, general systems, and implementations that need
  clear rules. It is not for agents' messages. A rule for each pair of nodes that talk would end
  as *"una enredadera de reglas OPA imposible de debug"* (FINDINGS A-113, decided).

So the reminders and `send_message` leave the AI node, together with the machinery they needed:
a send that is not a reply, and a turn that answers a conversation later. Their WF side is §6.3.
Two of the checked facts still stand:
- no IO node can write to a person first (FINDINGS A-114);
- a fired timer is sent once and never retried (FINDINGS A-115).

**0.1.89 builds the rest:**
- §6.2.1 `read_blob`;
- §6.2.2 `unpublish_html_page`;
- §6.2.3, the fixes found on the way.

#### 6.2.1 `read_blob`

- **Input:**
  - `blob_name`, as an attachment shows it (`<stem>_<hash16>.<ext>`);
  - `offset` in bytes, optional.
- **Text only, judged by its content** (changed by the review, 2026-10-27):
  - a blob whose bytes look binary (a NUL byte) is refused;
  - the rest is decoded as attachments are, so invalid UTF-8 shows as `�`;
  - so a log, a YAML file or a CSV exported in Windows-1252 reads too. The operator's txt, csv,
    json, md and html are examples, not a list.

  The first build judged by the name's extension and refused bytes that were not UTF-8. A cut
  attachment then named a blob that `read_blob` refused.
- **What it reads:**
  - its hive's store, which the motherbee and the workers share through Syncthing (on in PROD);
  - across tenants (A-106);
  - it waits up to 20 s for a blob still on its way from another hive.
- **Limits:**
  - files up to 10 MiB, the attachment cap;
  - each call returns at most 64 KiB of text from `offset`, cut on a character boundary;
  - with `size_bytes`, and `next_offset` (null at the end);
  - at most 192 KiB of `read_blob` text per message. The tool loop sends every earlier piece
    again on each of its turns, so an unbounded read of a large file costs hundreds of thousands
    of tokens.
- **Errors:** `invalid_blob_name`, `not_text`, `not_found`, `too_large`,
  `read_budget_exhausted`.
- **The SDK reads a blob by its name:** without following links, and checking the hash in the
  name.
- **Where the model gets names to read:**
  - an attachment now shows its blob name;
  - an attached text cut at 100,000 characters says so, names its blob, and gives the byte offset
    to read on from.
- **The journal carries neither a live link nor a blob name.** The incoming message's preview
  redacts both.
  - A person pastes a page's link to have it taken down.
  - A blob's name lets an agent read it across tenants (A-106).
- **With it (FINDINGS A-111):** a CSV or HTML attachment becomes text, as txt, md and json are.
  - Today it reaches the model as a document.
  - Claude accepts only PDF documents, so the whole message fails.

#### 6.2.2 `unpublish_html_page`

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

#### 6.2.3 Fixed on the way

- **FINDINGS A-112:** a router notice (`UNREACHABLE`, `TTL_EXCEEDED`) that reaches the AI node's
  control plane was answered with a `CONFIG_RESPONSE` to the router.
  - A test confirmed the loop.
  - The node now never answers a router notice or any infrastructure message: it logs it.
  - The frontdesk shares that control plane and gets the same fix.
- **FINDINGS A-121:** a node whose purge failed on SY.timer came back within 60 s. The purge now
  records "do not relaunch" right after the stop.
- **FINDINGS A-110:** the publish script writes every file of a version first and moves the
  manifest last. The catalog's source is a parameter. The IO publish scripts had the same defect
  and get the same fix.
- **The docs this iteration relies on match the code again:**
  - docs/sy-timer.md (FINDINGS A-115);
  - docs/blob-annex-spec.md;
  - the unpublish messages in docs/nodes/sy-admin.md.

### 6.3 Timers and routing in a WF (next; the operator, 2026-10-27)

The operator: the WF is *"el menos desarrollado y el que más trabajo complejo va a necesitar"*.
The work goes case by case.

- **The WF as a forwarder.** It rebuilds the message as it received it and never breaks its
  format. It may change the destination, and, when its logic requires it, some content, through
  its own functions (CEL). Today it has *"dos o tres funciones nada más"*.
- **The WF as a participant** whenever Archi's orchestration needs sequences, forwarding, timers
  or escalation.
- **The pattern, in the operator's words (2026-10-27):** a timer fires, launched by the WF's own
  logic. The WF sends the AI node a message. The AI node evaluates it and answers the WF, and the
  WF sends the result to its final destination: *"es un pasamanos pero queda todo
  determinístico"*.
  - When the WF needs the agent's judgment, the agent answers with the state of its evaluation. It
    *"puede colocar la variable con el valor correcto para que el CEL del WF ejecute y cambie
    dependiendo de la respuesta evaluada"*.
  - That is the structured answer (`response_envelope`): the WF names the fields, and its CEL
    branches on them.
- **Routing is by node name, written in the definition** (the operator, 2026-10-27).
  - A person enters and leaves Fluxbee only through an IO node, and exists only once registered,
    through the frontdesk or cloud identity.
  - So a WF never routes by ILK. Its CEL chooses among node names written in the definition, and
    nothing is computed.
  - The ILK travels intact inside the message, for the agent's memory and the IO node's delivery.
  - Today the engine also accepts `target_ref`, a destination computed from data. By this rule it
    does not fit, and the WF review decides whether it goes or is limited to names written in the
    definition.
- **Answering later a person who wrote before:** the WF keeps that message's `context.io` (its
  `reply_target`) in its state. When the timer fires, it sends the result to the IO node with it.
  The review confirms the engine allows it.
- **Writing first to a person who never wrote** stays with the IO family (FINDINGS A-114).
  - The WF sends to the IO node by its name, with the person's ILK, which is known because the
    person is registered.
  - The IO node then delivers to that ILK on its channel, without an inbound message.
- **Already there:**
  - a WF can ask an agent for a structured answer (`meta.context.response_envelope`), so the
    agent returns fields (when to remind, whether to escalate) and the WF acts on them;
  - a WF sees the IO node's `context.io`, with its `reply_target`.
- **Broken today (FINDINGS A-116):** a WF placed in front of a conversation loses the person and
  the conversation for the agent.
- **The full review of the WF** is in [wf-orchestration-v1](wf-orchestration-v1.md) (2026-10-27).
  It covers its engine, its definition language and CEL, and how it is deployed, authored and
  observed. The cases are taken from it one by one.

**Pending, by the operator's choice: `update_html_page`, the same link with new content**
(FINDINGS A-107).
- A link is content-addressed (`public/<sha256>`), and the edge refuses to rebind a key to other
  facts. So updating today means publishing again and taking the old page down.
- A stable link would need an explicit edge operation from SY.admin, for the publisher's own
  publications.
