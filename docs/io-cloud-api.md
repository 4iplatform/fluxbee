# IO.cloud — API for Fluxbee Cloud

> **For the Fluxbee Cloud developers.** You do not need to know fluxbee's internals.
>
> Contract as of **fluxbee 0.1.65** (2026-10-14). Read from the code, operation by operation; §10
> lists what was also checked live on the PROD hive. §9 lists what changed for Cloud in each
> release. The model behind it: `docs/ilk-tenant-model-v1.md` (an identity is the actor and is
> never duplicated; a tenant is a cost center; companies, members and billing are Cloud's).

---

## 1. What IO.cloud is

A fluxbee **hive** is a private installation. Its services (the vault, the identity registry, the
node launcher) talk over an internal network with no HTTP door. Fluxbee Cloud still has to manage
its customers inside a hive: create a company, store its credentials, list them, launch a node.

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
  "tenant_id":  "tnt:<uuid>",        // the company the op acts on; see rule 3
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
| `create_tenant` | relay | none | create (or find) a company |
| `update_tenant` | relay | yes | rename your company or change its domain |
| `get_tenant` | local | yes | does your company exist, and how many identities are charged to it |
| `delete_tenant` / `restore_tenant` | relay | yes | mark your company deleted / undo it |
| `register_human` | local | yes | a person's account, charged to your company |
| `set_ilk_tenant` | relay | yes | charge a person to your company |
| `get_ilk` | local | none | fast existence + summary of an identity |
| `list_ilks` | local | yes | the identities charged to your company |
| `get_ilk_details` | relay | none | everything about one identity |
| `delete_ilk` / `restore_ilk` | relay | none | delete a person's account / undo it |
| `put_token` | relay | yes | store a credential |
| `list_tokens` | relay | yes | your credentials: metadata and use, never the value |
| `delete_token` | relay | yes | delete one credential |
| `provision_node` | relay | yes | launch an `IO.*` node |
| `list_cloud_actions` | local | none | this list, from the hive itself |

*relay* = IO.cloud turns it into a hive admin action; *local* = IO.cloud answers it itself, the fast
identity reads straight from the hive's registry in memory. Examples use:

```bash
URL="https://hive-k3m9x7q2.fluxbee.ai/e/ich:14b66389-d425-531c-a140-a591d25e8f39"
TOK="<the token>"
post() { curl -sS -m 35 -X POST "$URL" -H "Authorization: Bearer $TOK" \
              -H 'Content-Type: application/json' -d "$1"; echo; }
```

### 5.1 Companies (tenants)

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
| `tenant_id` (root) | required | the company you edit |
| `params.name` | optional | non-empty |
| `params.domain` | optional | lowercased; `""` or `null` clears it |

At least one of the two. **Any other field is refused** (`status`, `settings` and the sponsor are the
operator's).

```json
{"status":"ok","op":"update_tenant","result":{"status":"ok","tenant_id":"tnt:94cd37b8-…","sponsor_tenant_id":null}}
```

It does not return the tenant: read it again (`get_ilk_details` carries the `tenant` block).

| `error_code` | when |
|---|---|
| `TENANT_NAME_TAKEN` | another company has that name (case and spaces ignored), including one marked deleted |
| `TENANT_DOMAIN_TAKEN` | the same for the domain |
| `TENANT_NOT_FOUND` / `TENANT_DELETED` | |
| `INVALID_REQUEST` | nothing to change, an empty name, another field |

A refused update changes nothing. Keeping the company's current name or domain is not a conflict.

#### `get_tenant` (local)

`tenant_id` at the root. `result: {"exists": true, "tenant_id": "tnt:…", "ilk_count": 2}`.

#### `delete_tenant` / `restore_tenant`

`tenant_id` at the root, no params. Delete **marks** the company and, in cascade, the people known
only through its channels (a Slack user, a phone that wrote in): reversible, hidden, its name and
domain stay reserved. Only the operator purges.

A signup charged to it that was never completed is not marked: it stays pending, so its email is not
locked out, and the next `register_human` of that email (from any company) picks it up.
`pending_signups` counts them.

- delete: `{"status":"ok","tenant_id":…,"state":"deleted","already_deleted":false,"cascaded_ilks":3,"pending_signups":0}`;
  idempotent (`already_deleted:true` carries no `pending_signups`).
- restore: `{"status":"ok","tenant_id":…,"state":"active","restored_ilks":3}`.
- Errors:
  - `TENANT_HAS_ACCOUNTS`: Cloud accounts are charged to it. Reassign them (`set_ilk_tenant`) or
    delete them first: an account's email is reserved in every tenant, so a cascade must not take it.
  - `TENANT_HAS_NODES`: node identities live in it.
  - `TENANT_HAS_CHILDREN`: it sponsors other companies.
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
| `params.subject.email` | required | a real address; the person's key in the company |
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

  To charge it to your company, call `set_ilk_tenant`.
- **A signup that was never completed** (from any company) is picked up by the next
  `register_human` of that email, charged to the new company.
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

`tenant_id` at the root (the company to charge the person to) + `params.ilk_id`. A tenant is a cost
center, so this grants and revokes nothing; the person keeps every channel and lookup.

```json
{"status":"ok","op":"set_ilk_tenant","result":{"status":"ok","ilk_id":"ilk:…","tenant_id":"tnt:…",
 "previous_tenant_id":"tnt:…","changed":true}}
```

- Already charged there: `changed:false`.
- Only a complete person's account; every change is recorded in the hive.
- Errors:
  - `DUPLICATE_EMAIL`: another person charged to that company has the same email in their
    identification.
  - `TENANT_NOT_ACTIVE` (pending or suspended), `TENANT_DELETED`, `TENANT_NOT_FOUND`,
    `TENANT_ROOT_PROTECTED`.
  - `INVALID_TENANT_TRANSITION` (not a complete account), `ILK_TYPE_MISMATCH` (not a person),
    `ILK_NOT_FOUND`.

#### `get_ilk` (local, fast)

Exactly one selector: `params.ilk_id` (`ilk:<uuid>`) or `params.email` (the account's email; ASCII
case-insensitive). No tenant needed.

| request | answer |
|---|---|
| `ilk_id` | `{"exists":true,"ilk":{…summary…}}`, whatever tenant it is charged to |
| `email` | the account: `{"exists":true,"ilk":{…},"matches":[{…}]}`; nobody: `exists:false` |

The summary is `{ilk_id, ilk_type, registration_status, tenant_id, display_name}`; `tenant_id` is
what the person is charged to.

`matches` holds more than one entry only for an email several people still hold from before
0.1.65 (the operator resolves those); then `ilk` is `null`:

```json
{"result":{"exists":true,"ilk":{…},"matches":[{…}]}}          // the account (the usual case)
{"result":{"exists":true,"ilk":null,"matches":[{…},{…}]}}     // several people, before 0.1.65
{"result":{"exists":false,"ilk":null,"matches":[]}}           // first time: register_human
```

#### `list_ilks` (local, fast)

`tenant_id` at the root: the identities charged to it.
`result: {"tenant_id":…,"count":2,"ilks":[{ilk_id, ilk_type, registration_status, display_name}]}`.

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
- Errors: `ILK_NOT_FOUND`, `ILK_TYPE_MISMATCH` (not a person), `ILK_NOT_DELETED` (restore),
  `TENANT_DELETED` (restore the company it is charged to first), `ILK_KEY_CONFLICT` (restoring it would
  bring back a second account for its email).
- A person deleted before 0.1.65 comes back with their account where `register_human` finds it,
  unless another identity holds that email.

### 5.3 Credentials (the hive's vault)

**Keys share one namespace for the whole hive.** A key belongs to the company that wrote it. Name
yours `<provider>:<tenant_id>:<identifier>`, which also makes them easy to filter.

#### `put_token`

| field | | |
|---|---|---|
| `tenant_id` (root) | required | the credential's company |
| `params.key` | required | 1–256 bytes of `[a-z0-9:_-]`, starting with `[a-z0-9]` |
| `params.value` | required | any JSON but `null`, ≤ 1 MiB serialized |
| `params.resource_type` | required (or in `params.metadata`) | normalized: lowercase, anything else → `_` (`"Google Calendar"` → `google_calendar`) |
| `params.owner_node` | optional | an `IO.*` node **that already exists**: only that node reads it. Omitted: the company's pool, any node of the company reads it |
| `params.metadata` | optional | `description` (string) and `tags` (string array) |

```json
{"status":"ok","op":"put_token","result":{"status":"ok","key":"gmail:tnt:94cd37b8-…:sales","version":1,"changed":true}}
```

- **The same value again does nothing**: `changed:false`, the version stays, **and the metadata is
  not updated either**. To change `description` or `tags`, change the value.
- **Metadata is a fixed schema.** Only `description` and `tags` are yours; the vault sets
  `tenant_id`, `resource_type`, `created_by` (the hive-side writer, never your user), `created_at`
  and `updated_at`. Other fields are dropped; a wrong type fails the write with `INVALID_VALUE`. Your
  provenance (who connected it, which account) goes in `tags`, e.g. `source:cloud`, `by:<ilk>`.
- **Order:** `provision_node` **before** a `put_token` with `owner_node` (the node must exist to own
  it). A credential the node needs at its first start goes in the pool.
- The value's shape is not checked anywhere: a wrong shape fails when the node uses it. Shapes the
  hive's nodes expect today: `openai`/`anthropic` → `{"api_key":…}`; `slack` →
  `{"app_token":…,"bot_token":…}`; `postgres` → `{"postgres_url":"postgresql://user:pass@host:5432"}`
  (no database name).
- It does not check that the company exists.

| `error_code` | when |
|---|---|
| `KEY_TAKEN` | the key belongs to another company. It never moves |
| `INVALID_REQUEST` | key charset/length, value over 1 MiB, missing field, `owner_node` not `IO.*` or not registered, a reserved infrastructure key (`edge_channel_secret:*`, `edge_tls*`, `ssh:*`, `edge_fanout_verify:*`) |
| `INVALID_VALUE` | metadata of the wrong type |
| `STORAGE_ERROR`, `ENCRYPTION_ERROR`, `MASTER_KEY_NOT_AVAILABLE` | the vault |

#### `list_tokens`

| field | | |
|---|---|---|
| `tenant_id` (root) | required | lists **only** that company's credentials |
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
- `metadata.ilk` appears only for a credential owned by a node (`owner_node`): that node's identity.
- A company with no credentials, or one that does not exist: `count:0`.

#### `delete_token`

`tenant_id` at the root + `params.key`.

```json
{"status":"ok","op":"delete_token","result":{"status":"ok","key":"gmail:tnt:94cd37b8-…:sales","deleted":true}}
```

| `error_code` | when |
|---|---|
| `KEY_NOT_FOUND` | the key does not exist **or belongs to another company**: same code and same text, on purpose |
| `INVALID_REQUEST` | missing key, a reserved infrastructure key |

The hive announces the deletion to its nodes. A node that already loaded the value may keep using it
until it reloads; revoking the credential at the provider is what makes it useless.

### 5.4 Nodes

#### `provision_node`

| field | | |
|---|---|---|
| `tenant_id` (root) | required | the node's company |
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
| `RUNTIME_NOT_AVAILABLE` | the runtime or version cannot be resolved |
| `INVALID_REQUEST` | not `IO.*`, runtime not derivable, bad version |
| `IDENTITY_REGISTER_FAILED`, `CONFIG_WRITE_FAILED`, `SERVICE_FAILED`, `SPAWN_FAILED` | the hive could not launch it |

### 5.5 `list_cloud_actions` (local)

No tenant, no params. `result: {"actions":[{"op":…,"category":"relay"|"local","summary":…}, …]}`:
every op of §5 with a one-line summary, from the hive itself.

---

## 6. Timeouts and retries

```text
you / the edge wait   30 s
IO.cloud waits        20 s for the hive (25 s for provision_node)
```

A request that takes between 20 (25) and 30 s answers `ADMIN_UNAVAILABLE` **while it may still be
completing inside**. Treat it as **unknown**, not failed:

- `create_tenant`, `update_tenant`, `put_token` (same value), `delete_*`, `restore_*`: safe to retry.
- `delete_token`: a retry of a delete that did complete answers `KEY_NOT_FOUND`.
- `provision_node`: a retry answers `NODE_ALREADY_EXISTS`, which may mean the timed-out attempt
  worked. There is no idempotency key (`request_id` is for traces only); ask the operator.

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

1. **One platform token; the tenant is trusted as sent.** No per-company credentials and no
   end-user identity: whoever holds the token acts on any company. Guard it as a platform secret.
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
   (`docs/ilk-tenant-model-v1.md`). Token spending per tenant is not recorded yet (stage 5 there).

---

## 9. What changed for Cloud

### 0.1.65

- **One email is one account, in the whole hive.** `register_human` for an email that already has
  one answers `existing: true` with that account and its tenant, and creates nothing.
  `ACCOUNT_AMBIGUOUS` and `ACCOUNT_IN_ROOT_TENANT` mark data from before.
- **New op `set_ilk_tenant`:** charge a person to your company.
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
- **`put_token` can no longer overwrite another company's key** (`KEY_TAKEN`), and
  `edge_fanout_verify:*` joined the reserved keys.
- **Every error carries `error_code`** and every reply carries `op`. IO.cloud's own validation
  errors are `INVALID_REQUEST`; a hive that does not answer in time is `ADMIN_UNAVAILABLE`.
- Fixed earlier, in 0.1.50: the admin's HTTP API decodes `%3A` in query strings (it affected tools
  that called the admin directly, not IO.cloud).

---

## 10. Checked live

**0.1.65:** on PROD, 2026-10-14, through the public door from outside, with Cloud's token: 30 checks,
all passing (`lab/DEPLOYMENTS.md`, 0.1.65). The data was made with 0.1.64 first: the same email from
two companies, a unique email, a deleted person.

- the email held twice answers `ACCOUNT_AMBIGUOUS`;
- the unique email, asked from another company, answers its account (`existing: true`, still
  charged where it was);
- a new account asked from two companies is one identity;
- `set_ilk_tenant` moves it (`changed: false` the second time), never to the root tenant;
- a person deleted before 0.1.65, once restored, is where `register_human` finds her;
- reads by id across companies work;
- `delete_ilk` of a node answers `ILK_TYPE_MISMATCH`;
- `delete_tenant` with an account charged answers `TENANT_HAS_ACCOUNTS`;
- once the operator purged the extra identity and the hive restarted, the email that had been held
  twice answered its one account.

**0.1.64:** on PROD, 2026-10-13, same door and token: 53 checks of §3–§5, all passing.

- the catalog lists 16 ops;
- rule 3: the envelope tenant only, never the root, the canonical spelling only;
- credentials:
  - `KEY_TAKEN` across companies, even with the same value;
  - the listing shows metadata and never the value;
  - another company's key reads exactly like a missing one;
  - reserved keys are refused;
- `update_tenant` with its `*_TAKEN` codes;
- a node in the company with its own credential, which the node's teardown deleted.

Two of its checks no longer hold since 0.1.65: a person read by id from another company (now
visible, since a tenant is a cost center), and `delete_ilk` from another company
(`ILK_TENANT_MISMATCH`; now `delete_ilk` takes no tenant and acts on people only).

The status and error codes, and the fields those checks read, matched §5. The rest of each example
body comes from the code.
