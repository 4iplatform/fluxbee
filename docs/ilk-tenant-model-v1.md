# ILK and tenant — the identity and cost-center model (v1)

**Status:** contract and order of work decided by the operator on 2026-10-14. Stage 1 in progress.
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

**Channel scope.**
- Each channel records the namespace its address lives in: `scope`, fixed when the channel is
  added.
  - IO channels: the tenant of the node that saw the address. That is today's behaviour, now frozen
    on the channel.
  - A Cloud account's email: the installation-wide namespace, which is the root tenant's.
- Keys always use the channel's scope, never the ILK's current tenant:
  - the in-memory lookup;
  - the SHM channel entries and their hash, which already have their own tenant field: no layout
    change;
  - `identity_ichs.tenant_id`, which is read back on load.
- The SHM `IlkEntry.tenant_id` carries the ILK's current tenant: its cost center.
- A channel without a recorded scope (in flight during an upgrade) takes its ILK's tenant, which is
  today's value.

**One Cloud account per email.**
- The `cloud` channel lives in the installation-wide scope.
- `register_human` for an email that already has an account answers that ILK (`existing:true`) and
  creates nothing. Charging it to another tenant is a separate, explicit reassignment.
- An email is evidence here because Cloud authenticated that login. Other channels (Slack, a
  phone) are never joined to a person automatically; linking them needs its own explicit step,
  later.

**Reassignment: `ILK_SET_TENANT`.**
- Callers: SY.admin and SY.architect. Cloud reaches it through IO.cloud.
- Stage 1 covers **human ILKs only**: a node's tenant still feeds its vault pool until stage 2.
- The target must exist, must not be deleted, and must not be the root tenant. People are never
  charged to the root tenant.
- Channels keep their scope. Each reassignment writes a history row (ILK, from, to, when, by whom).
- Replicas receive the new tenant like any ILK update.

**Merges.**
- `merge_from_ilk_id` (ILK_ADD_CHANNEL) gets the check `merge_registration` already has: source and
  target charged to the same tenant.
- Moved channels keep their scope everywhere: memory, DB, SHM.
- Cross-tenant linking waits for the explicit step above.

**Cloud's view of ILKs.**
- **Reads by id are not tenant-filtered:** the tenant is not a boundary, and Cloud has to see a
  person to reassign them.
- **Lists by tenant stay:** they mean "what is charged to T".
- **Cloud deletes and restores human ILKs only** (operator, 2026-10-14). Node identities are the
  orchestrator's. This is an account action, not tenant-scoped.

**One-time migration, explicit and reported.**
- At the first start on the new release, the existing `cloud` channels move to the
  installation-wide scope.
- When one email has accounts in several tenants, none of them moves. The report lists them for the
  operator: nothing is merged automatically.
- The report goes to the identity log and to a drift alert.

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
| 1 | ILK structure (§2.1) and its IO.cloud part. | On PROD, through Cloud's public door: <br>• the same email registered from two tenants gives one ILK; <br>• `set_ilk_tenant` moves the person, and the person's `cloud` and IO channels still resolve; <br>• the person keeps their channels after an identity restart (DB reload); <br>• worker1's identity replica shows the new tenant; <br>• a read by id across tenants works, and Cloud is refused a node's ILK on delete; <br>• the migration report. |
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
| D8 | Deleting a tenant is refused while ILKs are charged to it (replaces the cascade of people). | Proposed, stage 3 |
| D9 | The vault grant model's shape. | Stage 2 |

## 5. Related

FINDINGS A-61 (0.1.64, the tenant on every vault write), A-62 (any mesh node may call any admin
action: parked); `docs/io-cloud-api.md`; `docs/10-identity-v2.md` (the identity model, to update);
the 2026-09-28 delete model (mark and purge).
