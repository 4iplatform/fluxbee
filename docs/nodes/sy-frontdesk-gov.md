# SY.frontdesk.gov

SY.frontdesk.gov is the identity frontdesk: the node that turns an unknown or temporary sender
into a registered person. The router sends it the messages of senders with no usable identity.
It registers a person in one of two ways: a conversation in which its LLM collects the name and
email, or a structured `frontdesk_handoff` that it runs without an LLM. Either way the
registration is an `ILK_REGISTER` to SY.identity, in the tenant of the case.

Source: `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs` (the whole node), with
`nodes/gov/common/src/lib.rs` (case tenant, identity target) and
`nodes/gov/common/src/frontdesk_contract.rs` (the handoff contract).

| | |
| --- | --- |
| Binary | `/usr/bin/sy-frontdesk-gov` (crate `sy-frontdesk-gov`; `stage_bin` in `packaging/build-deb.sh`) |
| Node name | `SY.frontdesk.gov@<hive>` |
| Role | motherbee |
| systemd unit | `sy-frontdesk-gov.service`: `After=network.target rt-gateway.service sy-identity.service`, `Wants=rt-gateway.service sy-identity.service`, `Restart=always`, `RestartSec=5`, `TimeoutStopSec=15` |
| Listens on | nothing: it connects to the local router socket |
| State | `/var/lib/fluxbee/state/ai-nodes/SY.frontdesk.gov_<hive>/lancedb/threads/` |

## Where it runs

- It is listed only in `system_nodes.motherbee.nodes` (`packaging/hive.yaml.example`), before
  `SY.vault` as `validate_system_nodes` requires of a vault consumer. The orchestrator starts it
  and its watchdog restarts it like any system node.
- The unit has no role gate (`unit_role_gate` in `packaging/build-deb.sh` covers only sy-admin,
  sy-storage and sy-identity).
- Other code assumes the motherbee: the router takes its usage records only on a motherbee hive
  (`usage_producer_allowed` in `src/router/stream.rs`), and `add_hive` writes into a worker's
  hive.yaml `government.identity_frontdesk: "SY.frontdesk.gov@<motherbee>"`, so a worker's router
  sends its unknown senders to the motherbee's frontdesk (`add_hive_flow` in
  `src/bin/sy_orchestrator.rs`).

## How it starts

`main` and `run_unconfigured_bootstrap`:

1. Computes its own ILK, `deterministic_system_ilk_id("SY.frontdesk.gov@<hive>")`, from
   `/etc/fluxbee/hive.yaml`.
2. Resolves its name: `--node-name`, else `FLUXBEE_NODE_NAME`, `AI_NODE_NAME` or `NODE_NAME`,
   else `SY.frontdesk.gov@<hive_id>` from hive.yaml (`bootstrap_node_from_args`,
   `infer_frontdesk_node_name_from_hive`). The unit passes no arguments.
3. Builds its behavior from a baked document: an `ai_chat` behavior with the embedded prompt
   `FRONTDESK_DEFAULT_SYSTEM_PROMPT`, and the provider and model from the hive.yaml `ai` section.
   If that fails (hive.yaml unreadable, invalid `ai` section) it boots `FAILED_CONFIG`; otherwise
   `UNCONFIGURED`.
4. Connects to the router (`RouterDispatcher::connect_with_retry`, every 1 s); every message goes
   to the AI runtime channel (`build_frontdesk_gov_rpc_profile`).
5. Asks SY.vault for the AI key (`boot_self_configure`). Found: `CONFIGURED`. Not found: stays
   `UNCONFIGURED`, and the log says the deterministic handoff still works.
6. Runs the AI node runtime (`NodeRuntime::run_with_config`) with its own ILK on its replies.

## Who reaches it

**The router.** For a message addressed by resolution (`Destination::Resolve`), the router asks
the baked system policy whether to force it to the frontdesk (`policy/system.rego`, entrypoint
`fluxbee/system/frontdesk_route`):

- `frontdesk_route if not input.src_ilk_present`: the message carries no ILK at all;
- `frontdesk_route if input.registration_status == "temporary"`: the sender's ILK is temporary
  (provisioned, not yet registered).

`resolve_target_with_identity` and `apply_identity_pre_resolve` in `src/router/mod.rs` apply it.
The target is `government.identity_frontdesk` from hive.yaml (with `@<hive>` added if it has
none), else `SY.frontdesk.gov@<hive>` (`src/config/mod.rs`).

**Direct senders.**

- `IO.cloud` for Cloud's `register_human` (below).
- `IO.api` sends a handoff when its target is the frontdesk or its subject is not complete
  (`build_frontdesk_handoff_request` in `nodes/io/io-api/src/main.rs`; target
  `IO_API_FRONTDESK_TARGET`, default `SY.frontdesk.gov@motherbee`).
- IO nodes that use the shared gate (`gate_then_forward` in `nodes/io/common/src/frontdesk_gate.rs`,
  off unless `io.frontdesk.enabled`).

## Handling a message

`on_message`, in order:

1. `VAULT_SECRET_CHANGED`: re-resolve the AI key (a delete sets `UNCONFIGURED`).
2. Control plane (`msg_type` `system` or `admin`): `handle_control_plane` (see Config).
3. A `user` message whose payload parses as a `frontdesk_handoff`: the deterministic path.
4. A `user` message of a root-tenant case: a final answer (see Case tenant rules).
5. Everything else is the conversation. A `user` message needs the node `CONFIGURED`, else it
   gets `{"type":"error","code":"node_not_configured","message":"AI node is not configured yet. Retry later.","retryable":true,...}`
   (`node_not_configured_payload`), and a `meta.thread_id`, else `invalid_payload`.

## Conversational registration

- The prompt (`FRONTDESK_DEFAULT_SYSTEM_PROMPT`) asks for `name` and `email` (required) and keeps
  `phone` and `company_name` if given. It shows a summary, waits for a positive answer ("si, ok,
  confirmo, correcto, asi es, de acuerdo, dale"), calls `ilk_register` once, and stops. A failed
  registration ends as `completed_error` and is not retried ("STRICT NO-LOOP RULE"). Replies are
  "short in Spanish".
- Tools (`build_tool_registry`): `thread_state_get`, `thread_state_put`, `thread_state_delete`,
  scoped to the sender's `src_ilk` (registered only when there is one), and `ilk_register`, whose
  description says the tenant "comes with the case: never ask for one".
- Memory: the immediate memory is off (`ImmediateMemorySection::default`, `enabled: false`) and
  CONFIG_SET takes no behavior change, so what the model keeps between turns is the thread state,
  one JSON file per `src_ilk`.
- The reply to the person is the model's text. The result code comes from the thread state the
  model wrote (`build_frontdesk_result_for_conversation`): `completed` → `ALREADY_COMPLETE`,
  `completed_error` → `REGISTER_FAILED`, otherwise `MISSING_REQUIRED_FIELDS` or
  `IN_CONVERSATION`.

## Deterministic handoff

The contract (`frontdesk_contract.rs`):
`{type: "frontdesk_handoff", schema_version, operation, subject {display_name, email, phone, company_name, attributes}, tenant_id?, context?}`.

`handle_frontdesk_handoff` runs it with no LLM, no `CONFIGURED` state and no `thread_id`:

- `operation` must be `complete_registration`, else `INVALID_REQUEST`.
- The case tenant is checked first (next section).
- Missing name or email: `needs_input` / `MISSING_REQUIRED_FIELDS`, and the partial data is kept.
- Otherwise it calls `ilk_register`. Success: `REGISTERED` (or `MERGED`) and the thread state is
  deleted. Failure: the state is kept as `completed_error`; sending the handoff again retries.

**`register_human` from Fluxbee Cloud.** Cloud → SY.edge → `IO.cloud` (`handle_register_human`
in `nodes/io/io-cloud/src/main.rs`). It is a local IO.cloud operation that never goes through
SY.admin (`CLOUD_LOCAL_OPS` in `crates/fluxbee_sdk/src/cloud.rs`). IO.cloud:

1. validates the request; the root tenant is refused (`TENANT_ROOT_NOT_REGISTRABLE`);
2. provisions a temporary human ILK in the tenant Cloud named (`provision_ilk_in_scope`); an
   account that is already complete is answered at once (`existing_account_reply`);
3. sends the handoff as a `user` message to `SY.frontdesk.gov@motherbee`
   (`DEFAULT_FRONTDESK_TARGET`) with the temporary ILK as `src_ilk`, asks for a structured reply,
   and waits 20 s (`FRONTDESK_UNAVAILABLE`, `FRONTDESK_REJECTED`, `INVALID_FRONTDESK_RESPONSE` on
   failure);
4. answers Cloud with the frontdesk's verdict.

## Case tenant rules

The tenant of a case is the tenant SY.identity holds for the sender's ILK, read from the identity
SHM (`identity_shm_ilk_tenant`). `case_tenant_of` passes it to `resolve_case_tenant`:

```rust
let Some(ilk_tenant) = ilk_tenant.map(str::trim).filter(|tenant| looks_like_tenant_id(tenant))
else { return Err(CASE_TENANT_MISSING); };          // "missing_tenant_id"
if informed.is_some_and(|informed| informed != ilk_tenant) {
    return Err(CASE_TENANT_MISMATCH);                // "tenant_mismatch"
}
if ilk_tenant == fluxbee_sdk::DEFAULT_ROOT_TENANT_ID {
    return Err(TENANT_ROOT_NOT_REGISTRABLE);
}
```

- **Root-tenant case: stop.** `root_tenant_case_result` answers before any LLM, identity call or
  state write: `TENANT_NOT_REGISTRABLE` / `TENANT_ROOT_NOT_REGISTRABLE`, "No puedo completar el
  registro por este canal: no pertenece a ninguna organización." The ILK stays temporary.
- **Unknown or mismatched tenant.** The conversation goes on; `ilk_register` refuses without
  sending anything (`case_tenant_error_payload`). In a handoff the answer is immediate
  (`TENANT_UNRESOLVED` or `INVALID_REQUEST`).
- **Usage.** `usage_account` charges the case tenant with source `case`; when `case_tenant_of`
  fails, `AiAccount::charge` charges the root tenant as `fallback_root`
  (`crates/fluxbee_ai_sdk/src/usage.rs`).
- The frontdesk never creates a tenant and takes none from the person, the LLM or its config
  (doc comment of `resolve_case_tenant`).

## ILK_REGISTER

`IlkRegisterTool::call` checks a non-empty `src_ilk`, a name and an email, and the case tenant,
then sends:

```json
{ "ilk_id": "<src_ilk>", "ilk_type": "human", "tenant_id": "<case tenant>",
  "identification": { "display_name": "...", "email": "...", "phone": "...", "company_name": "...", "attributes": {} } }
```

as `msg_type: system`, `msg: ILK_REGISTER` to `SY.identity@motherbee` (`GovIdentityConfig`:
`GOV_IDENTITY_TARGET` / `IDENTITY_TARGET`, timeout 10 s, `GOV_IDENTITY_TIMEOUT_MS`). The answer
is `ILK_REGISTER_RESPONSE`.

On the identity side `ILK_REGISTER` is accepted only from names starting with
`SY.frontdesk.gov@` or `SY.orchestrator@`, plus the configured frontdesk name
(`src/bin/sy_identity.rs`; otherwise `UNAUTHORIZED_REGISTRAR`). SY.identity's own error code is
kept. The frontdesk maps outcomes to `REGISTERED`, `MERGED`, `IDENTITY_UNAVAILABLE` (transport
errors), `TENANT_UNRESOLVED`, `TENANT_NOT_REGISTRABLE`, `INVALID_REQUEST` or `REGISTER_FAILED`.
Logs show which fields were present, never their values.

## LLM, key and usage records

- **Provider and model**: the hive.yaml `ai` section (`HiveAiConfig::effective`).
- **Key**: from SY.vault, `resolve_resource(<provider resource type>, <root tenant>)` as itself:
  first a secret it owns, then one assigned to it, then the root tenant's general key. The value
  is a string or `{api_key}`.
- **Usage**: `UsageRecorder` appends one record per model call to the router stream subject
  `storage.usage`, with tenant source `case` (or `fallback_root`), the node's ILK, the sender as
  counterpart, the trace and the thread. SY.storage accepts `case` only from the frontdesk
  (`check_usage_record` in `src/bin/sy_storage.rs`). Handoffs and root-tenant answers use no LLM
  and leave no record.

## Config

There is nothing to configure. `handle_control_plane` answers:

| Request | Reply |
| --- | --- |
| `NODE_STATUS_GET` | `{status: ok, health_state: HEALTHY}`, whatever the node's state (`build_node_status_get_response`; `NODE_STATUS_DEFAULT_HEALTH_STATE` overrides it) |
| `CONFIG_GET` | `state`, `config_source: "baked"`, `api_key_source`, the provider and model |
| `CONFIG_SET` | refuses `ai`, `ai_providers`, `behavior`, `api_key`, `api_key_ref` (`config_not_accepted`); otherwise re-reads the key from the vault and persists nothing |
| `PING`, `STATUS` | `PONG`, `STATUS_RESPONSE` |

The router delivers `CONFIG_GET` and `CONFIG_SET` only from `SY.admin@motherbee`
(`policy/system.rego` rule (2)); SY.admin sends them for
`POST /hives/{hive}/nodes/SY.frontdesk.gov@motherbee/control/config-get` and `config-set`.

Environment: `RUST_LOG`; `GOV_IDENTITY_TARGET`, `GOV_IDENTITY_FALLBACK_TARGET`,
`GOV_IDENTITY_TIMEOUT_MS` (or the `IDENTITY_*` names); `AI_CONFIG_DIR`,
`AI_DYNAMIC_CONFIG_DIR`, `AI_ROUTER_SOCKET`, `AI_UUID_PERSISTENCE_DIR`, `AI_NODE_VERSION`.

## State on disk

- `/var/lib/fluxbee/state/ai-nodes/SY.frontdesk.gov_<hive>/lancedb/threads/`: one JSON file per
  person (`src_ilk`) in registration (`LanceDbThreadStateStore::path_for_node`). It holds the
  collected name, email, phone and company until a handoff succeeds.
- `/var/lib/fluxbee/state/nodes/`: the node UUID.
- Reads `/etc/fluxbee/hive.yaml` and the identity SHM.

## Failure behavior

- **No AI key**: `UNCONFIGURED`. Conversations get `node_not_configured`; handoffs still work.
  A later `VAULT_SECRET_CHANGED` for the key turns the conversation on.
- **Invalid hive.yaml or `ai` section**: `FAILED_CONFIG`; handoffs still work.
- **SY.identity down or slow** (10 s): `IDENTITY_UNAVAILABLE`. A handoff can be sent again; a
  conversation ends as `completed_error` (the prompt's no-loop rule).
- **Identity SHM unreadable**: the case tenant is unknown, so registration answers that the tenant
  is unresolved and usage goes to `fallback_root`; the conversation itself continues.
- **Handler errors**: each message runs under the AI runtime (`crates/fluxbee_ai_sdk/src/runtime.rs`:
  60 s per handler, up to 3 attempts for recoverable errors). A fatal error ends the runtime and
  the process; systemd restarts it after 5 s.

## Health

- `systemctl status sy-frontdesk-gov`; `journalctl -u sy-frontdesk-gov`. Lines to look for:
  `frontdesk-gov connected to router`, `AI token resolved from vault → Configured`,
  `frontdesk handoff REGISTERED (ILK_REGISTER complete)`.
- The real state is the `state` field of `CONFIG_GET` (through SY.admin's
  `node_control_config_get`); `NODE_STATUS_GET` always says `HEALTHY`.

## Links

- [SY.admin](sy-admin.md): the control-plane routes that reach it.
- [SY.orchestrator](sy-orchestrator.md): starts it; writes `government.identity_frontdesk` on workers.
- [SY.vault](sy-vault.md): where its AI key lives.
- [SY.identity](sy-identity.md): `ILK_REGISTER`, temporary ILKs, merges.
- [The router](rt-gateway.md): the forced route to the frontdesk.
- [ILK and tenant model](../ilk-tenant-model-v1.md): temporary ILKs, tenants as cost centers.
- [Frontdesk spec](../ai-frontdesk-gov-spec.md): the design.
- [IO.cloud API](../io-cloud-api.md): `register_human`.
- [Router stream](../router-stream-v1.md): usage records (§7).
- `nodes/gov/ai-frontdesk-gov/README.md`: the node's README.
