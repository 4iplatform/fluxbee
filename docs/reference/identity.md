# Identity (SY.identity): reference

**Status:** the live contract as built in 0.1.83 (2026-10-22).
**Audience:** developers of IO, AI, SY nodes, the router and Fluxbee Cloud.

This page describes what the code does today. Every statement names the code that backs it.
It takes over the contract parts of `docs/history/10-identity-v2.md`,
`docs/identity-v2.1-agent-definition-addendum.md` and `docs/ilk-tenant-model-v1.md` §0–2.
Those documents keep the history and the operator's decisions; where they disagree with this
page, the code and this page win.

The main source is `src/bin/sy_identity.rs` (store, handlers, database, sync); a function or type
named without a path lives there. The SHM is in `src/shm/mod.rs` and its reader in
`crates/fluxbee_sdk/src/identity.rs`.

---

## 1. The model in short

- **ILK** (`ilk:<uuid>`): one actor. A person, a node, a system service.
- **Tenant** (`tnt:<uuid>`): a cost center, a record of who pays. Every ILK is charged to exactly
  one tenant (`IlkRecord.tenant_id`). A tenant is never a company inside Fluxbee; Cloud maps
  companies to tenants.
- **ICH** (`ich:<uuid>`): a channel an ILK is reached on: a type and an address (a phone, a
  Slack user id, an email). It lives inside the ILK record (`IlkRecord.channels`).
- **Alias**: an old ILK id that points to the ILK it was merged into, for a limited time.

Ids are always prefixed strings in messages and JSON, in one spelling: the prefix, a colon, and
the uuid lowercase and hyphenated, with nothing around it. `parse_prefixed_uuid` refuses anything
else with `INVALID_REQUEST`, whatever field carries it, and a replica's push with such an id is
refused (§5.4). The store keys records by the string, while PostgreSQL stores `UUID` columns, so
another spelling (uppercase, no hyphens, braces, spaces) would name a second record in memory and
an existing row in the database. The SHM stores the raw 16 bytes.

Where identity lives:

- **Primary:** `SY.identity@motherbee`. It owns the PostgreSQL database `fluxbee_identity` and
  takes every write (§5).
- **Replicas:** `SY.identity@<worker>`. No database. They copy the primary over TCP port 9100 and
  answer reads (§5).
- **SHM:** every SY.identity writes `/jsr-identity-<hive>` on its own hive. Local nodes read it
  without a round trip (§6).

---

## 2. ILK

### 2.1 The record

`IlkRecord` in `src/bin/sy_identity.rs`:

| Field | Meaning |
| --- | --- |
| `ilk_id` | `ilk:<uuid>`. Never changes. |
| `ilk_type` | `human`, `agent` or `system` (§2.2). |
| `registration_status` | `temporary` or `complete` (§2.3). |
| `tenant_id` | The tenant it is charged to. |
| `identification` | Free JSON. The keys the code reads: `node_name`, `email` (an email the channel vouches for, §2.7), `display_name`. A person's `claimed_email`, an email they only typed, is stored and returned but never read as a key. |
| `definition` | An agent's cognitive definition (§4). `{}` for any other type. |
| `channels` | Its ICHs (`ChannelRecord`, §2.5). |
| `deleted_at_ms`, `deleted_reason` | The delete mark (§2.8). The only reason written is `direct`. |

Two identification keys are unique, and the database enforces both
(`ensure_primary_schema`):

- `node_name`: unique among unmarked ILKs, across the whole installation
  (`idx_identity_ilks_node_name`).
- `email`: unique per tenant among unmarked ILKs (`idx_identity_ilks_email` on
  `(email, tenant_id)`).

`claimed_email` is neither: it has no column and no index, and lives only in the
`identification` JSONB (`upsert_ilk_rows`).

### 2.2 Types

| Type | Who creates it | How |
| --- | --- | --- |
| `system` | SY.identity itself, at boot | One per `SY.*` entry of `hive.yaml` `system_nodes.<role>.nodes` (`ensure_system_ilks_from_hive`). |
| `agent` | SY.orchestrator | Every node it launches: AI, IO, WF and managed RT nodes (`derive_ilk_type_for_node` always answers `agent`). |
| `agent` | An IO node | `ILK_PROVISION` with `ilk_type: "agent"`, when the counterpart is an agent. |
| `human` | An IO node, then SY.frontdesk.gov | `ILK_PROVISION` creates it (the default type); `ILK_REGISTER` completes it (§2.6). |

Rules:

- `ILK_REGISTER` and `ILK_PROVISION` accept only `human` and `agent` (`validate_ilk_type`,
  `validate_provision_ilk_type`). Only SY.identity mints `system`.
- A system ILK's id is deterministic: `deterministic_system_ilk_id` in
  `crates/fluxbee_sdk/src/identity.rs` hashes `"fluxbee:identity:system-ilk:v1:" + node_name`
  with SHA-256 and shapes the first 16 bytes as a UUID. SY nodes compute their own ILK this way
  at boot; they never wait for the SHM.
- A system ILK is charged to the root tenant, is `complete`, and carries
  `identification.source = "hive.system_nodes"` (`is_well_known_system_ilk`).
- A system ILK cannot be registered over (`SYSTEM_ILK_PROTECTED` in `register_ilk`), marked or
  purged (`check_ilk_markable`).
- In the SHM the type is a byte: 0 human, 1 agent, 2 system (`SHM_ILK_TYPE_*`). An unknown
  string is written as `human`, the least privileged (`parse_ilk_type_for_shm`). The router shows
  an unknown byte to OPA as `"unknown"` (`inject_identity_data` in `src/router/mod.rs`).

### 2.3 Registration status

- `temporary`: created by `ILK_PROVISION` (`provision_temporary_ilk`). The person wrote in, and
  nobody has registered them yet.
- `complete`: set by `ILK_REGISTER` (`register_ilk`), and by the seeding of system ILKs.
- The SHM also has a code for `partial` (`SHM_REG_STATUS_PARTIAL`), but nothing in
  `sy_identity.rs` writes that value.

The router reads this status: a message to `Destination::Resolve` whose `src_ilk` is missing or
`temporary` goes to the frontdesk (§2.6, step 3).

### 2.4 Who owns which node identity

- **SY nodes:** the deterministic system ILK (§2.2).
- **Every node the orchestrator launches:** `ensure_node_identity_registered` in
  `src/bin/sy_orchestrator.rs` sends `ILK_REGISTER` to `SY.identity@motherbee` with
  `identification.node_name`, `display_name`, `runtime` and `runtime_version`, and the tenant
  from the request (`payload.tenant_id`, then `payload.config.tenant_id`, then
  `ORCH_DEFAULT_TENANT_ID`). A node name that already has an active ILK keeps it
  (`register_ilk` looks the name up first).
- An orchestrator registers, updates, marks and purges only its own hive's nodes (§7.1). A launch
  on another hive is forwarded to that hive's orchestrator, which registers the node itself.
- The orchestrator injects the result into the node's unit as `FLUXBEE_NODE_ILK_ID` and
  `FLUXBEE_NODE_TENANT_ID` (`ENV_SELF_ILK_ID`, `ENV_SELF_TENANT_ID` in the SDK).
- If the launch fails after a new registration, the orchestrator marks and purges that ILK
  (`delete_node_ilk_for_spawn_rollback`).
- The SHM writes `identification.node_name` as the ILK's `handler_node` (`ilk_entry_from_record`).
  That is how SY.cognition, SY.storage and SY.admin find a node's ILK from its name
  (`find_ilk_by_handler_node_from_hive_id` and `_from_hive_config` in the SDK).

### 2.5 Channels (ICH)

`ChannelRecord`: `ich_id`, `channel_type`, `address`, `owner_l2_name`, `enabled`, `scope`.

**The lookup key** is `(channel_type, address, scope)`, trimmed and lowercased (`channel_key`,
`canonical_ich_key`). It never uses the ILK's current tenant, so charging an ILK to another
tenant moves no key.

**The scope** is the namespace the address lives in. It is fixed when the channel is created
(`resolve_channel_scope`):

- `ILK_PROVISION`: the request's `scope`, else the request's tenant.
- `ILK_ADD_CHANNEL`: the channel's `scope`, else the target ILK's tenant.
- `ILK_UPDATE` (the orchestrator adding a node's own channels): always the ILK's tenant; another
  scope answers `INVALID_SCOPE`.
- The **installation-wide scope** is the root tenant's id (`INSTALLATION_SCOPE`). Only the Cloud
  relay (`IO.cloud@<primary hive>`) may name it, and only for channel type `cloud`.
- A `cloud` channel from any other caller answers `CLOUD_CHANNEL_RESERVED`. `ILK_UPDATE` never
  takes one.
- Any other requested scope answers `INVALID_SCOPE`.
- A record from an older peer without a scope takes its ILK's tenant once, on arrival
  (`fill_channel_scopes`).

**Other rules:**

- A channel key names one ILK, and nothing takes it over (`check_channel_free`). `ILK_UPDATE`,
  `ILK_ADD_CHANNEL` and a replica's push answer `ICH_CONFLICT` for a key another active ILK holds
  (in any letter case), and `ILK_DELETED` for one a marked ILK keeps reserved. The ILKs a request
  joins may hold it already: its target, and the temporary an `ILK_ADD_CHANNEL` merges. Provisioning
  answers the ILK that holds the key, as always. A node respawned after its ILK was purged takes its
  channels again.
- A new channel's `ich_id` must be nobody else's (`check_ich_id_free`, `ICH_ID_CONFLICT`). IO
  nodes derive it from the channel type, the address and the scope (the tenant when no scope is
  named) with `stable_ich_id` (`crates/fluxbee_sdk/src/identity.rs`,
  `nodes/io/common/src/provision.rs`).
- A new channel starts with `enabled: false`.
- `ICH_SET_ENABLED` turns it on or off. An IO caller may only change a channel whose
  `owner_l2_name` is itself (`authorize_ich_enabled_mutation`, `UNAUTHORIZED` otherwise).
- In the SHM, each channel entry carries its scope in its `tenant_id` field
  (`ich_entries_from_ilk_record`). The ILK entry carries the tenant it is charged to.

### 2.6 How a new person arrives

1. **An IO node sees an unknown sender.** Its shared inbound path (`nodes/io/common/src/inbound.rs`)
   charges the sender to the tenant that launched the node (`InboundConfig::self_tenant_id`). It
   looks `(channel, address, tenant)` up in its hive's identity SHM first
   (`resolve_ilk_from_hive_id`, `nodes/io/common/src/identity.rs`). On a miss it sends
   `ILK_PROVISION` to `SY.identity@motherbee` (the default target in
   `nodes/io/common/src/provision.rs`).
2. **SY.identity provisions** (`provision_temporary_ilk`):
   - The key exists: it answers that ILK with `created: false`.
   - A marked ILK holds the key: `ILK_DELETED`. Nothing new is minted.
   - Otherwise: a new `temporary` ILK, `human` unless the request says `agent`, charged to the
     request's tenant. With no tenant in the request it is the root tenant.
   - A marked tenant answers `TENANT_DELETED`. A pending or suspended tenant is not refused here.
   - The answer is `{ilk_id, registration_status, tenant_id, created}`.
3. **The router sends the person to the frontdesk.** On `Destination::Resolve`,
   `resolve_target_with_identity` (`src/router/mod.rs`) forces the frontdesk (`hive.yaml`
   `government.identity_frontdesk`, read in `src/config/mod.rs`) when the message has no
   `src_ilk`, or when its `src_ilk` (after alias resolution) is `temporary`. The decision is the
   system route policy (`route_to_frontdesk` in `src/router/system_policy.rs`).
4. **SY.frontdesk.gov registers the person** with `ILK_REGISTER`
   (`nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs`):
   - The tenant is the case's: the one SY.identity holds for the ILK, read from the identity SHM
     (`identity_shm_ilk_tenant`, `case_tenant_of`). The frontdesk never creates a tenant.
   - Two methods. A structured `frontdesk_handoff` runs deterministically, with no model call. A
     person chatting is the conversational method, driven by the frontdesk's LLM.
   - A handoff is taken only from an IO node: the router-stamped `src_l2_name` must start with
     `IO.` (`frontdesk_handoff_sender_allowed` in `nodes/gov/common/src/frontdesk_contract.rs`).
   - Its `subject.email` is vouched (the IO node answers for it) and goes in
     `identification.email`. An email the person typed in the conversation is a claim and goes in
     `identification.claimed_email`, also when a later handoff brings no email
     (`person_identification`).
   - A case of the root tenant is answered at once: nobody registers there
     (`TENANT_ROOT_NOT_REGISTRABLE`), and the ILK stays temporary.
5. **SY.identity registers** (`register_ilk`):
   - The tenant must exist and not be marked (`INVALID_TENANT`, `TENANT_DELETED`).
   - It must be active (`TENANT_PENDING`, `TENANT_SUSPENDED`).
   - No person registers into the root tenant (`TENANT_ROOT_NOT_REGISTRABLE`). Node ILKs
     (`agent`) still do.
   - A registration never changes an existing ILK's tenant (`INVALID_TENANT_TRANSITION`).
   - A `node_name` or an email that a marked ILK holds answers `ILK_DELETED`.
   - Registering an existing ILK replaces its `identification` whole and sets it `complete`.
   - Only a person's registration carries an email: a non-`human` one with `email` or
     `claimed_email` answers `INVALID_IDENTIFICATION`, so a node can neither hold an address nor
     learn which addresses are taken.
   - An `identification.email` another person of the same tenant already holds triggers a merge
     (§2.7). A `claimed_email` never does: the registration completes, with the same answer
     whether or not someone holds that email.

**Fluxbee Cloud's `register_human`** (`handle_register_human` in `nodes/io/io-cloud/src/main.rs`)
runs the same steps without a conversation:

1. IO.cloud provisions the person's `cloud` channel (the email) in the installation-wide scope,
   charged to the requesting tenant (`provision_ilk_in_scope`). One email is one account.
2. A complete account that already holds the email answers `existing: true`, and nothing is
   created (`existing_account_reply`).
3. A temporary from an abandoned signup in another tenant is charged to the requesting tenant,
   with a history row (`provision_temporary_ilk`, `TenantChange`).
4. IO.cloud unicasts the handoff to `SY.frontdesk.gov@motherbee` (`DEFAULT_FRONTDESK_TARGET` in
   `nodes/io/common/src/frontdesk_gate.rs`) and relays its structured verdict.

A `cloud` channel held by a node answers `CLOUD_CHANNEL_RESERVED`. An email the scope migration
left ambiguous answers `ACCOUNT_AMBIGUOUS` (`ambiguous_account_emails`, `report_account_scopes`).

### 2.7 Merge by email

Only an email the channel vouches for joins two ILKs, and only within one tenant
([control-path hardening](../control-path-hardening-v1.md) §7, A-42).

- **Vouched or claimed.** A vouched email is `identification.email`: the IO node's handoff
  (io.api, whose integrator holds the bearer; IO.cloud, for the login Fluxbee Cloud
  authenticated). An email a person typed is `identification.claimed_email`. A claim is stored and
  shown, never indexed, looked up or merged: a typed email can neither take over a registered
  person nor reserve an address for a later merge. If the claimant is the same person, joining
  the two ILKs is the operator's explicit step; SY.admin has no merge action yet.
- **`ILK_REGISTER` with a known email** (`merge_registration`): when another active `human` ILK
  of the same tenant holds the email, the temporary being registered merges into it. Conditions:
  - the request says `human`;
  - the source is `temporary` and charged to the same tenant;
  - the holder is `human`.

  The holder only gains the identification fields it had empty (`fill_missing_identification`).
  Any other case answers `DUPLICATE_EMAIL` and changes nothing. The answer carries
  `merged: true` and `merged_from_ilk_id`.
- **`ILK_ADD_CHANNEL` with `merge_from_ilk_id`** (`add_channel`): the same merge, asked
  explicitly by an IO node or the frontdesk. A tenant's IO node cannot merge away a temporary
  that holds a `cloud` channel (`CLOUD_CHANNEL_RESERVED`).
- **What a merge does** (`merge_temporary_ilk`):
  - The source's channels move to the target, and their lookups follow.
  - The source stays as an alias of the target for `identity.merge_alias_ttl_secs` (default
    3600 s, `DEFAULT_MERGE_ALIAS_TTL_SECS`).
  - Every 30 s the alias GC (`run_alias_gc`) drops expired aliases and marks their source
    deleted, if it is still temporary (`gc_expired_aliases`).
- `check_merge_source` refuses a source and a target charged to different tenants
  (`ILK_TENANT_MISMATCH`). Across tenants nothing joins: the same person in two tenants stays two
  ILKs.
- An alias resolves in reads (`get_ilk_payload` answers `alias_resolved`) and in the router,
  which rewrites a message's `src_ilk` to the canonical ILK (`apply_identity_pre_resolve`).

### 2.8 Delete, restore, purge

Delete is a reversible mark. Purge is a separate, final step.

**`ILK_DELETE`** (`delete_ilk`): marks the ILK (`deleted_at_ms`, reason `direct`).

- The ILK leaves the lookup and the SHM at once (`mark_ilk_unchecked`; `sync_identity_shm_mappings`
  skips marked ILKs).
- Its rows stay, so its keys stay reserved. A channel answers `ILK_DELETED` on provision and add;
  a `node_name` or email answers it on register.
- An ILK already marked answers `ok` with `already_deleted: true`.
- System ILKs answer `SYSTEM_ILK_PROTECTED`.

**`ILK_RESTORE`** (`restore_ilk`): removes the mark.

- A person comes back even while their tenant is marked. Any other ILK waits for its tenant
  (`TENANT_DELETED`).
- A key an active ILK holds now answers `ILK_KEY_CONFLICT` (`check_ilk_restorable`).
- A restored person whose `cloud` channel stayed in an old scope moves it to the
  installation-wide scope when that key is free (`move_account_to_installation_scope`).

**`ILK_PURGE`** (`purge_ilk`): only a marked ILK (`ILK_NOT_DELETED` otherwise). It removes the
ILK, its lookups and every alias that names it (`remove_ilk_physically`).

All three take two optional guards (`IlkDeleteRequest`, `check_ilk_tenant_scope`):

- `tenant_id`: the ILK must be charged there (`ILK_TENANT_MISMATCH`);
- `ilk_type`: the ILK must be that type (`ILK_TYPE_MISMATCH`).

Every lifecycle op validates before it changes anything. `run_lifecycle_op` snapshots the
store, writes the changes in one database transaction (`persist_lifecycle_in_db`), and restores
the snapshot on any error.

---

## 3. Tenant

### 3.1 What a tenant is

A tenant is a cost center. It records who pays for what a node spends; it is not a company and
not a permission boundary. Reads by id are not tenant-filtered for Cloud
(`enforce_cloud_relay_content`: `get_ilk` has no tenant rule), and a person is charged to another
tenant explicitly (§3.5). What a tenant gates is spending: its status decides whether its nodes
get secrets and start (§3.4). LLM usage is recorded per tenant (`llm_usage`, `docs/reference/storage.md`).

### 3.2 The record

`TenantRecord`: `tenant_id`, `name`, `domain`, `status`, `settings`, `sponsor_tenant_id`,
`deleted_at_ms`.

- **Status:** `pending`, `active` or `suspended`. `create_tenant` and `update_tenant` refuse any
  other value with `INVALID_REQUEST`.
- **Default status:** `TNT_CREATE` without a status creates `pending`. IO.cloud sends `active`
  unless Cloud asks otherwise (`translate_cloud_op`, `create_tenant`).
- **Create is idempotent** (`create_tenant`, `find_tenant_by_hint`): a tenant with the same
  domain, then the same name (trimmed, lowercased), is returned with `created: false` and
  `matched_by`. A marked one answers `TENANT_DELETED`.
- **Update** (`update_tenant`) refuses a name or domain another tenant holds, marked or not
  (`TENANT_NAME_TAKEN`, `TENANT_DOMAIN_TAKEN`). Every field is checked before anything changes.
- **`TNT_APPROVE`** sets a tenant `active` (`approve_tenant`). SY.identity accepts it from
  `SY.admin@`, but no node in the repository sends it. Tenants become active through create or
  `update_tenant`.
- `settings.max_ilks` is copied into the SHM tenant entry (`tenant_entry_from_record`). Nothing in
  SY.identity enforces it.

### 3.3 The root tenant

- Fixed id `tnt:00000000-0000-0000-0000-000000000001`, name `fluxbee`
  (`DEFAULT_ROOT_TENANT_ID`, `DEFAULT_DEFAULT_TENANT_NAME`).
- SY.identity rewrites it at every boot: active, no domain, no settings, no sponsor
  (`ensure_default_root_tenant`).
- `TNT_UPDATE`, `TNT_DELETE`, `TNT_RESTORE` and `TNT_PURGE` on it answer `TENANT_ROOT_PROTECTED`
  (`update_tenant`, `tenant_record_for_lifecycle`).
- No person registers into it (§2.6). `ILK_SET_TENANT` cannot target it (`TENANT_ROOT_PROTECTED`).
- **Only the core runs there.** The orchestrator refuses any other node in the root tenant with
  `TENANT_ROOT_NOT_ALLOWED` (`genesis_refusal`, `is_core_node` in `src/bin/sy_orchestrator.rs`).
  The core is:
  - `SY.*` and `RT.*` names;
  - `IO.*` nodes on the runtimes `io.cloud`, `io.blob` and `io.web` (`CORE_IO_RUNTIMES`).
- SY.vault treats it as always active (`ensure_tenant_active`).

### 3.4 What a tenant that is not active cannot do

"Not active" is pending, suspended, or marked deleted. A marked tenant is left out of the SHM
(`sync_identity_shm_mappings`).

- **No secrets.** SY.vault hands out a secret only while its tenant is active in the identity SHM
  (`ensure_tenant_active`, `tenant_check` in `src/bin/sy_vault.rs`):
  - pending or suspended: `UNAUTHORIZED`;
  - not in the SHM (deleted, or identity not loaded yet): `IDENTITY_UNAVAILABLE`, which is
    retryable.
- **No new nodes.** The orchestrator refuses `run_node`, start and restart with
  `TENANT_NOT_ACTIVE` (`tenant_gate`, `tenant_gate_refusal`). The boot and periodic relaunches
  skip the tenant's nodes.
  - When the hive cannot read the status (identity not up, tenant not synced), the launch goes on
    and the orchestrator logs it.
  - A node that already runs is not stopped.
- **No registrations.** `ILK_REGISTER` answers `TENANT_PENDING`, `TENANT_SUSPENDED` or
  `TENANT_DELETED` (§2.6).
- **No new charges.** `ILK_SET_TENANT` refuses a target that is not active (`TENANT_NOT_ACTIVE`)
  or is marked (`TENANT_DELETED`).

### 3.5 Charging a person to another tenant: `ILK_SET_TENANT`

`set_ilk_tenant` in `src/bin/sy_identity.rs`. It runs on the primary; the callers are `SY.admin@`
and `SY.architect@` of the same hive, and Cloud through SY.admin.

- Only an unmarked `human` ILK moves (`ILK_TYPE_MISMATCH` otherwise). A node is born and dies
  in its tenant.
- **The target** must exist, be active, not be marked, and not be the root tenant.
- **A temporary** moves only out of a marked tenant (`INVALID_TENANT_TRANSITION`). That is how a
  marked tenant is emptied before its purge.
- **An email** another ILK holds in the target, marked or not, refuses the move
  (`DUPLICATE_EMAIL`).
- **Channels keep their scope.** No lookup key moves.
- **History:** one row in `identity_ilk_tenant_history` (ILK, from, to, `changed_by`), in the
  same transaction (`commit_ilk_changes_with_history`). `changed_by` is the router-stamped caller,
  plus `for <on_behalf_of>` when SY.admin relays one.
- Already charged there: `changed: false`, and nothing is written.

### 3.6 Delete, restore, purge of a tenant

**`TNT_DELETE`** (`delete_tenant`) marks the tenant. It marks no ILK.

- Refused while it sponsors unmarked tenants (`TENANT_HAS_CHILDREN`).
- Refused while node identities remain in it (`TENANT_HAS_NODES`, from `tenant_node_ilks`: system
  ILKs or any ILK with a `node_name`).
- Its people stay usable and can be charged elsewhere. The answer counts them
  (`people_charged`).
- Repeating it answers `already_deleted: true`.

**`TNT_RESTORE`** (`restore_tenant`) removes the mark. Its sponsor must not be marked
(`TENANT_DELETED`). Nobody in it was touched, so it comes back as it was.

**`TNT_PURGE`** (`purge_tenant`) is final, and only for a marked tenant (`TENANT_NOT_DELETED`).
It is refused while:

- it sponsors any tenant, marked or not (`TENANT_HAS_CHILDREN`);
- node identities remain (`TENANT_HAS_NODES`);
- people are charged to it (`TENANT_HAS_PEOPLE`, the first 20 listed). People are charged
  elsewhere first, never removed with it. A temporary merged into another ILK does not count
  (`tenant_people` skips aliases);
- SY.vault holds secrets of the tenant (`TENANT_HAS_SECRETS`). SY.admin checks this before it
  forwards the purge, and refuses when the vault cannot be listed (`VAULT_CHECK_FAILED`,
  `purge_tenant_vault_block` in `src/bin/sy_admin.rs`).

When it goes through, it removes every ILK charged to the tenant (people already marked on their
own included), and drops the channels other ILKs still hold in its scope
(`drop_channels_in_scope`, `dropped_channels` in the answer).

### 3.7 Sponsor

- One optional sponsor per tenant (`sponsor_tenant_id`).
- `validate_sponsor_assignment` refuses itself and cycles (`INVALID_SPONSOR_RELATION`), and an
  unknown sponsor (`INVALID_SPONSOR_TENANT`). A marked sponsor answers `TENANT_DELETED`.
- `TNT_SET_SPONSOR` sets or clears it (`null` clears); `TNT_UPDATE` takes it too.
- SY.identity uses it for counts (`child_count`, `is_sponsor`) and for the delete and purge
  guards. SY.vault and SY.orchestrator do not read it.

---

## 4. An agent's definition

An `agent` ILK may carry a cognitive definition: hashes of assets, not the assets.

**`ILK_SET_DEFINITION`** (`set_ilk_definition`, `normalize_agent_definition`):

- Only on an unmarked `agent` ILK (`INVALID_ILK_TYPE`, `ILK_NOT_FOUND`).
- `definition` takes four keys only; any other answers `INVALID_DEFINITION`:
  - `role_hash`: one hash, optional;
  - `skill_hashes`: at most 16 (`IDENTITY_DEFINITION_MAX_SKILLS`);
  - `handbook_hashes`: at most 8 (`IDENTITY_DEFINITION_MAX_HANDBOOKS`);
  - `personality_hash`: one hash, optional.
- Each hash is 64 hex characters, stored lowercase (`INVALID_DEFINITION_HASH`). Too many
  answers `DEFINITION_TOO_LARGE`.
- A definition with nothing in it clears the definition (`{}`).
- Callers: `SY.admin@` and `SY.architect@` of the same hive, and `SY.identity@<hive>` itself.
- The operator's route: `POST /hives/{hive}/identity/ilks/{ilk_id}/definition`.
- SY.identity never checks that an asset exists.

**Where the hashes go:**

- The SHM `IlkEntry` carries `role_hash`, `skill_hashes` with `skill_count`, `handbook_hashes`
  with `handbook_count`, and `personality_hash` (`apply_definition_to_ilk_entry`).
- The router exposes them to OPA under `data.identity[<ilk>]` (`inject_identity_data`).

**The assets** are JSON files named by their hash, in `blob://agent-assets/<hash>.json`:

- SY.architect writes, lists, reads and deletes them (its tools and `/api/agent-assets` in
  `src/bin/sy_architect.rs`). Writing one never applies it to an ILK.
- ai.generic finds its ILK in the identity SHM by its node name
  (`find_ilk_by_handler_node_from_hive_config`). It reads each asset from the local disk:
  `<blob_root>/agent-assets/<hash>.json` (`load_cognitive_asset`,
  `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`).

**Known gap: FINDINGS A-72.** `blob/agent-assets` is not replicated to the workers; only
`blob/active` is. An agent with a definition on a worker cannot load its assets. Today an agent
with a role only works on the motherbee.

---

## 5. Primary and replicas

### 5.1 Roles

`main` reads `hive.yaml`. `role: motherbee` is the primary and requires `hive_id: motherbee`
(`PRIMARY_HIVE_ID`); `role: worker` is a replica and may not use that id. Any other role exits.

### 5.2 The primary's database

- **The URL:** from SY.vault, resource type `postgres` in the root tenant's pool
  (`resolve_database_url`, which waits for the vault up to `VAULT_BOOT_WAIT`, 60 s).
  The secret carries host and credentials. SY.identity adds the database name
  `fluxbee_identity` (`IDENTITY_DB_NAME`) and creates the database if it is missing.
- **The schema** (`ensure_primary_schema`): numbered migrations under a PostgreSQL advisory lock,
  recorded in `identity_schema_migrations`:
  - v1, baseline: `identity_tenants`, `identity_ilks`, `identity_ilk_aliases`, `identity_ichs`,
    `identity_vocabulary`;
  - v2: the lifecycle mark;
  - v3: the channel scope and `identity_ilk_tenant_history`;
  - v4: un-marks the people an old tenant delete had marked.
- `identity_ichs.tenant_id` stores the channel's scope; `UNIQUE(channel_type, address,
  tenant_id)` makes the key unique. `identity_vocabulary` is created, and nothing writes to it.
- **At boot** the primary loads the whole store from the database
  (`load_identity_store_from_db`), re-seeds the root tenant and the system ILKs, and logs the
  ambiguous account emails (`report_account_scopes`).
- **Without a database** the primary still starts. Every write then answers `DB_NOT_READY`.
- **A new secret:** on a `VAULT_SECRET_CHANGED` for `postgres` from its own hive's SY.vault, the
  primary exits with code 0 and systemd starts it again (`handle_vault_secret_changed`).
- **Every write** goes to the database before the answer: the store is snapshotted, the change is
  written in one transaction, and a database error restores the snapshot and answers the error
  (`commit_ilk_changes`, `run_lifecycle_op`).

### 5.3 Writes go to the primary

`action_requires_primary` lists every write:

- the ILK writes: `ILK_PROVISION`, `ILK_REGISTER`, `ILK_SET_TENANT`, `ILK_ADD_CHANNEL`,
  `ILK_UPDATE`, `ILK_SET_DEFINITION`;
- the channel switch `ICH_SET_ENABLED`;
- the tenant writes: `TNT_CREATE`, `TNT_UPDATE`, `TNT_SET_SPONSOR`, `TNT_APPROVE`;
- the lifecycle: delete, restore and purge, of ILKs and of tenants.

A replica answers them with `NOT_PRIMARY`; it does not forward them. The callers target
`SY.identity@motherbee` directly (`IdentityProvisionConfig` in `nodes/io/common/src/provision.rs`;
`resolve_identity_primary_hive_id` in the orchestrator). A replica answers the reads
(`ILK_LIST`, `ILK_GET`, `TNT_LIST`, `TNT_GET`) from its copy.

### 5.4 Sync on port 9100

TCP, one JSON line per frame, port `identity.sync.port` (default 9100,
`DEFAULT_IDENTITY_SYNC_PORT`). The primary listens on `0.0.0.0:<port>`. A replica connects to
`identity.sync.upstream`, which `add_hive` writes into the worker's `hive.yaml` with
`auth: required` (`src/bin/sy_orchestrator.rs`).

**Primary → replicas:**

- **Full sync:** the whole store in chunks of 256 records (`build_full_sync_chunks`,
  `IDENTITY_FULL_SYNC_CHUNK_ITEMS`). A replica takes one at boot (`fetch_full_sync_from_primary`)
  and one at the start of every delta subscription.
- **Deltas** (`IdentityDelta`, tagged by `type` in snake case): `tenant_upsert`, `tenant_delete`,
  `ilk_upsert`, `ilk_delete`, `alias_upsert`, `alias_delete`.
  - Each delta has a sequence number, and the replica acks it.
  - A gap makes the replica drop the stream and resubscribe from a full snapshot
    (`read_delta_stream`).
  - The primary registers every waiting subscriber before it builds a snapshot, so no delta falls
    between the snapshot and the stream.
- **Liveness:** the primary writes a heartbeat after 20 s without a frame
  (`IDENTITY_SYNC_HEARTBEAT_SECS`). A replica that hears nothing for 60 s resubscribes
  (`IDENTITY_SYNC_READ_IDLE_SECS`).
- **Bounds:**
  - 64 concurrent sync connections (`MAX_CONCURRENT_SYNC_CONNS`);
  - 256 subscribers, each with a queue of 1024 (`MAX_DELTA_SUBSCRIBERS`,
    `IDENTITY_SUBSCRIBER_CHANNEL_CAP`); a slow subscriber is dropped and resyncs;
  - 16 MiB per frame (`MAX_SYNC_LINE_BYTES`).

**Replica → primary (publish):** a replica pushes the ILKs of its own hive, so the primary
converges to the union of the mesh.

- Ownership is the `@<hive>` suffix of `identification.node_name` (`ilk_owning_hive`).
- It pushes its local deltas (`IDENTITY_DELTA_PUBLISH`), and every 30 s the full set of its own
  ILKs (`IDENTITY_PUBLISH_RECONCILE_SECS`). It holds the set until it has a baseline from the
  primary (`replica_has_baseline`).
- The primary applies a delta only if the replica owns the ILK (`delta_authorized_for_hive`).
  - A replica may claim `system` only for a deterministic `SY.*` ILK of its own hive
    (`ingest_ilk_type_authorized`).
  - It may not change an ILK's tenant, nor push a `cloud` channel, another scope, a channel
    another ILK holds or keeps reserved, or any id in another spelling (`replica_push_acceptable`,
    §2.5, §1). An `IlkDelete` or alias delta with such an id is refused too.
  - Tenant deltas from a replica are refused.
  - A publish that claims the primary's own hive is refused.
- A snapshot reconciles that hive's ILKs to exactly the pushed set (`reconcile_hive_ilks`). Well-known
  system ILKs are never removed this way.
- The primary persists the ILK changes it applied, updates its SHM, and broadcasts them to its
  delta subscribers.

**Authentication: a per-hive HMAC.**

- Each hive's key is `/var/lib/fluxbee/identity/keys/<hive>.key` (`KEYS_DIR`, `key_path` in
  `src/mesh_hmac.rs`).
- The handshake is a mutual challenge and response (`server_auth_handshake`,
  `client_auth_handshake`).
- `identity.sync.auth` is fail-closed (`auth_mode_required`). Absent, empty or unknown means
  `required`. Only `disabled`, `off` or `none` turn it off, with a warning.
- A replica that requires auth and has no key file refuses to start.

---

## 6. The identity SHM

- **Name:** `/jsr-identity-<hive>` (`identity_shm_name`); magic `JSID`, version 6
  (`IDENTITY_MAGIC`, `IDENTITY_VERSION` in `src/shm/mod.rs`).
- **One writer:** the SY.identity of that hive (`IdentityRegionWriter`). The primary writes the
  motherbee's region; each replica writes its own hive's region from what it synced.
- **What it holds** (`sync_identity_shm_mappings`):
  - tenants not marked, with their real status;
  - ILKs not marked, with tenant, type, status, display name, `handler_node` and definition
    hashes;
  - channels with their scope, and the `(type, address, scope)` mappings;
  - aliases.
- **Sizes** (`identity_region_limits`), from `hive.yaml` `identity`:
  - `max_ilks` 8192 (`DEFAULT_IDENTITY_SHM_MAX_ILKS`), `max_tenants` 1024, `max_vocabulary` 4096;
  - `max_ilk_aliases` defaults to `max_ilks`;
  - channels are 4 × `max_ilks`, mappings 2 × channels (`IdentityRegionLimits`);
  - when the limits change, SY.identity creates the region again at its next start, since readers
    lay it out from the limits in its header; limits below what the store holds stop the start
    instead ([shared-memory.md](shared-memory.md) §7).
- **Updates:**
  - each write applies its deltas incrementally (`apply_identity_shm_deltas`); a lifecycle change
    rebuilds the whole region;
  - an SHM failure is logged and does not block the answer nor the replication;
  - the heartbeat is refreshed every 5 s. The header `seq` changes on every write; SY.cognition
    uses it to know when its cached agent ILKs are stale (`identity_shm_seq_from_hive_id`).
- **On the motherbee the SHM is written before the answer.** A node that receives
  `ILK_PROVISION_RESPONSE` can read the new ILK at once.

**Readers:**

| Reader | What for |
| --- | --- |
| Router (`src/router/mod.rs`) | `data.identity` for OPA (`inject_identity_data`); the frontdesk pre-resolve and alias rewrite (`resolve_target_with_identity`). |
| SY.vault | A tenant's status before it hands out a secret (`ensure_tenant_active`). |
| SY.orchestrator | A node's tenant status before a launch (`tenant_gate`); a node's ILK by name. |
| SY.cognition | An agent node's ILK and tenant (`agent_identity_of`). |
| SY.storage | The tenant a usage record is charged to (`identity_tenant`, the motherbee's region). |
| SY.frontdesk.gov | The tenant of a case (`identity_shm_ilk_tenant`). |
| SY.admin | A caller's ILK by name; channel owners (`list_ich_options_from_hive_config`). |
| IO nodes | A sender's ILK before provisioning (`resolve_ilk_from_hive_id`). |
| IO.cloud | Its fast `get_ilk` and `list_ilks` reads (`CLOUD_LOCAL_OPS` in `crates/fluxbee_sdk/src/cloud.rs`). |
| ai.generic | Its own ILK and definition hashes. |
| Go SDK | `go/fluxbee-go-sdk/identity_shm.go`. |

---

## 7. Surfaces

### 7.1 Protocol messages and who may send them

All are system messages to `SY.identity@<hive>`. SY.identity checks the router-stamped
`src_l2_name` (`IdentityRuntime::is_authorized`); a refused caller gets
`UNAUTHORIZED_REGISTRAR`. Prefixes marked "same hive" must come from SY.identity's own hive
(`SAME_HIVE_ONLY_PREFIXES`: `SY.admin@`, `SY.architect@`, `SY.frontdesk.gov@`).

| Message | Allowed callers |
| --- | --- |
| `ILK_PROVISION` | `IO.*`, any hive |
| `ILK_REGISTER` | `SY.frontdesk.gov@` (same hive), `SY.orchestrator@` (its own hive's nodes), the configured frontdesk, `SY.identity@<hive>` |
| `ILK_ADD_CHANNEL` | `IO.*`, `SY.frontdesk.gov@` (same hive), the configured frontdesk |
| `ILK_UPDATE` | `SY.orchestrator@` (its own hive's nodes), `SY.identity@<hive>` |
| `ILK_SET_DEFINITION` | `SY.admin@`, `SY.architect@` (same hive), `SY.identity@<hive>` |
| `ILK_SET_TENANT` | `SY.admin@`, `SY.architect@` (same hive) |
| `ILK_DELETE` | `SY.admin@` (same hive), `SY.orchestrator@` (its own hive's nodes), `SY.identity@<hive>` |
| `ILK_RESTORE` | `SY.admin@` (same hive) |
| `ILK_PURGE` | `SY.admin@` (same hive), `SY.orchestrator@` (its own hive's nodes) |
| `ICH_SET_ENABLED` | `IO.*` (its own channel only), `SY.admin@`, `SY.architect@`, `SY.frontdesk.gov@` (same hive), the configured frontdesk |
| `ILK_LIST`, `ILK_GET`, `TNT_LIST`, `TNT_GET` | `SY.admin@` (same hive) |
| `TNT_CREATE`, `TNT_UPDATE`, `TNT_SET_SPONSOR` | `SY.admin@`, `SY.architect@` (same hive) |
| `TNT_APPROVE`, `TNT_DELETE`, `TNT_RESTORE`, `TNT_PURGE` | `SY.admin@` (same hive) |
| `CONFIG_GET`, `CONFIG_SET` | `SY.admin@` (same hive) |
| `IDENTITY_METRICS`, `PING`, `STATUS` | anyone |

The configured frontdesk is `hive.yaml` `government.identity_frontdesk`, made a full L2 name
(`configured_identity_frontdesk_node_name`). Each answer is `<MESSAGE>_RESPONSE`
(`response_name`); an error carries `status: "error"`, `error_code` and `message`.

**An orchestrator acts only on its own hive's nodes** (`authorize_orchestrator_ilk_scope`,
[control-path hardening](../control-path-hardening-v1.md) §4). The table admits
`SY.orchestrator@` from any hive; then, from `SY.orchestrator@H`:

- `ILK_REGISTER` is accepted only for a node of `H`: `identification.node_name` ends in `@H`,
  the type is not `human`, and the ILK it lands on (the node name's active holder, else the
  requested `ilk_id`), when it exists, is a node of `H`. So it takes no person's ILK and no other
  hive's.
- `ILK_UPDATE`, `ILK_DELETE` and `ILK_PURGE` are accepted only on an ILK whose `node_name` ends
  in `@H`.
- Anything else answers `UNAUTHORIZED`, with the warning `identity refused an orchestrator outside
  its own hive's nodes`. An ILK that does not exist answers as before (`ILK_NOT_FOUND`).

### 7.2 SY.admin (the operator)

`src/bin/sy_admin.rs` relays each route to `SY.identity@{hive}` (`handle_identity_command`,
`handle_identity_query`). SY.identity takes `SY.admin@` only from its own hive, and SY.admin runs
on the motherbee, so `{hive}` is `motherbee` in practice.

| Route | Action → message |
| --- | --- |
| `GET /hives/{hive}/identity/ilks[?include_deleted=true][&pending_signup=true]` | `list_ilks` → `ILK_LIST` |
| `GET /hives/{hive}/identity/ilks/{ilk_id}` | `get_ilk` → `ILK_GET` |
| `POST /hives/{hive}/identity/ilks/{ilk_id}/definition` | `set_ilk_definition` → `ILK_SET_DEFINITION` |
| `PUT /hives/{hive}/identity/ilks/{ilk_id}/tenant` | `set_ilk_tenant` → `ILK_SET_TENANT` |
| `DELETE /hives/{hive}/identity/ilks/{ilk_id}` | `delete_ilk` → `ILK_DELETE` |
| `POST /hives/{hive}/identity/ilks/{ilk_id}/restore` | `restore_ilk` → `ILK_RESTORE` |
| `POST /hives/{hive}/identity/ilks/{ilk_id}/purge` | `purge_ilk` → `ILK_PURGE` |
| `GET /hives/{hive}/identity/tenants[?include_deleted=true]` | `list_tenants` → `TNT_LIST` |
| `POST /hives/{hive}/identity/tenants` | `create_tenant` → `TNT_CREATE` |
| `GET /hives/{hive}/identity/tenants/{tenant_id}` | `get_tenant` → `TNT_GET` |
| `PUT /hives/{hive}/identity/tenants/{tenant_id}` | `update_tenant` → `TNT_UPDATE` |
| `POST /hives/{hive}/identity/tenants/{tenant_id}/sponsor` | `set_tenant_sponsor` → `TNT_SET_SPONSOR` |
| `DELETE /hives/{hive}/identity/tenants/{tenant_id}` | `delete_tenant` → `TNT_DELETE` |
| `POST /hives/{hive}/identity/tenants/{tenant_id}/restore` | `restore_tenant` → `TNT_RESTORE` |
| `POST /hives/{hive}/identity/tenants/{tenant_id}/purge` | `purge_tenant` → `TNT_PURGE`, after the vault check |
| `GET /hives/{hive}/identity/tenants/{tenant_id}/nodes` | `list_tenant_nodes` |

- `pending_signup` keeps only the temporary people who hold a Cloud account key, wherever they
  are charged (`is_pending_signup`).
- A tenant read answers `people_count` and `node_count` besides the record
  (`get_tenant_payload`).

### 7.3 Fluxbee Cloud

The contract is `docs/io-cloud-api.md`. Cloud reaches identity through IO.cloud:

- **Relayed to SY.admin** (`CLOUD_OP_ACTIONS` in `crates/fluxbee_sdk/src/cloud.rs`):
  - `get_ilk_details` → `get_ilk`;
  - `delete_ilk`, `restore_ilk`, `set_ilk_tenant`;
  - `create_tenant`, `update_tenant`, `get_tenant`, `list_tenants`, `delete_tenant`,
    `restore_tenant`;
  - `list_nodes` → `list_tenant_nodes`.
- **Local to IO.cloud** (`CLOUD_LOCAL_OPS`): `register_human` (§2.6), and the fast `get_ilk` and
  `list_ilks` reads from the identity SHM.

SY.admin re-checks every relayed request (`enforce_cloud_relay_content`, `stamp_relay_facts`):

- An action on an existing tenant names the tenant Cloud claims, never the root tenant
  (`cloud_claimed_tenant`). `create_tenant` has no tenant yet.
- `delete_ilk` and `restore_ilk` must carry `ilk_type: "human"`: Cloud deletes and restores
  people only.
- `update_tenant` may set the status to `active` or `suspended` only; `pending` stays the
  operator's.
- `list_tenants` from Cloud leaves out the root tenant and shows the marked ones.
- `set_ilk_tenant` gets `on_behalf_of` from SY.admin, never from the relay.
- Purges are not in Cloud's list: they are the operator's.

### 7.4 Common error codes

| Code | Meaning |
| --- | --- |
| `INVALID_REQUEST` | A malformed id or payload, or an id in another spelling than its canonical one (§1); unknown fields are refused (`deny_unknown_fields`). |
| `INVALID_IDENTIFICATION` | A non-`human` registration carries `email` or `claimed_email` (§2.6). |
| `NOT_PRIMARY` | A write sent to a replica. |
| `DB_NOT_READY` | A write on a primary without a database. |
| `UNAUTHORIZED_REGISTRAR` | The caller may not send this message. |
| `UNAUTHORIZED` | The caller may send this message, but not for that object: an IO node's channel it does not own (§2.5), or an ILK outside an orchestrator's own hive's nodes (§7.1). |
| `ILK_NOT_FOUND`, `NOT_FOUND`, `TENANT_NOT_FOUND` | Not found (a marked ILK counts as not found for most writes). |
| `ILK_DELETED` | A marked ILK holds that key. |
| `TENANT_DELETED`, `TENANT_PENDING`, `TENANT_SUSPENDED`, `TENANT_NOT_ACTIVE` | The tenant's state refuses the operation. |
| `TENANT_ROOT_PROTECTED`, `TENANT_ROOT_NOT_REGISTRABLE` | The root tenant is not editable, and takes no person. |
| `SYSTEM_ILK_PROTECTED` | A system ILK. |
| `DUPLICATE_EMAIL` | The vouched email (`identification.email`) is taken in that tenant and no merge applies. A `claimed_email` never answers it. |
| `CLOUD_CHANNEL_RESERVED`, `INVALID_SCOPE`, `ICH_ID_CONFLICT`, `ICH_CONFLICT`, `ACCOUNT_AMBIGUOUS` | Channel and scope rules (§2.5, §2.6). `ICH_CONFLICT`: another ILK holds the channel. |
| `TENANT_HAS_CHILDREN`, `TENANT_HAS_NODES`, `TENANT_HAS_PEOPLE`, `TENANT_HAS_SECRETS` | A tenant lifecycle guard (§3.6). |

Database errors map to their own codes (`map_db_write_error_code`). Their message never echoes
the row's values (`db_error_summary`).

---

## 8. Known gaps

A-72, agent assets missing on the workers (§4). Unused today: `TNT_APPROVE` (§3.2),
`identity_vocabulary` (§5.2) and the `partial` status (§2.3).
