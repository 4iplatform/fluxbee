# ILK and tenant — the identity and cost-center model (v1)

**Status:** contract and order of work decided by the operator on 2026-10-14. Stage 1 is live in
0.1.65, validated on the 8.x testbed on 2026-10-14 (`lab/DEPLOYMENTS.md`). Stage 2 is live in 0.1.66,
validated on 8.x the same day (FINDINGS A-64). Stage 3 is built for 0.1.67 (FINDINGS A-66).
This document is the single source for the change; FINDINGS and the ledger point here.

**How it was reached.** It came out of a conversation, not out of the code: the operator, Claude
and Astra (who evaluated the repository against the idea). Operator: *"la idea la hicimos entre los
tres, sin la charla no sale nada más que código basura"*.

---

## 0. The contract

- **ILK identifies the actor:** a person, a node, a system service. One ILK per actor in the
  installation. An ILK is never duplicated because of a tenant. Operator: *"la identidad dentro del
  sistema es prioridad a todo, no veo bien resolver esto creando miles de ilk para lo mismo"*.
- **A tenant is a cost center: a record of spending.**
  - It is usually a company, not always (operator, 2026-10-14).
  - Each ILK is charged to exactly one tenant at any moment, and it can be reassigned, explicitly
    and with a record.
  - Fluxbee records what is spent and under which tenant. Who pays, which company or legal entity a
    tenant stands for, and how it is billed are Fluxbee Cloud's business.
  - Operator: *"el tnt dentro de fluxbee tiene que ser visto … como una entidad que PAGA los tokens
    que se gastan en fluxbee … la conexión empresa-tenant la hace fluxbee cloud no el backend"*;
    *"el tenant es un registro de gastos, quién paga es problema del cloud"*.
- **Permissions authorize operations on resources, and they are explicit.** Changing an ILK's
  tenant neither grants nor revokes anything.
- **A suspended tenant cannot use the infrastructure** (operator, 2026-10-14).

The separation the operator called out: *"separar un sistema de infra del dinero y lo legal"*.
Fluxbee keeps the infrastructure (identity, permissions, the spending record); Cloud keeps
companies, members, billing and the law.

---

## 1. Today (verified in the code, 2026-10-14, 0.1.64)

- **One tenant per ILK everywhere:**
  - in memory: `IlkRecord.tenant_id`;
  - in Postgres: `identity_ilks.tenant_id NOT NULL`, a foreign key;
  - in the replication records;
  - in the identity SHM: a fixed 16-byte field in each `IlkEntry`, mirrored by the Go SDK.
- **A channel's lookup key is `(channel, address, the ILK's tenant)`.** It is recomputed from the
  ILK's tenant on every rebuild and on every DB load. So:
  - the same person seen in two tenants is two ILKs;
  - changing an ILK's tenant breaks its lookups;
  - a merge keys moved channels with the source's tenant in memory and with the target's in the DB,
    and the two diverge at the next restart.
- **The tenant is used as a permission in:**
  - SY.vault: a node reads its tenant's pool by tenant equality, and the SDK resolves a credential
    in this order: dedicated to the ILK, then its tenant's pool, then the root pool;
  - io.api: a `by_ilk` subject must be of the instance's tenant;
  - IO.cloud's rule 3 (0.1.64): reads by id answer NOT_FOUND for another tenant;
  - SY.identity: tenant-scoped reads.
- **Tenant lifecycle:**
  - deleting a tenant marks every person charged to it (the 2026-09-28 cascade);
  - node identities block the delete.
- **Tenant status is not enforced anywhere:** each tenant's real status (pending, active, suspended)
  reaches the identity SHM, and a deleted tenant is left out of it, but nothing reads it to refuse.
  (This line said "the SHM writes every tenant as active" until 2026-10-14: wrong, the operator
  caught it.)
- **No metering.** The AI SDK reads the tokens of each model call and discards them.
- **Sponsors** (one per tenant, no cycles) are stored and shown, and grant nothing.
- **Not tenant-based already:** SYSTEM authority (OPA, by node role and hive) and the unpublishing of
  artifacts (by publisher).

---

## 2. Target design

### 2.1 ILK structure (stage 1)

Revised after one adversarial review of this section against the code (2026-10-14: 7 major and 5
minor findings, all folded in below). PROD had nothing to migrate that day: no person, one tenant.

**Channel scope.**
- Each channel records `scope`, the namespace its address lives in, fixed when the channel is
  created:
  - `ILK_PROVISION`: the request's `scope` if it names one, else the request's tenant (today's
    value).
  - `ILK_ADD_CHANNEL`: the channel's `scope` if it names one, else the target ILK's current tenant
    (today's value).
  - The installation-wide scope (the root tenant's) is IO.cloud@motherbee's, for channel type
    `cloud` only. A `cloud` channel from any other caller is refused (`CLOUD_CHANNEL_RESERVED`), so
    no IO node can squat an account's email. The same holds on every other path: `ILK_UPDATE` (the
    orchestrator adding a node's own channels) takes no `cloud` channel, and a tenant's IO node
    cannot merge a temporary that holds one (the relay and a frontdesk can).
  - A new channel's ICH id must be nobody else's (`ICH_ID_CONFLICT`): the DB keys channels by it,
    and a second channel with the same id would quietly move the first one's row. A channel the
    migration moved keeps the id seeded from its old scope.
- **One helper computes every channel key from the channel's scope.** That covers:
  - provision, add and update, merge;
  - the "deleted ILK holds this channel" check, the restore check, unmark;
  - the replica's full-sync rebuild and its deltas;
  - the DB load;
  - the SHM full sync, the SHM deltas, and the SHM provision fast path in `src/shm`.

  Tenant checks (`TENANT_DELETED`, pending, suspended) stay on the ILK's tenant.
- **Persistence:**
  - `identity_ichs.tenant_id` stores the scope and is read back on load. Today the load ignores it
    and the upsert writes the ILK's tenant: both change.
  - A channel record without a scope (from an older peer during an upgrade) takes its ILK's tenant
    once, when it is deserialized, never on a DB load. Otherwise a reassigned person's keys would
    follow the new tenant after a restart.
- **Replica pushes:** a replica pushes its node ILKs, so a channel it adds must live in the node's
  own tenant, and never be a `cloud` channel or in the installation-wide scope. A channel the
  primary already holds passes as it is (the replica echoes what it was told). Both the snapshot and
  the delta path check this. A refused push leaves the primary's record as it is: it is never taken
  for a removal. That invariant holds until stage 2.
- **No SHM layout change:** the channel entries already carry their own tenant field. No reader
  joins a channel entry to its ILK entry by tenant.

**One Cloud account per email.**
- IO.cloud provisions the `cloud` channel in the installation-wide scope, with the ICH id seeded
  from that scope. The new ILK is charged to the request's tenant: only the channel is
  installation-wide, so the rule that no person registers into the root tenant is untouched.
- `ILK_PROVISION` answers `{created, registration_status, tenant_id}`.
- `register_human` then works as follows:
  - **A complete account already holds the email:** it answers `existing: true`, the `ilk_id`, its
    tenant and `registration_status: complete`. There is no frontdesk handoff and nothing is
    created. Charging it to another tenant is the separate reassignment.
  - **A temporary from an abandoned signup in another tenant holds it:** SY.identity re-charges it
    to the requesting tenant, with a history row, and the registration continues. An abandoned
    signup never locks an email out. Only the relay's provisioning in the installation-wide scope
    re-charges: no other scope is shared across tenants.
  - **A node's own `cloud` channel holds it** (IO.cloud's own, `demo`): a person's signup answers
    `CLOUD_CHANNEL_RESERVED`.
  - **A person charged to the root tenant holds it** (registered before 0.1.54): it answers
    `ACCOUNT_IN_ROOT_TENANT` until the operator reassigns or purges that person.
  - **An email the migration left ambiguous:** it answers `ACCOUNT_AMBIGUOUS`, whoever holds the
    key.
- IO.cloud's email reads (`get_ilk` and `get_ilk_details` by email) look in the installation-wide
  scope first, then in the claimed tenant's. That covers leftovers and the version skew between the
  core and the managed IO.cloud runtime.
- An email is evidence because Cloud authenticated that login. Other channels (Slack, a phone) are
  never joined to a person automatically; linking them needs an explicit step, later.

**Reassignment: `ILK_SET_TENANT`.**
- It runs on the primary. Callers: SY.admin and SY.architect; Cloud reaches it through IO.cloud.
- It takes only an active, complete human ILK.
- **The target** must exist, be active (not pending or suspended), not be deleted, and not be the
  root tenant.
- **Refused if another ILK in the target already holds the person's email** (`DUPLICATE_EMAIL`).
  Email is still unique per tenant in memory and in the DB, so this is checked before anything
  changes.
- **Channels keep their scope.** A history row (ILK, from, to, when, by whom: the operator or
  IO.cloud, as SY.admin relays it) is written in the same transaction.
- `ILK_REGISTER` no longer changes a human's tenant: that was a second, unrecorded reassignment
  path. Node ILKs keep today's behaviour.

**Merges.** `check_merge_source`, which runs before anything changes, requires the source and the
target to be charged to the same tenant (what `merge_registration` already checks). Moved channels
keep their scope everywhere.

**Cloud's view of ILKs.**
- **Reads by id are not tenant-filtered:** the tenant is not a boundary.
- **`delete_ilk` and `restore_ilk` from Cloud** carry a human-only guard (`ilk_type: human`;
  another type answers `ILK_TYPE_MISMATCH`), and they are not tenant-scoped: deleting an account is
  an account action. Operator decision D4. Node identities are the orchestrator's.
- **Lists by tenant stay:** "what is charged to T".
- These ship with stage 1, because the migration changes the keys Cloud's email reads use.

**Deleting a tenant** with accounts charged to it was refused in stage 1 (`TENANT_HAS_ACCOUNTS`),
and the other people charged to it were marked with it. Stage 3 replaced both: deleting a tenant
marks no one (§2.3).
- **A pending signup is readable as such** (D11, *"que se puedan leer las pendientes"*):
  - `pending_signup` in every ILK the operator, Archi and Cloud read;
  - the operator's list filters `?pending_signup=true`, wherever they are charged, deleted
    tenants included;
  - the boot report names the ones left charged to a deleted tenant.

**Purging a tenant** also drops the channels other ILKs still hold in its scope: a person reassigned
out keeps the keys of the tenant they left, and those keys go with the tenant. No node of that tenant
is left to look them up (`TENANT_HAS_NODES`). The answer counts them (`dropped_channels`). Without
this, the channel rows' foreign key to the tenant made the purge fail.

**The migration.**
- **When:** an SQL step (v3) in `ensure_identity_schema`, inside its transaction and advisory lock.
  It runs at every primary boot, before the store loads, so memory loads the migrated scopes. It is
  idempotent. Replicas take the result through their next full sync.
- **What moves:** the `cloud` channels of active, complete human ILKs move to the
  installation-wide scope. Addresses are compared trimmed and lowercased.
- **An email is ambiguous, and stays where it is,** when:
  - more than one active, complete human holds it;
  - or another ILK, marked or not, already holds it in the installation-wide scope (the DB's
    uniqueness counts marked rows).
- **After loading,** SY.identity rebuilds the set of ambiguous emails (provisioning one answers
  `ACCOUNT_AMBIGUOUS`) with the same two rules, and the list of root-charged people. It reports
  both in its log at every boot, by ILK id and never by email.
- **Resolving one:** the operator deletes and purges the extra ILKs, then restarts SY.identity: the
  next boot moves the email. Nothing moves on its own in between.
- **Restoring** a marked person whose `cloud` channel stayed in its old scope (marked before the
  migration):
  - another active ILK holds the installation-wide key: the restore is refused
    (`ILK_KEY_CONFLICT`), so it never brings back a second account;
  - the key is free (nobody holds it, marked or not, and the email is not ambiguous): the channel
    comes back in the installation-wide scope, as the migration would have moved it;
  - otherwise it comes back where it was.

### 2.2 Secrets (stage 2: SY.vault)

Decided with the operator on 2026-10-14, after stage 1 went live: *"tenant no es identidad ni
seguridad ... el secret se guarda con el tenant para filtrar el uso del que paga"*.

**What a secret carries:**
- **its tenant:** who pays for its use. It filters Cloud's listings and administration, and the
  vault checks it in one way only (below);
- **optionally an owner:** a node or a person (an ILK);
- **optionally the nodes it is assigned to:** one or several.

**Who reads it:**
- its owner, when the owner is a node;
- a node it is assigned to;
- for a **general** secret (no owner, no assigned node): every node charged to its tenant. Cloud
  decides it by saving the secret that way (*"es un recurso fácil de usar"*: a company's AI key for
  tests or a small system, a database for its new nodes). A node looks for its own secret first
  (owned or assigned), then its tenant's general one;
- the genesis tenant's general secrets are the system's keys (the database, the hive's AI keys):
  the core reads them as any tenant's nodes read theirs, and at boot even before the identity SHM
  exists (the well-known system ILK path, as today).

**The tenant check:** the vault hands out a secret only while its tenant exists and is active. It
reads that from the identity SHM, which already carries the real status and leaves deleted tenants
out. The genesis tenant always counts as active.
- A suspended or pending tenant is refused (UNAUTHORIZED), and the consumers drop the value.
- A tenant the SHM does not hold is not handed out either, but answered as retryable
  (IDENTITY_UNAVAILABLE). A degraded identity start writes only the genesis tenant, and the nodes
  must not drop their credentials during an identity outage; a deleted tenant has no node left to
  ask anyway.

**What keeps this sound:**
- **A node is born and dies in its tenant:** it is never reassigned; identity and the orchestrator
  refuse it. "Every node of the tenant" is therefore a stable set.
- **People are reassigned, and a person can own secrets:** for example, their access to an ERP or
  another system outside Fluxbee. Which node uses a person's secret is assigned explicitly when the
  case comes; for the vault, the person being an ILK is enough. Changing the person's tenant
  changes nothing about who reads it.
- **Only the core is born in the genesis tenant:** SY.\*, RT.\* and the core IO runtimes (io.cloud,
  io.blob, io.web). The orchestrator defines the core, since it is who gives out the genesis tenant,
  and refuses every other node there. Today it refuses other IO nodes only, and an AI or WF node can
  still be launched there, a leftover of the tests. Cloud never acts on the genesis tenant (rule
  3).
- **No access groups and no installation-wide key.**

**What changes:**
- **SY.vault:** the assigned nodes on the secret, the tenant check, and a person as owner. The read
  rules for owned and general secrets stay as they are.
- **Change notifications:** an assigned node is notified, and so is a node removed from the list,
  so it drops its cached value.
- **The SDK's resolution:** owned or assigned to me, then my tenant's general secret. The third
  step goes: the root tenant's secret for any node. It never worked for a company's node, because
  the vault refuses it, so A-35's fallback was dead; the docs that promise it change.
- **Identity and the orchestrator:** a node's tenant is fixed, and only the core is born in the
  genesis tenant.
- **Cloud (IO.cloud and SY.admin):**
  - `put_token` with an owner node, with nodes, or general. A person as owner goes through the
    operator for now; Cloud's surface for it comes with the case;
  - a new value keeps who uses the secret unless the put names it, so a refresh never turns a
    restricted secret general;
  - assigning and unassigning nodes without rewriting the value (`assign_token`);
  - `list_tokens` showing the owner, the nodes or "general".

  The assigned nodes must be charged to the secret's tenant: Cloud reads the nodes from Fluxbee's
  inventory and stores nothing of its own (*"cloud no guarda nada, tiene que leer todo y
  gestionar"*).
- **Later, not in stage 2:** the inventory reads for Cloud (which nodes use which token), and an
  email and password login. The vault would hold the credential, and an IO.login node would serve
  it.

**Migration:** none. Every secret keeps its meaning: a dedicated one stays dedicated, and an
owner-less one is its tenant's general secret, as today.

**Known limits (documented, not built):**
- A deleted node stays in the assigned lists. It reads nothing, and removing it could turn a
  secret general on its own (no owner, no node left), which is worse. Cloud cleans the list with
  the live nodes.
- io.linkedhelper reads its credential only at start and on CONFIG_SET: an unassignment or a
  suspension reaches it at its next restart. Its peers (io.slack, io.wapp) re-read on every change;
  align it when it is next touched.
- The vault no longer trusts the caller's `src_ilk` as declared (0.1.67, FINDINGS A-65). It must be
  the ILK of the node the router says sent the request: derived from its name for a system node,
  registered for its name for any other. Across hives that name is bound to the hive's certificate
  (EDGE-01). On the same hive it is the name the process declared in its HELLO, so a local process
  can still claim another local node's name, and its ILK, until P-1 (FINDINGS A-22, postponed).

### 2.3 The tenant as a cost center (stage 3)

Decided with the operator on 2026-10-14, after stage 2: *"no quiero borrar ni desaparecer ilk de
humanos"*; *"sobre nodos y recursos sí vale la pena actuar"*.

**The record.** `TenantRecord` holds labels and a state Cloud sets: name, domain, status,
settings, sponsor. The sponsor is stored flat and grants nothing.

**A tenant that is not active (pending, suspended or deleted) cannot use the infrastructure.**
- **Secrets:** SY.vault hands it none (stage 2). Its running nodes lose their credentials at once
  and get them back when it is active again. An AI node reads its key on every call, so it spends
  nothing.
- **Nodes:** the orchestrator starts none of its nodes.
  - `run_node`, start and restart answer `TENANT_NOT_ACTIVE`.
  - The boot and periodic relaunches skip its nodes: no relaunch, no move to a new version. Once
    it is active again, the next pass (within 60 s) relaunches the ones that died.
  - When the hive cannot tell (identity not up, or the tenant not synced yet), the launch goes on as
    before, said in the log. The vault hands the node nothing until its tenant is known to be
    active, and a deleted tenant is refused by identity when a new node registers
    (`TENANT_DELETED`). Nodes cannot outlive their tenant: deleting it waits for them.
  - A node that already runs is never stopped for it. Stopping and relaunching would be more
    machinery and less reversible, and without its secrets it spends nothing.
- `start_node` on a node that already runs answers `already_running` whatever its tenant.
- **Not built, documented:**
  - the router does not refuse the traffic of a suspended tenant's ILKs. It would be a lookup on
    every message, on the busiest path, and it adds little to the above;
  - a workflow rolled out to a suspended tenant: wf-rules writes the new version into the node's
    config, the restart is refused, and wf-rules reports `restart_failed`. Once the tenant is active
    again nothing restarts the running node, so it keeps the old package until a restart.
- The root tenant is never suspended.

**Deleting a tenant disables it.**
- **Only the tenant is marked.** No person charged to it is marked, whatever they are: an account,
  a contact known through a channel, a pending signup. They keep their identity and their
  channels, they can write to any company, and Cloud charges them elsewhere with `set_ilk_tenant`.
  What they spend with another company's nodes is that company's, because the node is the one
  that spends.
- **Refused** while it still runs nodes (`TENANT_HAS_NODES`: kill them first) or sponsors active
  tenants.
- **Restoring it is reactivating it:** it comes back as it was, because nobody in it was touched.
- The answer counts the people still charged to it (`people_charged`).
- A person deleted on their own comes back with `restore_ilk` even while their tenant is marked,
  still charged to it. Any other ILK waits for its tenant to be restored.
- This replaces the cascade of 2026-09-28 and stage 1's `TENANT_HAS_ACCOUNTS`.
- **Data from before:** until 0.1.66 a delete marked the people charged to the tenant (reason
  `tenant`). Migration v4 un-marks them at every primary boot, before the store loads; they stay
  charged to their tenant, and their ids are logged. One whose email an active ILK of the tenant now
  holds stays marked, and is logged too.

**Purging a tenant** (final, the operator only) is refused while people are charged to it
(`TENANT_HAS_PEOPLE`, the first 20 listed): they are charged elsewhere first, never removed with
it.
- `set_ilk_tenant` takes temporaries out of a deleted tenant, so a company with a thousand WhatsApp
  contacts can be emptied. A temporary of an active tenant stays: its frontdesk registers it in that
  tenant's case (`INVALID_TENANT_TRANSITION`). A bulk move ("everyone from T to U") is added if it is
  needed.
- A temporary merged into another ILK is an alias of it until the alias expires: it is not counted
  and goes with the purge.
- People marked on their own go with the purge.
- It is still refused while the vault holds its secrets (`TENANT_HAS_SECRETS`), node identities
  remain or it sponsors tenants.

**io.api `by_ilk`: people the tenant knows (D12).** A company's API can act for a person (the
request names their ILK) when the person is charged to the company or holds a channel in its scope,
that is, wrote to its WhatsApp, its Slack or its API. The channel, each company's space since stage
1, replaces the tenant as the permission, and nobody has to be moved for it.

**Contacts: the people who write to a company.**
- A person who writes to a company's channel is created charged to that company, and the channel
  lives in its scope: the IO node provisions with its own tenant. Operator: *"si hay alguien que se
  comunica es asociado a un evento sobre ese TNT"*.
- The same person writing to two companies is two ILKs. A phone number or a Slack ID is not
  evidence across companies; only an email that Cloud authenticated is (§2.1).
- Charging an account to another company is `set_ilk_tenant`; a contact that has not registered
  stays with the company it wrote to while that company is active. The companies that know a person
  are the scopes of their channels, which never move, so the company they leave still reaches
  them.
- **Open (D13):** joining a contact to an account, for example a customer of A who becomes the
  owner of B. It needs an explicit step (the person proves the channel from their account), never
  a match on the number. Not designed.

**Other tenant uses.**
- **OPA:** `data.identity[ilk].tenant_id` stays, documented as the cost center. Installed user
  policies must not use it as a permission (PROD has no user policy).
- **The root tenant** is the installation's own cost center: system nodes.
- **Docs:** "every ILK belongs to exactly one tenant" became "is charged to" (`docs/10-identity-v2.md`).

### 2.4 Cloud (stage 4; parts ship with stages 1–3)

IO.cloud applies all of the above:
- `set_ilk_tenant`;
- reads by id without a tenant filter;
- `register_human` with account semantics, and `get_ilk` by email answering the one account;
- `delete_ilk`/`restore_ilk` for people only;
- lists per cost center;
- credentials per stage 2;
- the tenant status that Cloud sets (activate, suspend).

`docs/io-cloud-api.md` and the Cloud team's note follow each stage.

### 2.5 Tokens (stage 5, designed later)

A consumption record per model call:
- when;
- the ILK that spent (the node that called the model) and its tenant at that moment;
- the counterpart ILK and its tenant at that moment;
- node, provider, model, input and output tokens.

It is stored centrally on the motherbee, with admin and Cloud reads by tenant and period. Stamping
the tenant at the moment keeps history right across reassignments. The gate of §2.3 stops a
suspended tenant from spending. The operator flagged this as possibly needing "something more
complex, central"; it gets its own design.

---

## 3. Stages and gates

Every stage goes through:

1. code and unit tests;
2. one adversarial review;
3. a release (version bump, ledger);
4. validation on the 8.x testbed;
5. FINDINGS and logbook.

| Stage | Content | Gate (beyond unit tests) |
|---|---|---|
| 1 | ILK structure (§2.1) and its IO.cloud part. | On PROD, through Cloud's public door: <br>• the same email registered from two tenants gives one ILK; <br>• `set_ilk_tenant` moves the person, and the person's `cloud` and IO channels still resolve; <br>• the person keeps their channels after an identity restart (DB reload); <br>• worker1's identity replica shows the new tenant; <br>• a read by id across tenants works, and Cloud is refused a node's ILK on delete; <br>• a tenant with an account charged is refused (`TENANT_HAS_ACCOUNTS`), and purging the tenant a person left drops their keys there; <br>• the migration on data made with 0.1.64: a unique email moves, an email in two tenants is reported and refused (`ACCOUNT_AMBIGUOUS`). |
| 2 | Secrets (§2.2). | On PROD: <br>• a company's token assigned to two of its nodes reaches both and not a third; unassigned, one stops at once; <br>• a general token reaches every node of its company and none of another; <br>• a suspended or deleted tenant's secrets are not handed out, and come back when it is reactivated; <br>• a person's secret assigned to a node reaches it, and the person's reassignment changes nothing; <br>• the genesis tenant refuses an AI node, and the core still reads its keys after a restart; <br>• a node's tenant cannot change; <br>• Cloud stores, assigns and lists through the public door. |
| 3 | Tenant firm (§2.3). | On PROD: <br>• suspended, a tenant's node does not start or restart and the running one keeps running without secrets; one that dies is not relaunched; reactivated, it is; <br>• deleted with people charged, nobody is marked and they still resolve; a person comes back while it is deleted; restored, it is as it was; <br>• a purge with people charged is refused, and goes once they are charged elsewhere (a temporary included, out of the deleted tenant). |
| 4 | Cloud (§2.4), whatever did not ship with 1–3. | The contract doc checked live. |
| 5 | Tokens (§2.5). | Designed first. |

---

## 4. Decisions

| ID | Decision | State |
|---|---|---|
| D1 | The contract of §0. | Decided 2026-10-14 |
| D2 | A tenant is a spending record; who pays is Cloud's. | Decided 2026-10-14 |
| D3 | A suspended tenant cannot use the infrastructure. | Decided 2026-10-14. Built: the vault (0.1.66) and the orchestrator (0.1.67); the router gate is not built (§2.3) |
| D4 | Cloud deletes and restores human ILKs only. | Decided 2026-10-14 |
| D5 | Order: ILK structure, tenant, Cloud, tokens. | Decided 2026-10-14 |
| D6 | People are reassigned (stage 1). A node is born and dies in its tenant and is never reassigned. | Decided 2026-10-14 |
| D7 | One Cloud account per email (installation-wide scope); no automatic merge of existing duplicates. | Live in 0.1.65 |
| D8 | Deleting a tenant disables it: only the tenant is marked, never a person; restoring it is reactivating it; the purge is refused while people are charged to it. Replaces the 2026-09-28 cascade and stage 1's `TENANT_HAS_ACCOUNTS`. | Decided 2026-10-14, stage 3 (0.1.67) |
| D9 | Secrets (§2.2): the tenant is who pays, and the vault checks only that it exists and is active. An owner (node or person), assigned nodes, or general (every node of its tenant). No groups, no installation-wide key. | Decided 2026-10-14 |
| D10 | Only the core (SY.\*, RT.\*, the core IO runtimes) is born in the genesis tenant; the orchestrator defines the core. | Decided 2026-10-14 |
| D11 | A pending signup is readable as such (`pending_signup`), wherever it is charged. | Decided 2026-10-14 |
| D12 | io.api `by_ilk` acts for the people the tenant knows: charged to it, or holding a channel in its scope. | Decided 2026-10-14, stage 3 (0.1.67) |
| D13 | A contact is charged to the company it wrote to. Joining a contact to an account is an explicit step, never a match on the number. | The first part is how it works; the joining is open, not designed |

## 5. Related

FINDINGS A-61 (0.1.64, the tenant on every vault write), A-62 (any mesh node may call any admin
action: parked); `docs/io-cloud-api.md`; `docs/10-identity-v2.md` (the identity model, to update);
the 2026-09-28 delete model (mark and purge).
