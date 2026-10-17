# IO.cloud — API for Fluxbee Cloud

> **For the Fluxbee Cloud developers.** You do not need to know fluxbee's internals.
>
> Contract as of **fluxbee 0.1.69** (2026-10-15). Read from the code, operation by operation; §10
> lists what was also checked live on the PROD hive. §9 lists what changed for Cloud in each
> release. The model behind it: `docs/ilk-tenant-model-v1.md` (an identity is the actor and is
> never duplicated; a tenant is a cost center, a workspace of the system, never a company).
>
> **Tenant, not company.** Fluxbee sees tenants only. Cloud associates each of its companies with
> a tenant (today one to one); companies, members and billing are Cloud's.

---

## 1. What IO.cloud is

A fluxbee **hive** is a private installation. Its services (the vault, the identity registry, the
node launcher) talk over an internal network with no HTTP door. Fluxbee Cloud still has to manage
its customers inside a hive: create a tenant, store its credentials, list them, launch a node.

**IO.cloud is the one door for that, and it is narrow on purpose.** It runs inside the hive,
publishes **one HTTPS endpoint**, and knows a fixed list of operations (§5). It is not a general API
with an allowlist on top: an operation it does not know does not exist as a path. Behind it, the
hive's admin checks every request again (§3), in case IO.cloud itself were compromised.

---

## 2. Connecting

```text
  Fluxbee Cloud
        │  POST https://<hive public host>/e/<ich>
        │  Authorization: Bearer <token>
        │  Content-Type: application/json
        ▼
  SY.edge (the only piece on the internet)
    1. is <ich> published?          no → 404
    2. is the method POST?          no → 405
    3. does the bearer match?       no → 401
    4. does the body fit in 64 KiB? no → 413
    → removes Authorization: it never travels inside
        ▼  (the hive's internal network)
  IO.cloud   translates {op, tenant_id, params} into one hive action
        ▼
  SY.admin   checks the request again and runs it
```

**The URL** is `https://<hive public host>/e/<ich>`. The operator gives it to you (§7).

**The ICH is not a secret.** It is derived deterministically, and with IO.cloud's fixed channel it
is the same on every installation: `ich:14b66389-d425-531c-a140-a591d25e8f39`. **The only
protection of the endpoint is the bearer.**

**The bearer** is one static token for the whole installation, for every operation and every
tenant:

- accepted prefixes are exactly `"Bearer "` and `"bearer "`; `BEARER <token>` answers **401**;
- compared in constant time; an empty secret never authorizes;
- minted by the hive's admin when the operator publishes the endpoint, stored in the hive's vault,
  and shown **once**. Nobody can read it back afterwards: if it is lost, it is rotated (§7);
- after a rotation the **previous token keeps working for 10 minutes**, so a request in flight is
  not refused at the switch. It is a safety net, not a migration window.

PROD:

```text
Host   https://hive-k3m9x7q2.fluxbee.ai
URL    https://hive-k3m9x7q2.fluxbee.ai/e/ich:14b66389-d425-531c-a140-a591d25e8f39
Token  given by the operator, never published here
```

---

## 3. The envelope and the three rules

Every request is one JSON object:

```jsonc
{
  "op":         "list_tokens",       // required, exact and case-sensitive (no trim)
  "tenant_id":  "tnt:<uuid>",        // the tenant the op acts on; see rule 3
  "params":     { },                 // per op (§5); absent = {}
  "request_id": "<your uuid>"        // optional, strongly recommended: it is the trace key
}
```

Extra fields at the root are ignored. The body limit is 64 KiB, applied to the raw body and again to
the internal envelope, so the useful budget is somewhat less.

**Rule 1 — each op takes the params listed for it.** IO.cloud rebuilds the hive request from those
fields. Other fields are ignored, except where an op refuses them (it says so).

**Rule 2 — `result` is the hive service's reply, untouched.** IO.cloud does not rewrite results.
An error of the service comes back as `status:"error"` with the service's `error_code`.

**Rule 3 — the tenant is at the envelope root.**

- `tenant_id` goes **at the root, next to `op`**. A `tenant_id` inside `params` is **refused**
  (`INVALID_REQUEST`), not ignored.
- It must be canonical (`tnt:` + a hyphenated, lowercase UUID) and it is **never the root tenant**
  (`tnt:00000000-0000-0000-0000-000000000001`, which holds the system and its nodes): that answers
  `TENANT_ROOT_PROTECTED` (`register_human`: `TENANT_ROOT_NOT_REGISTRABLE`).
- **A tenant is a cost center: what a person or a node is charged to.** It is not a permission and
  not a boundary.
- **Ops that act on a tenant act on that one:** its credentials, its name, the people and nodes
  charged to it, its own lifecycle. Something of another tenant answers exactly like something
  that does not exist (`exists:false`, `KEY_NOT_FOUND`). A credential key another tenant holds
  answers `KEY_TAKEN` on `put_token`, because keys share one namespace.
- **A person is not owned by a tenant, only charged to one.** Reading a person by id, reading their
  details, and deleting or restoring their account take no tenant. `set_ilk_tenant` charges them to
  another tenant.
- **Ops with no tenant:** `create_tenant` (the tenant does not exist yet), `list_cloud_actions`,
  `get_ilk` and `get_ilk_details`, and `delete_ilk` and `restore_ilk`.

The hive's admin checks rule 3 again on its side for every relayed op, and the vault and the
identity registry refuse anything of another tenant themselves.

**The tenant is trusted as sent.** The bearer is a platform credential: whoever holds it can name
any tenant. Choosing the right tenant for the person in the session is Cloud's job (§8).

---

## 4. Responses and errors

### 4.1 Two error shapes

| shape | who | how to tell |
|---|---|---|
| `{"ok":false,"error_code":"…","message":"…"}` with a real HTTP status | **the edge**: the request never reached IO.cloud | has `ok`, no `status` |
| `{"status":"error","error_code":"…","error_detail":…}` with **HTTP 200** | **IO.cloud or the hive**: it arrived and failed | has `status`, no `ok` |

**A business error comes back with HTTP 200.** Checking `r.ok` is not enough:

```js
const r = await fetch(url, { method: 'POST', headers, body });
if (r.status !== 200) {                       // the edge: 401 404 405 413 500 502 503 504
  const e = await r.json().catch(() => ({}));
  throw new EdgeError(r.status, e.error_code, e.message);
}
const body = await r.json();
if (body.status !== 'ok') throw new HiveError(body.error_code, body.error_detail, body.request_id);
return body.result;
```

### 4.2 Fields of every IO.cloud reply

| field | when | what |
|---|---|---|
| `status` | always | `"ok"` or `"error"` |
| `op` | always (when the request named one) | the op |
| `handled_by` | always | e.g. `"IO.cloud@motherbee"` |
| `ich` | always | echo of the channel |
| `request_id` | always when you sent one | send one per attempt: it is how the operator finds the trace |
| `result` | `ok` (`register_human` answers flat, §5.2) | the service's reply |
| `error_code` | **every error** | match on this, never on the text |
| `error_detail` | errors | text; an object when the service gave no text |

### 4.3 Codes IO.cloud itself writes

| `error_code` | meaning |
|---|---|
| `INVALID_REQUEST` | a missing or malformed field, an unknown op, a `tenant_id` inside `params`, a reserved key |
| `TENANT_ROOT_PROTECTED` | the op named the root tenant |
| `UNAUTHORIZED` | the hive's admin refused the request (its own re-check of §3). Seeing it means a bug, not a client error |
| `ADMIN_UNAVAILABLE` | IO.cloud reached no answer from the hive in time (§6): **the outcome is unknown** |
| `IDENTITY_SHM_UNAVAILABLE` | a fast identity read could not read the registry; retry |

The service codes of each op are in §5.

---

## 5. Operations

| op | kind | tenant | what |
|---|---|---|---|
| `create_tenant` | relay | none | create (or find) a tenant |
| `update_tenant` | relay | yes | edit your tenant: name, domain, status (suspend, reactivate), settings, sponsor |
| `get_tenant` | relay | yes | your tenant's whole record, and how many people and nodes are charged to it |
| `list_tenants` | relay | none | every tenant but the genesis one, deleted ones included |
| `delete_tenant` / `restore_tenant` | relay | yes | mark your tenant deleted / undo it |
| `register_human` | local | yes | a person's account, charged to your tenant |
| `set_ilk_tenant` | relay | yes | charge a person to your tenant |
| `get_ilk` | local | none | fast existence + summary of an identity |
| `list_ilks` | local | yes | the identities charged to your tenant |
| `get_ilk_details` | relay | none | everything about one identity |
| `delete_ilk` / `restore_ilk` | relay | none | delete a person's account / undo it |
| `put_token` | relay | yes | store a credential, and say which nodes use it |
| `assign_token` | relay | yes | change which nodes use a credential |
| `list_tokens` | relay | yes | your credentials: metadata, who uses them and use, never the value |
| `delete_token` | relay | yes | delete one credential |
| `provision_node` | relay | yes | launch an `IO.*` node |
| `list_nodes` | relay | yes | your tenant's nodes on every hive, with their state |
| `get_node_status` | relay | yes | one node's status |
| `start_node` / `stop_node` / `delete_node` | relay | yes | start, stop (it stays stopped) or delete a node you launched |
| `get_usage` | relay | yes | your tenant's model calls, one record each: by period, or as they arrive |
| `list_cloud_actions` | local | none | this list, from the hive itself |

*relay* = IO.cloud turns it into a hive admin action; *local* = IO.cloud answers it itself, the fast
identity reads straight from the hive's registry in memory. Examples use:

```bash
URL="https://hive-k3m9x7q2.fluxbee.ai/e/ich:14b66389-d425-531c-a140-a591d25e8f39"
TOK="<the token>"
post() { curl -sS -m 35 -X POST "$URL" -H "Authorization: Bearer $TOK" \
              -H 'Content-Type: application/json' -d "$1"; echo; }
```

### 5.1 Tenants

#### `create_tenant`

| field | | |
|---|---|---|
| `tenant_id` (root) | ignored | the tenant does not exist yet |
| `params.name` | required | non-empty |
| `params.domain` | recommended | **the dedup key** (lowercased) |
| `params.status` | optional | `pending` \| `active` \| `suspended`; IO.cloud defaults it to `active` |
| `params.settings` | optional | JSON object |
| `params.sponsor_tenant_id` | optional | `tnt:<uuid>` of an existing tenant |

```json
{"status":"ok","op":"create_tenant","result":{"status":"ok","tenant_id":"tnt:94cd37b8-…",
 "created":true,"matched_by":null,"sponsor_tenant_id":null}}
```

**Idempotent, by finding an existing tenant:** first by `domain`, then by `name` (trimmed,
lowercase). A match returns that tenant with `created:false` and `matched_by:"domain"|"name"` and
**changes nothing in it**. The match is global to the hive, so always send `domain`, and always
read `created` and `matched_by` before assuming the tenant is yours.

Errors: `INVALID_REQUEST` (bad `status`, a non-string field), `INVALID_SPONSOR_TENANT`. For this op
`error_detail` is always the constant `"failed to create tenant"`: the information is in the code.

#### `update_tenant`

| field | | |
|---|---|---|
| `tenant_id` (root) | required | the tenant you edit |
| `params.name` | optional | non-empty |
| `params.domain` | optional | lowercased; `""` or `null` clears it |
| `params.status` | optional | `active` or `suspended` |
| `params.settings` | optional | a JSON object; replaces the settings |
| `params.sponsor_tenant_id` | optional | another tenant (`tnt:<uuid>`, never the genesis one), or `null` to clear it |

At least one. **Any other field is refused**, and so is `pending`, which stays the operator's.

- **Suspending a tenant:** its nodes get no credentials (they drop the ones they hold) and no node of
  it starts (`TENANT_NOT_ACTIVE`). The nodes that run keep running. **Reactivating it:** they get
  their credentials back, and the ones that died are relaunched within a minute.
- The genesis tenant is never edited (`TENANT_ROOT_PROTECTED`).

```json
{"status":"ok","op":"update_tenant","result":{"status":"ok","tenant_id":"tnt:94cd37b8-…","sponsor_tenant_id":null}}
```

It does not return the tenant: read it again with `get_tenant`.

| `error_code` | when |
|---|---|
| `TENANT_NAME_TAKEN` | another tenant has that name (case and spaces ignored), including one marked deleted |
| `TENANT_DOMAIN_TAKEN` | the same for the domain |
| `TENANT_NOT_FOUND` / `TENANT_DELETED` | |
| `INVALID_SPONSOR_TENANT` / `INVALID_SPONSOR_RELATION` | the sponsor does not exist / it is the tenant itself or makes a cycle |
| `INVALID_REQUEST` | nothing to change, an empty name, `pending`, a bad value, another field |

A refused update changes nothing. Keeping the tenant's current name or domain is not a conflict.

#### `get_tenant`

`tenant_id` at the root. The whole record, from the hive's identity store:

```json
{"status":"ok","op":"get_tenant","result":{"status":"ok","tenant_id":"tnt:…",
 "tenant":{"tenant_id":"tnt:…","name":"Acme","domain":"acme.com","status":"active","settings":{},
           "sponsor_tenant_id":null,"deleted_at_ms":null},
 "sponsor":null,"children":[],"child_count":0,"ilk_count":4,"people_count":2,"node_count":2,
 "is_root":true,"is_sponsor":false}}
```

- `people_count`: the people charged to it (accounts, contacts, pending signups; not the deleted
  ones). `node_count`: the node identities charged to it. `ilk_count`: every identity.
- A deleted tenant answers with `deleted_at_ms` set.
- `is_root` means it has no sponsor; the genesis tenant itself is refused (`TENANT_ROOT_PROTECTED`).

#### `list_tenants`

No tenant, no params. Every tenant but the genesis one, deleted ones included (with
`deleted_at_ms`, so you can restore them): `result: {"status":"ok","count":2,"tenants":[{tenant_id,
name, domain, status, settings, sponsor_tenant_id, child_count, ilk_count, people_count, node_count,
is_root, is_sponsor, deleted_at_ms}, …]}`.

#### `delete_tenant` / `restore_tenant`

`tenant_id` at the root, no params. Delete **disables** the tenant: it is marked deleted,
reversibly, and its name and domain stay reserved. **No person is marked**, whatever they are: an
account, a contact known through a channel (a Slack user, a phone that wrote in), a signup never
completed. They keep their identity and channels, and `set_ilk_tenant` charges them to another
tenant. Restore brings the tenant back as it was. Only the operator purges, and only once nobody
is charged to it.

While it is deleted, like while it is suspended, its nodes do not start and its credentials are not
handed out (§5.3, §5.4).

- delete: `{"status":"ok","tenant_id":…,"state":"deleted","already_deleted":false,"people_charged":3}`;
  idempotent (`already_deleted:true`). `people_charged` counts the people still charged to it.
- restore: `{"status":"ok","tenant_id":…,"state":"active","people_charged":3}`.
- Errors:
  - `TENANT_HAS_NODES`: node identities live in it (kill its nodes first).
  - `TENANT_HAS_CHILDREN`: it sponsors other tenants.
  - `TENANT_NOT_DELETED` (restore), `TENANT_NOT_FOUND`.

### 5.2 People (identities, "ilk")

#### `register_human` (local)

`tenant_id` at the root; **`params` is a `frontdesk_handoff`**:

| field | | |
|---|---|---|
| `params.type` | required | `"frontdesk_handoff"` |
| `params.schema_version` | required | `1` (a number) |
| `params.operation` | required | `"complete_registration"` |
| `params.subject.display_name` | required | |
| `params.subject.email` | required | a real address; the person's key in the tenant |
| `params.subject.phone`, `.company_name`, `.attributes` | optional | stored on the identity |

**One email is one account, in the whole hive.** The person is charged to the tenant at the root;
their email (the account's `cloud` channel) belongs to no tenant.

- **A new email:** IO.cloud creates the person, hands them to the hive's frontdesk, and answers
  **flat** (no `result`):

  ```json
  {"status":"ok","op":"register_human","ilk_id":"ilk:…","registration_status":"complete",
   "success":true,"merged":false,"human_message":"…"}
  ```

- **An email that already has an account:** nothing is created and nothing is handed to the
  frontdesk. The answer names the account and the tenant it is charged to:

  ```json
  {"status":"ok","op":"register_human","ilk_id":"ilk:…","registration_status":"complete",
   "tenant_id":"tnt:…","existing":true,"success":true,"merged":false}
  ```

  To charge it to your tenant, call `set_ilk_tenant`.
- **A signup that was never completed** (from any tenant) is picked up by the next
  `register_human` of that email, charged to the new tenant.
- Errors:
  - `ACCOUNT_AMBIGUOUS`: that email is held twice from before 0.1.65 (by several people, or by a
    person and another identity); the operator resolves it.
  - `CLOUD_CHANNEL_RESERVED`: the email is the address of a node's own channel, not a person's.
  - `ACCOUNT_IN_ROOT_TENANT`: the account is charged to the root tenant (registered before 0.1.54);
    the operator reassigns it.
  - `TENANT_ROOT_NOT_REGISTRABLE`, `INVALID_REQUEST` (the handoff shape), `ILK_DELETED` (that
    account is marked deleted: restore it), `IDENTITY_UNAVAILABLE`, `FRONTDESK_UNAVAILABLE`.
  - On a frontdesk refusal, `success:false` with its code, and the identity stays temporary.

#### `set_ilk_tenant`

`tenant_id` at the root (the tenant to charge the person to) + `params.ilk_id`. A tenant is a cost
center, so this grants and revokes nothing; the person keeps every channel and lookup.

```json
{"status":"ok","op":"set_ilk_tenant","result":{"status":"ok","ilk_id":"ilk:…","tenant_id":"tnt:…",
 "previous_tenant_id":"tnt:…","changed":true}}
```

- Already charged there: `changed:false`.
- Any person: an account from anywhere; a contact that never registered (a temporary) only out of
  a deleted tenant, which is how a deleted tenant is emptied. A contact of an active tenant
  stays: that tenant registers it. Every change is recorded in the hive.
- Errors:
  - `DUPLICATE_EMAIL`: another person charged to that tenant has the same email in their
    identification.
  - `TENANT_NOT_ACTIVE` (pending or suspended), `TENANT_DELETED`, `TENANT_NOT_FOUND`,
    `TENANT_ROOT_PROTECTED`.
  - `INVALID_TENANT_TRANSITION` (a contact of a tenant that is not deleted),
    `ILK_TYPE_MISMATCH` (not a person), `ILK_NOT_FOUND` (or deleted).

#### `get_ilk` (local, fast)

Exactly one selector: `params.ilk_id` (`ilk:<uuid>`) or `params.email` (the account's email; ASCII
case-insensitive). No tenant needed.

| request | answer |
|---|---|
| `ilk_id` | `{"exists":true,"ilk":{…summary…}}`, whatever tenant it is charged to |
| `email` | the account: `{"exists":true,"ilk":{…},"matches":[{…}]}`; nobody: `exists:false` |

The summary is `{ilk_id, ilk_type, registration_status, tenant_id, display_name, pending_signup}`;
`tenant_id` is what the person is charged to, and `pending_signup` is `true` for a signup nobody
finished: a temporary person holding an account's email. The next `register_human` of that email
takes it over.

`matches` holds more than one entry only for an email several people still hold from before
0.1.65 (the operator resolves those); then `ilk` is `null`:

```json
{"result":{"exists":true,"ilk":{…},"matches":[{…}]}}          // the account (the usual case)
{"result":{"exists":true,"ilk":null,"matches":[{…},{…}]}}     // several people, before 0.1.65
{"result":{"exists":false,"ilk":null,"matches":[]}}           // first time: register_human
```

#### `list_ilks` (local, fast)

`tenant_id` at the root: the identities charged to it.
`result: {"tenant_id":…,"count":2,"ilks":[{ilk_id, ilk_type, registration_status, tenant_id, display_name, pending_signup}]}`.

#### `get_ilk_details` (relay)

`params.ilk_id` **or** `params.email` (the account). No tenant needed. The full record from the
registry's database:

```jsonc
{"status":"ok","op":"get_ilk_details","result":{
  "status":"ok","ilk_id":"ilk:…","canonical_ilk_id":"ilk:…","alias_resolved":false,
  "ilk":{"ilk_id":"ilk:…","ilk_type":"human","registration_status":"complete","tenant_id":"tnt:…",
         "identification":{"display_name":"…","email":"…","phone":"…","company_name":"…","attributes":{}},
         "channels":[{"ich_id":"ich:…","channel_type":"cloud","address":"juan@acme.com","enabled":false}]},
  "tenant":{"tenant_id":"tnt:…","name":"…","domain":"…","status":"active",…}}}
```

- The name field is **`identification.display_name`**.
- `channels[].enabled` says whether that channel is **published on the edge**. A person's `cloud`
  channel is an address, not a published endpoint, so `false` is normal and changes nothing.
- `ilk.tenant_id` and the `tenant` block are what the person is charged to.
- An identity that does not exist answers `NOT_FOUND` (by email too).

#### `delete_ilk` / `restore_ilk`

`params.ilk_id`. No tenant needed. **People only**: node identities belong to the hive's orchestrator.
Delete **marks** a person's account (reversible; its email and channels stay reserved until the
operator purges). Idempotent.

- delete: `{"status":"ok","ilk_id":…,"state":"deleted","already_deleted":false}`;
  restore: `{"status":"ok","ilk_id":…,"state":"active"}`.
- A person also comes back while the tenant they are charged to is deleted, still charged to it;
  `set_ilk_tenant` then moves them.
- Errors: `ILK_NOT_FOUND`, `ILK_TYPE_MISMATCH` (not a person), `ILK_NOT_DELETED` (restore),
  `ILK_KEY_CONFLICT` (restoring it would bring back a second account for its email).
- A person deleted before 0.1.65 comes back with their account where `register_human` finds it,
  unless another identity holds that email.

### 5.3 Credentials (the hive's vault)

**Keys share one namespace for the whole hive.** A key belongs to the tenant that wrote it. Name
yours `<provider>:<tenant_id>:<identifier>`, which also makes them easy to filter.

**Who uses a credential** (`docs/ilk-tenant-model-v1.md` §2.2). The tenant is who pays for its use;
it is not a permission. A credential is used by:
- **one node that owns it** (`owner_node`);
- **the nodes it is assigned to** (`nodes` in `put_token`, then `assign_token`): one or several, each
  charged to your tenant. Read them from `list_ilks` (the non-`human` identities);
- **every node of your tenant**, when it has neither: a *general* credential, the simple choice for
  a tenant's AI key in tests or a small setup.

A node uses one it owns, else one assigned to it, else its tenant's general one. The hive hands the
value out only while the tenant is active: a suspended tenant's nodes are refused it (and drop it);
while the hive cannot tell (its identity registry still loading), they keep what they have and retry.

#### `put_token`

| field | | |
|---|---|---|
| `tenant_id` (root) | required | the credential's tenant |
| `params.key` | required | 1–256 bytes of `[a-z0-9:_-]`, starting with `[a-z0-9]` |
| `params.value` | required | any JSON but `null`, ≤ 1 MiB serialized |
| `params.resource_type` | required (or in `params.metadata`) | normalized: lowercase, anything else → `_` (`"Google Calendar"` → `google_calendar`) |
| `params.owner_node` | optional | an `IO.*` node **that already exists**: it owns the credential and uses it |
| `params.nodes` | optional | the nodes it is assigned to: node names (`IO.slack.sales@motherbee`) or identity ids (`ilk:…`), each **existing and charged to your tenant**, never a person |
| (neither) | | a general credential: every node of your tenant uses it |
| `params.metadata` | optional | `description` (string) and `tags` (string array) |

```json
{"status":"ok","op":"put_token","result":{"status":"ok","key":"gmail:tnt:94cd37b8-…:sales","version":1,"changed":true}}
```

- **The same value again does nothing**: `changed:false`, the version stays, **and the metadata is
  not updated either**. To change `description` or `tags`, change the value; to change who uses it,
  `assign_token`.
- **A new value keeps who uses it**: refreshing an existing credential without `owner_node` or
  `nodes` leaves its owner and nodes as they are (it never becomes general by accident). Naming
  either replaces both.
- **Metadata is a fixed schema.** Only `description` and `tags` are yours; the vault sets
  `tenant_id`, `resource_type`, `created_by` (the hive-side writer, never your user), `created_at`
  and `updated_at`. Other fields are dropped; a wrong type fails the write with `INVALID_VALUE`. Your
  provenance (who connected it, which account) goes in `tags`, e.g. `source:cloud`, `by:<ilk>`.
- **Order:** `provision_node` **before** a `put_token` with `owner_node` or `nodes` (a node must
  exist to own or use it). A credential the node needs at its first start is general, or assigned
  right after the node exists.
- The value's shape is not checked anywhere: a wrong shape fails when the node uses it. Shapes the
  hive's nodes expect today: `openai`/`anthropic` → `{"api_key":…}`; `slack` →
  `{"app_token":…,"bot_token":…}`; `postgres` → `{"postgres_url":"postgresql://user:pass@host:5432"}`
  (no database name).
- It does not check that the tenant exists.

| `error_code` | when |
|---|---|
| `KEY_TAKEN` | the key belongs to another tenant. It never moves |
| `INVALID_REQUEST` | key charset/length, value over 1 MiB, missing field, `owner_node` not `IO.*` or not registered, a node in `nodes` that does not exist, is charged to another tenant or is a person, a reserved infrastructure key (`edge_channel_secret:*`, `edge_tls*`, `ssh:*`, `edge_fanout_verify:*`) |
| `INVALID_VALUE` | metadata of the wrong type |
| `STORAGE_ERROR`, `ENCRYPTION_ERROR`, `MASTER_KEY_NOT_AVAILABLE` | the vault |

#### `list_tokens`

| field | | |
|---|---|---|
| `tenant_id` (root) | required | lists **only** that tenant's credentials |
| `params.resource_type` | optional | exact, in its normalized form |
| `params.prefix` | optional | key prefix |
| `params.limit` | optional | 1–1000; the vault's default is 200 |

**Never the value.** `result` is the vault's reply:

```json
{"status":"ok","op":"list_tokens","result":{"status":"ok","count":1,"secrets":[{
  "key":"gmail:tnt:94cd37b8-…:sales","version":1,"access_count":0,"last_accessed_at":null,
  "metadata":{"tenant_id":"tnt:94cd37b8-…","resource_type":"gmail","created_by":"ilk:…",
              "created_at":"2026-10-13T17:57:39Z","updated_at":"2026-10-13T17:57:39Z",
              "description":"Sales Gmail","tags":["source:cloud"]}}]}}
```

- `access_count` / `last_accessed_at` move when a node reads the **value**, which nodes do when they
  load, not on every use: "a node took it", not "it was used". The vault does not know whether a
  credential works.
- `metadata.ilk` appears only for a credential with an owner (`owner_node`): its identity.
- `metadata.assigned_ilks` lists the identities of the nodes it is assigned to. A credential with
  neither `ilk` nor `assigned_ilks` is general.
- A tenant with no credentials, or one that does not exist: `count:0`.

#### `assign_token`

`tenant_id` at the root + `params.key` + `params.nodes`: the nodes the credential is assigned to from
now on (names or `ilk:` ids, each existing and charged to your tenant, never a person). It replaces
the list; `[]` leaves it with none, so a credential with no owner becomes general again. The value
does not change.

```json
{"status":"ok","op":"assign_token","result":{"status":"ok","key":"slack:tnt:94cd37b8-…:bot",
 "assigned_ilks":["ilk:…","ilk:…"],"general":false,"changed":true}}
```

- `params.nodes` must be a list; `[]` is the explicit "no node".
- Every node that had it or has it now is told and reloads it; a node it was taken from stops using
  it. A node that loads its credential only when it starts (today `io.linkedhelper`) stops at its next
  restart or configuration.
- The same list again: `changed:false`, nobody is told.
- A node that is deleted stays listed in `assigned_ilks` (it reads nothing, and the credential does
  not become general because of it). To clean it, send `assign_token` with the nodes that exist, as
  `list_ilks` shows them: an id that no longer exists answers `INVALID_REQUEST`.

| `error_code` | when |
|---|---|
| `KEY_NOT_FOUND` | the key does not exist or belongs to another tenant |
| `INVALID_REQUEST` | `params.nodes` missing or not a list; a node that does not exist, is charged to another tenant or is a person; a reserved infrastructure key |

#### `delete_token`

`tenant_id` at the root + `params.key`.

```json
{"status":"ok","op":"delete_token","result":{"status":"ok","key":"gmail:tnt:94cd37b8-…:sales","deleted":true}}
```

| `error_code` | when |
|---|---|
| `KEY_NOT_FOUND` | the key does not exist **or belongs to another tenant**: same code and same text, on purpose |
| `INVALID_REQUEST` | missing key, a reserved infrastructure key |

The hive announces the deletion to its nodes. A node that already loaded the value may keep using it
until it reloads; revoking the credential at the provider is what makes it useless.

### 5.4 Nodes

#### `provision_node`

| field | | |
|---|---|---|
| `tenant_id` (root) | required | the node's tenant |
| `params.node_name` | required | **must start with `IO.`**; a `@hive` suffix picks the server (omit it if unsure) |
| `params.runtime` | send it | what to run (e.g. `io.api`). Omitted, it is derived from the name's first two segments, which rarely exists |
| `params.runtime_version` | optional | default `"current"` |
| `params.config` | optional | object; IO.cloud overwrites `config.tenant_id` with the envelope's `tenant_id` |
| `params.add_channels` | optional | `[{"ich_id":"ich:…","type":…,"address":…}]` |
| `params.identity_change_reason` | optional | text |

```json
{"status":"ok","op":"provision_node","result":{"status":"ok","node_name":"IO.acme@motherbee",
 "runtime":"io.api","version":"…","hive":"motherbee","identity":{"register":{"ilk_id":"ilk:…"}},
 "config":{"created":true,"config_version":1}}}
```

**Not idempotent:** a second call with the same name always fails with `NODE_ALREADY_EXISTS`.

| `error_code` | when |
|---|---|
| `NODE_ALREADY_EXISTS` | a node with that name exists |
| `TENANT_NOT_ACTIVE` | the tenant is pending or suspended: it starts no node |
| `RUNTIME_NOT_AVAILABLE` | the runtime or version cannot be resolved |
| `INVALID_REQUEST` | not `IO.*`, runtime not derivable, bad version |
| `IDENTITY_REGISTER_FAILED`, `CONFIG_WRITE_FAILED`, `SERVICE_FAILED`, `SPAWN_FAILED` | the hive could not launch it; for a deleted tenant, `IDENTITY_REGISTER_FAILED` with `TENANT_DELETED` in the detail |

The hive records that you launched it (`launched_by: "cloud"`): only those nodes can you start, stop
or delete. Nodes launched before 0.1.68 carry no `launched_by`: `list_nodes` shows them
(`launched_by: null`), and start, stop and delete answer `NODE_NOT_FOUND`; the operator handles
them. Configuring a node after it is launched, routing it or wiring it into a solution is the
operator's work, through Archi.

#### `list_nodes`

`tenant_id` at the root, no params. Every node charged to your tenant, on every hive: the ones you
launched and the ones the operator launched.

```json
{"status":"ok","op":"list_nodes","result":{"status":"ok","tenant_id":"tnt:…","count":1,
 "nodes":[{"node_name":"IO.slack.acme@motherbee","hive":"motherbee","kind":"IO",
           "runtime":{"name":"io.slack","version":"0.1.68"},
           "lifecycle_state":"RUNNING","health_state":"HEALTHY",
           "identity":{"ilk_id":"ilk:…","tenant_id":"tnt:…"},"config_version":1,
           "launched_by":"cloud","relaunch_on_boot":true}],
 "unreachable_hives":[]}}
```

You see a node's state, not how the hive runs it: no process, unit or host paths.

- `lifecycle_state`: `RUNNING`, `STARTING`, `STOPPED` or `FAILED`, from the hive itself, for every
  node.
- `health_state`: what the node reports about itself. IO nodes are not all at the same stage, and
  an older one may answer `UNKNOWN`.
- `identity.ilk_id` is what `list_tokens` shows in `assigned_ilks`.
- `relaunch_on_boot: false` means it was stopped and stays stopped until `start_node`.
- A hive whose nodes are missing from `nodes` is listed in `unreachable_hives`, never left out in
  silence: it did not answer in time, it is not connected (`HIVE_NOT_CONNECTED`), or its core
  predates 0.1.68 (`HIVE_OUTDATED`, the operator updates it).
- A node appears once it really exists, so after `provision_node` read it here, not from your own
  records.

#### `get_node_status`

`tenant_id` at the root + `params.node_name` (`IO.…@hive`). The same view as `list_nodes`, for one
node (`launched_by` and `relaunch_on_boot` included), plus when the hive looked (`observed_at`),
where the health came from (`health_source`) and the node's last error when it reported one:

```json
{"status":"ok","op":"get_node_status","result":{"status":"ok","node_status":{
 "node_name":"IO.slack.acme@motherbee","hive":"motherbee","lifecycle_state":"RUNNING",
 "health_state":"HEALTHY","health_source":"NODE_REPORTED","observed_at":"…",
 "runtime":{"name":"io.slack","version":"0.1.68"},"identity":{"ilk_id":"ilk:…","tenant_id":"tnt:…"},
 "config_version":1, …}}}
```

A node of another tenant, or one that does not exist, answers `NODE_NOT_FOUND`.

#### `start_node` / `stop_node` / `delete_node`

`tenant_id` at the root + `params.node_name`. **Only the IO nodes you launched in your tenant**: any
other node answers `NODE_NOT_FOUND`, exactly as one that does not exist.

| op | what it does |
|---|---|
| `start_node` | starts it; `state: "started"`, or `"already_running"`. Refused with `TENANT_NOT_ACTIVE` while the tenant is pending or suspended |
| `stop_node` | stops it, and it **stays stopped**: the hive does not bring it back, not even after a reboot, until `start_node`. Its config, identity and credentials stay. A node that dies on its own is relaunched; one you stopped is not |
| `delete_node` | stops it and removes it: its timers, the credentials it **owns** and its identity go, and its name is free again. Credentials only **assigned** to it stay (they are the tenant's). Final |

```json
{"status":"ok","op":"stop_node","result":{"status":"ok","state":"stopped",
 "node_name":"IO.slack.acme@motherbee","unit_was_active":true,"relaunch_on_boot":false}}
```

The answer says what happened (`state`: `started`, `already_running`, `stopped`, `deleted`), never
how: no unit, signal or host path.

To cut a node's access without stopping it, take its credential away (`assign_token`) or delete
the credential (`delete_token`); to cut a whole tenant, suspend it (`update_tenant`).

### 5.5 Model usage

The hive records **every call its nodes make to a model**: one record per call, charged to a
tenant. Your tenant's records are the calls of its nodes and the calls the hive makes for them: its
memory of a conversation is charged to the tenant of the agent in it, its frontdesk to the case's
tenant. **The hive's own spending (Archi, the admin) is charged to the root tenant, which no op
reads.** Pricing is yours, record by record.

#### `get_usage`

`tenant_id` at the root: **that tenant's records only.** Two ways to ask:

- **a period**, by the time of each call: grouped (`group_by`), or record by record;
- **since a cursor**, by the time each record was stored: to sync as the records arrive.

| field | | |
|---|---|---|
| `params.from`, `params.to` | a period | RFC 3339 instants, at most 92 days apart: from `from`, included, to `to`, not included |
| `params.group_by` | optional, with a period | `day`, `model` or `node`; without it, the records themselves |
| `params.since` | instead of a period | an RFC 3339 instant to start, then the `next` of each answer. Records only |
| `params.after` | optional | the `next` of the previous answer: the next page of the same period |
| `params.limit` | optional | rows per page, 1–1000; every page also stays under 64 KB |

**Any other field is refused**, and so is a period with `since`.

A period, grouped (`"group_by":"model"`):

```json
{"status":"ok","op":"get_usage","result":{"status":"ok","next":null,"groups":[
 {"key":"claude-sonnet-5-5","calls":412,"uncached_input":381220,"cache_read":95000,
  "cache_write":12000,"output":40310,"reasoning":0}]}}
```

`key` is the day (`YYYY-MM-DD`, UTC), the model or the node; `calls` counts the calls, and the other
five are the sums of their counts (below). Groups come sorted by `key`.

Record by record (a period without `group_by`, or `since`):

```json
{"status":"ok","op":"get_usage","result":{"status":"ok","next":"…","records":[{
 "record_id":"8b6b3c1e-…","provider_response_id":"msg_01…",
 "ts":"2026-10-17T12:00:00Z","stored_at":"2026-10-17T12:00:03Z","verified":true,
 "hive":"motherbee","node_name":"AI.sales@motherbee","node_ilk":"ilk:…",
 "tenant_id":"tnt:…","tenant_source":"node","counterpart_ilk":"ilk:…",
 "provider":"anthropic","model":"claude-sonnet-5-5","served_model":"claude-sonnet-5-5",
 "kind":"function_turn","outcome":"ok",
 "uncached_input":1200,"cache_read":0,"cache_write":0,"output":80,
 "raw_usage":{"input_tokens":1200,"output_tokens":80},"trace_id":"…","thread_id":"…"}]}}
```

| field | what |
|---|---|
| `record_id` | the call; unique |
| `provider_response_id` | the provider's id of the answer, when it gave one: one answer is stored once |
| `ts` | when the answer arrived (or the node gave up), RFC 3339 UTC, by the node's clock. Periods go by it |
| `stored_at` | when the hive stored the record. `since` goes by it |
| `hive`, `node_name`, `node_ilk` | the node that made the call |
| `tenant_id` | the tenant charged |
| `tenant_source` | why: `node` (your node), `agent` (the hive's memory, for your agent's conversation), `case` (the frontdesk, for your case) |
| `claimed_tenant_id` | the tenant the node's record named. It differs from `tenant_id` when the hive charged the node's own tenant instead |
| `verified` | `false` when the hive could not check the tenant against its identity registry and kept the one the record named |
| `counterpart_ilk` | who the call was for: the person your agent answered, or your agent for the hive's memory |
| `provider`, `model`, `served_model`, `service_tier` | the provider, the model asked for, the model that answered, and OpenAI's service tier |
| `kind` | `generate` (one answer) or `function_turn` (one turn of a tool loop) |
| `outcome` | `ok`; `truncated` (cut at the output limit); `refused` (the model refused); `unreadable` (an answer paid for whose counts could not be read); `abandoned` (the node stopped waiting: maybe paid for) |
| `uncached_input`, `cache_read`, `cache_write`, `output`, `reasoning` | the counts (below); none for `unreadable` and `abandoned` |
| `raw_usage` | the provider's own usage object, up to 2 KB |
| `trace_id`, `thread_id` | the message and the conversation, when there is one |
| `work` | what the work belongs to, for the hive's own tools; yours have none |

**The counts are the same for every provider:** `uncached_input` + `cache_read` + `cache_write` is
every input token, and `output` includes the reasoning. `reasoning` is the part of `output` the
provider reports as such (OpenAI), absent otherwise. Anthropic's split of cache writes by lifetime
(5 minutes, 1 hour) stays in `raw_usage`.

**Pricing is per record:** by its `model` (`served_model` says what actually answered), its
`service_tier` and its counts; `raw_usage` holds anything else a price needs. An `unreadable` or
`abandoned` record has no counts: it says a call happened that may have been paid for.

**Records arrive late, by design.** A record travels through its hive's stream, which holds it up
to 2 minutes while the store is busy or out of reach, and the store retries one it cannot check yet
for up to 10 minutes. So a period is final only about 12 minutes after it ends. To bill, sync
rather than re-read periods:

1. `{"op":"get_usage","tenant_id":…,"params":{"since":"2026-10-01T00:00:00Z"}}`;
2. keep the answer's `next` and ask again with `"since"` set to it, until an answer brings no
   records;
3. later, ask again from the last `next` you kept. An answer without records comes with
   `next: null`: keep yours.

Records can still be missing: a call in flight when its node is killed, or records held past the
stream's 2 minutes while the motherbee is down. The hive counts what it loses; the operator sees it.

**Paging a period:** send the same question with `params.after` set to the answer's `next`;
`next: null` means there are no more pages.

| `error_code` | when |
|---|---|
| `INVALID_REQUEST` | a field outside the list or of the wrong type, a bad instant, a period over 92 days or with `to` not after `from`, a period or `after` with `since`, a `limit` outside 1–1000, a cursor that is not a `next` the hive gave |
| `TENANT_ROOT_PROTECTED` | the root tenant |
| `USAGE_UNAVAILABLE` | the store did not answer in time or could not run the question (the detail says why): retry later |

### 5.6 `list_cloud_actions` (local)

No tenant, no params. `result: {"actions":[{"op":…,"category":"relay"|"local","summary":…}, …]}`:
every op of §5 with a one-line summary, from the hive itself.

---

## 6. Timeouts and retries

```text
you / the edge wait   30 s
IO.cloud waits        20 s for the hive (25 s for provision_node, stop_node, delete_node and get_usage)
```

A request that takes between 20 (25) and 30 s answers `ADMIN_UNAVAILABLE` **while it may still be
completing inside**. Treat it as **unknown**, not failed:

- `create_tenant`, `update_tenant`, `put_token` (same value), `delete_*`, `restore_*`: safe to retry.
- `get_usage`: a read, safe to retry.
- `delete_token`: a retry of a delete that did complete answers `KEY_NOT_FOUND`.
- `provision_node`: a retry answers `NODE_ALREADY_EXISTS`, which may mean the timed-out attempt
  worked. There is no idempotency key (`request_id` is for traces only); ask the operator.
- `delete_node`: a retry answers `NODE_NOT_FOUND` once the timed-out attempt finished; read
  `list_nodes` to know.

**IO.cloud serves one request at a time.** Concurrent calls queue, and a queued one can spend the
edge's 30 s and get a 504. Serialize your calls.

---

## 7. Operator checklist

IO.cloud runs on every hive's motherbee as a base node. Opening the door is one admin action, on the
motherbee:

```bash
curl -s -X POST http://127.0.0.1:8080/hives/motherbee/cloud/endpoint \
     -H 'Content-Type: application/json' -d '{"edge_node":"SY.edge@ingress1"}'
```

It publishes IO.cloud's channel on that edge, mints the token, stores it in the vault, records the
edge in IO.cloud's config (no restart) and answers with `url` (`/e/<ich>`) and `token`. **The token
is shown only here.** Give Cloud `https://<public host><url>` and the token.

- **Rotate:** run it again. A new token is minted; the previous one works 10 more minutes.
- **Check:** `curl 'http://127.0.0.1:8080/channels/externalized?edge_node=SY.edge@ingress1'` lists a
  row owned by `IO.cloud@motherbee`.
- **Smoke test** (no token needed for the first three):

```bash
URL="https://<host>/e/<ich>"
curl -s -o /dev/null -w 'no bearer -> %{http_code} (401)\n' -X POST "$URL" -d '{}'
curl -s -o /dev/null -w 'GET       -> %{http_code} (405)\n' "$URL"
curl -s -o /dev/null -w 'bad ich   -> %{http_code} (404)\n' -X POST "https://<host>/e/ich:00000000-0000-0000-0000-000000000000" -d '{}'
post '{"op":"list_cloud_actions"}'
```

| Cloud sees | likely cause on the hive |
|---|---|
| **404** | the endpoint was never published, or the hive was reinstalled: publish again |
| **401** with the right token | the door was published again (new token), or the edge could not read the token from the vault (`could not fetch channel secret from vault` in its log) |
| **504** | IO.cloud is down or busy (§6), or its config does not name this edge: publish again |

---

## 8. Limits and what is not there

1. **One platform token; the tenant is trusted as sent.** No per-tenant credentials and no
   end-user identity: whoever holds the token acts on any tenant. Guard it as a platform secret.
2. **`create_tenant` is not scoped:** it can set `status`, `settings` and a sponsor.
3. **The edge host is the trust anchor:** it checks the token and sees the traffic in clear. Moving
   Cloud's channel end to end is planned (host-posture spec, O5).
4. **No rate limit or quota.** The only protection is the edge's global concurrency cap (503).
5. **No compound transaction.** `create_tenant` → `put_token` → `provision_node` are independent
   calls; mind the `owner_node` order (§5.3).
6. **No idempotency key** (§6).
7. **The credential's value shape is not validated** (§5.3), and the vault has no "last error" or
   health per credential.
8. **Companies and members are Cloud's.** Fluxbee knows each person once and the tenant they are
   charged to now; which companies a person belongs to, and which one they act for, is Cloud's
   (`docs/ilk-tenant-model-v1.md`). Model spending is recorded per call and read with `get_usage`
   (§5.5); pricing it is Cloud's.

---

## 9. What changed for Cloud

### 0.1.79

- **New op `get_usage`** (§5.5): your tenant's model calls, one record each, with their tokens.
  A period grouped by day, model or node, or record by record; or the records stored since a
  cursor, to sync as they arrive. Pricing is yours, per record.

### 0.1.68

- **"Tenant", never "company", in this contract.** Fluxbee sees tenants; which company a tenant
  stands for is yours.
- **`update_tenant` edits the whole tenant:** also `status` (`active` or `suspended`), `settings`
  and `sponsor_tenant_id`. Suspending a tenant cuts its credentials and stops it starting nodes;
  reactivating it brings everything back.
- **`get_tenant` answers the whole record** through the hive's admin (it was a fast local read of
  `exists` and `ilk_count`): name, domain, status, settings, sponsor, `deleted_at_ms`,
  `people_count` and `node_count`.
- **New `list_tenants`:** every tenant but the genesis one, deleted ones included.
- **Your nodes:** new `list_nodes` and `get_node_status` (state, not process), and `start_node`,
  `stop_node` (it stays stopped) and `delete_node` for the IO nodes you launched.
- **`set_ilk_tenant`'s refusal of a contact** of a tenant that is not deleted answers
  `INVALID_TENANT_TRANSITION` alone (in 0.1.67 the detail came after a `|`).

### 0.1.67

- **`delete_tenant` disables the tenant and marks no person.** `TENANT_HAS_ACCOUNTS` is gone, and
  so are `cascaded_ilks` and `pending_signups` in its answer; it answers `people_charged`.
  `restore_tenant` answers `people_charged` instead of `restored_ilks`.
- **`set_ilk_tenant` moves contacts too, out of a deleted tenant,** to empty it before the
  operator purges it. `INVALID_TENANT_TRANSITION` now means a contact of a tenant that is not
  deleted.
- **`restore_ilk` brings a person back while their tenant is deleted** (it answered
  `TENANT_DELETED`).
- **A tenant that is not active starts no node:** `provision_node` answers `TENANT_NOT_ACTIVE`
  while it is pending or suspended (a deleted one already failed to register the node). Its running
  nodes keep running without credentials.

### 0.1.66

- **`pending_signup`** in every summary `get_ilk` and `list_ilks` answer: `true` for a signup nobody
  finished.
- **Who uses a credential** (§5.3): `put_token` takes `params.nodes`, and the new `assign_token`
  changes them without touching the value; `list_tokens` shows `metadata.assigned_ilks`. A
  credential with no owner and no node is general, as the pool was.
- **A suspended or deleted tenant's credentials are not handed out** to its nodes.

### 0.1.65

- **One email is one account, in the whole hive.** `register_human` for an email that already has
  one answers `existing: true` with that account and its tenant, and creates nothing.
  `ACCOUNT_AMBIGUOUS` and `ACCOUNT_IN_ROOT_TENANT` mark data from before.
- **New op `set_ilk_tenant`:** charge a person to your tenant.
- **People are not tenant-scoped:**
  - `get_ilk`, `get_ilk_details`, `delete_ilk` and `restore_ilk` take no tenant;
  - `get_ilk` by email answers the account;
  - `delete_ilk` and `restore_ilk` act on people only (`ILK_TYPE_MISMATCH` for anything else).
- **`delete_tenant` is refused while accounts are charged to it** (`TENANT_HAS_ACCOUNTS`). A signup
  never completed does not block it and is not marked (`pending_signups` counts them).
- The existing data moves when the hive upgrades: an email held by one person becomes their
  account; one held twice waits for the operator.

### 0.1.64

- **New ops:** `list_tokens`, `delete_token`, `update_tenant`.
- **`tenant_id` moved to the envelope root for the reads:** `get_ilk`, `get_tenant`, `list_ilks` and
  `get_ilk_details` by email used `params.tenant_id`. Now a `tenant_id` inside `params` is refused on
  every op.
- **`get_ilk` and `get_ilk_details` by `ilk_id` need the tenant** and answer only within it.
- **`get_ilk_details` by email answers `NOT_FOUND`** (was `ILK_NOT_FOUND`), like by id.
- **No op acts on the root tenant** (`TENANT_ROOT_PROTECTED`; `provision_node` used to answer
  `TENANT_ROOT_NOT_ALLOWED` from the hive).
- **`put_token` can no longer overwrite another tenant's key** (`KEY_TAKEN`), and
  `edge_fanout_verify:*` joined the reserved keys.
- **Every error carries `error_code`** and every reply carries `op`. IO.cloud's own validation
  errors are `INVALID_REQUEST`; a hive that does not answer in time is `ADMIN_UNAVAILABLE`.
- Fixed earlier, in 0.1.50: the admin's HTTP API decodes `%3A` in query strings (it affected tools
  that called the admin directly, not IO.cloud).

---

## 10. Checked live

**0.1.68 and 0.1.69:** on PROD, 2026-10-15, through the public door with Cloud's token, with the
operator launching a node of its own in the same tenant (`lab/DEPLOYMENTS.md`):

- `list_cloud_actions` lists 24 ops;
- `update_tenant` with settings and a sponsor, read back with `get_tenant`; `pending` and the
  genesis tenant refused; `list_tenants` without the genesis tenant;
- a node launched by Cloud shows in `list_nodes` with `launched_by: cloud` and its state only; a
  node of another tenant answers `NODE_NOT_FOUND`, and so does the operator's node, for start, stop
  and delete;
- `stop_node`: the node stays down past the hive's relaunch pass; `start_node` brings it back;
- a tenant Cloud suspended starts no node and its node is refused its credential; reactivated, the
  node reads it again;
- `delete_node` (0.1.69): its own credential goes, the assigned one stays, and the name is free
  again;
- a refused `set_ilk_tenant` of a contact answers `INVALID_TENANT_TRANSITION` alone.

**0.1.67:** on PROD, 2026-10-15, through the public door with Cloud's token, with the operator
suspending, reactivating and purging (`lab/DEPLOYMENTS.md`, 0.1.67):

- a suspended tenant's node is refused a restart (`TENANT_NOT_ACTIVE`) and keeps running without its
  credential; a new node is refused; stopped, it is not relaunched; reactivated, it comes back
  within a minute;
- `delete_tenant` with people charged marks nobody: `people_charged` counts them, they still answer
  (by id and by email), a deleted person comes back with `restore_ilk`, and `restore_tenant`
  brings the tenant back as it was;
- `delete_tenant` with a node answers `TENANT_HAS_NODES`;
- `set_ilk_tenant` moves an account and a contact out of the deleted tenant; a contact of a tenant
  that is not deleted is refused. **In 0.1.67 that refusal arrives with its detail after a `|` in
  `error_code` (`INVALID_TENANT_TRANSITION|…`); fixed for the next release.**

**0.1.66:** on PROD, 2026-10-14, through the public door with Cloud's token: 22 checks, all passing
(`lab/DEPLOYMENTS.md`, 0.1.66). What each node read was taken from the hive's vault audit, with four
IO.slack nodes of two tenants:

- a credential assigned to two nodes reaches those two only; taken from one, that one stops at once;
- general, it reaches every node of its tenant and none of another;
- assigning a node of another tenant, or a person, answers `INVALID_REQUEST`; another tenant
  assigning your key answers `KEY_NOT_FOUND`;
- a suspended tenant's nodes are refused it, and get it back when the tenant is reactivated;
- `get_ilk` and `list_ilks` summaries carry `pending_signup`.

**0.1.65:** on PROD, 2026-10-14, through the public door from outside, with Cloud's token: 30 checks,
all passing (`lab/DEPLOYMENTS.md`, 0.1.65). The data was made with 0.1.64 first: the same email from
two tenants, a unique email, a deleted person.

- the email held twice answers `ACCOUNT_AMBIGUOUS`;
- the unique email, asked from another tenant, answers its account (`existing: true`, still
  charged where it was);
- a new account asked from two tenants is one identity;
- `set_ilk_tenant` moves it (`changed: false` the second time), never to the root tenant;
- a person deleted before 0.1.65, once restored, is where `register_human` finds her;
- reads by id across tenants work;
- `delete_ilk` of a node answers `ILK_TYPE_MISMATCH`;
- `delete_tenant` with an account charged answers `TENANT_HAS_ACCOUNTS` (no longer since 0.1.67:
  deleting a tenant marks no one);
- once the operator purged the extra identity and the hive restarted, the email that had been held
  twice answered its one account.

**0.1.64:** on PROD, 2026-10-13, same door and token: 53 checks of §3–§5, all passing.

- the catalog lists 16 ops;
- rule 3: the envelope tenant only, never the root, the canonical spelling only;
- credentials:
  - `KEY_TAKEN` across tenants, even with the same value;
  - the listing shows metadata and never the value;
  - another tenant's key reads exactly like a missing one;
  - reserved keys are refused;
- `update_tenant` with its `*_TAKEN` codes;
- a node in the tenant with its own credential, which the node's teardown deleted.

Two of its checks no longer hold since 0.1.65: a person read by id from another tenant (now
visible, since a tenant is a cost center), and `delete_ilk` from another tenant
(`ILK_TENANT_MISMATCH`; now `delete_ilk` takes no tenant and acts on people only).

The status and error codes, and the fields those checks read, matched §5. The rest of each example
body comes from the code.
