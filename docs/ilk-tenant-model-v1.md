# ILK and tenant — the identity and cost-center model (v1)

**Status:** contract and order of work decided by the operator on 2026-10-14. Stage 1 built for 0.1.65.
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
- **Tenant status is not enforced anywhere:** the SHM writes every tenant as active.
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

**Deleting a tenant that has accounts charged to it** is refused (`TENANT_HAS_ACCOUNTS`) until they
are reassigned or deleted. A cascade would mark an account whose email is reserved in every tenant,
and a purge would remove it. This is the first piece of D8.
- **A pending signup** (a temporary person holding an account's key) is nobody's account yet: it
  does not block the delete and it is not marked, because a mark would lock the email out for
  everyone until a purge. It stays pending; the relay's next signup of that email takes it over;
  purging the tenant removes it if it is still charged there. The answer counts them
  (`pending_signups`).
- Other humans charged to it (channel identities, temporaries without an account) still cascade as
  decided on 2026-09-28.

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

### 2.2 Permissions on resources (stage 2: SY.vault)

**Today:** a secret dedicated to an ILK; the root pool for system callers; a tenant's pool by tenant
equality. **The third one goes.**

**Target:**
- explicit grants `ILK → secret → {use, administer}`, plus access groups for sharing among nodes. A
  group stands for access only, never for billing or a company;
- a secret's `tenant_id` becomes metadata: the cost center it is administered under, which Cloud's
  listings use.

**What has to change:**
- the SDK's resolution: dedicated, then granted;
- the change notifications and the credential caches: revoking a grant must stop later use;
- `vault_list`: discovery limited by grant.

**Migration:** every pool secret becomes explicit grants to the ILKs that can read it today. They are
recorded and reported before the tenant rule is removed.

**To decide at stage 2:** the shape of groups, and Cloud's credential operations (whom a token is
for).

### 2.3 The tenant as a cost center (stage 3)

**The record.** `TenantRecord` holds labels and a state Cloud sets: name, domain, status,
settings, sponsor. The sponsor is stored flat and grants nothing.

**The status gate: a suspended tenant cannot use the infrastructure.**
- The SHM carries the real tenant status.
- The orchestrator refuses to start a node charged to a suspended tenant.
- The router refuses traffic from and to ILKs charged to one.
- AI nodes do not spend for one.
- The root tenant is never suspended.

**Deleting a tenant.**
- Refused while ILKs are charged to it: reassign or delete them first.
- This replaces the cascade that marked people (2026-09-28). Node identities already blocked it.

**Other tenant uses.**
- **io.api's `by_ilk` check:** it rests on the tenant, so it moves to a real rule. The candidate is
  the instance's own channels.
- **OPA:** `data.identity[ilk].tenant_id` stays, documented as the cost center. Installed user
  policies must not use it as a permission (PROD has no user policy).
- **The root tenant** is the installation's own cost center: system nodes.
- **Docs:** "every ILK belongs to exactly one tenant" becomes "is charged to".

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
| 2 | Vault grants (§2.2). | The pool migration report, and a node's credentials unchanged after it. The two invariants: sharing a tenant grants nothing, and a different tenant blocks nothing that was granted. |
| 3 | Tenant firm (§2.3). | Suspend a test tenant: its node does not start, its traffic is refused, it spends nothing. Reactivate: everything works again. A tenant delete with ILKs charged is refused. |
| 4 | Cloud (§2.4), whatever did not ship with 1–3. | The contract doc checked live. |
| 5 | Tokens (§2.5). | Designed first. |

---

## 4. Decisions

| ID | Decision | State |
|---|---|---|
| D1 | The contract of §0. | Decided 2026-10-14 |
| D2 | A tenant is a spending record; who pays is Cloud's. | Decided 2026-10-14 |
| D3 | A suspended tenant cannot use the infrastructure. | Decided 2026-10-14 |
| D4 | Cloud deletes and restores human ILKs only. | Decided 2026-10-14 |
| D5 | Order: ILK structure, tenant, Cloud, tokens. | Decided 2026-10-14 |
| D6 | Stage 1 reassigns human ILKs only; nodes with stage 2. | Proposed |
| D7 | One Cloud account per email (installation-wide scope); no automatic merge of existing duplicates. | Proposed |
| D8 | Deleting a tenant is refused while ILKs are charged to it (replaces the cascade of people). | Stage 1 takes the account part (§2.1); the rest is proposed for stage 3 |
| D9 | The vault grant model's shape. | Stage 2 |

## 5. Related

FINDINGS A-61 (0.1.64, the tenant on every vault write), A-62 (any mesh node may call any admin
action: parked); `docs/io-cloud-api.md`; `docs/10-identity-v2.md` (the identity model, to update);
the 2026-09-28 delete model (mark and purge).
