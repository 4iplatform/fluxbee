# Agent tools v1: what an AI agent can do, declared once

**Status:**

- The direction was agreed with the operator on 2026-10-26.
- The first release, 0.1.86, is validated on 8.x (2026-10-27; ledger 0.1.86). It covers §3.1–§3.4:
  the cards for the 14 existing base tools, the catalog, `NODE_HELP`, and T8, T9 and A-95.
- Next: the new base tools of §3.5, one per iteration.

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

### 6.2 Iteration 2 (0.1.88): decided by the operator, 2026-10-27

Items 3–6 act beyond the agent's own reply. The operator decided their rules:
*"la carga de programación decidila vos, lo que no quiero es dejar cosas escondidas, registrá si
no se hace"*.

1. **`read_blob`:** an agent reads a file from the hive's blob store by its reference.
   - Text formats only (txt, csv, json, md, html), with a size cap that protects the model's
     context.
   - **Scope: the whole hive, for now.** A blob reference carries no tenant, and the store has no
     per-tenant directory (FINDINGS A-106). The operator's interim choice: *"hoy prefiero que lea
     todo lo del hive"*.
   - **Not in it:**
     - reading a PDF: today the model gets a PDF only when it arrives as an attachment;
     - finding a blob: a search service, in a future hive of its own.

     Both are recorded in FINDINGS A-106.
2. **`set_reminder`, with list and cancel:** a reminder to the agent itself, through SY.timer.
   - When it fires, it comes back to the agent in the same conversation. The agent's answer goes
     to that conversation's destination, a human's channel included.
   - Limits: at least 60 s, at most 30 days, and a cap of pending reminders per agent. The agent's
     node owns them, and they are purged with it.
   - Anything that is a flow belongs in a WF.
3. **`send_message`:** a message to a destination in the agent's own tenant: another agent, a WF,
   or a human's channel. *"Un mensaje con destino."*
   - The destination's answer comes back to the agent as an incoming message.
   - The user policy (OPA) decides what passes. The tool refuses a destination outside the tenant.
   - A cap on back-and-forth between two agents in one thread stops loops.
   - Checked 2026-10-27: today an agent can only reply. `AiBehaviorOutput` is a text or a final
     output, sent back to whoever wrote.
4. **`unpublish_html_page`:** takes down a page the agent published, by its `publication_id`,
   through SY.admin's `unpublish_artifact`, which already admits an AI node on its own
   publications.

**Pending, by the operator's choice: `update_html_page`, the same link with new content**
(FINDINGS A-107).
- A link is content-addressed (`public/<sha256>`), and the edge refuses to rebind a key to other
  facts. So updating today means publishing again and taking the old page down.
- A stable link would need an explicit edge operation from SY.admin, for the publisher's own
  publications.
