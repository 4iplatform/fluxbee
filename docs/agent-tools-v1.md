# Agent tools v1: what an AI agent can do, declared once

**Status:** direction agreed with the operator on 2026-10-26. Nothing built. This is the single
place for the track; the next iteration starts here.

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
