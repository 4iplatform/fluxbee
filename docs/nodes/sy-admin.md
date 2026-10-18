# SY.admin

SY.admin is the control-plane gateway of a Fluxbee mesh. The operator's actions, and the admin
actions that nodes ask for, reach it two ways: an HTTP API on the motherbee's loopback, and the
`ADMIN_COMMAND` message over the mesh. SY.admin does little work itself. For each action it finds
the node that owns it (SY.orchestrator, SY.config.routes, SY.identity, SY.vault, SY.opa.rules,
SY.wf-rules, SY.timer, SY.storage, SY.edge, IO.blob), forwards the request, and returns one
envelope. It also runs the executor, which carries out executor plans one step at a time with an
LLM.

Source: `src/bin/sy_admin.rs`. The Fluxbee Cloud allowlist is `CLOUD_EXPOSED_ACTIONS` in
`crates/fluxbee_sdk/src/cloud.rs`.

**Live schema.** `GET /admin/actions` returns every action with its routes, request contract,
executor schema and an example; `GET /admin/actions/{action}` returns one
(`build_admin_actions_catalog_response`, `build_admin_action_help_response`). The
[action table](#action-table) at the end of this page is generated from the same functions.

| | |
| --- | --- |
| Binary | `/usr/bin/sy-admin` (`packaging/build-deb.sh`, `RUST_BINS` entry `sy-admin:sy_admin`) |
| Node name | `SY.admin@motherbee` (`admin_node_name`) |
| Role | motherbee only |
| systemd unit | `sy-admin.service`: `ExecCondition` on `role: motherbee`, `Restart=always`, `RestartSec=5`, `TimeoutStopSec=15` (`gen_unit`, `unit_role_gate`) |
| Listens on | HTTP `127.0.0.1:8080` by default; reaches the mesh through the local router socket |
| State | `/var/lib/fluxbee/state/sy-admin/`, `/var/lib/fluxbee/state/admin-executor/`, `/var/lib/fluxbee/nodes/SY/SY.admin@motherbee/config.json` |

## Where it runs

- Only on the motherbee. `main` reads `/etc/fluxbee/hive.yaml`: on another role it logs
  `SY.admin solo corre en motherbee; role != motherbee` and exits 0; with `role: motherbee` the
  `hive_id` must be `motherbee` (`PRIMARY_HIVE_ID`), and `hive_id: motherbee` on another role is
  an error.
- The packaged unit carries the same check as an `ExecCondition` (`unit_role_gate` in
  `packaging/build-deb.sh`), so a start pulled in on another role is skipped, not looped.
- It is a vault consumer: `validate_system_nodes` in `src/bin/sy_orchestrator.rs` requires
  `SY.admin` to come before `SY.vault` in `system_nodes.motherbee.nodes`.

## How it starts

`main`, in order:

1. Answers `--help` and `--version` before anything else (no router connection).
2. Exits 1 on a non-Linux target. Log level from `JSR_LOG_LEVEL` (default `info`).
3. Reads hive.yaml and applies the role checks above.
4. Picks the HTTP address: `JSR_ADMIN_LISTEN`, else `admin.listen`, else `127.0.0.1:8080`.
5. Connects to the router as `SY.admin` (`RouterDispatcher::connect_with_retry`, retry every
   1 s), with a persistent UUID under `/var/lib/fluxbee/state/nodes`.
6. Computes its ILK from its L2 name (`deterministic_system_ilk_id`), without waiting for the
   identity SHM.
7. Builds the executor's AI runtime (`build_admin_executor_ai_runtime`, see
   [The executor](#the-executor)). No key in the vault: the executor stays unavailable. An invalid
   `ai` section in hive.yaml: `main` returns the error and the process exits.
8. Starts the HTTP server, the `NODE_STATUS_GET` loop, the `ADMIN_COMMAND` loop, the system loop
   (`CONFIG_GET`, `CONFIG_SET`, `VAULT_SECRET_CHANGED`) and the publication expiry sweep (every
   600 s, `run_publication_expiry_sweep`).

## HTTP API

- **Loopback, no authentication.** `handle_http` reads the headers and never checks them. The
  address defaults to `127.0.0.1:8080` ("Default to loopback for safer deployments" in `main`).
  FINDINGS A-46 decided that SY.admin and SY.architect stay on loopback; the orchestrator's
  watchdog runs `ss -H -tlnp` about once a minute and logs a warning plus a `posture` drift alert
  if `sy-admin` listens anywhere else (`LOOPBACK_ONLY_PROCESSES`, `watchdog_listener_check` in
  `src/bin/sy_orchestrator.rs`). It does not stop the process.
- **One task per connection** (`run_http_server`); there is no global lock (the comment at the
  top of `handle_http` explains the deadlock it caused).
- **Body.** The body is read up to `Content-Length`, with no size cap (`read_http_request`).
- **Envelope.** Every answer is `{status, action, payload, error_code, error_detail}`. The HTTP
  status comes from `error_code` (`error_code_to_http_status`): for example `INVALID_REQUEST` 400,
  `UNAUTHORIZED` and the `*_PROTECTED` codes 403, the `*_NOT_FOUND` codes 404, state conflicts
  (`HIVE_EXISTS`, `JOIN_IN_PROGRESS`, `KEY_TAKEN`, `TENANT_HAS_NODES`, ...) 409, `COMPILE_ERROR`
  422, `TIMEOUT` 504, any code not in the list 500. A forwarded answer with status `accepted` or
  `sync_pending` becomes 202 (`build_admin_http_response`).
- **Unknown route.** `404 UNKNOWN_ROUTE`, or `405 METHOD_NOT_ALLOWED` with `allowed_methods` when
  the path exists with another method; both carry `"see": "GET /admin/actions"`
  (`admin_unknown_route_response`).
- **Health.** `GET /health` answers `{"status":"ok"}` without touching the mesh.

## ADMIN_COMMAND over the mesh

A node asks SY.admin for an action with a message of `msg_type: "admin"`, `msg: "ADMIN_COMMAND"`,
addressed to `SY.admin@motherbee`:

```json
{ "action": "list_nodes", "target": "worker-1", "params": {}, "request_id": "optional" }
```

`target` is the hive. For node actions a `node_name@hive` in `params` wins over it, and a
`params.hive_id` is refused (`resolve_internal_action_hive`,
`internal_admin_payload_contract_error`). Actions with `requires_target` in the registry answer
`INVALID_REQUEST` without a hive. The reply is `ADMIN_COMMAND_RESPONSE`, sent back to the caller,
with payload
`{status, action, payload, error_code, error_detail, request_id?}` (`send_admin_command_response`).
A payload that does not parse, or one without `action`, is answered `INVALID_REQUEST`; a failure
before any reply was built is answered `INTERNAL_ERROR` (`handle_internal_admin_command`).

- The router profile (`build_admin_rpc_profile`) sends `admin`/`ADMIN_COMMAND` to one channel,
  `CONFIG_GET`/`CONFIG_SET`/`VAULT_SECRET_CHANGED` to another and `NODE_STATUS_GET` to a third.
- Every command runs in its own task (`run_internal_admin_loop`). A handler that calls SY.admin
  back (a node teardown, a workflow deploy, an executor plan) would otherwise wait for itself
  (FINDINGS A-67, A-68).
- The caller is the router-stamped `msg.routing.src_l2_name`, never a field of the payload.
- The SDK side is `RouterDispatcher::send_admin_rpc` (named in the request contract of the
  mesh-only actions, `admin_action_request_contract`).

Five commands exist only over the mesh and are not in the catalog: `externalize`,
`unexternalize`, `list_externalized` (the operator reaches them through `/channels/*`),
`executor_validate_plan` and `executor_execute_plan` (`dispatch_internal_admin_command`).

## Who may call what

The HTTP API and the executor are trusted paths: they reach `dispatch_internal_admin_command`
with no caller (`None`), and every gate below lets `None` through except `publish_artifact`,
which needs a producer (and so has no HTTP route). Over the mesh the gates are:

| Caller | May | Code |
| --- | --- | --- |
| `IO.cloud@motherbee` | only `CLOUD_EXPOSED_ACTIONS`, plus `externalize`/`unexternalize` of its own channel; payload rules apply (next section) | `authorize_cloud_relay`, `enforce_cloud_relay_content`, `stamp_relay_facts` |
| `SY.architect@motherbee` | every action, including the Cloud-exposed ones and `get_llm_usage` of all tenants | `authorize_cloud_relay`, `authorize_llm_usage_scope` |
| `SY.orchestrator@<any hive>` | `delete_ilk`, `vault_list`, `vault_delete` (node teardown), besides the open ones | `ORCHESTRATOR_TEARDOWN_ACTIONS` |
| `SY.wf-rules@<hive>` | `run_node` although Cloud-exposed; the node actions `run_node`, `restart_node`, `kill_node`, `set_node_config`, `get_node_config`, `get_node_status` only on `WF.*@<its hive>`, with runtime `wf.<name>` on `wf.engine` | `authorize_wf_rules_caller` |
| `IO.*` node | `externalize`/`unexternalize` of a channel it owns; `list_externalized` shows only its rows | `authorize_channel_command`, `filter_externalized_channels_for_caller` |
| `AI.*`, `IO.*`, `WF.*` | `publish_artifact` (the caller needs a complete identity registration; its tenant owns the publication); `unpublish_artifact` of its own publications | `authorize_artifact_publisher`, `resolve_artifact_publisher`, `handle_unpublish_artifact` |
| any other mesh node | every action not exposed to Cloud, plus `CLOUD_SHARED_ACTIONS` (`get_tenant`, `list_tenants`, `list_tenant_nodes`, `get_node_status`, `start_node`, `kill_node`) | none |

The last row is FINDINGS A-62 (parked on cost/benefit): any node on the router may ask for
`add_hive`, `vault_get`, `kill_node`, the `opa_*` writes, `publish_cloud_endpoint` and the rest.
That includes `executor_execute_plan`, and the executor then runs every step as the operator
(`AdminExecutorStepTool::call` passes `None`), so a one-step plan reaches even the actions the
gates above reserve.

Other checks:

- `CONFIG_GET`/`CONFIG_SET` addressed to SY.admin itself are accepted only from `SY.architect`,
  `SY.config-routes` or `SY.vault` of the same hive (`admin_origin_authorized`), otherwise
  `FORBIDDEN`. The list says `SY.config-routes`; the node is named `SY.config.routes`.
- Fanout `externalize` (with `fanout_family`) is operator-only (`handle_externalize`).
- The HTTP route `POST /artifacts/unpublish` may revoke any tenant's publication
  (`handle_unpublish_artifact` with no caller).
- What SY.admin sends as SYSTEM messages passes the router's system policy
  (`policy/system.rego`): rule (1) lets `SY.admin@motherbee` send the `EDGE_*` commands, rule (2)
  `CONFIG_GET`, `CONFIG_SET`, `SYSTEM_UPDATE`, `SYSTEM_SYNC_HINT` and `CONFIG_CHANGED` to any
  hive, and rule (4) the other protected actions within its own hive.

### Cloud relay rules

Fluxbee Cloud reaches SY.admin through `IO.cloud@motherbee` (Cloud → SY.edge → IO.cloud →
SY.admin). `CLOUD_EXPOSED_ACTIONS` is the one list that both `list_cloud_actions` advertises and
`authorize_cloud_relay` enforces: `create_tenant`, `vault_put`, `run_node`, `get_ilk`,
`delete_ilk`, `restore_ilk`, `delete_tenant`, `restore_tenant`, `vault_list`, `vault_delete`,
`update_tenant`, `set_ilk_tenant`, `vault_assign`, `get_tenant`, `list_tenants`,
`list_tenant_nodes`, `get_node_status`, `start_node`, `kill_node`, `get_llm_usage`.

`enforce_cloud_relay_content` re-checks what IO.cloud sends, so a compromised relay cannot widen
it:

- The tenant actions, `set_ilk_tenant`, `get_llm_usage`, the node actions and the vault actions
  must name the tenant Cloud claims, in canonical form `tnt:<uuid>`, and never the root tenant
  (`cloud_claimed_tenant`). `create_tenant`, `get_ilk`, `list_tenants`, `delete_ilk` and
  `restore_ilk` take none.
- `delete_ilk` and `restore_ilk` must carry `ilk_type: "human"`.
- `vault_put` may not set `metadata.ilk`, `owner_ilk` or `owner_l2`; `owner_node` must be an
  `IO.*` node; `vault_put`, `vault_assign` and `vault_delete` may not touch the reserved keys
  (`edge_channel_secret:`, `edge_tls`, `ssh:`, `edge_fanout_verify:`,
  `is_cloud_reserved_vault_key`).
- `run_node`, `start_node`, `kill_node` and `get_node_status` act only on `IO.*` nodes.
- `update_tenant` sets `status` only to `active` or `suspended`; `get_llm_usage` never reads all
  tenants.
- Most actions accept only a fixed set of fields (`only_fields`).

`stamp_relay_facts` then adds what SY.admin knows and the caller cannot forge: `launched_by` on
`run_node` (`cloud`, `archi`, `operator`, or the caller's name; `launched_by_for_caller`),
`on_behalf_of` on `set_ilk_tenant`, and for Cloud `require_tenant_id` (and, for start and kill,
`require_launched_by: "cloud"`) on node actions, `include_deleted` and `exclude_root` on
`list_tenants`.

## Where actions go

SY.admin stamps its own name, `SY.admin@motherbee`, as `src_l2_name` on what it forwards
(`send_admin_request`), so the receivers can check it.

| Actions | Sent to | How |
| --- | --- | --- |
| node lifecycle, node config/state/status, hives, versions, runtimes, deployments, drift alerts, storage path, `core_rollback` (`action_routes_via_local_orchestrator`) | `SY.orchestrator@motherbee`, with `payload.target` = the hive; `node_name@hive` overrides `target` (`normalize_admin_payload`) | `msg_type: admin`, `meta.action` |
| routes, VPNs, taps | `SY.config.routes@<hive>` | `msg_type: admin` (`build_admin_request`) |
| identity actions | `SY.identity@<hive>` | `ILK_*`, `TNT_*` (`handle_identity_command`, `handle_identity_query`) |
| vault actions | `SY.vault@<hive>` | `VAULT_*` (`handle_vault_command`) |
| `update`, `sync_hint` | `SY.orchestrator@<hive>` | `SYSTEM_UPDATE` (`category` runtime, core or vendor; default runtime), `SYSTEM_SYNC_HINT` |
| `opa_*` writes | `SY.opa.rules@motherbee`, then waits for every hive's answer | `CONFIG_CHANGED`, subsystem `opa` (`send_opa_config_changed`) |
| `wf_rules_*` | `SY.wf-rules@<hive>` | `command` / `query` messages (`send_l2_action_request`) |
| `timer_*` | `SY.timer@<hive>` | `TIMER_*` → `TIMER_RESPONSE` (`handle_timer_rpc`) |
| `list_streams` | every `RT.<router>.stream@<hive>` in the LSA | `STREAM_STATUS` (`handle_list_streams`) |
| `get_llm_usage`, `GET /config/storage/metrics` | `SY.storage@motherbee` | `USAGE_QUERY`, `STORAGE_METRICS_GET` |
| `node_control_config_get/set` | the node | `CONFIG_GET`, `CONFIG_SET` |
| `externalize`, `unexternalize`, `list_externalized` | `SY.edge@<hive>` | `EDGE_OPEN_URL`, `EDGE_CLOSE_URL`, `EDGE_LIST_URLS` |
| `publish_artifact`, `unpublish_artifact` | `IO.blob@motherbee`, then the public edge | `BLOB_CURATE`, `EDGE_PUBLISH_BLOB` and back |
| catalog, help, `list_recent_commands` | answered by SY.admin | |

Timeouts (`admin_action_timeout`; seconds, with environment overrides): `add_hive` 30
(`JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS`; the join itself runs in the background), `update` 60,
`sync_hint` 45, `core_rollback` 300, `get_llm_usage` 20, catalog/help/timer/`hive_status`/
`list_streams` 5 (`JSR_ADMIN_TIMEOUT_SECS`), everything else 30 (`JSR_ADMIN_ORCH_TIMEOUT_SECS`).

### ORCHESTRATOR_BOOTING

The orchestrator joins its router before its boot ends, and until `finish_bootstrap` returns it
answers every admin command with `ORCHESTRATOR_BOOTING` ("the orchestrator is still booting;
retry shortly", `run_admin_worker` in `src/bin/sy_orchestrator.rs`). SY.admin passes the code
through unchanged. The code is not in `error_code_to_http_status`, so over HTTP it arrives as a
500. Retry after a few seconds.

## The executor

The executor runs an `executor_plan`: a list of admin actions with their arguments, written by
SY.architect (which sends `executor_validate_plan` and `executor_execute_plan` over the mesh).

Plan shape (`AdminExecutorPlan`, unknown fields refused):

```json
{
  "plan_version": "0.1",
  "kind": "executor_plan",
  "metadata": {},
  "execution": {
    "strict": true, "stop_on_error": true, "allow_help_lookup": true,
    "steps": [ { "id": "s1", "action": "create_tenant", "args": { "hive": "motherbee", "name": "Acme" } } ]
  }
}
```

`executor_execute_plan` takes `{execution_id, plan, executor_options: {dry_run}, labels}`
(`AdminExecutorRunRequest`).

- **Validation** (`validate_executor_plan`): version and kind, at least one step, unique ids,
  every action in the registry and not mesh-only, args valid against the action's function
  schema, no `<placeholder>` strings. `executor_validate_plan` also runs an LLM review of each
  step against its help (`semantic_review_executor_plan`); issues give `PLAN_REVIEW_FAILED`.
- **Execution** (`execute_admin_executor_plan`): for each step, an LLM run whose only tools are
  that step's action (and `get_admin_action_help` if `allow_help_lookup`). The prompt tells it to
  call the action with exactly the step's args (`admin_executor_prompt`). If the model calls no
  tool, SY.admin runs the step directly (`execute_admin_executor_step_fallback`). Later steps can
  use earlier results with `$steps.<step_id>.<path>`. With `stop_on_error` the run stops at the
  first failure. `dry_run` validates only.
- **Result**: `{status: done|failed, execution_id, events, summary}`; HTTP-style status 200 when
  done, 409 otherwise. A redacted copy is written to
  `/var/lib/fluxbee/state/admin-executor/<hive>/executions/<execution_id>.json`.
- **Model and key**: provider and model from the hive.yaml `ai` section (fallback provider when
  it is absent); the key from SY.vault, resource type = the provider, root tenant
  (`build_admin_executor_ai_runtime`). Without a key `executor_execute_plan` answers
  `EXECUTOR_UNAVAILABLE`. A `VAULT_SECRET_CHANGED` for that provider rebuilds the runtime
  in place (`handle_vault_secret_changed_admin`).
- **Usage records**: every model call is charged to the root tenant (`AiAccount::root`, tenant
  source `root`), with work `{execution_id, caller, stage}` where stage is `executor` or
  `executor_review` (`executor_usage_account`). The record goes to the node's router stream
  under `storage.usage` (`UsageRecorder` in `crates/fluxbee_ai_sdk/src/usage.rs`) and is read
  back with `get_llm_usage`.
- **Catalog**: `GET /admin/executor/functions` lists the functions the executor exposes. The
  catalog mode is `full` (every non-mesh-only action) or `subset`; `subset` with no list uses
  `ADMIN_EXECUTOR_PILOT_ACTIONS`. It is set with `CONFIG_SET` (`config.catalog.mode`,
  `config.catalog.actions`, `apply_mode` must be `replace`) and stored in
  `/var/lib/fluxbee/nodes/SY/SY.admin@motherbee/config.json`. AI keys are refused there
  (`reject_admin_executor_secret_fields`).

## Publications and the audit log

- **Public artifacts.** `publish_artifact` (mesh-only) has `IO.blob` curate a blob, records it in
  `/var/lib/fluxbee/state/sy-admin/publications.json`, and asks the public edge to serve it under
  `/public/<key>` (`handle_publish_artifact`). The edge is `admin.public_edge_node`, or the one
  connected ingress hive (`resolve_public_edge_node`). Expiry: default 1 day, 60 s to 30 days
  (`PUBLICATION_*_SECS`). The sweep retires expired ones every 600 s.
- **Audit log.** Commands are appended to
  `/var/lib/fluxbee/state/sy-admin/command-log/commands.jsonl`: one JSON line with time, action,
  hive, origin (`http`, `l2:<caller>`, `internal` or `executor`), redacted params (cut at 2048
  bytes), status, error code, HTTP status and duration. The file rotates every 100 entries and
  keeps 3 rotations. `GET /hives/{hive}/commands` (`list_recent_commands`) reads it: default 50,
  max 500, filters `action`, `status`, `since_ms`. Not logged: `update` and `sync_hint` (comment
  above `ADMIN_COMMAND_LOG_ROTATE_ENTRIES`).

## Config

`/etc/fluxbee/hive.yaml` (`HiveFile`):

| Key | Use |
| --- | --- |
| `hive_id`, `role` | role checks |
| `admin.listen` | HTTP address (env `JSR_ADMIN_LISTEN` wins) |
| `admin.public_edge_node`, `admin.public_base_url` | public artifacts (env `SY_ADMIN_PUBLIC_EDGE_NODE`, `SY_ADMIN_PUBLIC_BASE_URL` win) |
| `ai` | executor provider and model |
| `blob.path` | blob root, default `/var/lib/fluxbee/blob` |
| `storage.path` | root of the `/modules` store, default `/var/lib/fluxbee` |
| `wan.authorized_hives` | listed in the OPA rollout report (`expected_hive_sets`) |

Environment: `JSR_LOG_LEVEL`, `JSR_ADMIN_LISTEN`, the timeout overrides above.

## State on disk

- `/var/lib/fluxbee/state/sy-admin/command-log/`: the audit log.
- `/var/lib/fluxbee/state/sy-admin/publications.json`: the publication ledger.
- `/var/lib/fluxbee/state/admin-executor/<hive>/executions/`: executor run logs.
- `/var/lib/fluxbee/state/runtime-package-staging/`: staging for `publish_runtime_package`.
- `/var/lib/fluxbee/nodes/SY/SY.admin@motherbee/config.json`: executor catalog config.
- `<storage.path>/modules/`: the `/modules` store.
- `publish_runtime_package` and `remove_runtime_version` rewrite the motherbee's
  `dist/runtimes` manifest under one lock (`runtime_manifest_lock`); both refuse to run off the
  motherbee (`NOT_PRIMARY`).

## Failure behavior

- **Wrong role**: exits 0 (and the unit's `ExecCondition` skips it). Bad hive.yaml: exits with an
  error; systemd restarts it every 5 s.
- **Router down**: `connect_with_retry` keeps retrying; nothing is served until it connects.
- **A target node down**: the action answers `TIMEOUT` (504) or `TRANSPORT_ERROR` (502) after its
  timeout; SY.admin keeps serving.
- **No AI key**: everything works except the executor (`EXECUTOR_UNAVAILABLE`).
- **Audit log write fails**: logged, the command is not affected.

## Health

- `curl -s http://127.0.0.1:8080/health` → `{"status":"ok"}` (on the motherbee).
- `curl -s http://127.0.0.1:8080/hive/status` goes through the orchestrator, so it also shows
  whether the mesh answers.
- `systemctl status sy-admin`; the log line `sy.admin http listening` marks the HTTP server up.
- `GET /hives/motherbee/commands?limit=20` shows what was run lately and how it ended.

## Action table

Generated from `INTERNAL_ACTION_REGISTRY` (registry version 13, 100 actions, in registry order),
`admin_action_path_patterns` and `admin_action_summary`. A few long descriptions are cut to their first
sentence; `GET /admin/actions/{action}` has the full text, the fields and an example.

| Action | HTTP route(s) | Description |
| --- | --- | --- |
| `hive_status` | `GET /hive/status` | Read the local hive status summary. |
| `list_hives` | `GET /hives` | List all known hives. |
| `list_admin_actions` | `GET /admin/actions` | List the dynamic admin action catalog and help metadata. |
| `list_cloud_actions` | — (no HTTP route) | List the provisioning actions IO.cloud (Fluxbee Cloud) may invoke — the exposed subset. |
| `get_admin_action_help` | `GET /admin/actions/{action}` | Return help metadata for one admin action. |
| `publish_runtime_package` | `POST /admin/runtime-packages/publish` | Publish one runtime package into dist/manifest on motherbee. |
| `publish_artifact` | — (mesh only) | Publish an existing BlobRef through SY.edge. Mesh-only: the router-stamped AI.*, IO.* or WF.* caller becomes the publication owner. |
| `unpublish_artifact` | — (mesh only) | Revoke a public artifact owned by the calling producer. Mesh-only and identity-bound. |
| `list_versions` | `GET /versions` | List core and runtime versions across hives. |
| `get_versions` | `GET /hives/{hive}/versions` | Read core and runtime versions for one hive. |
| `list_runtimes` | `GET /hives/{hive}/runtimes` | List runtimes for one hive. |
| `get_runtime` | `GET /hives/{hive}/runtimes/{runtime}` | Read one runtime definition in a hive. |
| `remove_runtime_version` | `DELETE /hives/{hive}/runtimes/{runtime}/versions/{version}` | Delete one published runtime version from motherbee dist/manifest. |
| `list_routes` | `GET /hives/{hive}/routes` | List route rules for a hive. |
| `list_vpns` | `GET /hives/{hive}/vpns` | List VPN patterns for a hive. |
| `list_nodes` | `GET /hives/{hive}/nodes` | List nodes for one hive only. Use inventory for system-wide node visibility. |
| `list_streams` | `GET /hives/{hive}/streams` | Read the status of every router stream of one hive: backlog, deliveries and drops. |
| `get_llm_usage` | `GET /hives/{hive}/usage` | Read the LLM usage records, one per model call, of one tenant or of every tenant: a period grouped by day, model, node or tenant or record by record, or the records stored since a cursor. |
| `get_storage` | `GET /config/storage` | Read storage configuration and status. |
| `add_hive` | `POST /hives` | Create a hive and bootstrap it. |
| `get_hive` | `GET /hives/{hive}` | Read one hive definition. |
| `remove_hive` | `DELETE /hives/{hive}` | Remove a hive. |
| `add_route` | `POST /hives/{hive}/routes` | Add a route rule to a hive. |
| `delete_route` | `DELETE /hives/{hive}/routes/{prefix}` | Delete a route rule from a hive. |
| `add_vpn` | `POST /hives/{hive}/vpns` | Add a VPN pattern to a hive. |
| `delete_vpn` | `DELETE /hives/{hive}/vpns/{pattern}` | Delete a VPN pattern from a hive. |
| `list_taps` | `GET /hives/{hive}/taps` | List router-level taps installed on a hive. Read-only. |
| `add_tap` | `POST /hives/{hive}/taps` | Add a router-level tap on a hive. |
| `delete_tap` | `DELETE /hives/{hive}/taps?match_src=...&match_dst=...&target=...` | Delete an installed router-level tap by its (match_src, match_dst, target) natural key. |
| `run_node` | `POST /hives/{hive}/nodes` | Create and start a new managed node instance in a hive. |
| `start_node` | `POST /hives/{hive}/nodes/{node_name}/start` | Start an existing persisted managed node instance in a hive. |
| `restart_node` | `POST /hives/{hive}/nodes/{node_name}/restart` | Restart an existing managed node instance in a hive. |
| `kill_node` | `DELETE /hives/{hive}/nodes/{node_name}` | Stop a node in a hive. Optional purge_instance also removes its persisted instance directory. |
| `remove_node_instance` | `DELETE /hives/{hive}/nodes/{node_name}/instance` | Delete a node instance from disk/runtime state. |
| `get_node_config` | `GET /hives/{hive}/nodes/{node_name}/config` | Read the stored effective config.json payload for a managed node. |
| `set_node_config` | `PUT /hives/{hive}/nodes/{node_name}/config` | Persist node config changes. |
| `get_node_state` | `GET /hives/{hive}/nodes/{node_name}/state` | Read the persisted state payload of one node. |
| `get_node_status` | `GET /hives/{hive}/nodes/{node_name}/status` | Read the effective runtime status of one node. |
| `list_tenant_nodes` | `GET /hives/{hive}/identity/tenants/{tenant_id}/nodes` | List the managed nodes charged to one tenant on every hive, with their runtime, state and who launched them. |
| `send_node_message` | `POST /hives/{hive}/nodes/{node_name}/messages` | Send a system message to a node. |
| `list_recent_commands` | `GET /hives/{hive}/commands` | Audit log: list the most recent admin commands executed on this backend (all origins — operator HTTP, internal L2 callers like SY.architect, and executor-plan steps), with redacted params, result status and duration. Read-only. |
| `node_control_config_get` | `POST /hives/{hive}/nodes/{node_name}/control/config-get` | Send CONFIG_GET to a node that participates in live control-plane config and return its CONFIG_RESPONSE. |
| `node_control_config_set` | `POST /hives/{hive}/nodes/{node_name}/control/config-set` | Send CONFIG_SET to a node that participates in live control-plane config and return its CONFIG_RESPONSE. |
| `list_ilks` | `GET /hives/{hive}/identity/ilks` | List identity ilks in a hive. |
| `get_ilk` | `GET /hives/{hive}/identity/ilks/{ilk_id}` | Read one identity ilk. |
| `set_ilk_definition` | `POST /hives/{hive}/identity/ilks/{ilk_id}/definition` | Set the cognitive definition hashes for one identity agent ILK. |
| `delete_ilk` | `DELETE /hives/{hive}/identity/ilks/{ilk_id}` | Mark one identity ILK deleted (reversible: hidden, its keys stay reserved until purge). Refuses well-known SY system ILKs (SYSTEM_ILK_PROTECTED). |
| `restore_ilk` | `POST /hives/{hive}/identity/ilks/{ilk_id}/restore` | Undo the deleted mark of one identity ILK (a person also while their tenant is marked; any other ILK waits for its tenant). |
| `set_ilk_tenant` | `PUT /hives/{hive}/identity/ilks/{ilk_id}/tenant` | Charge one person to another tenant, its cost center (a temporary only out of a deleted tenant); channels keep their scope. |
| `purge_ilk` | `POST /hives/{hive}/identity/ilks/{ilk_id}/purge` | Physically and finally remove one identity ILK that is ALREADY marked deleted (ILK_NOT_DELETED otherwise). |
| `delete_tenant` | `DELETE /hives/{hive}/identity/tenants/{tenant_id}` | Mark one identity tenant deleted, i.e. disable it (reversible; name/domain stay reserved). No ILK is marked: its people stay usable. |
| `restore_tenant` | `POST /hives/{hive}/identity/tenants/{tenant_id}/restore` | Undo the deleted mark of one tenant: it comes back as it was. |
| `purge_tenant` | `POST /hives/{hive}/identity/tenants/{tenant_id}/purge` | Physically and finally remove one tenant ALREADY marked deleted and the ILKs marked in it. |
| `list_tenants` | `GET /hives/{hive}/identity/tenants` | List identity tenants in a hive. |
| `get_tenant` | `GET /hives/{hive}/identity/tenants/{tenant_id}` | Read one identity tenant in a hive. |
| `create_tenant` | `POST /hives/{hive}/identity/tenants` | Create an identity tenant in a hive. |
| `update_tenant` | `PUT /hives/{hive}/identity/tenants/{tenant_id}` | Update mutable fields of one identity tenant in a hive. |
| `set_tenant_sponsor` | `POST /hives/{hive}/identity/tenants/{tenant_id}/sponsor` | Set or clear the sponsor relationship for one identity tenant in a hive. |
| `vault_list` | `GET /hives/{hive}/vault/secrets` | List SY.vault secret metadata visible to the caller. |
| `vault_put` | `POST /hives/{hive}/vault/secrets` | Write or update one encrypted secret in SY.vault. |
| `vault_get_metadata` | `GET /hives/{hive}/vault/secrets/{key}/metadata` | Read one SY.vault secret metadata record without returning the secret value. |
| `vault_get` | `GET /hives/{hive}/vault/secrets/{key}` | Read one SY.vault secret value. Secret-sensitive; prefer metadata inspection when possible. |
| `vault_delete` | `DELETE /hives/{hive}/vault/secrets/{key}` | Delete one encrypted secret from SY.vault. |
| `vault_rotate` | `POST /hives/{hive}/vault/secrets/{key}/rotate` | Rotate one encrypted secret value while keeping the previous version for rollback. |
| `vault_assign` | `PUT /hives/{hive}/vault/secrets/{key}/assigned` | Replace the nodes one secret is assigned to (who uses it besides its owner), without touching its value. |
| `vault_rollback` | `POST /hives/{hive}/vault/secrets/{key}/rollback` | Rollback one encrypted secret to its previous version. |
| `core_rollback` | `POST /hives/{hive}/core/rollback` | Restore this hive's core binaries from a stamped backup generation (undo a core update). |
| `list_deployments` | `GET /deployments` | List historical deployments globally. |
| `get_deployments` | `GET /hives/{hive}/deployments` | List historical deployments targeting one hive. |
| `list_drift_alerts` | `GET /drift-alerts` | List historical drift alerts globally. |
| `get_drift_alerts` | `GET /hives/{hive}/drift-alerts` | List historical drift alerts for one hive. |
| `set_storage` | `PUT /config/storage` | Persist storage configuration changes. |
| `update` | `POST /hives/{hive}/update` | Run hive update workflow. |
| `publish_cloud_endpoint` | `POST /hives/{hive}/cloud/endpoint` | Open IO.cloud's public Fluxbee Cloud endpoint on an edge and return its bearer token ONCE (admin mints it, the vault stores it, the edge only ever gets a reference). |
| `sync_hint` | `POST /hives/{hive}/sync-hint` | Trigger a sync hint workflow. |
| `inventory` | `GET /inventory`<br>`GET /inventory/summary`<br>`GET /inventory/{hive}`<br>`GET /hives/{hive}/inventory/summary` (listed, not served)<br>`GET /hives/{hive}/inventory/hive` (listed, not served) | Read global or per-hive inventory, including system-wide node visibility. |
| `opa_compile_apply` | `POST /opa/policy` | Compile the user OPA policy on the motherbee and apply it on every hive (one global policy). |
| `opa_compile` | `POST /opa/policy/compile` | Compile OPA policy. |
| `opa_apply` | `POST /opa/policy/apply` | Apply the staged user OPA policy on every hive. |
| `opa_rollback` | `POST /opa/policy/rollback` | Roll every hive back to the previous user OPA policy. |
| `opa_clear` | `POST /opa/policy/clear` | Remove the user OPA policy on every hive (current, staged, backup): back to a fresh install; routers unload it. |
| `opa_check` | `POST /opa/policy/check` | Validate OPA policy text without applying it. |
| `opa_get_policy` | `GET /hives/{hive}/opa/policy` | Read the user OPA policy a hive runs: version and hash, and the rego on the motherbee only (the other hives receive just the wasm). |
| `opa_get_status` | `GET /hives/{hive}/opa/status` | Read OPA status for a hive. |
| `wf_rules_compile_apply` | `POST /hives/{hive}/wf-rules` | Compile and apply a workflow definition through SY.wf-rules. |
| `wf_rules_compile` | `POST /hives/{hive}/wf-rules/compile` | Compile a workflow definition without applying it. |
| `wf_rules_apply` | `POST /hives/{hive}/wf-rules/apply` | Apply the staged workflow definition. |
| `wf_rules_rollback` | `POST /hives/{hive}/wf-rules/rollback` | Rollback to the backup workflow definition. |
| `wf_rules_delete` | `POST /hives/{hive}/wf-rules/delete` | Delete a managed workflow through SY.wf-rules. |
| `wf_rules_get_workflow` | `GET /hives/{hive}/wf-rules` (with `?workflow_name=`) | Read the current workflow definition managed by SY.wf-rules for a hive. |
| `wf_rules_get_status` | `GET /hives/{hive}/wf-rules/status` | Read workflow status from SY.wf-rules for a hive. |
| `wf_rules_list_workflows` | `GET /hives/{hive}/wf-rules` | List workflows managed by SY.wf-rules for a hive. |
| `timer_help` | `GET /hives/{hive}/timer/help` | Read the self-described SY.timer capability catalog for a hive. |
| `timer_get` | `GET /hives/{hive}/timer/timers/{timer_uuid}` | Read one timer by uuid from SY.timer for a hive. |
| `timer_list` | `GET /hives/{hive}/timer/timers` | List timers from SY.timer for a hive, optionally filtered by owner. |
| `timer_now` | `GET /hives/{hive}/timer/now` | Read the current UTC time from SY.timer for a hive. |
| `timer_now_in` | `POST /hives/{hive}/timer/now-in` | Read the current time in one IANA timezone from SY.timer for a hive. |
| `timer_convert` | `POST /hives/{hive}/timer/convert` | Convert one UTC instant through SY.timer for a hive. |
| `timer_parse` | `POST /hives/{hive}/timer/parse` | Parse a date/time string through SY.timer for a hive. |
| `timer_format` | `POST /hives/{hive}/timer/format` | Format a UTC instant through SY.timer for a hive. |

### HTTP routes outside the catalog

Served by `handle_http`, `handle_hive_paths` and `handle_modules_paths` but not in the registry:

| Route | What it does |
| --- | --- |
| `GET /health` | Liveness, answered locally |
| `GET /admin/executor/functions` | The executor's function catalog |
| `POST /channels/externalize`, `POST /channels/unexternalize`, `GET /channels/externalized?edge_node=` | Edge channels as the operator (`ADMIN_ROUTES_OUTSIDE_ACTION_REGISTRY`) |
| `POST /artifacts/unpublish` | Revoke any publication, body `{"publication_id": "pub:<uuid>"}` |
| `GET /config/storage/metrics` | SY.storage inbox metrics |
| `GET /hives/{hive}/status` | Inventory of one hive (`inventory`, scope `hive`) |
| `GET /inventory/nodes` | Global inventory, nodes |
| `GET /versions?hive=`, `GET /deployments?hive=`, `GET /drift-alerts?hive=` | The per-hive form of the global route |
| `GET /opa/policy?hive=`, `GET /opa/status?hive=` | Hive-less forms of the OPA reads |
| `POST /wf-rules`, `POST /wf-rules/{compile,apply,rollback,delete}`, `GET /wf-rules`, `GET /wf-rules/status` | Hive-less forms of the wf-rules actions (hive from the body or query) |
| `GET /modules`, `GET /modules/{name}`, `GET /modules/{name}/{version}`, `POST /modules/{name}/{version}` | The module store: list, versions, download (tar or file), upload a tar |

`GET /hives/{hive}/inventory/summary` and `GET /hives/{hive}/inventory/hive` are listed for
`inventory` but have no handler: they get `405 METHOD_NOT_ALLOWED` naming `GET` as allowed. Use
`GET /hives/{hive}/status` or `GET /inventory/{hive}`.

## Links

- [SY.orchestrator](sy-orchestrator.md): where node and hive actions land.
- [SY.architect](sy-architect.md): writes the executor plans.
- [SY.edge](sy-edge.md): `externalize` and public artifacts.
- [SY.vault](sy-vault.md): the vault actions and their rules.
- The other targets: [SY.identity](sy-identity.md), [SY.config.routes](sy-config-routes.md),
  [SY.opa.rules](sy-opa-rules.md), [SY.wf-rules](sy-wf-rules.md), [SY.timer](sy-timer.md),
  [SY.storage](sy-storage.md), and [the router](rt-gateway.md) for the system policy.
- [Admin internal gateway spec](../history/admin-internal-gateway-spec.md): the `ADMIN_COMMAND` contract.
- [IO.cloud API](../io-cloud-api.md): the Cloud side of the relay.
- [ILK and tenant model](../ilk-tenant-model-v1.md): tenants, Cloud rules.
- [Host posture spec](../host-posture-and-exposure-spec-v1.md): why SY.admin stays on loopback.
- `lab/logbook/FINDINGS.md`: A-46 (no HTTP auth), A-62 (mesh callers), A-68 (one task per command).
