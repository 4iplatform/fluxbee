# The Digital Executive v1, and the operator's large topics of 2026-11-20

**Status:** talk notes, kept as the talk goes; **no design yet.** The operator, 2026-11-24: the
design will not come out at once, the ideas come as everything connects; the details are recorded as
they are talked, then all reviewed together, and only then a solution is documented. This is the single place for the
Digital Executive. §1 also keeps the other three large topics the operator raised the same day,
until each gets a talk and a document of its own.

---

## 1. The operator's large topics (2026-11-20)

io.web's track closed the same day. The operator: the IO family's revamp is not next; these are.

1. **IO nodes from a package.** Fluxbee should one day add IO nodes from a kind of package: a
   toolkit or skeleton, beyond today's SDK, where every tool the nodes use is solved inside and only
   the outward interface changes. To check first whether it is easy or possible, before a large
   revamp of the IO nodes.
2. **Tasks on timers.** A timer could carry a task area in its JSON. With a description, a timer is
   a task: almost a TODO with a calendar included, which wakes its owner or its assignees. Something
   atomic, a task with a description, to integrate actions between agents, or for one agent, without
   a WF. The operator thinks it is easy to build.
3. **A conversation viewer in Archi.** Conversations in sequence, ordered by interlocutor and time,
   organized by thread, with cognition's results too: an upgrade of what Archi shows today. The
   starting point is a tool Fluxbee Cloud's team wrote, which the operator liked
   (`4iplatform/WhatsappAutomation`, branch `261023-DataVisualization`, `tools/visor-casos`); the
   operator copies it to the Mac.
4. **The Digital Executive** (§2 on), where everything starts. The operator: a complete and complex
   topic, and long.

---

## 2. The Digital Executive, from outside (the operator's words)

- Fluxbee does not solve "agents". It groups varied tools to solve complex tasks, imitating humans
  in their work. Hence a new kind of ILK, the **Digital Executive** (its sales name; a simpler one
  may come).
- A Digital Executive has a set of roles or capabilities beyond one agent: several of Archi's
  solutions grouped under the concept.
- It has an identity recognized on the internet. A possible way: `did:web` or `did:webvh` with an
  `agent-card.json`, and `did:key` with Verifiable Credentials.
- It is an entity with a phone, an email, a first name and a last name: a virtual human that solves
  problems with several Fluxbee solutions and is responsible for some things, its capabilities or
  roles.
- In Fluxbee Cloud, an admin creates a Digital Executive and assigns it a role or capability, maybe
  with template solutions. The admin can then create solutions of their own, which add to it, to
  adapt its behaviour or give it new capabilities, so that it fits the problem.
- The legacy website already sold the idea (`docs/website/legacy/preview-3.html`): *"Digital
  Workforce, with Digital Executives for concrete jobs"*, one booking meetings from LinkedIn, another
  selling and supporting customers through WhatsApp and Instagram.

---

## 3. What exists (read in the code, 2026-11-20)

- **ILK types:** `human`, `agent` and `system` (`src/bin/sy_identity.rs`). An ILK is the actor,
  unique, never duplicated per tenant; its tenant is the cost center it is charged to, and grants
  nothing ([ilk-tenant-model-v1](ilk-tenant-model-v1.md)).
- **Every managed node is an actor of its own.** The orchestrator gives each AI, IO, WF and managed
  RT instance its own ILK of type `agent` at spawn (`derive_ilk_type_for_node`,
  `src/bin/sy_orchestrator.rs`). `system` is only for SY's own.
- **An agent's definition lives on its ILK** ([identity-v2.1-agent-definition-addendum](identity-v2.1-agent-definition-addendum.md)):
  hashes of a role (what it does, one), skills (several), handbooks (several) and a personality (who
  it is as a person: nationality, languages, timezone, background; at most one). `ai.generic`
  composes its prompt from them; Archi builds the assets.
- **Memory follows the agent's ILK.** Cognition's unit is a communication `(thread_id, agent ILK)`
  ([cognition-hands-v1](cognition-hands-v1.md)): what one agent lived with a person is its own; a
  WF or another agent in the same conversation is another hand, named as such.
- **Channels belong to ILKs** (email, phone, a Cloud account), each key in a scope
  ([ilk-tenant-model-v1](ilk-tenant-model-v1.md)). Today a person holds them; the number or address
  an agent writes from is the IO node's configuration, not an ILK's channel.
- **Solutions:** the solution manifest ([solution-manifest-spec_1](solution-manifest-spec_1.md), one
  per tenant-solution, partly built) and Archi's solutions chapter, open
  ([wf-orchestration-v1](wf-orchestration-v1.md) §5.5.3; the operator, 2026-11-11: a repository
  that keeps solutions as sources with their versions).
- **Nothing** in the code or docs holds a DID, an agent card or a Verifiable Credential.

---

## 4. Notes, 2026-11-20: one person, born and then hired

The question: what a Digital Executive is in Fluxbee's identity, and how it relates to the nodes
that do its work. The example asked: "Laura" answers Ana on WhatsApp as sales, and a week later
writes to her by email as collections; with today's model those are two agents, two memories, two
names.

**The operator's answer:** the focus is to imitate a person. *"Una persona no tiene un wapp por cada
rol (ventas, cobranzas), es una sola que responde según el caso."* Hence:

- **One person, one set of channels.** The executive's channels in and out, at least those that
  reach humans, are unique: one WhatsApp, one email, one Instagram, one LinkedIn profile. Whoever
  creates it sees what it can do; the channels do not multiply with its roles.
- **Born: the basics, the same for everyone.** Creating an executive creates its email, its face,
  its WhatsApp, its first and last name, its LinkedIn profile and so on, and its human capabilities:
  an AI agent that holds its "soul" and answers as a human, able to write an email or chat on
  Instagram. **The core is created the same for all: IO + AI + something more Fluxbee builds that is
  useful.**
- **Hired: responsibilities.** When a company hires it, it gets responsibilities, with the
  capabilities and the roles inside that company. The operator sees roles and capabilities as
  responsibilities, the word that holds everything: *"si es responsable es porque tiene la
  capacidad"*.
- **A solution gives a responsibility; it does not define the executive.** An Archi solution, with
  its deploy plan and the rest, separates one responsibility from another.
- **It grows with its responsibilities.** The executive that was one agent becomes more complex
  with internal agents that handle each task with flows: multi-agent for sure, more responsibility,
  more agents, always atomized and relative to the problem the company must solve. So Archi must be
  able to make a custom solution for each responsibility.
- **The company's people are in Fluxbee, with their roles,** so that the executive knows where to
  pass a problem: to its supervisor, for one.

**What this is in the model:** option (a) of the question: one actor, many hands. The executive
is one ILK of its own kind, the one that speaks outward and holds the channels; its memory with each
person is the executive's; the nodes of its responsibilities keep their own ILKs and work for it.
Options (b), a label over several agents, and (c), one node that does everything, were not taken.

---

## 5. Notes, 2026-11-24: one conscious agent, and many channels

- **Asked:** who speaks to the humans, the soul alone, with the responsibilities working behind it,
  or each responsibility's agents through the executive's channels as the executive.
- **The operator:** better that the conscious one is always the same agent. The executive's manager,
  its conscious, needs an agent dedicated to the interaction. The concern: whether the number of IO
  channels causes a problem with this. The structure may be more complex for that reason, because
  communications can fire WFs, and what comes first and what after is important.
- **Facts read for it (2026-11-24):**
  - `ai.generic` runs at most 4 model turns at once (`worker_pool_size`, default 4) and queues up to
    128 more in the order they came; the next is refused `ai_runtime_busy`
    (`crates/fluxbee_ai_sdk/src/runtime.rs`).
  - Nothing keeps one conversation's turns in order: two messages of the same person can run at once
    on two workers, and their answers can cross.
  - Each IO node relays what comes in to one node, its `io.dst_node` (usually a WF), or lets the
    router resolve it.
  - Each node instance gets its own ILK at spawn, so a conscious that had to run as several processes
    would be several actors today.

- **The operator, on the order (2026-11-24):** one line per person is right, by ILK. The thread id is
  per channel and interlocutor, so the line needs another id if it is not the thread id. Time orders
  everything inside a line. The line should be ordered naturally by the system, with a tool of its
  own: not a WF doing that work (*"no me gusta el WF haciendo ese trabajo"*); to think about. On the
  volume: *"sería por IO, según entiendo"* (asked back what it means, see the chat).
- **Ids that exist, read for it (2026-11-24):**
  - **The old CTX** ([history/11-context](history/11-context.md), v1.16 of 2026-02-04): "keep
    conversations with history between interlocutors across different channels", but built as
    `ctx = hash(ilk + ich)`, one per interlocutor **and** channel. It left the protocol in 0.1.79
    (commit `41decbd`, the cleanup of 2026-10-17): nothing read it. The thread id took its place, and
    the IO owns it (the operator, 2026-10-30).
  - **Cognition's `context_id` and `scope_id`** (`src/bin/sy_cognition.rs`,
    [scope-binding-energy-spec](scope-binding-energy-spec.md)): a context is a topic ("billing
    dispute"), a scope a set of contexts that hold together. Cognition's scene, not a conversation's
    key.
  - **What crosses channels today is the person's ILK:** one person, one ILK with several channels.
    A Cloud account is one per email; a contact is charged to the company it wrote to, and the same
    person from a phone and from an email can be two ILKs until an explicit proof joins them (D13,
    open since 2026-10-15, [ilk-tenant-model-v1](ilk-tenant-model-v1.md)).
  - So the line's natural key is the pair (the person's ILK, the executive's ILK), and time orders
    it; the person who is two ILKs is D13's question.
- **Where a tool that orders the lines could live** (noted, nothing chosen): the router, which sees
  every message (the operator, 2026-10-16: *"si hay algo global que resolver en los hives que sea el
  router"*); the AI runtime of the conscious, which today starts turns in arrival order on 4 workers
  with no line per person; or a node of its own.

## 6. Later questions (not yet)

- The tool that orders the lines, and the person who is two ILKs (D13).
- The core: what "something more" holds besides IO and AI (an agenda: the tasks on timers of §1.2?).
- The company's people and their roles in Fluxbee. An earlier decision to weigh
  ([ilk-tenant-model-v1](ilk-tenant-model-v1.md), the operator 2026-10-14 and 10-15): companies,
  their members and billing live in Fluxbee Cloud; Fluxbee keeps flat relations with no rights, and
  a tenant is a cost center, never a company. Knowing one's supervisor is new: knowledge for the
  executive, not a permission.
- A responsibility against a solution: what a template holds, and what Archi makes to measure.
- Its definition: which part is the executive's (its personality, who it is) and which is each
  responsibility's (what it does).
- Its channels: owned by its ILK, and the IO nodes that carry them.
- Its identity on the internet: `did:web` / `did:webvh` + `agent-card.json`, `did:key` +
  Verifiable Credentials; who issues and who verifies.
- Fluxbee Cloud's flow: create one, assign a role, add solutions; what Cloud stores and what the hive
  does.
- Its costs (its tenant), its responsibilities, its permissions, its lifecycle (an executive that
  made history is disabled, never deleted).
