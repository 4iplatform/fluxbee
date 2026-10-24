# SY.frontdesk.gov - Technical specification (v2)

Status: current (updated 2026-10-02: the case's tenant, no tenant creation, merge by email,
nobody registers in the root tenant; 2026-10-22: a handoff only from an IO node, and only an
email the channel vouches for merges, `docs/control-path-hardening-v1.md` §7)

> Deprecation note:
> `frontdesk_result` is deprecated as the output contract of `SY.frontdesk.gov`.
> Current state:
> - without an envelope, frontdesk responds `payload.type = "text"`
> - with `meta.context.response_envelope`, it responds `payload.type = "text"` with structured JSON
> Mentions of `frontdesk_result` in old documents or historical notes must be read as legacy/superseded.

## 1. Role

`SY.frontdesk.gov` is the system's specialized identity/onboarding node.

Its job is to:

- receive cases with a `temporary` or incomplete identity;
- complete the human registration when the case already carries enough data;
- continue the conversational flow when data is still missing;
- run the upgrade via `ILK_REGISTER`;
- respond with an application contract compatible with the current consumer.

It does not replace a general-purpose `AI.*`.

## 2. Identity and naming

- Fixed L2 name: `SY.frontdesk.gov`
- Qualified name: `SY.frontdesk.gov@<hive_id>`

Operational rule:

- there is a single canonical instance per hive;
- it must be considered part of the `system default set`;
- it can receive traffic routed to it by IO/AI nodes or other business layers.

## 3. Responsibilities

`SY.frontdesk.gov`:

- accepts two official input contracts:
  - `payload.type = "text"`
  - `payload.type = "frontdesk_handoff"`
- reuses state by `src_ilk`;
- completes or corrects the human's minimal data;
- calls `ilk_register` when the case is ready, in the case's tenant (section 4.3);
- responds by default with `payload.type = "text"`;
- when it receives `meta.context.response_envelope`, it can respond with a structured `payload.type = "text"` compatible with that envelope.

It must not:

- write directly to the identity DB;
- create tenants. It does not send `TNT_CREATE`, and `SY.identity` would not allow it either: `TNT_CREATE`,
  `TNT_UPDATE` and `TNT_SET_SPONSOR` are not in its allowlist. A tenant is created by the operator through
  `SY.admin`, or by Fluxbee Cloud with its own process (io.cloud's `create_tenant`, which goes through
  `SY.admin`);
- take the tenant from the person, from the LLM, from its config or from its environment (section 4.3);
- register anybody in the root tenant (section 4.4);
- invent ILKs;
- assume that all consumers need the same output contract.

## 4. Official inputs

### 4.1 Conversational

Required carrier:

- `meta.src_ilk`
- `meta.thread_id`
- payload `text`

Use:

- when the case comes from a normal human channel;
- when data is missing and frontdesk must ask for it.

### 4.2 Structured

Required carrier:

- `meta.src_ilk`
- `meta.thread_id`
- `meta.msg = "frontdesk_handoff"`, the producer's mark
- payload `frontdesk_handoff`

A payload in this shape without the mark is not a handoff: an IO node may relay a payload someone
else wrote, so it goes to the conversational path (section 4.1), with a warning in the log.

Canonical shape:

```json
{
  "type": "frontdesk_handoff",
  "schema_version": 1,
  "operation": "complete_registration",
  "subject": {
    "display_name": "Juan Perez",
    "email": "juan@example.com",
    "phone": "+5491100000001",
    "company_name": "Acme Support",
    "attributes": {
      "crm_customer_id": "crm-123"
    }
  },
  "tenant_id": "tnt:...",
  "context": {
    "source_node": "IO.api.support@motherbee",
    "external_user_id": "crm:123"
  }
}
```

Rules:

- only an IO node sends it: the sender the router stamps on the message (`routing.src_l2_name`)
  must start with `IO.`. Any other sender (an `AI.*`, `WF.*` or `SY.*` node, or no stamped name)
  gets `INVALID_REQUEST`, and nothing is registered or saved;
- `subject.email` is vouched: the IO node answers for it (io.api's integrator, who holds the
  bearer; io.cloud, for the login Fluxbee Cloud authenticated). It is the only vouched email
  (section 7.1);
- `operation = "complete_registration"` is the only operation settled in this version;
- `subject.display_name` and `subject.email` are required for that operation;
- `subject.company_name` is data about the person: it never chooses or creates a tenant;
- `tenant_id` is optional. If present, it has to be the case's tenant (section 4.3); if it is not,
  nothing is registered (`INVALID_REQUEST`, `error_code = tenant_mismatch`).

### 4.3 The case's tenant

The tenant of a registration is the one `SY.identity` holds for the case's temporary ILK
(`meta.src_ilk`). It was set by the IO node that created that ILK with `ILK_PROVISION`:

- an IO node provisions with its own tenant (`FLUXBEE_NODE_TENANT_ID`, the one the
  orchestrator gave it);
- io.api and io.cloud provision with the tenant they were called for: io.api, the tenant of its
  integration; io.cloud, the `tenant_id` of the Cloud envelope, the tenant Cloud created;
- an IO node without a tenant provisions in the default tenant (`fluxbee`), which is the root tenant:
  that person cannot be registered (section 4.4).

Rules:

- the frontdesk reads that tenant from its hive's identity SHM (`jsr-identity-<hive>`), which
  `SY.identity` writes before it answers the `ILK_PROVISION`;
- `frontdesk_handoff.tenant_id`, if present, is only compared with that tenant;
- on the conversational path the only source is the ILK's tenant: the thread state is written by
  the LLM, so it is not a tenant source;
- if the ILK's tenant cannot be read (the ILK is not in the SHM, or the SHM cannot be read), no
  `ILK_REGISTER` is sent and nothing is created. The response is explicit: `status = "error"`,
  `result_code = TENANT_UNRESOLVED`, `error_code = missing_tenant_id`;
- there are no other sources: the tool has no `tenant_id` or `identity_candidate.tenant_hint`, and
  neither `effective_config.tenant_id` nor `GOV_IDENTITY_TENANT_ID` exists;
- a `pending`, `suspended` or deleted tenant does not receive registrations: `SY.identity` responds
  `TENANT_PENDING`, `TENANT_SUSPENDED` or `TENANT_DELETED`, which are final results
  (`REGISTER_FAILED`, not retryable).

### 4.4 Root tenant

Nobody registers in the root tenant (`tnt:00000000-0000-0000-0000-000000000001`, the `fluxbee`
of the system and its nodes; operator decision 2026-10-02). If the case's tenant is the root:

- the frontdesk does not call `SY.identity` or save state, and it responds with a final result that is
  not retryable: `status = "error"`, `result_code = TENANT_NOT_REGISTRABLE`,
  `error_code = TENANT_ROOT_NOT_REGISTRABLE`, `registration_status = "temporary"` and the message
  "No puedo completar el registro por este canal: no pertenece a ninguna organización." ("I cannot complete the registration through this channel: it does not belong to any organization.");
- on the handoff, before asking for missing data; on the conversational path, before opening the
  conversation (the LLM is not needed: it responds the same even if the node is `UNCONFIGURED`); the
  `ilk_register` tool also rejects it;
- `SY.identity` applies the same rule for any caller (`TENANT_ROOT_NOT_REGISTRABLE`), and
  the frontdesk translates it into the same result.

The person stays `temporary`: `ILK_PROVISION` in the root tenant still works. In the root
tenant only the core IO nodes run (io.cloud, io.blob and io.web), and none provisions people there:
the orchestrator rejects any other IO node with `TENANT_ROOT_NOT_ALLOWED` (operator decision
2026-10-02, A-43). That leaves
the case of an IO node that provisions without a tenant (section 4.3): if it routes through `Resolve`, every message
from that person comes back to the frontdesk and gets the same answer. This frontdesk rule stays as
defense in depth.

## 5. State per thread

It keeps state by `src_ilk` using `thread_state_*`.

Minimal store:

- `thread_state_get`
- `thread_state_put`
- `thread_state_delete`

The effective state key is tied to the `src_ilk`.

Current minimal state:

```json
{
  "status": "collecting|awaiting_confirmation|completed|completed_error",
  "collected": {
    "name": null,
    "email": null,
    "phone": null,
    "company_name": null
  },
  "tenant_id": null,
  "registration_status": null,
  "register_attempted": false,
  "register_error": null
}
```

`tenant_id` is the case's tenant. The handoff saves it also when the registration fails
(`completed_error`), so a retry keeps reporting it. It is informational: the tenant of each attempt
is read again from the ILK (section 4.3).

`collected.email` is only ever a claim, whoever wrote it: the LLM writes this state (section 7.1).

The email a handoff vouched for waits apart, in the same store under `vouched_email:<src_ilk>`,
until the case registers: only the frontdesk's code writes it, and the LLM's state tools, scoped to
`src_ilk`, never reach it. A registration of the case (the handoff's or the conversation's) takes
it from there and deletes it once `SY.identity` accepts.

## 6. Internal working modes

### 6.1 `register_automatic`

It is activated when a `frontdesk_handoff` comes in.

Rule:

- it does not open an unnecessary conversation;
- it refuses a sender that is not an IO node (section 4.2) before anything else;
- it first resolves the case's tenant (section 4.3); with no tenant, with one that does not match, or with the
  root tenant (section 4.4), it responds with the error and saves nothing;
- it takes what the handoff lacks from the previous state, if any; an email taken from there stays
  a claim, even when the handoff carries none (section 7.1). An email an earlier handoff of the case
  vouched for (section 5) stays vouched;
- if it already has the full minimum, it tries to register;
- if it is still incomplete, it responds `text` with the corresponding human message.

### 6.2 Conversational

It is activated with `payload.type = "text"`.

Rule:

- a root-tenant case does not open a conversation: it gets the final result of section 4.4, without
  the LLM;
- it can collect missing data;
- it can ask for confirmation;
- on a positive confirmation it calls `ilk_register`;
- it responds with `text`.

## 7. Completion tool

The node uses the `ilk_register` tool.

Minimal payload:

- `identity_candidate.name`
- `identity_candidate.email`

Optional:

- `identity_candidate.phone`
- `identity_candidate.company_name`
- `identity_candidate.attributes` (free-form data; goes as is to `identification.attributes`)

There are no more fields. The ILK is not a parameter: the tool acts only on the case's own, the
message's `meta.src_ilk`, never on one the model names, and a message without one registers nobody
(`missing_src_ilk`). Nor is the tenant (section 4.3). The case state is indexed by `src_ilk`
(section 5); the tool does not receive `thread_id`.

The emails go to `SY.identity` in two keys (section 7.1). `identification.email` is the case's
vouched email: the handoff's own `subject.email`, or the one an earlier handoff left (section 5).
`identification.claimed_email` is `identity_candidate.email` when it is another one (typed in the
conversation, or taken from the state). A key without a value goes as `null`.

Successful response:

```json
{
  "status": "ok",
  "registered": true,
  "merged": false,
  "ilk_id": "ilk:...",
  "tenant_id": "tnt:...",
  "identity_payload": { "status": "ok", "ilk_id": "ilk:...", "merged": false }
}
```

### 7.1 Vouched and claimed emails; a vouched email already registered merges

Only an email the channel vouches for identifies the person (`docs/control-path-hardening-v1.md`
§7, A-42):

- **Vouched:** the `subject.email` of a handoff from an IO node (section 4.2), kept for the case
  until it registers (section 5). It goes in `identification.email`, the person's key within the
  tenant.
- **Claimed:** an email typed in the conversation, or one a handoff without an email takes from
  the state (section 5). It goes in `identification.claimed_email`. `SY.identity` stores and shows
  it, but never indexes it, looks it up or merges on it: a typed email can neither take over a
  registered person nor reserve an address for a later merge.
- **The registration completes either way.** A claimed email is never the reason for a
  `DUPLICATE_EMAIL` or an `ILK_DELETED`, and when someone already holds it the frontdesk registers
  a new person anyway. The answer is the same whether or not the email is known (`REGISTERED`,
  never `MERGED`), so a channel cannot probe which emails are registered. If it is the same
  person, joining the two ILKs is an explicit step of the operator.

If the registration carries a vouched email that another human ILK of the same tenant already has,
the person is already registered and the registration does not fail: `SY.identity` does the merge
inside the same `ILK_REGISTER` (details in `10-identity-v2.md` 6.5):

- the existing ILK keeps its id, its tenant and its `complete` state;
- the temporary ILK's channels move to that ILK, and the temporary one stays as its alias until
  `merge_alias_ttl_secs` expires (after that, the alias GC marks it deleted);
- the `identification` fields that are empty in the existing ILK are filled with those of the
  registration; the ones that already have a value are not touched;
- `SY.identity` responds `merged: true`, the existing ILK's `ilk_id` and the temporary ILK's
  `merged_from_ilk_id`; frontdesk responds `MERGED` (section 9) and tells the person that they were already registered and
  that this channel is now associated with their registration.

An explicit update does overwrite: an `ILK_REGISTER` of the already-registered ILK itself (for example a
repeated `register_human` from Cloud for the same email) replaces its whole `identification`; the last
one wins.

## 8. Default output

Default output when there is no envelope:

- `meta.type = "user"`
- `payload.type = "text"`

Minimal canonical shape:

```json
{
  "type": "text",
  "content": "Necesito tu email para continuar."
}
```

Rule:

- without an envelope, frontdesk must not expose a custom structured payload as a mandatory contract for the consumer;
- the human message must go out as normal text.

## 8.1 Opt-in structured output through the envelope

When the incoming message carries `meta.context.response_envelope`, `SY.frontdesk.gov` can respond with:

- `meta.type = "user"`
- `payload.type = "text"`
- `payload.content = "<structured json>"`

In this first cut, the supported and validated shape is:

```json
{
  "success": true,
  "human_message": "Registro completado correctamente."
}
```

or, if there was a functional block/error:

```json
{
  "success": false,
  "human_message": "No pude completar el registro en este momento.",
  "error_code": "register_failed"
}
```

Rules:

- the envelope is opt-in and hop-by-hop;
- if there is no envelope, frontdesk responds normal `text`;
- `success` is `true` only with `status = "ok"` (`REGISTERED`, `MERGED`, `ALREADY_COMPLETE`);
- `error_code` is the `result_code` in lowercase (section 9) and is only present when `success = false`;
- `error_code` must not be emitted as `null` in v1;
- in this cut, frontdesk only declares explicit support for:
  - `success:boolean`
  - `human_message:string`
  - `error_code:string`
- if the envelope is invalid or asks for a shape frontdesk cannot meet, it must fail with `invalid_response_contract`.

## 9. Result semantics

Closed states:

- `ok`
- `needs_input`
- `error`

Closed `result_code` values:

| `result_code` | state | when |
| --- | --- | --- |
| `REGISTERED` | `ok` | the temporary ILK became `complete` |
| `MERGED` | `ok` | the vouched email already belonged to another ILK of the tenant: this channel moved to that ILK (section 7.1); `ilk_id` is that ILK. A claimed email never gives it |
| `ALREADY_COMPLETE` | `ok` | conversational: the thread state already says `completed` |
| `MISSING_REQUIRED_FIELDS` | `needs_input` | `display_name` and/or `email` are missing |
| `IN_CONVERSATION` | `needs_input` | conversational turn with no registration (greeting, request for a piece of data) |
| `INVALID_REQUEST` | `error` | invalid request: a handoff from a sender that is not an IO node, unsupported operation, invalid data, `tenant_mismatch`, `INVALID_*` from `SY.identity` |
| `TENANT_UNRESOLVED` | `error` | there is no tenant for the case (`missing_tenant_id`, section 4.3); nothing was registered or created |
| `TENANT_NOT_REGISTRABLE` | `error` | the case's tenant is the root tenant (`TENANT_ROOT_NOT_REGISTRABLE`, section 4.4); nothing was registered and the person stays `temporary` |
| `REGISTER_FAILED` | `error` | `SY.identity` rejected the registration with a final verdict |
| `IDENTITY_UNAVAILABLE` | `error` | transient failure, the only retryable one |

`human_message` is always mandatory.

Registration failures (`ILK_REGISTER`):

- `IDENTITY_UNAVAILABLE` only for transient failures, the only retryable ones: `SY.identity` did not respond (`UNREACHABLE`, `TTL_EXCEEDED`, `TIMEOUT`, `IDENTITY_ERROR`) or responded `NOT_PRIMARY`, `DB_NOT_READY` or `DB_WRITE_FAILED`;
- any other code `SY.identity` responds with is a final verdict, not retryable: `INVALID_*` -> `INVALID_REQUEST`; `TENANT_ROOT_NOT_REGISTRABLE` -> `TENANT_NOT_REGISTRABLE` (the same result as when the frontdesk sees it); the rest (`TENANT_PENDING`, `TENANT_SUSPENDED`, `TENANT_DELETED`, `ILK_DELETED`, `ILK_NOT_FOUND`, `SYSTEM_ILK_PROTECTED`, `DUPLICATE_*`, `UNAUTHORIZED_REGISTRAR`, ...) -> `REGISTER_FAILED`;
- `error_code` keeps the `SY.identity` code as is (or the tool's: `missing_src_ilk`, `invalid_identity_candidate`, `missing_tenant_id`, `tenant_mismatch`, and `TENANT_ROOT_NOT_REGISTRABLE`, the same code `SY.identity` uses).

`DUPLICATE_EMAIL` is left only for a vouched email that cannot be merged: the registration's ILK is not a
temporary human ILK of the same tenant (for example an ILK that is already `complete` and wants someone else's email).

`missing_fields`:

- if `status = "needs_input"`, it contains the exact list of missing fields;
- in any other case, it must be `[]`.

## 10. Integration with consumers

### 10.1 Conversational consumers

They must:

- treat frontdesk's default output as normal text;
- not assume a single output form if the hop uses an envelope.

### 10.2 Non-conversational consumers

They must:

- if they do not use an envelope, consume normal text;
- if they use an envelope, consume the structured response defined by that hop.

### 10.3 `IO.api`

`IO.api` must:

- build `frontdesk_handoff`, with its integration's `tenant_id` (the same one it provisioned the ILK with);
- use `SY.frontdesk.gov` as an intermediate step when the subject is not fully registered;
- add `meta.context.response_envelope` for the synchronous regularization hop;
- consume the resulting structured response and:
  - if `success = true`, let the original message continue to `dst_final`;
  - if `success = false`, map it to the `IO.api` HTTP response.

No `IO.api` instance runs in the root tenant: its tenant launches it, and the orchestrator rejects it
in the root (`TENANT_ROOT_NOT_ALLOWED`; operator decision 2026-10-02, A-43). If a `by_data` subject
with a root-tenant ILK arrives anyway, the frontdesk responds `success = false`,
`error_code = tenant_not_registrable` (section 4.4), and the message does not continue.

### 10.4 io.cloud `register_human`

io.cloud provisions the temporary ILK in the tenant of the Cloud envelope (channel `cloud`, address = the
email) and sends the frontdesk the `frontdesk_handoff` with that same `tenant_id`. It asks for `ilk_id` and
`merged` in the envelope, so its response to Cloud carries the ILK the person ended up in: after
a `MERGED`, the one that already had the email (with `merged_from_ilk_id`, the temporary one it provisioned).

An envelope with the root tenant is rejected before anything is provisioned (`TENANT_ROOT_NOT_REGISTRABLE`),
so no temporary ILK with the person's email is left in the root tenant.

## 11. Configuration and operation

`SY.frontdesk.gov` is an autonomous system node:

- it runs as a system service (`sy-frontdesk-gov.service`) with no arguments and no node YAML;
  it takes `node_name` from `hive.yaml` as `SY.frontdesk.gov@<hive_id>`;
- the prompt is embedded in the binary; it is not configured;
- the AI provider and model are the hive's: the `ai` section of `hive.yaml` (with a fallback
  baked in if it is missing);
- the provider key is read from `SY.vault` by the provider's `resource_type` (`openai` or
  `anthropic`), in the root tenant, at startup and on each `VAULT_SECRET_CHANGED`. Without a key the node
  stays `UNCONFIGURED`: the structured path keeps working and the conversational path responds
  `node_not_configured`;
- `CONFIG_SET` does not accept configuration: it rejects `ai`, `ai_providers`, `behavior`, `api_key` and
  `api_key_ref` with `config_not_accepted`, ignores any other field, reads the key from the vault again
  and persists nothing;
- `CONFIG_GET` reports the state, the provider and the model, and whether the key was resolved.

Lifecycle: it is a singleton per hive and is updated with the `.deb`, like the rest of the `SY.*`; it is not
deleted or respawned like an `IO.*`. It keeps no local config: its only state is the threads' state
(section 5).

## 12. Minimal observability

It must expose:

- `state`: `UNCONFIGURED|CONFIGURED|FAILED_CONFIG`
- suggested counters (pending, section 13):
  - `threads_active`
  - `identity_upgrades_ok`
  - `identity_upgrades_error`
  - `frontdesk_handoff_ok`
  - `frontdesk_handoff_needs_input`

The logs carry no personal data: from the registration they log `ilk_id`, `tenant_id`, the codes and
which fields came in, never their values.

## 13. Open items

- A way to turn a claimed email into a vouched one (for example a one-time code sent to it). Until
  then a typed email stays a claim and never merges (section 7.1), and joining a claimant to the
  person who holds the email is the operator's explicit step, which SY.admin does not offer yet.
- The counters of section 12 (G6).
- Conversational retries: the prompt's "no loop" rule makes a transient error
  terminal (G8).
