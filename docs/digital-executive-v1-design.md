# The Digital Executive (ED) v1: the design

**Status:** draft for the panel review, 2026-11-25. Nothing of it is built, and the order of the
work is not decided: after the review the operator and I choose what to touch first.

**Where it comes from:** the talk of 2026-11-20 to 2026-11-25, recorded as it went in
[digital-executive-v1](digital-executive-v1.md) (cited below as *notes §n*). The notes keep each
decision in the operator's words and each fact read in the code for it. This document puts them
together into one design. Where they differ, the later decision holds, and so does this document.

**Its name:** the operator's short name is **ED**, from the Spanish *Ejecutivo Digital*. In sales it
is the Digital Executive.

---

## 1. What an ED is

Fluxbee does not solve "agents". It groups varied tools to solve complex tasks, imitating people at
work (the operator, 2026-11-20). An ED is that idea made into an actor:

- **A virtual person.** It has what a person has: a first and last name, a face, an email, a
  phone, a Slack account, a LinkedIn profile. For every purpose except legal ones, it is a person
  (notes §16).
- **One person, one set of channels.** *"Una persona no tiene un wapp por cada rol"*: one WhatsApp,
  one email, one Instagram, whatever it does (notes §4).
- **Born, then hired.** It is born with the same core as every other ED, from a fixed source: a
  plain ED. When a company hires it, it gets **responsibilities**. To the operator, a
  responsibility, a capability and a role in a company are one thing: *"si es responsable es porque
  tiene la capacidad"*. Each responsibility is a solution deployed into the ED (notes §4, §9, §14).
- **It grows by parts.** More responsibilities bring more internal agents and flows. An ED is
  multi-agent, and each part is added without touching the others (notes §4, §10).
- **One conscious agent.** One AI agent holds its "soul" and speaks to people. The responsibilities'
  agents work behind it (notes §5).
- **It works with the company's people.** It knows its boss, and who handles what, so it knows
  where to pass a problem (notes §4, §22, §23).
- **Fluxbee Cloud creates it,** for a company. Fluxbee keeps its identity, its deployments and its
  records (notes §14, §15, §24).

The legacy website already sold the idea: *"Digital Workforce, with Digital Executives for concrete
jobs"* (`docs/website/legacy/preview-3.html`).

### 1.1 The words

| Word | Meaning here |
| --- | --- |
| ED | The virtual person: one ILK of a person, marked virtual (§3). |
| The conscious | The ED's AI agent that speaks for it. |
| Responsibility | What the ED is hired to do. One deployed solution, seen from the ED. |
| Source | A solution as Archi writes it: a manifest with versions. This is CI. |
| Deployment | One source's version running, its blanks filled, for one owner. This is CD. |
| Blanks | What a source declares that each deployment must give it. |
| Grouping | The *why* of a deployment: the ED it serves, or a test. |
| The pair | Two people who talk, whatever the channel (§8). |
| Profile | A person's position, boss and responsibilities (§10). |

An agent's *role* and *skills* keep their present meaning: the cognitive definition of an `agent`
ILK (identity v2.1, Archi's assets). The ED's words were chosen so the two never mix (notes §23).

---

## 2. The rules it keeps

Earlier decisions of the operator that this design obeys. Each one is quoted in the notes where it
was weighed.

- **Every user action goes through SY.admin.** A node never acts on another node outside the listed
  exceptions (2026-10-15). An ED's creation, its deployments, and a change to a person's profile all
  go through SY.admin.
- **The datum's owner decides** (2026-10-30). The IO owns its thread ids and knows which person it
  carries. Identity owns a person's data. SY.deploy owns the deployments. Archi owns the sources.
- **Shared state stays in shared memory** (2026-11-10). What every node reads lives in identity's
  SHM, and readers read it locally. Reading the identity SHM is one of the accepted exceptions to
  "through SY.admin".
- **A tenant is a cost center, not a company** (2026-10-14/15). Cloud keeps companies, members,
  billing and the law. Fluxbee keeps identity, permissions and what is spent.
- **Archi is the operator's interface** (2026-11-10). Whatever an operator does with an ED must be
  reachable by Archi.
- **ILKs with history are disabled, never deleted.** Only Fluxbee Cloud deletes, and only with a
  whole tenant (2026-11-11).
- **No magic automation; operational first.** Transitions are explicit and reported. Simple first,
  learn, then tune (2026-11-25, notes §23).

---

## 3. The ED's identity: a person with a flag

**The decision** (notes §16, §17): the ED is a person's ILK (`ilk_type` `human`) with a
**`virtual` flag**. The decision does not add an ILK type.

- **Why a flag and not a type.** Identity's SHM gives each ILK entry a 16-bit `flags` field
  (`IlkEntry`, `src/shm/mod.rs`), and one of its free bits carries `virtual`. The layout does not
  change, and a reader that does not know the bit ignores it. A new type value, by contrast, would
  read as `unknown` to every router and SDK not yet updated (the fail-safe that keeps an odd byte
  from ever reading as `system`, `src/router/mod.rs`).
- **Every rule of a person holds as it is.** It may hold an email, which identity refuses to
  non-humans today. It holds channels in its tenant's scope, a complete registration, and a tenant
  it is charged to.
- **Its channels are its ILK's,** like a person's (notes §16). No two EDs hold the same address.
  Several EDs may *use* a common one, such as a company's phone, through an IO of its own made for
  that (§7.4).
- **Born with its company** (notes §24). Cloud's request that creates an ED names its company and
  the tenant it is charged to.
- **Its conscious and its responsibilities' agents are `agent` ILKs,** each with its own node, as
  today (`derive_ilk_type_for_node`, `src/bin/sy_orchestrator.rs`). The ED's ILK has no node of its
  own: its IOs carry it, and its conscious speaks for it (§7, §8).

**Open:** whether an ED holds a Fluxbee Cloud account. A Cloud account is a person's `cloud`
channel, created by `register_human`, and it lets that person sign in to Cloud. An ED has no reason
to sign in. Its birth may need its own path rather than `register_human` (§4).

---

## 4. Birth, life and death

### 4.1 The path of an order

**Decided** (notes §16, §17): Cloud → the edge → IO.cloud → **SY.admin** → **SY.deploy** →
SY.admin, once for each step.

1. An admin creates an ED in Fluxbee Cloud: its name, its photo, its company, its channels'
   addresses, and its boss.
2. Cloud sends one action through IO.cloud to SY.admin. SY.admin checks that Cloud may send it,
   records it, and hands it to SY.deploy.
3. SY.deploy reads the plain-ED source from Archi, fills its blanks with what Cloud sent, and builds
   the plan.
4. SY.deploy asks SY.admin for each step, in the order the deploy needs. The steps are the ED's ILK
   with its flag and its profile, the vault keys of its channels, a `run_node` for each IO and for
   its conscious, and its conscious's definition.
5. SY.deploy records the deployment, and the answer goes back to Cloud.

The path is the one WFs already take. SY.admin hands a deploy to SY.wf-rules, and SY.wf-rules
launches, rebinds, restarts and deletes its WF nodes through SY.admin, which lets it touch only its
own hive's `WF.*` nodes (`go/sy-wf-rules/node/node_lifecycle.go`,
[wf-standard-path-v1](wf-standard-path-v1.md)). Archi's plans run the same way: SY.admin's executor
runs them step by step. One door is kept for Cloud, Archi and the HTTP API alike.

**To settle:** a birth launches several nodes, which may take longer than one mesh wait. SY.deploy
probably answers "accepted" with the deployment's id, and Cloud then reads its state.

### 4.2 The list, and who sees it

Cloud does not keep the list of EDs (notes §15). SY.deploy keeps the list of the deployments in the
infra. An ED is one entry, and its responsibilities are listed under it as deployments. SY.admin
reads the list from SY.deploy and serves it to Cloud, Archi and the HTTP API.

### 4.3 Editing, killing, disabling

- **Editing** an ED's data is a change of its profile (§10) or of its channels.
- **Killing** an ED removes its deployments: SY.deploy removes every node of its grouping (§5.4).
- Its ILK, and the ILKs of its agents, are **disabled, never deleted** when they have history. Only
  Cloud deletes, and only with a whole tenant (notes §11 and the deletion rule).

---

## 5. SY.deploy: the node of the CD

**Decided** (notes §11, §12, §16, §17): one node is responsible for every CD of user solutions,
**`SY.deploy`**. It began as "the ED's node". It became the CD's node, and the ED became one kind of
deployment: *"eso me cierra más"*.

### 5.1 What it does

- Takes orders from SY.admin: deploy, upgrade, remove, list, and read one deployment.
- Reads Archi's sources (CI) and computes each deployment's desired state as the source plus the
  blanks' values.
- Builds the plan and asks SY.admin for each step. Archi's reconciler is made of pure functions, so
  it can be shared or moved (`src/bin/sy_architect/reconciler.rs`).
- Keeps each deployment's manifest and its history (§5.3), and the list of the infra's deployments.
- Is a system node (`SY.`): it is infra. Everything it deploys is a user's (notes §12).

### 5.2 What it does not do

- **The platform's software.** The package installs the core (SY.*, RT.*) and boots the system IO
  nodes. Runtimes grow by publishing and `SYSTEM_UPDATE`, and the orchestrator keeps that history
  per hive (`packaging/base-nodes.json`). That path stays as it is.
- **Loose launches.** A `run_node` for debugging or maintenance is Archi's, and Archi removes it
  after (notes §12). *"No deployments a lo loco"*: everything that runs for a user runs as a
  solution.
- **CI.** Sources, versions and the catalog stay with Archi (notes §9).

### 5.3 A deployment's manifest, which is its record

Nothing running today records who deployed it. SY.wf-rules keeps no owner for a WF, and a node
carries no owner at all. Archi infers which solution a WF or the user policy belongs to by matching
its saved manifests against what runs (`wf_declared_by`, `opa_policy_declared_by`). A deployment's
manifest (notes §13, §14) holds:

- **its owner, the grouping of its why:** "ED Laura, collections". It lets anyone list at a glance
  what runs and why (notes §12);
- **its tenant;**
- **its source and version:** "collections v3";
- **its blanks' values;**
- **the nodes it made;**
- **its history:** applied, upgraded and removed, when, and by whom.

The concrete desired state is computed from the source and the values; nobody writes it by hand.
Archi owns the source, and SY.deploy owns the deployment's manifest.

### 5.4 Groupings

Groupings sit at the same level as one another (notes §11, §12):

- **ED:** tied to the ED's ILK. Its first deployment is the plain ED, and each responsibility is one
  more deployment.
- **Test:** what a gate deploys, removed as a whole when the test is done.
- More kinds when they appear. Removing a grouping removes everything it deployed.

Each ED needs its own connector to its company's CRM: nothing is shared between EDs (notes §12). A
resource common to several EDs, such as the company's phone, is an IO made for that (§7.4).

### 5.5 Checks

Most checks are fixed checks, run before a plan (notes §13, §14):

- every blank is filled;
- the keys a blank names exist in SY.vault, in the ED's tenant;
- the ILKs a blank names exist (the boss, for one);
- the node names are free on the hive.

Two AI reviews already exist and stay where they are:

- **Archi's design auditor** reviews a source before it is applied
  (`run_design_auditor_with_context`).
- **SY.admin's executor** reviews each step of a plan against the action's help, and never sees a
  secret's value (`semantic_review_executor_plan`, A-337).

A new AI check inside SY.deploy is open. It would cover only what needs judgment, such as whether
30 days of arrears suits this solution.

### 5.6 Growing by parts, and its known conflict

Several solutions are deployed into one ED, and each is upgraded alone (notes §10). Archi's
reconciler already leaves another solution alone in most ways:

- it removes a WF or the user policy only when the solution that declared them removes them;
- it never kills a node that is not in the manifest.

One thing blocks growing by parts: **the user OPA policy is one for the whole installation.** A
solution that declares one replaces the policy another solution declared: *"the last one applied
wins"* (`reconcile_opa`). This is a pending topic of its own (notes §11).

---

## 6. Solutions for an ED

### 6.1 Source and deployment

A solution is two things (notes §9):

- **the source,** what Archi writes: today a versioned manifest (`SolutionManifestV2`), kept per
  `solution_id` with one file per version;
- **the deployment,** the nodes that run it, with their ILKs, on one hive and charged to one tenant.

For an ED, "solution" means the deployment. One ED has many deployments, and two EDs never share
one.

### 6.2 Three layers

The operator confirmed these layers (notes §9, §14):

1. **Native:** the plain ED, a fixed source that every ED is born from. It holds the ED's ILK, an
   IO for each of its channels, and its conscious. It deploys like any other source, so a new
   version of the plain ED reaches every ED by an ordinary CD.
2. **Standard:** a template from Archi's catalog, made ahead for EDs, given to an ED when it is
   hired, and to others.
3. **Particular:** made to measure by Archi for one ED. It is a new source, deployed the same way.

### 6.3 Blanks

A source made ahead declares what each deployment must give it, and a deployment fills those blanks
and nothing more (notes §13). For "collections v3", the blanks are:

- the ED it serves (its node names come from it, distinct per ED: `AI.collections.pedro`);
- its CRM connector's credential, a key in SY.vault in the ED's tenant;
- whom it escalates to;
- a business value, such as the days of arrears before a call.

A different flow is a particular solution: a new source, written by Archi. The CD never changes what
a solution is. It only completes it.

**What the source format needs:**

- **Blanks.** The manifest has no parameters today: node names and configs are written out. The
  only template today is a runtime's, which a node's config is written from at launch.
- **Its agents' definitions.** A responsibility's agents need their role, skills and handbooks
  (identity v2.1). A manifest refuses an `identity` section today, so these are set apart with
  `set_ilk_definition`.

### 6.4 What is infra

Infra is everything a user needs to run an ED with its own solutions (notes §12). It is the system's,
not a user's deployment. Where the line falls is open for three things:

- a hive (a manifest declares hives in its topology today);
- a tenant's provider key in SY.vault;
- an edge exposure.

---

## 7. Channels and their IO nodes

### 7.1 Assigned by configuration

**Decided** (notes §19): a person's channels are carried by IO nodes, each **assigned by
configuration** to the person it carries. The assignment is not restricted to EDs: the config can
name any person's ILK, real or virtual.

- The IO family shares one inbound configuration (`InboundConfig`,
  `nodes/io/common/src/inbound.rs`): TTL, dedup, `dst_node` (from `io.dst_node`),
  `provision_on_miss`, the blob runtime, and the node's own tenant. The assigned ILK sits next to
  `io.dst_node`.
- The ED's first layer sets both fields: the assigned ILK is the ED, and `dst_node` is its
  conscious.

### 7.2 What the messages carry

**Decided** (notes §18, §19): in the messages, **the ED is the person, and the conscious is the
hand that wrote.**

- **Today:**
  - an inbound message carries the writer's ILK as `src_ilk` and no `dst_ilk`
    (`build_inbound_user_message_to_router`);
  - an agent's reply carries the person as `dst_ilk` and the agent node's own ILK as `src_ilk`,
    stamped by the runtime (`build_reply_meta`, `crates/fluxbee_ai_sdk/src/message.rs`).

  So the far end of a conversation is a node, not the person it speaks for.
- **Then:**
  - an IO assigned to a person stamps that person as `dst_ilk` on the way in;
  - the conscious's reply goes out as the ED, and the node that wrote it stays named as the hand
    (the message's trace and cognition's hands already name it).

### 7.3 The email

An ED has an email, no matter what (notes §22). **No email IO exists.** The IO nodes are io.api,
io.blob, io.cloud, io.linkedhelper, io.slack, io.wapp and io.web. So an email IO is part of the
ED's native layer.

### 7.4 A common channel

Two EDs never hold the same number, but they may use a common one, such as the company's phone.
That takes an IO of its own, made as a common resource (notes §16). **Open:** whose deployment
that IO is, and which ED answers a message that comes in through it.

---

## 8. Conversations across channels

### 8.1 The pair

**Decided** (notes §17, §18): conversations are joined by **the pair**: two people, real or
virtual, whatever the channel.

- **Who pairs with whom:**
  - a person and an ED: Ana and Laura;
  - two EDs: Laura in sales and Martín in projects, who settle by chat and then by email whether a
    billing is approved;
  - two people the system sees talk.
- **The id is the same from both sides.** {Laura, Martín} joins their Slack and their email.
- **It is computed, not stored.** Once both people ride in the message (§7.2), the pair comes from
  `src_ilk` and `dst_ilk`.
- **Each side keeps its own memory of the other.** What Laura remembers of Martín is Laura's. The
  pair joins the channels, and the owner keeps the memories apart: today's rule, that memory belongs
  to whoever serves the conversation.

### 8.2 What cognition does today

- **Memory, feelings and last turns are per communication `(thread_id, agent ILK)`**
  (`CommunicationKey`, `src/bin/sy_cognition.rs`). A thread is per channel and interlocutor, so
  Ana's WhatsApp and Ana's email are two memories.
- **The news already cross channels, by person.** A news item is an episode of a conversation's
  people: public gossip, not memories or feelings. It is kept for the whole installation, fades with
  time, and belongs to its protagonist. An agent gets up to 5, and never passes them on
  ([cognition-hands-v1](cognition-hands-v1.md) §12).
- **Cognition analyzes the turns of people,** and between agents it keeps only the immediate memory
  (cognition-hands §12.3, 2). Once EDs ride as people (§7.2), an ED–ED talk is a person–person talk
  that cognition already analyzes.

### 8.3 What changes in cognition

The operator expects cognition to need **one more id when an ED is involved**: it groups an ED's
threads with the same person, so they are analyzed together as a subdomain of each communication
(notes §7, §17). This is reviewed with cognition when it is touched. Two points to verify:

- with the reply going out as the ED (§7.2), whose ILK keys the memory: the ED's or the
  conscious's node's;
- what of the package (memories, feelings, turns) the pair brings across channels.

### 8.4 What waits

- **A matter that spans several people** (order #123, with Ana by WhatsApp and her boss by email).
  It needs judgment to assign a message to a matter, so it would come later as cognition's analysis
  inside each pair, like today's contexts (notes §17).
- **The person who is two ILKs** (D13). Within one tenant, the merge already joins them (§9).
  Across tenants it stays open.

---

## 9. The contact circuit

**Built** (notes §21):

1. **A stranger writes.** The IO provisions a temporary ILK for them in its own tenant
   (`provision_on_miss`).
2. **The IO's frontdesk gate** (io.slack and io.wapp, `nodes/io/common/src/frontdesk_gate.rs`): for
   a sender who is not `complete`, it sends the frontdesk a structured handoff with the channel and
   the address, then delivers the message anyway. The gate is off by default and turned on with
   `io.frontdesk.enabled`.
3. **The router's force rule:** a message the router resolves, from a temporary sender, goes to the
   frontdesk.
4. **SY.frontdesk.gov is the one registrar.** It registers by command (`frontdesk_handoff`, taken
   only from an `IO.` sender) or by chat (its own LLM and `ilk_register`).
5. **Vouched and claimed.** A handoff's own email is vouched, and a vouched email a person of the
   tenant already has merges the temporary ILK into that person (`MERGED`). An email typed in a chat
   is claimed: it is stored, and nothing merges on it.
6. **Cloud's `register_human`** hands a vouched email.

**For an ED** (notes §20, §21):

- **The ED does the human part.** "¿Con quién hablo?", as a person would. The frontdesk stays the
  one registrar.
- **A join needs what a channel vouches.** "Soy Ana, mi mail es `ana@acme.com`" is a claim. A code
  sent to that address and repeated in the WhatsApp is a proof. Who sends the frontdesk that proof
  is open; the IO already may, and it owns the channel's datum.
- **For now, simple.** The process will be reinforced later: a contact who becomes a "user" of the
  system will need a mechanism, and maybe an approval.

---

## 10. People, their positions and responsibilities

### 10.1 What the ED needs

An ED needs to know its boss, and who in its company handles what, so it can pass a problem on.
Start simple, learn and tune, but with all of it: the hierarchy, the responsibilities, and the
capabilities the assigned solutions make explicit (notes §23). Identity v3's claims stay out for
now.

### 10.2 What exists, and what is not touched

- **Identity v2's roles and capabilities:** retired, never built. Identity rejects `roles` and
  `capabilities` ([history/10-identity-v2](history/10-identity-v2.md)).
- **Identity v2.1's agent definition:** built. An `agent` ILK's role, skills, handbooks and
  personality are named by hash (§1.1). It is not touched.
- **Identity v3's claims and recognition:** drafted on 2026-04-13, not built
  ([future_implementations/10-identity-v3](future_implementations/10-identity-v3.md)). This
  document leaves them as the later direction.
- **A relation between two people** (a boss, who reports to whom) exists in no design and no code.

### 10.3 The profile

A person's profile is for people only, real or virtual, never for an agent node:

- **position:** free text, such as "sales manager";
- **reports to:** another person's ILK. This is the hierarchy;
- **responsibilities:** short free texts;
- for an ED, **capabilities are not typed:** they are its deployments in SY.deploy, each with its
  why;
- the company, given at birth (§3).

The texts are free and there is no catalog: learn first. This is knowledge for the EDs, neither a
membership nor a permission. That keeps the decision of 2026-10-14/15: the admin edits it from
Cloud, and Fluxbee keeps it because the ED acts on it here (the operator, 2026-10-30: before moving
data, ask what acts on it).

### 10.4 Where it lives, and how it travels

**Decided** (notes §24, §25): **identity owns it, and nothing large changes in identity.**

- **Stored with the person** in identity's database, as identity keeps the person's other data.
  Cloud's `register_human` already stores `phone`, `company_name` and a free `attributes` object in
  the person's `identification`.
- **Projected to a blob the agents read:** `people/<ilk_id>/<version>.json`, one file per person
  and version, written once. A change is a new version.
- **Named in the SHM.** Each `IlkEntry` ends in 8 reserved bytes, written as zeros and read by
  nobody. They hold the profile's version (0 means none). The ILK id is already in the entry, so id
  and version name the file. There is no new field and no new SHM version (today 6, with the 1160-byte
  `IlkEntry` that the Go SDK mirrors).
- **Changed by one command.** Today nothing changes a person after registration: `ILK_UPDATE` only
  adds channels, and only SY.orchestrator may send it. It would also take the profile, from
  SY.admin, which exposes one action to Cloud and Archi. At birth, the profile comes in the creation
  itself.
- **Read locally.** An agent's tool reads the SHM entry, then reads the file with `read_blob`. There
  is no request to anyone.
- **Syncthing's lag** (A-338). On another hive, the SHM can name a version whose file has not
  arrived yet. Old versions stay, so a reader takes the newest one present.

---

## 11. The ED's agents

- **The conscious:** one agent, dedicated to the interaction (notes §5). Its personality is the
  ED's: who it is (nationality, languages, time zone, background). It comes in the native layer.
- **The responsibilities' agents:** they come with their solutions, each with its own ILK and
  definition. They are listed in the ED's grouping in SY.deploy (notes §6, §9).
- **Their tools are an unfinished topic** (notes §24). The base tools are the thread state, the
  artifact generators, `get_current_time`, `read_blob`, and the HTML pages
  (`docs/reference/agent-tools.json`). An ED needs at least a people tool ("my boss", "who in my
  tenant handles X"), reading the SHM and the profile blobs. An agent's tool may already go through
  SY.admin, as `publish_html_page` does.
- **The conscious as several instances** of the same agent is agreed (notes §7). How a message
  reaches one, and the order within a person's line, waits for the end (§13).

---

## 12. Its identity on the internet

**The operator's starting point** (2026-11-20): *"tiene identidad reconocida en internet [tal vez
resolverlo así… (did:web / did:webvh + agent-card.json) + (did:key + Credenciales Verificables)]"*.

**The operator's reference:** a link the operator passes, to add here.

**Nothing in Fluxbee holds a DID, an agent card or a credential today.** What each piece would be
for an ED:

- **A DID** (W3C [Decentralized Identifiers v1.0](https://www.w3.org/TR/did-core/)), an identifier
  that resolves to a document with the ED's public keys and endpoints:
  - **`did:web`** ([the method](https://w3c-ccg.github.io/did-method-web/)) is anchored in a domain:
    `did:web:<domain>` resolves to `https://<domain>/.well-known/did.json`, and
    `did:web:<domain>:<path>` to `https://<domain>/<path>/did.json`. It is simple, and as strong as
    the domain and its web server.
  - **`did:webvh`** ([DIF, v1.0](https://identity.foundation/didwebvh/), formerly `did:tdw`) is
    `did:web` with a verifiable history: a log of the document's versions, with an identifier whose
    security does not rest on DNS.
  - **`did:key`** (the W3C CCG method) is a public key written as a DID, with nothing to host. It
    fits signing keys, such as the key that signs the ED's credentials.
- **An agent card** ([A2A](https://a2a-protocol.org/latest/specification/)) is a JSON document at
  `https://<domain>/.well-known/agent-card.json` (the path since A2A v0.3; before, `agent.json`). It
  names the agent, its skills, its endpoints and its security schemes, so other agents can find it
  and talk to it.
- **Verifiable credentials** ([W3C VC Data Model 2.0](https://www.w3.org/TR/vc-data-model/), a
  Recommendation since May 2025) are signed claims among issuers, holders and verifiers. An example
  is "Laura works for Acme, in sales", issued by Acme and verifiable by anyone.

**Open** (a talk of its own, after the review):

- who hosts the DID document and the agent card (Cloud, or the installation's edge through its web
  node);
- who holds the keys (SY.vault is the natural custodian);
- who issues the credentials (the company through Cloud, Fluxbee Cloud itself) and who verifies
  them;
- how the agent card's skills relate to the ED's responsibilities.

---

## 13. Open questions

Kept for later, each with where it was raised:

1. **The instances and the line,** left for the end (notes §8, §19). How a message reaches one of
   the conscious's instances, and how one person's messages keep their order:
   - `ai.generic` runs 4 turns at once and queues 128, with no order per conversation (notes §5);
   - the router delivers to one node by name;
   - the operator dislikes the router deciding anything about load;
   - an ILK's `handler_node` already reaches OPA, a possible place for "who answers for this person".
2. **The user OPA policy, one for the installation** (§5.6).
3. **The matter across people,** and **D13 across tenants** (§8.4).
4. **The common channel:** whose it is, and who answers (§7.4).
5. **The frontdesk's reinforced process:** a contact who becomes a "user" (§9).
6. **An AI check of deployments** (§5.5).
7. **The ED's Cloud account,** and its birth path against `register_human` (§3).
8. **Its identity on the internet** (§12).
9. **The agents' tools** (§11).
10. **"Something more" in the core:** IO + AI + something Fluxbee builds that is useful (notes §4).
    One candidate is the tasks on timers (notes §1.2).
11. **The line between infra and an ED's deployments** (§6.4).
12. **Identity v3's claims,** when simple is not enough (§10).

---

## 14. What it touches

This is an inventory for choosing what to touch first, not an order:

| Part | What changes |
| --- | --- |
| SY.identity | The `virtual` flag bit. The profile: stored, projected to the blob, its version in the SHM's reserved bytes. `ILK_UPDATE` takes the profile from SY.admin. |
| SY.admin | The ED's actions handed to SY.deploy. The deployments' list and reads. The profile's change. Their exposure to IO.cloud and Archi. |
| SY.deploy | New: deployments, their manifests and history, groupings, checks, plans run through SY.admin. |
| Archi | Sources with blanks, the plain-ED source, a catalog for EDs, its agents' definitions in a source, and cleaning up its loose launches. |
| The reconciler | Shared with SY.deploy, or moved to it. |
| The IO family | The assigned ILK in `io.*`; `dst_ilk` stamped on the way in; a new email IO; the common-resource IO. |
| The AI SDK and ai.generic | The reply goes out as the person, with the hand named; a people tool reading the SHM and the profile blobs. |
| SY.cognition | Grouping by pair (§8.3). |
| SY.frontdesk.gov | Later (§9). |
| Fluxbee Cloud's contract | Create an ED with its company; list, edit, and kill. |
| The SDKs' SHM readers | The `virtual` bit and the profile's version. |

**Also recorded:** io.web and io.cloud are system nodes and should be `SY.web` and `SY.cloud`
(FINDINGS A-363).

---

## 15. The other large topics of 2026-11-20

Each gets a talk of its own (notes §1):

- IO nodes from a package;
- tasks on timers;
- a conversation viewer in Archi, from Fluxbee Cloud's team's `tools/visor-casos`.
