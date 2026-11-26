# The Digital Executive (ED) v1: the design

**Status:** draft, reviewed by a panel of five on 2026-11-25. The panel's lenses were the code's
facts, fidelity to the talk, identity and security, CD and operation, and messaging and cognition.
Its findings are applied below, and the operator decided on them the same day (§16.6). Nothing is
built, and the order of the work is not decided: the operator and I choose what to touch first.

**Where it comes from:** the talk of 2026-11-20 to 2026-11-25, recorded as it went in
[digital-executive-v1](digital-executive-v1.md) (cited below as *notes §n*). The notes keep each
decision in the operator's words and each fact read in the code for it. Where they differ, the later
decision holds, and so does this document.

**How to read it:**

- **Decided:** the operator decided it in the talk.
- **Proposed:** the talk or the panel brought it, and it waits for the operator.
- **Open:** nobody has an answer yet.

**Its name:** the operator's short name is **ED**, from the Spanish *Ejecutivo Digital*. In sales it
is the Digital Executive.

---

## 1. What an ED is

Fluxbee does not solve "agents". It groups varied tools to solve complex tasks, imitating people at
work (the operator, 2026-11-20). An ED is that idea made into an actor:

- **A virtual person.** It has a first and last name, a face, an email, a phone, a Slack account and
  a LinkedIn profile. For every purpose except legal ones, it is a person (notes §16).
- **One person, one set of channels.** *"Una persona no tiene un wapp por cada rol"*: one WhatsApp,
  one email, one Instagram, whatever it does (notes §4).
- **Born, then hired.**
  - **Born** with the same core as every other ED, from a fixed source: a plain ED, born with its
    company.
  - **Hired** by a company, which gives it **responsibilities**. To the operator a responsibility, a
    capability and a role in a company are one thing: *"si es responsable es porque tiene la
    capacidad"*. Each one is a solution deployed into the ED (notes §4, §9, §14, §24).
  - How roles and capabilities are ordered is still *"desordenado, para revisar mejor"*. The two
    levels of §10 are liked a priori (notes §15).
- **It grows by parts.** More responsibilities bring more internal agents and flows. Each part is
  added without touching the others (notes §4, §10).
- **One conscious agent.** One AI agent holds its "soul" and speaks to people. The responsibilities'
  agents work behind it (notes §5).
- **It works with the company's people.** It knows its boss and who handles what, so it knows where
  to pass a problem (notes §4, §23).
- **Fluxbee Cloud creates it,** for a company. Fluxbee keeps its identity, its deployments and their
  records (notes §14, §15, §24).

The legacy website already sold the idea: *"Digital Workforce, with Digital Executives for concrete
jobs"* (`docs/website/legacy/preview-3.html`).

### 1.1 The words

| Word | Meaning here |
| --- | --- |
| ED | The virtual person: one ILK of a person, marked virtual (§3). |
| The conscious | The ED's AI agent that speaks for it. |
| Responsibility | What the ED is hired to do: one deployed solution, seen from the ED. |
| Source | A solution as Archi writes it: a manifest with versions. This is CI. |
| Deployment | One source's version running, its blanks filled, for one owner. This is CD. |
| Blanks | What a source declares that each deployment must give it. |
| Grouping | A deployment's *why*: the ED it serves, infra, or a test. |
| The pair | Two people who talk, whatever the channel (§8). |
| Profile | A person's position, boss and responsibilities (§10). |

An agent's *role* and *skills* keep their present meaning: the cognitive definition of an `agent`
ILK (identity v2.1, Archi's assets). The ED's words were chosen so the two never mix (notes §23).
The A2A agent card also has *skills* (§12), which are the ED's responsibilities.

---

## 2. The rules it keeps, and the ones it changes

### 2.1 Earlier decisions it keeps

- **Every user action goes through SY.admin.** A node never acts on another outside the listed
  exceptions (2026-10-15). One exception already exists for Cloud: `register_human` is handled
  inside IO.cloud, which provisions the person and hands the frontdesk the case directly, without
  SY.admin (`nodes/io/io-cloud/src/main.rs`).
- **The datum's owner decides** (2026-10-30):
  - the IO owns its thread ids and knows the person it carries;
  - identity owns a person's data;
  - SY.deploy owns the deployments;
  - Archi owns the sources.
- **Shared state stays in shared memory** (2026-11-10). Reading the identity SHM is an accepted
  exception to "through SY.admin".
- **A tenant is a cost center, not a company** (2026-10-14/15).
- **"Si hay algo global que resolver en los hives que sea el router"** (2026-10-16).
- **A solution runs on one hive** (2026-11-06, [cognition-hands-v1](cognition-hands-v1.md) §12.3).
- **Archi is the operator's interface** (2026-11-10).
- **ILKs with history are disabled, never deleted.** Fluxbee Cloud deletes them atomically only with
  a whole tenant. The operator purges one at a time through SY.admin's HTTP API, Archi's SCMD, or an
  Archi plan with CONFIRM (2026-11-11 and 2026-11-20).
- **Between agents, only the immediate memory** (cognition-hands §12.3, 2).
- **Simple first: learn, then tune** (notes §23).

### 2.2 Earlier decisions it changes (confirmed by the operator, 2026-11-25)

1. **Cloud keeps companies and their members** (2026-10-14/15;
   [io-cloud-api](io-cloud-api.md) §8; [ilk-tenant-model-v1](ilk-tenant-model-v1.md) §0).
   - **The change:** an ED is born with its company, and a person's profile names a boss and a
     position (§10).
   - **The argument** (notes §23): this is knowledge for the EDs, neither a membership nor a
     permission. The admin edits it from Cloud, and Fluxbee keeps it because the ED acts on it here.
2. **Cloud launches IO nodes only so a credential has somewhere to live; solutions are the
   operator's, through Archi** ([ilk-tenant-model-v1](ilk-tenant-model-v1.md) §7).
   - **The change:** Cloud orders an ED's birth and its end, and they reach solutions and AI nodes
     through SY.deploy (notes §14, §16).
   - Cloud's `provision_node` stays as it is (IO nodes for credentials) until Cloud's contract is
     written.
3. **One manifest per tenant-solution** ([solution-manifest-spec_1](solution-manifest-spec_1.md)
   §3).
   - **The change:** a tenant has many deployments: one per ED and solution.
4. **A reply never carries the human's ILK as its own** (2026-10-17,
   [router-stream-v1](router-stream-v1.md) §6.2, pinned by a contract test in
   `crates/fluxbee_ai_sdk/tests/contracts.rs`).
   - **The change:** the conscious's replies carry the ED (§7.2), and only the conscious's.

---

## 3. The ED's identity: a person with a flag

**Decided** (notes §16, §17): the ED is a person's ILK (`ilk_type` `human`) with a **`virtual`
flag**. There is no new ILK type.

- **Why a flag and not a type:**
  - Identity's SHM gives each ILK entry a 16-bit `flags` field (`IlkEntry`, `src/shm/mod.rs`), in
    which identity writes only `FLAG_ACTIVE` today. Every reader masks that bit, so a new bit
    changes no layout, and a reader that does not know it ignores it.
  - A new type value would read as `unknown` to every router and SDK not yet updated. That is the
    fail-safe that keeps an odd byte from ever reading as `system` (`src/router/mod.rs`).
  - **Decided:** `virtual` gets a constant of its own for ILK entries. The shared `FLAG_DELETED`,
    `FLAG_STALE` and `FLAG_FROZEN` belong to other tables.
- **OPA does not see it.** The router's OPA input for an ILK carries its type, tenant, registration,
  `handler_node` and definition, but no flags. A policy can tell an ED from a person only after a
  router change.
- **Born with its company** (notes §24). Cloud's request that creates an ED names the company and
  the tenant it is charged to.
- **Its channels are its ILK's,** like a person's (notes §16). No two EDs hold the same address.
  Several EDs may *use* a common one, through an IO of its own (§7.4).
  - Its channels' credentials in SY.vault are assigned to its IO nodes, since a secret is assigned
    to a node, never to a person.
- **Its conscious and its responsibilities' agents are `agent` ILKs,** each with its own node, as
  today (`derive_ilk_type_for_node`, `src/bin/sy_orchestrator.rs`).
- **The ED's ILK has no node of its own.** Its IOs carry it, and its conscious speaks for it.
  io.linkedhelper is a working precedent: it provisions the LinkedIn profile it carries as an ILK
  with no node, and stamps it on inbound messages.
- **Its ILK never takes a node's name as its `handler_node`** (decided). `handler_node` is a node's
  key: SY.vault binds secrets with it, and `find_ilk_by_handler_node` (cognition, SY.storage, the
  orchestrator, SY.admin, ai.generic) returns its first match. If the ED shares its conscious's
  name, any of them may get the wrong ILK.

### 3.1 Where an ED is not a person (decided, 2026-11-25)

Treating every person's rule as holding for an ED opens two holes the panel found:

- **A merge into an ED.** A stranger gets a temporary ILK on WhatsApp and says "I'm Laura,
  `laura@acme.com`". If a code is sent to that address, Laura's email IO hands it to her conscious,
  an LLM, and the stranger can ask it for "the code you just got". The proof passes, and the
  stranger's WhatsApp merges into Laura's ILK (`merge_registration`, `src/bin/sy_identity.rs`). From
  then on, the stranger speaks as Laura. A Cloud signup for that address fails the same way: its
  verification link reaches the same inbox.
- **Cloud's person actions.** `delete_ilk` and `restore_ilk` take any person, so a delete would mark
  an ED while its nodes keep running. `set_ilk_tenant` can move an ED to another tenant.

**So:**

- identity refuses any merge into a virtual ILK, by `merge_registration`, by `ILK_ADD_CHANNEL` with
  `merge_from_ilk_id`, or by `register_human`;
- no proof code is ever sent to, or accepted for, an address a virtual ILK holds;
- **an ED holds no Fluxbee Cloud account** (no `cloud` channel);
- Cloud's person actions refuse a virtual ILK. An ED's life goes only through SY.deploy, and its
  channels reach it only through SY.deploy and SY.admin.

---

## 4. Birth, life and its end

### 4.1 The path of an order

**Decided** (notes §16, §17): Cloud → the edge → IO.cloud → **SY.admin** → **SY.deploy** →
SY.admin, once for each step.

1. An admin creates an ED in Fluxbee Cloud: its name, its photo, its company, and its channels'
   addresses. Its position and its boss are given when it is hired (notes §23).
2. Cloud sends one action through IO.cloud to SY.admin. SY.admin checks that Cloud may send it,
   records it, and hands it to SY.deploy.
3. SY.deploy reads the plain-ED source from Archi, fills its blanks with what Cloud sent, and builds
   the steps.
4. SY.deploy asks SY.admin for each step, in the order the deploy needs:
   - the ED's ILK, with its flag and its company;
   - its channels' credentials;
   - its conscious first, with its definition;
   - its IOs last, assigned to the ED.

   The conscious goes first so that no message reaches a node that does not exist yet.
5. SY.deploy records the deployment.

The path is the one WFs take: SY.admin hands a deploy to SY.wf-rules, and SY.wf-rules launches,
rebinds, restarts and deletes its nodes through SY.admin ([wf-standard-path-v1](wf-standard-path-v1.md)).
One door is kept for Cloud, Archi and the HTTP API alike. The panel found what the comparison hides:

- **SY.deploy needs its own row in SY.admin's mesh list (`mesh_access`).** A new mesh caller is
  closed until it has one ([control-path-hardening-v1](control-path-hardening-v1.md)).
  - SY.wf-rules' row admits only `WF.*` nodes of its own hive.
  - SY.deploy needs AI and IO nodes in users' tenants, on any hive, plus vault keys,
    `set_ilk_definition` and config writes. Over the mesh, all of these are Archi's alone today.
  - **Decided:** its row is bounded to the node names and tenants its recorded deployments hold.
    Unbounded, SY.deploy would be a second Archi with no operator's CONFIRM.
- **Nothing in SY.admin creates a person.** `register_human` runs inside IO.cloud. **Decided:** an
  action that creates a virtual person, for SY.deploy.
- **An ED's birth does not fit in one wait.**
  - The edge gives Cloud 30 s, and IO.cloud waits 20 to 25 s.
  - The motherbee's orchestrator runs node actions one at a time.
  - SY.wf-rules budgets 15 s for each `run_node`.

  **Required:** SY.deploy answers *accepted* with the deployment's id, and Cloud reads the state
  after.
- **A retry must not birth a second ED.** SY.admin has no dedup of requests. **Required:** an
  idempotency key from Cloud, its own id for the ED.

### 4.2 The list, and who sees it

**Decided** (notes §15): Cloud does not keep the list of EDs.

- SY.deploy keeps the list of the deployments in the infra. An ED is one entry, and its
  responsibilities are listed under it.
- SY.admin reads the list from SY.deploy and serves it to Cloud, Archi and the HTTP API.
- **Open:** whether the ED's list of ILKs must also be in the shared memory, for nodes that read it
  at each message (notes §26).

### 4.3 Editing, ending, disabling

- **Editing** an ED's data is a change of its profile (§10) or of its channels.
- **Ending** an ED removes its deployments. Its ILK and its agents' ILKs, which have history, are
  **disabled, never deleted** (notes §12).

**The panel found that no removal today keeps an ILK:**

- **Freeing a node's name purges its ILK.** `remove_node_instance`, and `kill_node` with purge, go
  through the orchestrator's `teardown_node`, which marks and then purges the node's ILK and its
  vault secrets.
- **Not purging holds the name.** A plain `kill_node` keeps the node's config, so the name stays
  taken.
- **A recreate gives the node a new ILK** unless the old one is passed. If an upgrade recreates the
  conscious, it loses its definition and its memory with every person, which cognition keys by the
  agent's ILK (§8.3).

**Decided:**

- a **retire** mode in the orchestrator: stop the node, remove its files, mark its ILK, and never
  purge;
- a recreate that passes the old `ilk_id`.

**What an ending leaves:** exposures are Cloud's alone, so SY.deploy cannot unpublish them. Pages
stay until their fixed expiry, and the answer names them. Cognition's rows outlive the ILKs
(A-159).

**Nodes record who launched them,** in `_system.launched_by` (`cloud`, `archi`, `operator`, or the
caller's name). Cloud's node actions reach only the nodes Cloud launched. The nodes SY.deploy
launches carry its name, so Cloud's own node actions do not reach them: an ED's nodes change only
through SY.deploy.

---

## 5. SY.deploy: the node of the CD

**Decided** (notes §11, §12, §16, §17): one node is responsible for every CD of user solutions,
**`SY.deploy`**. It began as "the ED's node". It became the CD's node, and the ED became a kind of
grouping: *"eso me cierra más"*.

### 5.1 What it does

- **Takes orders from SY.admin:** deploy, upgrade, remove, list, and read one deployment.
  - The names `list_deployments` and `get_deployments` are taken: they read the orchestrator's
    software history.
- **Reads Archi's sources (CI)** and computes each deployment's desired state as the source plus
  its blanks' values.
- **Builds the steps** and asks SY.admin for each.
- **Keeps** each deployment's manifest, its history, and the list.
- **Is a system node** (`SY.`), part of the infra. What it deploys is a user's (notes §12).

**What the panel found about building the steps:**

- **Archi's reconciler is pure, but only its delta is.** Archi turns the delta into a plan with its
  LLM plan compiler (`run_plan_compiler_transaction`, `src/bin/sy_architect.rs`). The admin steps
  of each change class are named without their arguments (`compiler_class_admin_steps`). The
  reconciler has no section for an ILK, a vault key, a definition or a profile, and it refuses
  `identity`.
- **So SY.deploy needs a deterministic step builder,** with exact arguments, and reconcile sections
  for what an ED holds. **Decided.**
- **Its steps run one by one,** deterministic and with no AI, as SY.wf-rules does (decided). The
  other way, through SY.admin's executor, does not fit:
  - only Archi may call it over the mesh;
  - it refuses to run without SY.admin's AI, even with nothing to fill;
  - it reviews every step with a model call, so an upgrade of 100 EDs would cost hundreds of calls,
    and would stop when the AI key fails.

### 5.2 What it does not do

- **The platform's software.** The package installs the core and boots io.cloud and io.blob;
  io.web runs where the operator starts it. Runtimes grow by publishing and `SYSTEM_UPDATE`, and the
  orchestrator keeps that history per hive. That path stays as it is.
- **Loose launches.** A `run_node` for debugging or maintenance is Archi's, and Archi removes it
  after (the operator imagines so, notes §12). *"No deployments a lo loco"*: what runs for a user
  runs as a solution.
- **CI.** Sources, versions and the catalog stay with Archi (notes §9).

### 5.3 A deployment's manifest, which is its record

Nodes record who launched them, not which solution or why. Archi infers which solution a WF or the
user policy belongs to by matching its saved manifests against what runs (`wf_declared_by`,
`opa_policy_declared_by`).

**Decided** (notes §12, §13, §14): a deployment's manifest is also its record, and holds:

- **its owner, the grouping of its why:** "ED Laura, collections", so anyone can list at a glance
  what runs and why;
- **its tenant;**
- **its source and version,** pinned with the content's hash (decided);
- **its blanks' values;**
- **the nodes it made,** with their ILKs;
- **its history:** applied, upgraded and removed, when, and by whom.

The concrete desired state is computed from the source and the values; nobody writes it by hand.

**Decided, for operation (2026-11-25):**

- **A journal written before each step,** with deterministic node names and ILK ids. A restart of
  SY.deploy, or of the motherbee during a core deploy, then never leaves a node it does not know of.
- **A retry re-reads the state and converges,** rather than replaying the steps.
- **A lock per grouping,** so that a Cloud edit during an upgrade is not written back over by the
  upgrade's older view.
- **A check of the records against what runs,** after a restored motherbee snapshot.
- **Where SY.deploy runs** (the motherbee, as SY.admin does), how its state persists, and its
  backup. Open.

### 5.4 Groupings

**Decided** (notes §11, §12): the why groups deployments, all at the same level.

- **ED:** tied to the ED's ILK. Its first deployment is the plain ED, and each responsibility is one
  more deployment.
- **Infra:** what a user needs to run an ED with its own solutions (§6.4).
- **Test:** what a gate deploys, removed as a whole when the test is done.
- More kinds when they appear. Removing a grouping removes all it deployed.

Each ED needs its own connector to its company's CRM: nothing is shared between EDs (notes §12). A
resource common to several EDs, such as the company's phone, is an IO made for that (§7.4).

### 5.5 Checks

**Decided** (notes §13, §14; 2026-11-25): fixed checks first, before any step:

- every blank is filled;
- the keys a blank names exist in SY.vault, in the ED's tenant;
- the ILKs a blank names exist, **in the deployment's tenant** (the boss, for one);
- the node names are free.
  - A WF's name is installation-wide, since a WF package is refused while any hive runs
    `WF.<name>`.
  - The name grammar is lowercase with no hyphens, so a person's first name collides across
    companies. **Decided:** names come from a short unique id.
- the hive has room (§5.7).

Archi's design auditor reviews a source before it is applied. SY.admin's executor reviews each step
of a plan it runs (A-337); with steps sent one by one, no step is reviewed (§5.1). A new AI check
inside SY.deploy, only where judgment is needed, is **open**.

### 5.6 Growing by parts, and what is shared

**Decided** (notes §10): several solutions are deployed into one ED, and each is upgraded alone.

Archi's reconciler already leaves another solution's parts alone in two ways:

- it removes a WF or the user policy only when the solution that declared them removes them;
- it never kills a node missing from the manifest, so a node dropped from a solution keeps running.

**What is shared and gets in the way:**

| Shared thing | What happens with two deployments |
| --- | --- |
| The user OPA policy, one for the installation | The last one applied wins (`reconcile_opa`). A topic of its own (notes §11). |
| Routes, keyed by hive and prefix | The last one applied wins, and removing one ED's route breaks the other's. |
| VPN rules, keyed by pattern | The last one applied wins. |
| A runtime's `current` version | Upgrading it for one ED moves every ED's nodes at their next restart. |
| Node names | Archi's reconciler takes any running node with a declared name as its own and pushes its config. An Archi solution, or an operator's config write, can rewrite an ED's node, and the next upgrade reverts it. |

**Decided:**

- blanks cover routes and VPN patterns, or SY.deploy refuses sources that declare them;
- runtime versions are pinned per deployment;
- Archi refuses nodes that SY.deploy records, and refuses to apply sources with blanks;
- reading a deployment shows its drift.

### 5.7 Capacity

The panel's numbers:

- **The limits:** a router holds at most 1024 nodes, and the installation at most 1024 remote nodes
  (`MAX_NODES`, `MAX_REMOTE_NODES`, `src/shm/mod.rs`). Past them, link-state writes fail.
- **What that allows:** at about five nodes for the plain ED and two for each responsibility, about
  150 EDs.
- **One hive per ED:** a solution runs on one hive, so an ED's deployments sit on its conscious's
  hive.

**Decided:** about 150 EDs is fine for now (the operator). Each grouping has a fixed hive, and
SY.deploy checks capacity before it deploys.

---

## 6. Solutions for an ED

### 6.1 Source and deployment

**Decided** (notes §9). A solution is two things:

- **the source:** what Archi writes. Today it is a versioned manifest (`SolutionManifestV2`), kept
  per `solution_id` with one file per version;
- **the deployment:** the nodes that run it, with their ILKs, on one hive, charged to one tenant.

For an ED, "solution" means the deployment. One ED has many deployments, and two EDs never share
one.

**Versions** (decided, 2026-11-25). Every save in Archi makes a new version (`v<epoch ms>`), and
none is marked released, so "the latest" can be a draft from the middle of a talk. So:

- SY.deploy deploys **released versions only,** pinned by version and content hash;
- a blank a new version adds carries **a default,** or every upgrade waits for each ED's value;
- **an upgrade of many EDs goes in waves,** paced so it does not fill the orchestrator's queue,
  and stops on failure;
- **a rollback** deploys the previous pinned version.

### 6.2 Three layers

**Decided** (notes §9, §14):

1. **Native:** the plain ED, a fixed source every ED is born from. It brings the ED's ILK, an IO for
   each channel and its conscious, deployed like any source. A new version of it reaches every ED
   by an ordinary CD, in waves.
2. **Standard:** a template from Archi's catalog, made ahead for EDs, given to an ED when it is
   hired, and to others.
3. **Particular:** made to measure by Archi for one ED, as a new source deployed the same way.

### 6.3 Blanks

**Decided** (notes §13): a source made ahead declares what each deployment must give it, and a
deployment fills those blanks and nothing more. For "collections v3":

- the ED it serves, from which its node names come (§5.5);
- its CRM connector's credential, a key in SY.vault in the ED's tenant;
- whom it escalates to;
- a business value, such as the days of arrears before a call.

A different flow is a particular solution: a new source. The CD never changes what a solution is.

**What the source format needs:**

- **Blanks.** A manifest has no parameters today: names and configs are written out. The only
  template today is a runtime's, from which a node's config is written at launch.
- **An identity part.** The plain ED brings an ILK, and a responsibility's agents need their role,
  skills and handbooks (identity v2.1). A manifest refuses an `identity` section, so today these are
  set apart, with `set_ilk_definition`.

### 6.4 What is infra

**Decided** (notes §11, §12): infra is everything a user needs to run an ED with its own solutions,
and it is one of the groupings (§5.4).

**Open:** the line between infra and an ED's deployments, for three things:

- a hive (a manifest declares hives in its topology today);
- a tenant's provider key in SY.vault;
- an edge exposure.

---

## 7. Channels and their IO nodes

### 7.1 Assigned by configuration

**Decided** (notes §19): a person's channels are carried by IO nodes, each **assigned by
configuration** to the person it carries. The assignment is not restricted to EDs: the config may
name any person's ILK, real or virtual.

- **Where it goes.** io.slack, io.wapp, io.api and io.sim share one inbound configuration
  (`InboundConfig`, `nodes/io/common/src/inbound.rs`): TTL, dedup, `dst_node` (from `io.dst_node`),
  `provision_on_miss`, the blob runtime and `self_tenant_id`. The assigned ILK sits next to
  `io.dst_node`. io.linkedhelper reads its own `adapter.dst_node`.
- **For an ED,** the first layer sets the ED as the assigned ILK and its conscious as `dst_node`.
- **Decided:** the assigned ILK must be charged to the IO's own tenant, the same rule io.api
  applies to a `by_ilk` subject (`validate_explicit_subject_ilk`).

### 7.2 What the messages carry

**Decided** (notes §18, §19): in the messages, **the ED is the person, and the conscious is the
hand that wrote.**

**Today:**

- **Inbound.** io.slack and io.wapp build each inbound message in
  `InboundProcessor::process_message_parts` (`nodes/io/common/src/inbound.rs`), with the writer's
  ILK as `src_ilk` and no `dst_ilk`. io.linkedhelper already stamps the profile it carries as
  `dst_ilk`: a working precedent.
- **Reply.** `build_reply_meta` (`crates/fluxbee_ai_sdk/src/message.rs`) sets the person as
  `dst_ilk` and clears `src_ilk`. Then `stamp_reply` (`crates/fluxbee_ai_sdk/src/runtime.rs`) sets
  the agent node's own ILK as `src_ilk`, if empty, and names it as `meta.context.writer`.
- **The router binds the sending node's name** (`routing.src_l2_name`) to its socket. It **does not
  bind `src_ilk` to the sender**: it reads it as given ([control-path-hardening-v1](control-path-hardening-v1.md)).
  io.api's own comment says its `by_ilk` is safe only while no consumer treats a forwarded `src_ilk`
  as an authenticated principal (`nodes/io/io-api/src/main.rs`).

**Then:**

- **An IO assigned to a person stamps that person as `dst_ilk`** on the way in.
- **The conscious's reply carries the ED as `src_ilk`.** **Decided** as the review asked, since the
  natural way to build it is a hole:
  - **Where the ED comes from.** The ED the conscious speaks as comes **from its own config**,
    written by SY.deploy and checked there: virtual, and in the same tenant. It is never copied from
    the incoming message. Copying it, the mirror of `build_reply_meta`, would let whoever writes a
    `dst_ilk` choose who an agent speaks as, a real person included.
  - **Who stamps it.** The runtime stamps the ED on that node's replies only. The responsibilities'
    agents never stamp it, and no node ever replies as a real person.
  - **The rule changed** (§2.2, 4) and its contract test change on purpose.
- **A person's `src_ilk` is accepted only from the person's IO, or from that person's registered
  hand.** **Decided.**
  - **Why.** Once an ED acts on who is talking (its boss asks it to approve a refund), an unbound
    `src_ilk` is a hole: any node, in any tenant, could write as the boss.
  - **The model.** SY.vault's binding of a caller to its node (`check_caller_binding`,
    `src/bin/sy_vault.rs`), placed in the router, which is the place for what is global (§2.1).
  - **io.api.** Its `by_ilk` refuses virtual ILKs.
- **The hand stays recorded** in four places:
  - `routing.src_l2_name`, bound by the router;
  - the trace entry's `from`;
  - `meta.context.writer`, which SY.storage keeps as `writer_ilk` and `writer_name`;
  - cognition's `agent_node`.
- **The writer stays the conscious, for now** (decided; revisited with ED–ED talk and cognition).
  What each choice means (§8.2):
  - **If it stays the conscious,** cognition reads the ED's words as an agent's on any path that
    keeps the writer (a WF's forward, for one). They are not analyzed and give no feelings.
  - **If it becomes the ED,** the node is named only by `routing.src_l2_name` and the trace.

### 7.3 The email

An ED has an email, no matter what (notes §22). **No email IO exists.** The IO nodes are io.api,
io.blob, io.cloud, io.linkedhelper, io.slack, io.wapp, io.web and the simulator io.sim. An email IO
is part of the ED's native layer.

**Today an email is an `identification` field, not a channel** (`src/bin/sy_identity.rs`). An email
IO's sender ids must match the address registered on the person, or every message by email is a
stranger.

### 7.4 A common channel

**Decided** (notes §16): two EDs never hold the same number, but they may use a common one, such
as the company's phone, through an IO of its own made as a common resource.

**Open:** whose deployment that IO is, and which ED answers what comes in through it.

### 7.5 Who writes through an ED's IOs

A WF's `send_message` to an IO names no `src_ilk`, and its writer is the WF
(`go/nodes/wf/wf-generic/node/actions.go`). If a responsibility's WF writes to people through the
ED's IO, it does not speak as the ED. **Open:** who may write through an ED's IOs, and as whom.

---

## 8. Conversations across channels

### 8.1 The pair

**Decided** (notes §17, §18):

- **Start with the pair:** a person and an ED, and two EDs too. The operator's example: Laura in
  sales and Martín in projects settle by chat, and maybe by email after, whether a billing is
  approved.
- **The id.** The operator expects the messages to carry another id that joins a pair's channels,
  to analyze them as a subdomain of each communication.

**Proposed in the talk:**

- **The same id from both sides:** {Laura, Martín} joins their Slack and their email.
- **Computed, not stored,** from `src_ilk` and `dst_ilk` once both people ride in the message (§7.2).
- **Each side keeps its own memory of the other.** What Laura remembers of Martín is Laura's. The id
  joins the channels, and the owner keeps the memories apart.

**What the panel found:**

- **Each IO names its senders inside itself.**
  - io.slack builds a sender's external id as `<node name>:<user>`.
  - io.wapp builds it as `<phone_number_id>:<wa_id>`, under the stated rule that two IO nodes
    messaged by the same customer must resolve to two identities.
  - The lookup is tenant-scoped.
- **So, today:**
  - Ana writing to Laura and to Martín is two ILKs, and Ana on two channels is two ILKs, until a
    frontdesk merge on a vouched email joins them.
  - Laura's address seen by Martín's IO resolves to a new temporary ILK, never to Laura.
  - Across tenants, one ED never finds the other.
- **The pair holds only within one tenant, and after a merge,** unless the IOs assigned to a person
  resolve their senders across the tenant, which reverses the per-IO rule. **Open.**

### 8.2 What cognition does today

- **Memory, feelings and the last turns are per communication.**
  - The key is `(thread_id, agent ILK)` (`CommunicationKey`, `src/bin/sy_cognition.rs`).
  - The agent ILK is the ILK of the node that produced the turn.
  - The router finds a package by `(thread_id, agent_node)`.
  - Only a thread's first agent takes it, and each agent sees only its own.
- **The news cross channels, by person.** A news item is an episode: public gossip about a
  conversation's people, kept for the whole installation, fading with time, belonging to its
  protagonist. An agent gets up to 5 ([cognition-hands-v1](cognition-hands-v1.md) §12).
- **Whether a person is at the other end** is decided from the writer first (`meta.context.writer`),
  and only then from the ILKs. A turn written by an agent is not analyzed: between agents there is
  only the immediate memory.

**So two of the talk's expectations do not hold as they were said:**

- **An ED–ED talk is not "a person–person talk cognition already analyzes".**
  - The conscious is the writer of each reply, so its turns read as an agent's.
  - The analysis of an ED–ED talk works only where the receiving IO resolves the other ED's address
    to its ILK, which the per-IO naming of §8.1 prevents.
  - Analyzing ED–ED talks changes the rule "between agents, only the immediate memory". **Open,**
    with cognition.
- **On the ED's side, the episodes name the conscious, not the ED.**
  - Cognition takes the holding agent's ILK as one of an episode's people, and narrates it as "the
    agent".
  - So the episodes of Laura's conversations are about {the conscious, Ana}, never {Laura, Ana}, and
    never reach a conversation where Laura is the party.

### 8.3 What changes in cognition

The operator expects cognition to need one more id when an ED is involved, to group an ED's threads
with the same person and analyze them together (notes §7, §17). Reviewed with cognition when it is
touched. What the panel settled for that review:

- **The memory follows the node, whatever `src_ilk` says.**
  - The ED on the reply does not change whose memory it is.
  - With several instances, each instance is its own node, with its own ILK and its own memory of
    Ana, and its package carries only the turns it handled.
  - The pair's id still works across instances, because it comes from the message; the memory does
    not follow it.
  - For an ED's instances to share memory, **cognition's key and the router's package lookup move
    from the node to the ED.** Decided as the direction, and built with the instances (§13.1);
    with one instance, the memory by node is enough.
- **Cognition takes the ED in place of the agent's ILK** for episodes, for the narrator, and for
  whose news a package carries. Decided, in the same turn of cognition.
- **News whose protagonist is virtual** reach every tenant today, since news are filtered by person
  and not by tenant. Either they stay in the ED's tenant, or the operator accepts it. **Open.**
- **Cognition's tuned numbers move.**
  - With the ED in every turn's ILKs, the ILK similarity of a person's turn and its answer goes
    from 0.707 to 1.0, so scopes unbind less.
  - Each context and reason gains weight for the ED.

  Benign, but to know.

### 8.4 What waits

- **A matter spanning several people,** such as order #123, with Ana by WhatsApp and her boss by
  email. Proposed in the talk: it needs judgment to assign a message to a matter, so it would come
  later, as cognition's analysis inside each pair. **Open.**
- **The person who is two ILKs** (D13): within one tenant a merge joins them (§9); across tenants it
  stays open.

### 8.5 Two EDs that answer each other

`ai.generic` answers every user message, and each inbound message starts a new cycle, with a new
trace and a TTL of 16. Two EDs that answer each other by email or WhatsApp loop without end.

- **Today the only stop is Slack's,** and only by accident: io.slack drops every post that has a
  `bot_id`, so an ED–ED talk in Slack never arrives at all.

**Decided for now** (the operator, 2026-11-25: *"ok por ahora, pero veremos el tema seguro"*):
ED–ED talk is out of the first stage, and EDs talk with people first. Before it is enabled:
something that stops such an exchange, a decision on Slack's filter, the per-IO naming of senders
(§8.1), and the analysis between agents (§8.2).

---

## 9. The contact circuit

**Built** (notes §21):

1. **A stranger writes.** The IO provisions a temporary ILK for them in its own tenant
   (`provision_on_miss`).
2. **The IO's frontdesk gate** (io.slack and io.wapp, `nodes/io/common/src/frontdesk_gate.rs`).
   - **What it does:** for a sender who is not `complete`, it hands the frontdesk the channel and the
     address, then delivers the message anyway.
   - **When:** it is off by default, and turned on with `io.frontdesk.enabled`.
3. **The router's force rule** sends a temporary sender to the frontdesk.
   - It applies only to a message the router resolves.
   - An ED's IOs set `dst_node`, so their messages are never resolved: the rule never applies to an
     ED's channels. For an ED, only the IO's gate involves the frontdesk.
   - If they were moved to the router's resolve, every temporary sender would go to the frontdesk
     first, which contradicts "the ED does the human part". With no user policy loaded, everything
     else would be dropped.
4. **SY.frontdesk.gov is the one registrar.** It registers by command (`frontdesk_handoff`, taken
   only from an `IO.` sender) or by chat (its own LLM and `ilk_register`).
5. **Vouched and claimed.** A handoff's own email is vouched. A vouched email that a person of the
   tenant already has merges the temporary ILK into that person (`MERGED`). An email typed in a chat
   is claimed, and nothing merges on it.
6. **Cloud's `register_human`** hands a vouched email, through IO.cloud straight to the frontdesk
   (§2.1).

**For an ED** (notes §20, §21):

- **The ED does the human part,** "¿con quién hablo?", as a person would. The frontdesk stays the
  one registrar.
- **For now, simple.** A join across a contact's channels is fine to leave for later. Later, the
  process is reinforced: a contact who becomes a "user" of the system will need a mechanism, and
  maybe an approval.
- **Never a proof through an ED's own mailbox** (§3.1). How a contact's channels are joined is
  **open**.
- **Identity trusts an IO by its name's prefix.** Any `IO.*` node may add an address to any ILK, and
  may vouch an email in a handoff. That is fine while every IO is Fluxbee's own code. Before IOs
  come from Archi's particular sources or from packages, the IO's tenant must match the target's.
  **Decided.**

---

## 10. People: their positions and responsibilities

### 10.1 What the ED needs

An ED needs to know its boss, and who in its company handles what, so it can pass a problem on.

The operator: start simple, learn and tune, but with all of it (notes §23):

- the hierarchy;
- the responsibilities;
- the capabilities that the assigned solutions make explicit.

Identity v3's claims stay out for now.

### 10.2 What exists, and what is not touched

- **Identity v2's roles and capabilities:** retired, never built. Identity rejects `roles` and
  `capabilities` ([history/10-identity-v2](history/10-identity-v2.md)).
- **Identity v2.1's agent definition:** built. An `agent` ILK's role, skills, handbooks and
  personality are named by hash. Not touched.
- **Identity v3's claims and recognition:** drafted on 2026-04-13, not built
  ([future_implementations/10-identity-v3](future_implementations/10-identity-v3.md)). Left as the
  later direction.
- **A relation between two people,** a boss or who reports to whom, exists in no design and no
  code.

### 10.3 The profile

**Proposed in the talk** (notes §23), for the review:

- **For people only,** real or virtual, never for an agent node.
- **Position:** free text, such as "sales manager".
- **Reports to:** another person's ILK, in the same tenant. This is the hierarchy.
- **Responsibilities:** short free texts.
- **An ED's capabilities are not typed:** they are its deployments in SY.deploy, each with its why.
- **The company** comes at birth (§3). The position and the boss come when it is hired.

The texts are free and there is no catalog: learn first. It is knowledge for the EDs, neither a
membership nor a permission, and an exception to the decision of 2026-10-14/15 (§2.2, 1).

### 10.4 Where it lives, and how it travels

**Decided** (notes §24, §25):

- identity owns it;
- nothing large changes in identity;
- the extra data goes in a blob that agents read, and the shared memory keeps its name.

**Decided** (2026-11-25), with the folder reviewed in the code at the operator's request (§10.5):

- **Its own typed field on the person,** in identity's database, with its replication. Never inside
  `identification`:
  - a repeated `ILK_REGISTER` replaces `identification` whole, so a repeated `register_human` would
    wipe a profile kept there;
  - `identification.node_name` becomes the SHM's `handler_node`;
  - `identification.email` drives merges.
- **One file per version,** written once. A change is a new version.
- **Its name in the SHM.**
  - The blob store names files flat, `<name>_<hash16>.<ext>`, and checks the content against that
    hash. It refuses `/`, and allows only `[A-Za-z0-9._-]` (`crates/fluxbee_sdk/src/blob/mod.rs`).
  - So the 8 reserved bytes at the end of each `IlkEntry` hold the profile's **16-hex hash
    prefix**: exactly 8 bytes. The file name is fixed from it, such as `profile-<uuid>_<hash16>.json`.
  - The bytes are written as zeros and read by nobody today. The size stays 1160 and the SHM version
    stays 6, in its three copies: `src/shm/mod.rs`, the Rust SDK's `identity.rs` and the Go SDK's
    `identity_shm.go`.
  - The value lives in the database record, since the SHM is rebuilt from it.
- **Not through `read_blob`.**
  - `read_blob`'s model is that a blob's name is the permission to read it, from any tenant (A-106),
    and every node reads every tenant's SHM.
  - A name computed from the SHM would make one company's org chart readable to another's
    prompt-injected agent.
  - So the profiles live in a folder of their own, outside `read_blob`'s namespace, like Archi's
    agent assets. The motherbee sends that folder, and the spokes only receive it. It is exempt
    from the blob GC, which, when enabled, reaps active blobs older than 30 days.
  - **A people tool** reads them and serves only the ILKs whose SHM `tenant_id` is the agent's own.
- **Changed by one command, scoped by tenant.**
  - **No command today edits a person field by field:** a re-registration replaces
    `identification` whole, and `ILK_UPDATE` only adds channels, from SY.orchestrator, to its own
    hive's nodes.
  - **The new command:**
    - takes the profile from **exactly `SY.admin@motherbee`** (as `ILK_RETIRE_HIVE` does);
    - answers *not found* unless the person and the boss are charged to the envelope's tenant;
    - SY.admin exposes it to Cloud and Archi as one action.
  - **Who adds channels:** SY.orchestrator alone, as today.
  - **At an ED's birth** the company comes in the creation itself.
- **Syncthing's lag** (A-338). On another hive the SHM can name a file that has not arrived yet.
  **Open:** the tool answers "not yet", or the record keeps the previous hash to fall back on.

### 10.5 The profiles' folder, reviewed in the code (2026-11-25)

The operator: *"sería como activar un área de system que syncthing, solo revisar"*. Reviewed, not
built.

**The precedent is exact:** Archi's agent assets (FINDINGS A-72, 0.1.80), `src/bin/sy_orchestrator.rs`.

- **One Syncthing folder of the dist family,** `fluxbee-dist-agent-assets`, at
  `/var/lib/fluxbee/blob/agent-assets`. Its path is under `blob/`, but it is a `fluxbee-dist*`
  folder, so what handles every dist folder covers it: the `dist` sync hint, and `remove_hive`'s
  unlink.
- **The motherbee sends it,** and its tree is root's: SY.architect writes there, and Syncthing only
  reads to send.
- **The workers receive it.** On a worker the tree is the Syncthing user's, which writes what it
  receives, and ai.generic reads as root.
- **Ingress and egress never get it.** `folder_forbidden_for_role` takes it out of any hive that is
  not a worker, and its members follow the runtimes folder's.
- **ai.generic reads** `<blob_root>/agent-assets/<hash>.json` from its own disk and checks the
  content against the hash, so an edited file is refused (`ai_node_runner.rs`).
- **The blob GC does not reach it:** the GC works on `blob/staging` and `blob/active` only.

**A profiles folder is its sibling,** `fluxbee-dist-people` at `/var/lib/fluxbee/blob/people`.

- **It touches the same places in the orchestrator:**
  - the constant;
  - the tree's owner per role;
  - the spoke's folder list;
  - `folder_forbidden_for_role`;
  - the members it follows;
  - their tests.
- **The writer is SY.identity on the motherbee.** It runs there, where Archi writes the assets.
- **The reader is the people tool,** which checks the hash as ai.generic checks the assets, and the
  tenant against the SHM (§10.4).
- **Outside `read_blob`:** `read_blob` reads named blobs only, and refuses a path.
- **Multi-node:**
  - workers and the motherbee run agents, and get it;
  - ingress and egress run none, and do not;
  - a worker that joins later receives it with the rest of its dist folders.

---

## 11. The ED's agents

- **The conscious:** one agent, dedicated to the interaction (notes §5).
  - Its personality is the ED's: who it is (nationality, languages, time zone, background).
  - It comes in the native layer: the node raises the skeleton empty, and Archi assigns it the
    infra's tools (notes §6).
- **The responsibilities' agents** come with their solutions, each with its own ILK and definition.
  They are listed under the ED's grouping in SY.deploy.
- **Their tools are an unfinished topic** (notes §24).
  - **Today:** the base tools are the thread state, the artifact generators, `get_current_time`,
    `read_blob` and the HTML pages (`docs/reference/agent-tools.json`). An agent's tool may go
    through SY.admin, as `publish_html_page` does.
  - **The ED needs at least a people tool** (§10.4): "my boss", "who in my tenant handles X".
  - **Thread-state tools are per node,** so instances do not share them.
- **The conscious as several instances** of the same agent is agreed (notes §7). How a message
  reaches one, and the order of a person's line, wait for the end (§13).

---

## 12. Its identity on the internet

**The operator's starting point** (2026-11-20): *"tiene identidad reconocida en internet [tal vez
resolverlo así… (did:web / did:webvh + agent-card.json) + (did:key + Credenciales Verificables)]"*.

**Nothing in Fluxbee holds a DID, an agent card or a credential today.**

### 12.1 The pieces

- **A DID** (W3C [Decentralized Identifiers v1.0](https://www.w3.org/TR/did-core/)) is an
  identifier that resolves to a document with the ED's public keys and endpoints. Three methods
  matter here:
  - **`did:web`** ([the method](https://w3c-ccg.github.io/did-method-web/)) is anchored in a domain:
    `did:web:<domain>` resolves to `https://<domain>/.well-known/did.json`, and
    `did:web:<domain>:<path>` to `https://<domain>/<path>/did.json`. It is simple, and only as strong
    as the domain and its web server.
  - **`did:webvh`** ([DIF, v1.0](https://identity.foundation/didwebvh/), formerly `did:tdw`) is
    `did:web` with a verifiable history: a hash-chained, signed log of the document's versions, with
    a self-certifying id. Its history can be verified without trusting the web host, though it
    still resolves through the domain over HTTPS.
  - **`did:key`** (the W3C CCG method) is a public key written as a DID, with nothing to host and
    nothing to pay. It is an Ed25519 key pair whose public key is the identifier (`did:key:z6Mk…`),
    with no key rotation. It is unreadable to people, and not discoverable on the web by itself.
- **An agent card** ([A2A 1.0.0](https://a2a-protocol.org/latest/specification/)) is a JSON
  document at `https://<domain>/.well-known/agent-card.json` (the path since A2A v0.3; before, it
  was `agent.json`). It names the agent, its skills, its endpoints and its security schemes, so
  other agents can find it and talk to it.
- **Verifiable credentials** ([W3C VC Data Model 2.0](https://www.w3.org/TR/vc-data-model-2.0/), a
  Recommendation since May 2025) are signed claims among issuers, holders and verifiers. An example:
  "Laura works for Acme, in sales", issued by Acme and verifiable by anyone.

### 12.2 The operator's ideas (2026-11-25)

The operator brought two methods and a combination, as ideas to adapt.

- **Method 1, a domain and open standards:** `did:web` or `did:webvh` plus `agent-card.json`. It
  is the freest and most universal method, and the one open protocols such as A2A adopt.
  - **Cost:** a domain, or nothing under a subdomain of the company's own; a free TLS certificate
    (Let's Encrypt).
  - **Portability:** total. If the company leaves Fluxbee, it points its DNS or the file elsewhere,
    and the ED's URL and DID stay the same.
- **Method 2, keys alone:** `did:key` plus verifiable credentials. The company signs a credential
  that says the key belongs to its ED.
  - **Cost:** nothing.
  - **Portability:** high, since the keys go with the company.
  - **Limit:** nobody can discover the ED by it alone.
- **The two combined, the recommended architecture:** the domain gives discovery on the web, and the
  Ed25519 signature gives authenticity that does not depend on Fluxbee.

**Method 1's weak points, and what covers each:**

| Weak point | What covers it |
| --- | --- |
| The domain expires or its DNS is hijacked: the identity goes away, or to someone else | Renewal and DNS security are the domain owner's. |
| Silent changes: the file can be edited with no public trace | `did:webvh`'s history, or signed versions. |
| A hacked web server changes the card's contacts to impersonate the ED | The card's signature. It helps only when the verifier takes the key from a place the attacker cannot change: `did:webvh`'s log, or a `did:key` the company vouched for in a credential. |
| Every lookup tells the server who looked, and when (*phone home*) | Inherent to `did:web`. |

**The card is a passport, with the least exposure.**

- **It holds:** the ED's name, a description of its position, its company (`provider`), its skills,
  its connection URL (`supportedInterfaces`), how to authenticate (`securitySchemes`) and its
  signatures.
- **It never holds:** prompts, personality instructions, clients' history, keys or tokens.
- **No channel goes in the public card.** A WhatsApp number or an email published on the web is
  harvested within hours, for spam and prompt injection. A2A has two levels for this:
  1. **The public card:** who the ED is, the company behind it, and how to start a secure technical
     contact.
  2. **The extended card:** when the public card sets `capabilities.extendedAgentCard`, a caller who
     authenticates with a scheme the public card declares gets an extended card
     (`GetExtendedAgentCard`). It may reveal the direct channels and special permissions.
- **Who reads the card:** machines, not people:
  - other companies' agents, to learn the ED's skills and how to talk to it;
  - security gateways and B2B systems, which check the signature before they accept a mail, an
    invoice or an API call from the ED;
  - registries and indexes.

**Checked against A2A 1.0.0 (2026-11-25):**

- **The card's fields.** Its required fields are `name`, `description`, `supportedInterfaces`,
  `version`, `capabilities`, `defaultInputModes`, `defaultOutputModes` and `skills`.
- **Each interface** carries its own `protocolVersion`, next to its `url` and its `protocolBinding`
  (`JSONRPC`, `GRPC` or `HTTP+JSON`).
- **`provider`** takes `organization` and `url`.
- **A skill** requires `id`, `name`, `description` and `tags`.
- **A signature** is a JWS (RFC 7515) with `protected` and `signature`.
- The example the operator brought fits all of the above, with two exceptions:
  - **There is no plural `/.well-known/agents.json`.** A2A defines only one card per host, at the
    well-known path, and leaves registries without a standard API. So each ED needs a host of its
    own, such as `laura.acme.com/.well-known/agent-card.json`, or a registry, or direct
    configuration. `did:web` does allow paths (`did:web:acme.com:agents:laura`).
  - **In 1.0.0, `securitySchemes` wraps one scheme kind.** The OpenAPI-style
    `{"type": "http", "scheme": "bearer"}` of the example may not be its shape. Check it against the
    specification at build time, along with the signature's canonicalization (its §8.4).

### 12.3 What it means in Fluxbee

**Decided** (the operator, 2026-11-25; notes §27, §28):

- **It shares data with identity.** Part of the card comes from what §10 keeps on the person: its
  name, its position and its company. That overlap is why the operator wanted it in this design.
- **Private data stays private, and people are the standard.** An ED exposes what a person would
  expose, and nothing more.
- **Everything is served by Fluxbee, through the edge.**
- **The easy part first, in stages.**
  1. The DID document (the passport and its keys) and the credential the company signs. Neither
     needs an interface to talk through.
  2. The A2A card, later, and only while it costs nothing.
- **Not through io.api:** better not to mix the two. How identity is exposed, only for this, is seen
  when it is built.
- **The name:** the DID and its host come from the ED's first and last name. Names repeat, which is
  no problem for now; whether to handle repeats, or to move to A2A, is seen later.

**Proposed** (ideas for the talk):

- **The card's skills are the ED's responsibilities:** its deployments in SY.deploy, one skill each.
  They are not an agent's *skills* (identity v2.1). The card can be generated from the ED's grouping
  and its profile.
- **The card's interface is a channel,** for stage 2. `supportedInterfaces` is required, and points
  at an A2A endpoint into Fluxbee, through which other companies' agents talk to the ED. It is one
  more of the ED's channels (§7), and subject to §8.5. Not io.api (decided above). **Open** until
  stage 2.
- **The extended card's "legitimate client"** is the reinforced contact process of §9: someone who
  has proved who they are.
- **Keys:** the ED's Ed25519 key goes in SY.vault, in its tenant. For Method 2's portability, the
  company must be able to take it out.
- **Hosting through the edge:** the company points a name of its own at the installation, such as
  `laura.acme.com`. The edge serves the card and the DID document there, with SY.admin's own ACME
  certificate. `did:webvh` adds its log file to serve.

**Open** (a talk of its own, after the review):

- which node serves the well-known files behind the edge;
- who holds the keys, and how the company takes them out;
- who issues the credentials (the company through Cloud, or Fluxbee Cloud itself), and who verifies
  them;
- the A2A IO;
- what the extended card reveals, and to whom.

---

## 13. Open questions

1. **The instances and the line,** left for the end (notes §5, §6, §7, §8, §26):
   - how a message reaches one of the conscious's instances, and how one person's messages keep
     their order;
   - the operator wants this done by a tool of the system's own, never a WF, and dislikes the router
     deciding anything about load;
   - each instance is its own ILK in the ED's list, and today each holds its own memory (§8.3);
   - `ai.generic` runs 4 turns at once and queues 128, with no order per conversation;
   - the router delivers to one node by name.
2. **What the writer names** on the conscious's replies: the conscious for now; revisited with ED–ED
   talk and cognition (§7.2).
3. **Cognition's key and the router's package lookup,** from the node to the ED (§8.3).
4. **ED–ED talk,** out of the first stage for now (§8.5), and to be seen for sure:
   - loops (§8.5);
   - Slack's bot filter (§8.5);
   - per-IO naming of senders (§8.1);
   - analysis between agents (§8.2).
5. **The mixing of channels,** and analysis beyond today's cognition (notes §16, §17).
6. **The user OPA policy,** one for the installation, and the other shared things (§5.6).
7. **Where SY.deploy runs,** and its state's persistence and backup (§5.3). Its steps run one by one
   (decided, §5.1).
8. **The matter across people,** and **D13 across tenants** (§8.4).
9. **The common channel:** whose it is, and who answers (§7.4). **Who writes through an ED's
   IOs** (§7.5).
10. **The frontdesk's reinforced process,** and how a contact's channels are joined (§9).
11. **An AI check of deployments** (§5.5).
12. **Its identity on the internet** (§12).
13. **The agents' tools** (§11).
14. **"Something more" in the core:** IO + AI + something Fluxbee builds that is useful (notes §4).
    One candidate is the tasks on timers (notes §1.2).
15. **The line between infra and an ED's deployments** (§6.4).
16. **Where the ED's list of ILKs lives,** and whether nodes read it in the shared memory (§4.2).
17. **Roles and capabilities, to review better** (notes §15), and **identity v3's claims** when
    simple is not enough (§10).
18. **Cloud's flow for hiring:**
    - assigning responsibilities and solutions;
    - what Cloud stores;
    - Cloud's contract (create with the company, hire, assign, list, edit, end);
    - what becomes of `provision_node` (§2.2, 2).
19. **The ED's permissions** (notes §26).
20. **Where sources live,** as a repository with versions: Archi's solutions chapter (notes §8).
21. **News about a virtual protagonist** across tenants (§8.3).
22. **The profile file's lag** across hives (§10.4).

---

## 14. What it touches

This is an inventory for choosing what to touch first, not an order.

| Part | What changes |
| --- | --- |
| SY.identity | The `virtual` bit with its own constant. Merges into a virtual ILK refused. Cloud's person actions refuse it. The profile as its own field, its file, and its hash in the reserved bytes. A profile command scoped by tenant, from exactly `SY.admin@motherbee`. |
| The router | A person's `src_ilk` accepted only from the person's IO or registered hand. Maybe the `virtual` bit in OPA's input. The package lookup by ED, for instances. |
| SY.admin | A bounded `mesh_access` row for SY.deploy. A "create a virtual person" action. The ED's actions handed to SY.deploy. The deployments' list. The profile action. Their exposure to IO.cloud and Archi. |
| SY.deploy | New: deployments, manifests, journal and history, groupings, a deterministic step builder, checks, waves, capacity. |
| SY.orchestrator | A retire mode that never purges. A recreate that keeps the ILK. |
| Archi | Sources with blanks and an identity part, released versions, the plain-ED source, a catalog for EDs. Refusing SY.deploy's nodes and sources with blanks. Cleaning up its loose launches. |
| The reconciler | Shared with SY.deploy, or moved to it, with sections for an ED. |
| The IO family | The assigned ILK in `io.*`, in the IO's tenant. `dst_ilk` stamped on the way in. Sender resolution for IOs assigned to a person. A new email IO. The common-resource IO. Slack's bot filter. Matching the IO's tenant before IOs come from packages. |
| io.api | `by_ilk` refuses virtual ILKs. |
| The AI SDK and ai.generic | The ED stamped from the conscious's config. The writer's choice. The contract test changed. A stop for ED–ED loops. A people tool. |
| SY.cognition | Grouping by pair. The memory keyed by ED. The ED in episodes, the narrator and news. News of a virtual protagonist. |
| The blob tree | A profiles folder outside `read_blob`'s, sent by the motherbee, exempt from the GC. |
| SY.frontdesk.gov | Later (§9). |
| Fluxbee Cloud's contract | Create with the company and an idempotency key, answered *accepted*. Hire, assign, list, edit, end. |
| The SHM readers (three copies) | The `virtual` bit, and the profile's hash. |

**Also recorded:** io.web and io.cloud are system nodes and should be `SY.web` and `SY.cloud`, with
the open-source install's question (FINDINGS A-363).

---

## 15. The other large topics of 2026-11-20

Each gets a talk of its own (notes §1):

- IO nodes from a package;
- tasks on timers;
- a conversation viewer in Archi, from Fluxbee Cloud's team's `tools/visor-casos`.

---

## 16. The panel review (2026-11-25)

Five reviewers read the first draft against the code and the notes, each through one lens. Each
heading below names its lens.

### 16.1 Facts

- **One blocker:** the profile's blob name. Blob names are flat and hashed, so the first draft's
  `people/<ilk>/<version>.json` could not exist (§10.4).
- **Five majors:**
  - a re-registration does edit a person;
  - nodes do record who launched them;
  - the inbound function the draft cited is unused, and io.linkedhelper is a precedent;
  - cognition reads the writer first;
  - IO.cloud's `register_human` skips SY.admin.
- **Minors and §12 corrections:**
  - the VC 2.0 link;
  - `did:webvh` still resolves through the domain.

### 16.2 Fidelity

- **Proposals stated as decisions:** the pair's details, the profile's shape and its command.
- **ED–ED analysis against cognition's rule.**
- **Infra dropped as a grouping.**
- **Unstated changes to earlier decisions:** companies in Cloud, Cloud's role, one manifest per
  tenant. Now listed in §2.2.
- **Open points missing from §13.**

### 16.3 Identity and security

- **High:** an unbound `src_ilk` becomes something EDs act on (§7.2).
- **High:** a stranger can merge into an ED through its LLM-read mailbox (§3.1).
- **Medium:** a cross-tenant read through `read_blob` (§10.4).
- **Medium:** a profile command with no tenant scope (§10.4).
- **Medium:** `handler_node` (§3).
- **Medium:** news across tenants (§8.3).
- **Low:** Cloud's person actions, trust in the `IO.` prefix, and the flag's constant and OPA.

### 16.4 CD and operation

- **Steps:** no deterministic step builder exists, and the executor's limits (§5.1).
- **ILKs:** no removal keeps an ILK; recreate makes a new one (§4.3).
- **Admission:** SY.deploy's admission in SY.admin (§4.1).
- **Timeouts:** Cloud's waits, and duplicate births (§4.1).
- **Partial failures and restarts** (§5.3).
- **Versions:** they are neither released nor pinned (§6.1).
- **Shared things:** routes, VPN rules, runtimes and node names (§5.6).
- **Capacity** (§5.7).

### 16.5 Messaging and cognition

- **The source of the reply's ED** (§7.2).
- **Per-IO naming breaks the pair across EDs** (§8.1).
- **ED–ED loops** (§8.5).
- **Memory keyed by the node** (§8.3).
- **Episodes name the conscious** (§8.2).
- **Minors:** the writer, the force rule, `handler_node`, and cognition's numbers.

### 16.6 The operator's calls (2026-11-25)

The recommendations went to the operator one by one. The operator answered *"sí a todo"*, with two
notes:

1. **The four changes to earlier decisions** (§2.2). Yes. Cloud's `provision_node` stays as it is
   for now.
2. **An ED is not a person for merges, for a Cloud account, or for Cloud's person actions**
   (§3.1). Yes.
3. **A person's `src_ilk` bound in the router** (§7.2). Yes.
4. **The conscious's ED taken from its config,** and the writer stays the conscious for now
   (§7.2). Yes.
5. **The memory keyed by the ED** (§8.3). Yes, as the direction, built with the instances.
6. **SY.deploy's steps one by one, with no AI, and its bounded admission** (§5.1, §4.1). Yes.
7. **The retire mode, and a recreate that keeps the ILK** (§4.3). Yes.
8. **The profile's storage:** its own field and folder, and a tool and a command scoped by tenant.
   Yes. The operator asked for the folder to be reviewed in the code and across nodes, "like turning
   on a system area that Syncthing carries". Reviewed in §10.5: it is the agent assets' sibling.
9. **ED–ED talk out of the first stage** (§8.5). Yes for now: *"veremos el tema seguro"*.
10. **Released versions and waves** (§6.1). Yes.
11. **Capacity, about 150 EDs** (§5.7). Fine for now.

The minor items that follow from these come with them:

- the flag's own constant;
- io.api's `by_ilk` refusing a virtual ILK;
- an IO assigned to a person of its own tenant.

The operator also said that 8.x is not untouchable: it may be reinstalled when a change needs it.
