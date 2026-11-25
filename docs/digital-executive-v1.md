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

## 6. Notes, 2026-11-25: a type of its own, the list of its agents, and who keeps it

- **Its name:** from here on the operator calls it the **ED** (its Spanish initials, *Ejecutivo
  Digital*); so does this doc.
- **The ids:** the old CTX is deprecated, and cognition's scope will not serve either (the operator).
  The relation between a person and the ED across channels needs new infrastructure of relations.
- **Its identity: an ILK, of a type of its own.** It interacts like every other actor, so it is an
  ILK; it is not a normal agent, so it needs an extra type that tells it apart.
- **It is a set of agents,** so it holds the list of the ILKs that make it up. That is the new data
  structure the operator meant on 2026-11-24.
- **The question to review: who keeps it.** Either a node of its own that manages EDs, where they
  are created and their solutions, skills, responsibilities and capabilities are associated; or all
  of it stays in what exists (identity, Archi with its solutions, roles, skills and so on). The
  operator leans to a node of its own, *"aunque me duele"*.
- **The conscious's AI node:** either part of a template solution that starts the ED, launched by
  Archi; or that part is the manager node's, *"casi que mejor"*: the node raises the skeleton empty,
  and Archi assigns it the infra's tools.
- **The channels, the problem of 2026-11-24 said plainly:** an ED can handle several channels; the
  healthier shape is either one agent per IO channel, or everything into the conscious.
- **Facts and earlier decisions that bear on it (read 2026-11-25):**
  - The ILK types are a closed set of three, checked by name across `src/bin/sy_identity.rs` and
    written to identity's SHM as a code each (`SHM_ILK_TYPE_HUMAN`, `_AGENT`, `_SYSTEM`). A fourth
    type touches identity's rules, its SHM and the nodes that read it.
  - Identity holds no relation from one ILK to others today. The one ILK-to-ILK link is an alias (a
    merged temporary pointing at its `canonical_ilk_id`); a tenant's sponsor links tenants. The agent
    definition's hashes point at assets, not at ILKs.
  - A list of the nodes that make a whole exists on paper: the solution manifest's `nodes.instances`
    ([solution-manifest-spec_1](solution-manifest-spec_1.md), partly built). It lists one
    solution's nodes, not an actor's.
  - Every user action goes through SY.admin; a node never acts on another node outside the listed
    exceptions (the operator, 2026-10-15). A manager node would create and change through SY.admin,
    as Archi does; and whatever it does must be reachable by Archi, the operator's interface, and by
    Cloud through IO.cloud.
  - State that every node reads stays in shared memory (the operator, 2026-11-10), as identity's
    does.
  - Cognition keys a memory by the ILK of the agent that answered (`CommunicationKey { thread_id,
    agent_ilk }`, `src/bin/sy_cognition.rs`). A conscious that ran as several instances would split
    a person's memory by instance today.
  - In both channel shapes, a person's WhatsApp and email are two threads, so the memory is split
    by thread even with everything into the conscious; the person's line is what joins them, and two
    messages of the same line can run at once in either shape.

## 7. Notes, 2026-11-25 (later): the operator's calls

- **A new ILK type for the ED:** yes.
- **A node of its own gives birth to EDs and manages them.** It groups all of an ED's secondary
  identity, everything except what identity keeps.
- **Cognition:** if it has to be touched for this, it is reviewed then. An ED may need one more id
  in cognition, to group cognition's threads with the same interlocutor and evaluate them together.
- **Instances:** the conscious runs as several instances of the same agent, each its own ILK in the
  ED's list (agreed).
- **Facts read for it (2026-11-25):**
  - What identity keeps of an ILK (`IlkRecord`, `src/bin/sy_identity.rs`): its id, type,
    registration status, tenant, identification, definition (the hashes of its role, skills,
    handbooks and personality), its channels (each with its scope and owner node), and its mark.
    That is the line between identity and the manager node's secondary identity.
  - Cognition keys a communication by `(thread_id, agent_ilk)`. It already weighs both
    interlocutors of a turn, the sender's ILK and the receiver's, for its scope binding
    (`current_turn_ilk_weights`), but nothing groups communications by person.
  - The router delivers a resolved message to one node by name (`resolve_by_name`; a name ending
    in `@*` tries the sender's hive first, then any hive). Nothing picks among several instances, by
    load or by person.

## 8. Notes, 2026-11-25 (later): solutions and the ED

- **The instances wait for the end** (the operator): how a message reaches one of the conscious's
  instances is left for last. Picking by person in the router could be, but the operator dislikes the
  router deciding anything about load.
- **What a solution is to an ED:** Archi's solutions give an ED its tools. An ED may have general
  solutions and particular ones.
- **The relation is the ED node's** (the operator's lean, not yet clear to the operator which is
  better): there is no place for solutions tied to a project; the relation is between solutions and
  EDs. A particular solution's skills, when needed, are associated with one ED; a standard one would
  be native to the ED.
- **Facts read for it (2026-11-25):**
  - Archi's solution today is a versioned manifest (`SolutionManifestV2`,
    `src/bin/sy_architect/pipeline_types.rs`), kept per `solution_id`, one file per version
    (`manifests/<solution_id>/<version>.json`). Its desired state holds topology, runtimes, nodes,
    routing, WF deployments, the OPA policy and ownership; an `identity` section is refused.
  - The tenant is not a field of the solution: each node's config names its own. The spec's "one
    manifest per tenant-solution" ([solution-manifest-spec_1](solution-manifest-spec_1.md) §3) is
    the design, not the key the code uses.
  - A solution runs on one hive (the operator, 2026-11-06).
  - Where solutions live as sources with their versions (the solutions chapter, the operator
    2026-11-11) is still open.
  - The operator, 2026-11-20 (§2): an admin creates an ED and assigns it a role, maybe with template
    solutions, then adds solutions of their own.

## 9. Notes, 2026-11-25 (later): source and deployment

- **Two things called "solution":** the source (the manifest with its versions, what Archi writes)
  and the deployment (the nodes that run it, with their ILKs, on a hive and charged to a tenant).
  For an ED the operator means the deployment.
- **Three layers, right** (the operator): native (what every ED has at birth, the core IO + AI +
  something more, raised by the ED node: not a solution that is associated); standard (a template
  from Archi's catalog, given to an ED when it is hired, and to others); particular (made to measure
  by Archi for one ED).
- **One deployment per ED,** agreed: a standard solution two EDs use is deployed twice. So an agent
  works for one ED only, and the ED's list of ILKs follows from its core and its deployments.
- **CI stays with Archi** (the sources, their versions, the catalog); each version is there to be
  deployed into each executive.
- **The CD's events need records,** a whole topic, the ED node's as the operator sees it.
- **Facts read for it (2026-11-25):**
  - Archi deploys today: its reconciler (`src/bin/sy_architect/reconciler.rs`, pure deterministic
    functions, no AI and no network) diffs a manifest against the actual state, and SY.admin's
    executor runs the plan after the operator's CONFIRM.
  - What is recorded today: SY.admin's command log (each command and its outcome, an executor
    plan's steps), Archi's tracked operations, and the orchestrator's deployment history, which is a
    hive's software (core, runtime and vendor updates), not solutions. Nothing records "this
    version of this solution, deployed for this ED, on these nodes".

## 10. Notes, 2026-11-25 (later): growing by parts, and CD without an ED

- **Not one deployment per ED: incremental** (the operator). Several solutions can be deployed into
  one ED, so it grows by parts without touching what is already deployed; the ED node manages the
  versions running. (What §9 meant by "one deployment per ED" was only that two EDs never share one
  deployment; it stands as one deployment per ED and solution.)
- **The whole CD by the ED node, and its cost** (the operator): then there can be no CD without an
  existing ED. The operator sees no problem, but it makes the platform rigid; its other side effects
  are not clear yet.
- **Facts read for it (2026-11-25), Archi's reconciler today:**
  - Deploying one solution leaves another's alone: a WF or the user OPA policy is removed only by
    the solution that declared it (`declared_by == solution_id`), and the reconciler never kills a
    node that is not in the manifest (a node dropped from a solution keeps running).
  - The user OPA policy is one for the whole installation: a solution that declares one replaces the
    one another solution declared, *"the last one applied wins"* (`reconcile_opa`). Two parts of the
    same ED, each with its own policy, would overwrite each other.
  - Node names are unique per hive, so a standard solution deployed for two EDs needs its names made
    per ED.

## 11. Notes, 2026-11-25 (later): a CD node, and the ED as a deployment

- **Pending, for later:** the user OPA policy that is one for the whole installation (§10).
- **Node names:** distinct per ED, yes.
- **One responsible for the CD** (the operator). Each deployment carries its tenant (that exists)
  and a *why*, what it is for, which groups deployments: an ED, or infra, maybe others. The other
  groupings sit at the same level as an ED.
- **So the node is not the ED's but the CD's, and the ED becomes a deployment** (the operator: *"eso
  me cierra más"*). The node of §6 and §7, which gives birth to EDs, is the CD node; an ED is one
  kind of what deployments are grouped by.
- **Facts read for it (2026-11-25), what is deployed today outside Archi's solutions:**
  - The platform's own software: the package installs the core (SY.*, RT.*) and boots `io.cloud` and
    `io.blob` (`packaging/base-nodes.json`); runtimes grow by publish plus `SYSTEM_UPDATE`
    (category runtime), and the orchestrator keeps that history per hive.
  - `io.web` runs only where the operator starts it, by one `run_node` in the root tenant.
  - Fluxbee Cloud launches a tenant's IO nodes through IO.cloud's `provision_node`.
  - The operator launches any node with `run_node` (SY.admin's HTTP API or Archi's SCMD).

## 12. Notes, 2026-11-25 (later): the record's why, templates, and what infra is

- **All of this is the user's, not the system's** (the operator): the solutions talked about are user
  solutions. In passing: io.web is system and should be `SY.web` (FINDINGS A-363).
- **A name of its own per ED,** as in every CD.
- **The CD's record needs an owner,** not as the tenant but as the grouping of the why: to understand
  why something is running, at a glance, e.g. "ED such, capability such", something that says what
  it is, to list it fast and see what each deployed thing does.
- **Solutions made ahead for EDs:** defaults already built for EDs, and a deployment only assigns
  them things: the assignment of general solutions to many deployments.
- **No deployments "a lo loco":** ordering what each solution is for, everything runs as a solution;
  loose launches are only for debugging, maintenance and the like. Those are Archi's, and Archi
  removes them after (the operator imagines so).
- **Removing a whole grouping** (a test's, for one): yes, and the CD node does it.
- **Infra is everything a user needs to run an ED with its own solutions.**
- **Each ED needs its own CRM connector:** nothing is shared between EDs.
- **Facts read for it (2026-11-25):**
  - Nothing running records who deployed it. SY.wf-rules keeps no owner for a WF; Archi infers which
    solution a WF or the user policy is by matching its saved manifests against what runs
    (`wf_declared_by`, `opa_policy_declared_by`, `src/bin/sy_architect.rs`); a node carries no owner
    at all.
  - A solution's manifest has no parameters: node names and configs are written out. The only
    template today is a runtime's: a node's config is written at its launch from the template of its
    runtime's version.
  - Removing what a grouping deployed kills nodes; an ILK with history is disabled, never deleted
    (the operator, 2026-11-11).

## 13. Notes, 2026-11-25 (later): a deployment fills the blanks its solution declares

- **The idea, agreed:** a solution made ahead declares what each deployment must give it (the ED it
  serves, from which its names come; its connector's credential; whom it escalates to; a business
  value), and a deployment fills those blanks and nothing more. A different flow is a particular
  solution: a new source in Archi (CI), deployed the same way. The CD never changes what a solution
  is; it completes it.
- **It leaves the operator thinking:** whether a deployment needs a manifest of its own, and maybe a
  verification by AI.
- **Facts read for it (2026-11-25), the AI checks that exist:**
  - Archi's design auditor: an AI that audits a solution's manifest before it is applied and
    submits a verdict (`run_design_auditor_with_context`, `src/bin/sy_architect.rs`). It looks at
    the source.
  - SY.admin's executor reviews each step of a plan with AI before running it
    (`semantic_review_executor_plan`, `src/bin/sy_admin.rs`): the step's fields and their shape
    against the action's help, never a secret's value (A-337).

## 14. Notes, 2026-11-25 (later): the ED is born from Cloud

- **Checks and manifest, as proposed:** a deployment has a small manifest of its own, which is also
  its record (its owner, the source and its version, the blanks' values, the nodes it made, its
  history); the concrete desired state is source plus values, computed. Fixed checks first; an AI
  only where judgment is needed (open).
- **The ED is born from Fluxbee Cloud.** There its identity data is added, and there the EDs are
  listed: born, killed, edited and so on, through Cloud's normal path.
- **Its birth is a fixed CI, a plain ED: the first layer.** Assigning it solutions comes after.
- **The operator's doubt:** whether to bring everything under capabilities, skills and roles, which
  were half built in Archi.
- **Facts read for it (2026-11-25):**
  - Built: an agent's cognitive definition
    ([identity-v2.1-agent-definition-addendum](identity-v2.1-agent-definition-addendum.md)). Archi
    creates role, skill, handbook and personality assets, immutable and named by their hash
    (`blob://agent-assets/<hash>.json`), and lists, reads and deletes them; SY.admin's
    `set_ilk_definition` puts the hashes on an agent's ILK; `ai.generic` composes its prompt from
    them and recomposes it when identity's SHM changes.
  - A role is what one agent does (one per agent), and Archi's handbook keeps roles reusable: a role
    names no channel and no tool. "Role" in a company (sales, collections) is another thing with the
    same word.
  - "Capability" is no concept in the code.
  - A solution's manifest refuses an `identity` section, so a solution cannot carry its agents'
    definitions today: they are set apart, with `set_ilk_definition`.
  - Cloud's normal path is Cloud → the edge → IO.cloud → SY.admin, over the list of exposed actions
    ([io-cloud-api](io-cloud-api.md)).

## 15. Notes, 2026-11-25 (later): the list is the CD node's

- **Cloud does not keep the list of EDs.** SY.admin must be able to create everything and see what
  exists. The keeper is the CD node: a list of the deployments in the infra, where an ED is one of
  them, and the capabilities attached to an ED are listed there too, as deployments.
- **Roles, capabilities and the rest are messy,** to review better; a priori the operator likes the
  two levels: at the ED's level, a responsibility, a capability and a company role are one thing, a
  solution deployed in the ED; at an agent's level, the role, skills, handbooks and personality
  already built, which each solution would declare for its agents (the manifest refuses them today).
  The ED's personality is its conscious's, in the first layer.
- **Facts read for it (2026-11-25), an ED against identity's rules today:**
  - Only a person may hold an email: identity refuses one in a non-human ILK's identification
    (`src/bin/sy_identity.rs`, "Only a person has an email").
  - `set_ilk_definition` takes only an ILK of type `agent` (`INVALID_ILK_TYPE` otherwise).
  - The number or address an agent writes from today is its IO node's configuration (§3), not a
    channel of any ILK.

## 16. Notes, 2026-11-25 (later): the order's path, and the ED is a person

- **The CD node's name:** `SY.cd`, or `SY.cdeploy` (open).
- **The operator's picture of the path:** SY.cd takes orders from Cloud, and from there runs in the
  infra what it knows a deploy needs, using SY.admin and reading Archi's CI; SY.admin runs the
  commands SY.cd asks, in the order and the way the deploy needs. The operator finds Cloud → SY.admin
  → SY.cd → SY.admin "medio complicado".
- **The ED is a virtual person** (the operator: the ED's first difference). It has all a person can
  have: email, phone, Slack, a LinkedIn identity and so on; for every purpose but the legal ones, it
  is a person.
- **Still pending:** the mixing of channels, and the full analysis of the conversations beyond
  cognition's analysis of today.
- **No two EDs share a number,** but they may use a common one, such as the company's phone: an IO
  of its own, special for that, a common resource.
- **The ED's ILK type:** maybe only a flag that says virtual or real, and nothing more; touch little.
- **Facts read for it (2026-11-25):**
  - The same path exists today for WFs: SY.admin hands a deploy to SY.wf-rules, and SY.wf-rules
    launches, rebinds, restarts and deletes its WF nodes through SY.admin with the node actions
    every launcher uses; the admin lets it act only on its own hive's `WF.*` nodes
    (`go/sy-wf-rules/node/node_lifecycle.go`, [wf-standard-path-v1](wf-standard-path-v1.md)).
  - Archi's plans run the same way: Archi builds a plan, and SY.admin's executor runs it step by
    step, in order.
  - Every user action goes through SY.admin (the operator, 2026-10-15), and IO.cloud relays only to
    SY.admin: Cloud reaches what SY.admin exposes to it.
  - Identity's shared memory gives each ILK a 16-bit `flags` field (`IlkEntry`, `src/shm/mod.rs`),
    of which four bits are in use (`FLAG_ACTIVE`, `FLAG_DELETED`, `FLAG_STALE`, `FLAG_FROZEN`). A
    "virtual" bit fits there with no change to the layout, and a reader that does not know it
    ignores it. A person's rules (an email, channels, a complete registration) would then hold for an
    ED as they are.
  - A new type value would read as `unknown` to a router or an SDK that does not know it (the fail-safe
    against a byte up-converted to `system`, `src/router/mod.rs`); a flag bit does not have that cost.
  - Each ILK in the SHM carries a `handler_node` (its identification's `node_name`), and the router
    gives it to OPA with the ILK's type and definition. A node that answers for an ILK has a place
    there already (for the instances, at the end).

## 17. Notes, 2026-11-25 (later): closed points, and an id across channels

- **Closed:** the CD node is **`SY.deploy`**; Cloud's order goes through SY.admin, as WFs do; the ED
  is a virtual person, a person's ILK with a flag.
- **IO.cloud is system** (the operator): Cloud does not work without it, so `SY.cloud`, the same
  naming slip as io.web (FINDINGS A-363, with the open-source install's question).
- **The large topic: the mixing of channels,** and the concepts that cross channels, or news. The
  operator: messages will need another id that joins that, to analyze it as a subdomain of the
  others.
- **Facts read for it (2026-11-25):**
  - A message's meta carries `src_ilk`, `dst_ilk`, `ich`, `thread_id`, the memory package and a
    `context` (where the trace rides), among others (`Meta`, `crates/fluxbee_sdk/src/protocol.rs`).
    Its `scope` is the delivery's (`global` for a broadcast), not cognition's.
  - **The news already crosses channels, by person.** Cognition's news are the episodes of a
    conversation's people: public gossip, not memories or feelings, kept for the whole
    installation, fading with time (halved every 7 days), each belonging to its protagonist. An
    agent gets up to 5 of its conversation's people, by weight, in its memory package, and never
    passes them on ([cognition-hands-v1](cognition-hands-v1.md) §12.3 and §12.4; package version 5).
  - What does not cross channels is the rest of the package: memories, feelings and the last turns
    are per communication `(thread_id, agent ILK)`.

## 18. Notes, 2026-11-25 (later): the pair, and two EDs

- **Start with the pair** (the operator), but not only ED and person: **ED and ED** too. The
  example: two EDs of one company, one in sales and one in project development, talk to learn
  whether a billing is approved. They need a chat, and maybe an email after: they talk through
  channels, as people do.
- **Facts read for it (2026-11-25):**
  - An inbound message from an IO carries the person's ILK as `src_ilk` and no `dst_ilk`
    (`build_inbound_user_message_to_router`, `nodes/io/common/src/router_message.rs`).
  - An agent's reply carries the person as `dst_ilk` and the agent node's own ILK as `src_ilk`,
    stamped by the runtime (`build_reply_meta`, `crates/fluxbee_ai_sdk/src/message.rs`). So today
    the other end of a conversation is the agent node, not a person it speaks for.
  - Cognition analyzes the turns of people with no agent at either end; between agents it keeps only
    the immediate memory ([cognition-hands-v1](cognition-hands-v1.md) §12.3, 2).

## 19. Notes, 2026-11-25 (later): the IO is assigned by configuration

- **Agreed:** in the messages the ED is the person (its IO marks it on the way in, the reply goes out
  as the ED) and the conscious is the hand that wrote.
- **By configuration** (the operator): expand the configuration that exists. Assigning an IO to an
  agent or to an ED is the config's; it is not restricted to an ED: the config assigns the IO to a
  person's ILK in general, virtual or real.
- **Facts read for it (2026-11-25):**
  - The IO family shares one inbound configuration (`InboundConfig`,
    `nodes/io/common/src/inbound.rs`): its TTL and dedup, `dst_node` (from `io.dst_node`, the node
    every inbound message goes to), `provision_on_miss`, the blob runtime and the tenant the node
    runs as. The assigned ILK would sit next to `io.dst_node`.
  - The same person on two channels of one tenant can already be one ILK: an `ILK_REGISTER` with an
    email a person of that tenant has merges the temporary ILK into that person (`merge_registration`,
    `src/bin/sy_identity.rs`; also `ILK_ADD_CHANNEL` with `merge_from_ilk_id`). Only a temporary
    person of the same tenant merges. D13's open part is across tenants.
  - Only the frontdesk and the orchestrator may register. A stranger reaches the frontdesk through
    the router's force rule (a temporary sender goes to the frontdesk), which applies only to a
    message the router resolves; an IO with `io.dst_node` sends straight to that node, and its own
    frontdesk gate is off by default (`nodes/io/common/src/frontdesk_gate.rs`). So today a stranger
    who writes to an agent's IO reaches the agent and stays temporary.

## 20. Notes, 2026-11-25 (later): the frontdesk stays the one registrar

- **The ED does the human part** (the operator: *"sí, es el humano"*), but the idea was that the
  frontdesk is the only interlocutor for registering, and it has two ways: by command, and by a chat
  with the human.
- **Facts read for it (2026-11-25), SY.frontdesk.gov** (`nodes/gov/ai-frontdesk-gov/README.md`):
  - A core system node on the motherbee, autonomous, with two inputs. The command,
    `frontdesk_handoff`: already structured data (`register_human` from IO.cloud, for one), with no
    LLM, taken **only from an IO node** (the sender the router stamps starts with `IO.`; any other
    gets `INVALID_REQUEST`). The chat: its LLM gathers the data and calls its own `ilk_register`.
  - **Vouched and claimed:** only a handoff's own `subject.email` is vouched, and a vouched email a
    person of the tenant already has merges the temporary ILK into that person (`MERGED`). An email
    typed in a conversation is `claimed_email`: stored, never merged on.
  - So what an agent learns in a chat ("soy Ana, mi mail es `ana@acme.com`") is a claim and joins
    nothing; a join needs what a channel vouches.

## 21. Notes, 2026-11-25 (later): the general circuit, kept simple

- **The frontdesk will get complicated** (the operator): until now it is lax, for testing. The
  channels are there; the process needs reinforcing. Ana would become a "user" of the system, which
  at first will need a mechanism and maybe an approval. For now, simple: close the general circuit,
  which the operator believes is built.
- **It is built (read 2026-11-25):**
  1. A stranger writes: the IO provisions a temporary ILK for them in its own tenant
     (`provision_on_miss`).
  2. The IO's frontdesk gate (io.slack and io.wapp, `nodes/io/common/src/frontdesk_gate.rs`): for a
     sender not `complete`, it sends the frontdesk a structured handoff with the channel and the
     address, then delivers the message anyway (a gate that does not hold). Off by default, on with
     `io.frontdesk.enabled`; its handoff goes as a `user` message since the fix of 2026-10-02.
  3. A message the router resolves: a temporary sender is forced to the frontdesk.
  4. The frontdesk registers by command or by chat, merges on a vouched email, never on a claimed one.
  5. IO.cloud's `register_human` hands a vouched email: Cloud accounts register that way.
- **What the circuit does not give an ED yet:** the IO's assignment to a person (§19, new config);
  a join across channels, since the gate's handoff carries no email (fine for now); and **an email
  IO, which does not exist**: the IO nodes are io.api, io.blob, io.cloud, io.linkedhelper, io.slack,
  io.wapp and io.web (`nodes/io`).

## 22. Notes, 2026-11-25 (later): the company's people and their roles, what exists

- **An ED has an email, no matter what** (the operator: inevitable). So an email IO is part of what
  the ED needs (§21: none exists).
- **The company's people and their roles:** to order it if needed, reviewing well what exists first
  (the operator: there is something).
- **What exists (read 2026-11-25):**
  - **Identity v2's roles and capabilities, retired.** Tags from a controlled vocabulary (roles such
    as `operator` or `supervisor`, capabilities such as `billing` or `escalation`)
    ([history/10-identity-v2](history/10-identity-v2.md)). Never built: `ILK_REGISTER` and
    `ILK_UPDATE` reject `roles` and `capabilities`, and identity keeps no vocabulary.
  - **Identity v2.1's agent definition, built** (§14): role, skills, handbooks and personality by
    hash, for `agent` ILKs only.
  - **Identity v3's claims, drafted and not built**
    ([future_implementations/10-identity-v3](future_implementations/10-identity-v3.md), 2026-04-13;
    [the claims and normative model](future_implementations/fluxbee_identity_claims_normative_model.md)).
    A descriptive model: *"identity describes, the normative layer orients, enforcement acts"*.
    - A claim is a statement about one ILK, with exactly one facet: `domain` (what it may speak
      about: `billing`, `sales`), `function` (what it knows how to do: `can_invoice`, `can_refund`)
      or `responsibility` (what it is assigned to handle as a duty: `billing_intake`). Declared by
      the ILK itself, by an authority or by the system; a key in the code's canonical catalog is
      `canonical`, any other is emergent (not for hard authorization).
    - **Recognition:** another ILK recognizes a claim, explicitly and on record.
    - An institutional role (office, jurisdiction, a charter's reference) for gov and system nodes
      only. The normative layer (a constitution, charters) is documents.
    - Nothing of it is in the code.
  - **Companies and their members are Cloud's** ([io-cloud-api](io-cloud-api.md) §8; the operator,
    2026-10-14 and 10-15): Fluxbee keeps the infrastructure (identity, permissions, what is spent),
    Cloud keeps companies, members, billing and the law.
  - **A relation between two people** (one's supervisor, who reports to whom) is in no design and no
    code: v2's `supervisor` was a tag, v3's claims are about one ILK.

## 23. Notes, 2026-11-25 (later): people and roles, simple first

- **The operator:** simple, and without v3 for now, but with everything: the hierarchy,
  responsibilities, and capabilities made explicit by the solutions assigned. Long; start simple, to
  learn and tune it before solving it. Do not mix it with what exists for agents in Archi.
- **Facts read for it (2026-11-25):**
  - The words taken: an agent's *role* and *skills* are its cognitive definition (v2.1, Archi's
    assets), and identity rejects the fields `roles` and `capabilities` as v2's legacy.
  - Nothing changes a person's data after the registration: `ILK_UPDATE` only adds channels, and
    only SY.orchestrator may send it. Cloud's `register_human` stores `display_name`, `email`,
    `phone`, `company_name` and a free `attributes` object, once.
  - An agent cannot read identity: the base tools are the thread state, the artifact generators,
    `get_current_time`, `read_blob` and the HTML pages (`docs/reference/agent-tools.json`). What
    reaches it about people is the message, the turns and the news.
- **A first shape, proposed in the talk (for the review, not decided):**
  - **For people only** (a person's ILK, real or virtual), never an agent node's. Its own words, so
    it does not mix with Archi's agents: a **position** (free text, "sales manager"), **reports to**
    (another person's ILK: the hierarchy) and **responsibilities** (short free texts).
  - **A human's** position, boss and responsibilities are typed by the company's admin.
  - **An ED's** position and boss are typed when it is hired; its **capabilities are not typed:**
    they are its deployments in SY.deploy, each with its why.
  - **Its home: identity,** on the person, the one place for humans and EDs; outside the shared
    memory, since no node needs it at each message. Written through SY.admin (Cloud through
    IO.cloud, Archi, the HTTP API), by a new action.
  - **Read by** Archi and Cloud through SY.admin, and by the ED's conscious through a new tool:
    "my boss", "who in my tenant handles X" (it answers the tenant's people with their positions
    and responsibilities, and the model judges).
  - **Free text, no catalog:** learn first, tune later.
  - **Against the decision of 2026-10-14/15** (Cloud keeps companies and members): this is
    knowledge for the EDs, neither a membership nor a permission; Cloud stays where the admin edits
    it, and Fluxbee keeps it because the ED acts on it here (the operator, 2026-10-30: before moving
    data, ask what acts on it).

## 24. Later questions (not yet)

- The frontdesk's reinforced process: a contact who becomes a "user", its mechanism and approval.

- The common IO (a company's phone shared by its EDs): whose deployment it is, and which ED answers
  a message that comes in through it.

- The line between infra and an ED's deployments: a hive, a tenant's provider key in SY.vault, an
  edge exposure (a solution's manifest declares hives in its topology today).

- How a message reaches one of the conscious's instances, and the order within a person's line (left
  for the end).
- The person who is two ILKs (D13).
- What the manager node keeps, what identity keeps, and where the ED's list of ILKs lives (read by
  every node: shared memory?).
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
