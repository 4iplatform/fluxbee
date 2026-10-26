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

## 3. Open, for the next iteration

1. **The card's exact format.** Its fields, and how its JSON schemas become the model's tool
   definition and the catalog.
2. **Where the catalog lives and how Archi reads it.** For example, next to the runtime in
   `dist/runtimes/ai.generic/<version>/`, or through the HELP.
3. **The HELP message:** one generic verb for every node, or one per family, as `TIMER_HELP`.
4. **New base tools, one at a time, each with its own rule.** The operator's examples:
   - the current time;
   - timers (SY.timer);
   - a message to another node (with the rule that SY.admin mediates every action);
   - reading from the blob.
5. **The frontdesk runtime** (`ai-frontdesk-gov`) has its own tools. Does it get its own catalog?
6. **Fluxbee tools** (T1), after the base ones.

## 4. Related

- FINDINGS A-93: an AI node cannot choose its provider (pending).
- FINDINGS A-94: the cleanup of "capabilities".
- FINDINGS A-95: `ai.generic` logs the full link of a page it publishes.
