# The Digital Executive v1, and the operator's large topics of 2026-11-20

**Status:** the talk opened on 2026-11-20; nothing is decided yet. This is the single place for the
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

## 4. The first question: one actor over many hands? (open)

The skeleton before the organs: what a Digital Executive is in Fluxbee's identity, and how it
relates to the nodes that do its work. Asked to the operator on 2026-11-20; see the chat.

---

## 5. Later questions (not yet)

- Roles and capabilities against solutions: is a role one solution, and what does a template hold?
- Its definition: which part is the executive's (its personality, who it is) and which is each
  solution's (what it does).
- Its channels: the phone and the email it writes from, owned by its ILK, and the IO nodes that
  carry them.
- Its identity on the internet: `did:web` / `did:webvh` + `agent-card.json`, `did:key` +
  Verifiable Credentials; who issues and who verifies.
- Fluxbee Cloud's flow: create one, assign a role, add solutions; what Cloud stores and what the hive
  does.
- Its costs (its tenant), its responsibilities, its permissions, its lifecycle (an executive that
  made history is disabled, never deleted).
