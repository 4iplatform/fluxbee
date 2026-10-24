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
8. Reads the `web:` section (`WebConfig::from_hive`) and logs the web hosts it declares, then the
   `acme:` section and the certified names (`AcmeConfig::from_hive`): the main name, then those
   hosts. A section that cannot work declares nothing (`web:`) or turns ACME off (`acme:`) with a
   reason, in the log (and in `GET /acme/status`); neither stops the start.
9. Starts the HTTP server, the `NODE_STATUS_GET` loop, the `ADMIN_COMMAND` loop, the system loop
   (`CONFIG_GET`, `CONFIG_SET`, `VAULT_SECRET_CHANGED`), the publication expiry sweep (every
   600 s, `run_publication_expiry_sweep`), the ACME check (2 min after the start, then about
   every 6 h, `AcmeService::run`; see [The ACME client](#the-acme-client)) and the web routes push
   (at once, then every 60 s, `WebRoutes::run`; see [The web hosts](#the-web-hosts)).

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
`INVALID_REQUEST` without a hive. `inventory` is narrowed to one hive by `params.filter_hive` or
`params.hive` (the executor's argument) in any `scope`, and by `target` only with
`scope: "hive"`: SY.architect targets its own hive on every hive-less read, the global summary
included (`internal_inventory_payload`).

The reply is `ADMIN_COMMAND_RESPONSE`, sent back to the caller, with payload
`{status, action, payload, error_code, error_detail, request_id?}` (`send_admin_command_response`).
A payload that does not parse, or one without `action`, is answered `INVALID_REQUEST`; a failure
before any reply was built is answered `INTERNAL_ERROR` (`handle_internal_admin_command`).

- The router profile (`build_admin_rpc_profile`) sends `admin`/`ADMIN_COMMAND` to one channel,
  `CONFIG_GET`/`CONFIG_SET`/`VAULT_SECRET_CHANGED` to another and `NODE_STATUS_GET` to a third.
- Every command runs in its own task (`run_internal_admin_loop`). A handler that calls SY.admin
  back (a node teardown, a workflow deploy, an executor plan) would otherwise wait for itself
  (FINDINGS A-67, A-68).
- The caller is the router-stamped `msg.routing.src_l2_name`, never a field of the payload. The
  A-62 table admits it or not before anything runs ([Who may call what](#who-may-call-what)).
- The SDK side is `RouterDispatcher::send_admin_rpc` (named in the request contract of the
  mesh-only actions, `admin_action_request_contract`).

Five commands exist only over the mesh and are not in the catalog: `externalize`,
`unexternalize`, `list_externalized` (the operator reaches them through `/channels/*`),
`executor_validate_plan` and `executor_execute_plan` (`dispatch_internal_admin_command`).

## Who may call what

Every command reaches `dispatch_internal_admin_command` with its origin (`AdminOrigin`): the HTTP
API on loopback, the executor running the steps of a plan already approved, or the mesh with the
router-stamped caller. The first two are trusted by their entry point, never by a missing caller.
A mesh caller is admitted only by the A-62 table ([control-path hardening
§2](../control-path-hardening-v1.md)), checked before anything else (`authorize_mesh_caller`).
Anything else answers `403 UNAUTHORIZED` and logs one warning, `mesh caller refused`, naming the
caller and the action; a command with no stamped caller is refused too.

| Caller | May ask for | Checked in |
| --- | --- | --- |
| any caller on an ingress or egress hive | nothing, whatever the rows below say | `caller_hive_role` |
| `SY.architect@motherbee` | every action `mesh_access` lists, `get_llm_usage` of all tenants included | `authorize_llm_usage_scope` |
| `IO.cloud@motherbee` | the Cloud-exposed actions (`CLOUD_EXPOSED_ACTIONS`); payload rules apply (next section) | `enforce_cloud_relay_content`, `stamp_relay_facts` |
| `SY.wf-rules@H` | the node actions `run_node`, `restart_node`, `kill_node`, `get_node_config`, `set_node_config`, `get_node_status`, only on `WF.*@H`, binding or launching `wf.<name>` on `wf.engine`; the package actions and `sync_hint` (below) | `authorize_wf_rules_node`, `authorize_wf_rules_package` |
| `SY.orchestrator@H` | `teardown_node` for a node `*@H` ([Node teardown](#node-teardown)) | `authorize_orchestrator_teardown` |
| `IO.*@H` (IO.cloud included) | `externalize` and `unexternalize` of a channel it owns; `list_externalized` shows only its rows | `authorize_channel_command`, `filter_externalized_channels_for_caller` |
| `AI.*@H`, `IO.*@H`, `WF.*@H` | `publish_artifact` (the caller needs a complete identity registration; its tenant owns the publication); `AI.*@H` also `unpublish_artifact` of its own publications | `authorize_artifact_publisher`, `resolve_artifact_publisher`, `handle_unpublish_artifact` |

- **The table lives next to the catalog.** `mesh_access`, right after `INTERNAL_ACTION_REGISTRY`,
  gives every action of the catalog and every mesh command off it its row. An action it does not
  list is closed to the whole mesh, Archi included, and the unit test
  `every_action_has_a_mesh_decision` fails until it is listed.
- **A hive's role comes from the hive registry**, `/var/lib/fluxbee/hives/<hive>/info.yaml`
  (`caller_hive_role`, over the SDK's strict reading `HiveRegistry::role`): `status: connected`,
  its join done, and `role` worker, ingress or egress.
  It fails closed: no entry, no role, a hive still joining or a removed one gets nothing. The
  local hive is the motherbee. At start SY.admin logs the role of each registered hive, and a
  warning for each one that gets none (`log_registry_caller_roles`).
- **Only Archi and the HTTP API** ask for `executor_validate_plan`, `executor_execute_plan` and
  `publish_cloud_endpoint`. The executor then runs every step of the plan as the operator.
- **SY.wf-rules@H's packages**, with only the fields its deploys and deletes send
  (`go/sy-wf-rules/node/admin.go`):
  - `publish_runtime_package` `{source: {kind, files}, set_current}`: only an inline package
    whose `package.json` (read with the publisher's own `PackageMetadata`) says `type: workflow`,
    `runtime_base: wf.engine` and `name: wf.<x>`, never `wf.engine` itself; so no `sync_to`,
    `update_to` or bundle upload, and each file under one plain path;
  - `remove_runtime_version` `{runtime, runtime_version}`: a version of a `wf.<x>`, named in
    lowercase. A `test_hold_ms` would hold the manifest lock every publish and deploy waits for;
  - both are refused while a `WF.<x>` is connected on another hive (`workflow_nodes_elsewhere`):
    the package is the same on every hive. SY.admin reads the connected nodes from shared memory,
    asking no one (`connected_nodes_from_shm`): the motherbee's own from the regions of its
    routers, every other hive's from the LSA, as the router reaches them. Regions that cannot be
    read answer `503 INVENTORY_UNAVAILABLE`, to retry, never `UNAUTHORIZED`;
  - `sync_hint` `{channel, folder_id, wait_for_idle, timeout_ms}`: only a scan of
    `fluxbee-dist-runtimes` (channel `dist`) on the motherbee, as a worker's deploy asks for one
    (`announceRuntimes`), with `timeout_ms` given and at most 5000. A longer wait would hold the
    motherbee orchestrator's system worker, which takes one message at a time.

Other checks:

- `CONFIG_GET`/`CONFIG_SET` addressed to SY.admin itself are accepted only from
  `SY.admin@motherbee` (`admin_origin_authorized`), otherwise `FORBIDDEN`. The router delivers
  them from no one else (rule (2) of `policy/system.rego`), so what reaches SY.admin is its own
  `node_control_config_get`/`node_control_config_set` on `SY.admin@motherbee`, which reads and
  sets the executor catalog. Over the mesh only Archi asks for those two and for
  `send_node_message`.
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
the A-62 table admits from IO.cloud (`authorize_mesh_caller`): `create_tenant`, `vault_put`,
`run_node`, `get_ilk`, `delete_ilk`, `restore_ilk`, `delete_tenant`, `restore_tenant`,
`vault_list`, `vault_delete`, `update_tenant`, `set_ilk_tenant`, `vault_assign`, `get_tenant`,
`list_tenants`, `list_tenant_nodes`, `get_node_status`, `start_node`, `kill_node`,
`get_llm_usage`.

`enforce_cloud_relay_content` re-checks what IO.cloud sends, so a compromised relay cannot widen
it:

- The tenant actions, `set_ilk_tenant`, `get_llm_usage`, the node actions and the vault actions
  must name the tenant Cloud claims, in canonical form `tnt:<uuid>`, and never the root tenant
  (`cloud_claimed_tenant`). `create_tenant`, `get_ilk`, `list_tenants`, `delete_ilk` and
  `restore_ilk` take none.
- `delete_ilk` and `restore_ilk` must carry `ilk_type: "human"`.
- `vault_put` may not set `metadata.ilk`, `owner_ilk` or `owner_l2`; `owner_node` must be an
  `IO.*` node; `vault_put`, `vault_assign` and `vault_delete` may not touch the reserved keys
  (`edge_channel_secret:`, `edge_tls`, `ssh:`, `edge_fanout_verify:`, `acme_account`,
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
| `inventory` | `SY.orchestrator@motherbee` | `INVENTORY_REQUEST` (`handle_inventory_http`) |
| identity actions | `SY.identity@<hive>` | `ILK_*`, `TNT_*` (`handle_identity_command`, `handle_identity_query`) |
| vault actions | `SY.vault@motherbee`; another hive is refused with `400 INVALID_REQUEST` ("SY.vault runs only on the motherbee") | `VAULT_*` (`handle_vault_command`) |
| `teardown_node` | `SY.identity@motherbee`, then `SY.vault@motherbee`, then SY.identity again | `ILK_GET`, `VAULT_LIST`, `VAULT_DELETE`, `ILK_DELETE`, `ILK_PURGE` ([Node teardown](#node-teardown)) |
| `update`, `sync_hint` | `SY.orchestrator@<hive>` | `SYSTEM_UPDATE` (`category` runtime, core or vendor; default runtime), `SYSTEM_SYNC_HINT` |
| `opa_*` writes | `SY.opa.rules@motherbee`, then waits for every hive's answer | `CONFIG_CHANGED`, subsystem `opa` (`send_opa_config_changed`) |
| `opa_check` | `SY.opa.rules@motherbee` only; nothing is announced | `CONFIG_CHANGED`, action `check`, `version: 0` (a check takes no number from the counter); the answer is matched on the `config.check_id` it echoes (`send_opa_check`) |
| `wf_rules_*` | `SY.wf-rules@<hive>` | `command` / `query` messages (`send_l2_action_request`) |
| `timer_*` | `SY.timer@<hive>` | `TIMER_*` → `TIMER_RESPONSE` (`handle_timer_rpc`) |
| `list_streams` | every `RT.<router>.stream@<hive>` in the LSA | `STREAM_STATUS` (`handle_list_streams`) |
| `get_llm_usage`, `GET /config/storage/metrics` | `SY.storage@motherbee` | `USAGE_QUERY`, `STORAGE_METRICS_GET` |
| `node_control_config_get/set` | the node | `CONFIG_GET`, `CONFIG_SET` |
| `externalize`, `unexternalize`, `list_externalized` | `SY.edge@<hive>` | `EDGE_OPEN_URL`, `EDGE_CLOSE_URL`, `EDGE_LIST_URLS` |
| `publish_artifact`, `unpublish_artifact` | the producer's hive's `SY.orchestrator` when the blob is not on the motherbee yet, then `IO.blob@motherbee`, then the public edge | `SYSTEM_SYNC_HINT` (publish only), `BLOB_CURATE`, `EDGE_PUBLISH_BLOB` and back |
| `acme_status`, `acme_renew`, `acme_test`, `acme_reload` | answered by SY.admin's ACME client, which asks the public edge, SY.vault, the CA and SY.orchestrator | `EDGE_TLS_STATUS`, `EDGE_TLS_RELOAD`, `EDGE_ACME_CHALLENGE`, `VAULT_*`, `record_drift_alert` ([The ACME client](#the-acme-client)) |
| `web_status` | answered by SY.admin from what it holds, plus two reads of io.web: `SY.orchestrator@motherbee` and `IO.web@motherbee` | `get_node_status` (`msg_type: admin`), `NODE_STATUS_GET` ([The web door's status](#the-web-doors-status)) |
| catalog, help, `list_recent_commands` | answered by SY.admin | |

Timeouts (`admin_action_timeout`; seconds, with environment overrides): `add_hive` 30
(`JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS`; the join itself runs in the background), `update` 60,
`sync_hint` 45, `core_rollback` 300, `get_llm_usage` 20, `acme_reload` 90 (the edge has 75 s),
`kill_node` and `remove_node_instance` 180 (a purge waits for the node's teardown: the
orchestrator gives `teardown_node` 120 s, and the motherbee gives a spoke's purge 150 s),
catalog/help/timer/`hive_status`/`list_streams`/`web_status` 5 (`JSR_ADMIN_TIMEOUT_SECS`; for
`web_status`, its read of the orchestrator's view of io.web), everything else 30
(`JSR_ADMIN_ORCH_TIMEOUT_SECS`).

### ORCHESTRATOR_BOOTING

The orchestrator joins its router before its boot ends, and until `finish_bootstrap` returns it
answers every admin command with `ORCHESTRATOR_BOOTING` ("the orchestrator is still booting;
retry shortly", `run_admin_worker` in `src/bin/sy_orchestrator.rs`). SY.admin passes the code
through unchanged. The code is not in `error_code_to_http_status`, so over HTTP it arrives as a
500. Retry after a few seconds.

## Node teardown

`teardown_node {node_name, ilk_id}` removes what a purged node leaves in the stores
(`handle_teardown_node`). `SY.orchestrator@H` asks for it for a node of H when `kill_node` runs
with `purge_instance`; the operator (`POST /hives/{hive}/nodes/{node_name}/teardown`, body
`{"ilk_id": "ilk:<uuid>"}`, a bare node name being a node of the path's hive), Archi and the
executor may ask for it too. In order:

1. **The request**: a node `<role>@<hive>` of the kind run_node launches (never `SY.*` or
   `RT.*`) and its ILK as `ilk:<uuid>` in canonical form, else `400 INVALID_REQUEST`. **A node
   still connected is alive**: only its own orchestrator, whose call follows its kill, tears it
   down then. Anyone else gets `409 NODE_STILL_CONNECTED` while the shared-memory regions list
   the node, or `503 INVENTORY_UNAVAILABLE` when they cannot be read, and nothing is removed
   (`teardown_live_node_refusal`). Otherwise a node redeployed onto the ILK of a teardown that
   failed half-way could be purged alive.
2. **The ILK is the node's**: `ILK_GET` to SY.identity, whose `identification.node_name` must be
   the node, else `409 NODE_ILK_MISMATCH` and nothing is removed. An ILK SY.identity does not
   hold is reported `not_found` and skips step 4. Over the mesh it skips step 3 too, since
   nothing ties those secrets to the node any more; the operator's teardown (HTTP, the executor)
   still deletes them.
3. **Its secrets**: `VAULT_LIST` with `filter.ilk`, then `VAULT_DELETE` of each in its own tenant,
   through `handle_vault_command` like every admin vault call.
4. **The ILK**: `ILK_DELETE` (the mark), then `ILK_PURGE`; `ILK_NOT_FOUND` counts as done.

The secrets go first because the ILK is what ties them to the node: when one cannot be deleted,
step 4 does not run, so a retry can still prove which secrets are the node's. The payload is the
same whatever happened:

```json
{
  "node_name": "AI.sales@worker1", "ilk_id": "ilk:...", "ilk_deleted": true, "not_found": false,
  "vault_secrets_purged": { "status": "ok", "scanned": 2, "deleted": 2, "errors": [] },
  "ilk_delete": { "status": "ok" }
}
```

`status` is `ok` only when every step was done. Otherwise it is `502 TEARDOWN_INCOMPLETE` with the
same payload: a step that failed says `error` with `error_code` and `error_detail` (per key in
`errors`, or on `vault_secrets_purged` itself when the listing failed), and one that did not run
says `skipped`.

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
- **add_hive**: the orchestrator answers `accepted` and joins in the background (PB-7), so before
  the next step the executor follows the join (`follow_add_hive_join`). Every 5 s it reads the
  hive as `GET /hives/{hive}` answers it, without an audit entry per read, until the entry's
  `join.phase` is `done`; `status` alone is not enough, since the join flows rewrite the entry
  without its join record while they run. `connected` ends the step; any other status fails it
  with the join's `error_code` and `error_detail`, a removed entry with `NOT_FOUND`. After 30 min
  the step fails with `JOIN_WAIT_TIMEOUT` and the join goes on. The bound covers up to 15 min
  waiting for a fresh box to settle, a join measured at more than 6 min and the WAN and LSA
  gates, with margin. The step's output stays the add_hive answer.
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
- **A producer on a worker** (FINDINGS A-81) promotes its blob there, and Syncthing brings it to
  the motherbee about 10 s later. Before the publication lock, unless the blob is in the
  motherbee's `active/` already, SY.admin asks the producer's hive to scan its blob folder
  (`SYSTEM_SYNC_HINT` to its `SY.orchestrator`, `fluxbee-blob`, `wait_for_idle: false`, 10 s at
  most, in a task of its own) while it looks for the blob every half second for up to 30 s
  (`await_producer_blob`). A producer on the motherbee gets the looks only. Either way the curate
  answers for the blob. Any curate error, a lost answer included, releases what `IO.blob` may
  have curated (`curate_publication_blob`).
- **One budget of 110 s** covers a whole `publish_artifact`, under the 120 s ai.generic waits for
  its answer: the wait for the blob, the publication lock, the curate (30 s at most) and the
  edge's publish (75 s at most) each get what is left. A step with nothing left answers
  `TIMEOUT`; at the edge's step, after releasing what was curated.
- **Audit log.** Commands are appended to
  `/var/lib/fluxbee/state/sy-admin/command-log/commands.jsonl`: one JSON line with time, action,
  hive, origin (`http`, `l2:<caller>` or `executor`), redacted params (cut at 2048
  bytes), status, error code, HTTP status and duration. The file rotates every 100 entries and
  keeps 3 rotations. `GET /hives/{hive}/commands` (`list_recent_commands`) reads it: default 50,
  max 500, filters `action`, `status`, `since_ms`. Not logged: `update` and `sync_hint` (comment
  above `ADMIN_COMMAND_LOG_ROTATE_ENTRIES`).

## The ACME client

SY.admin orders and renews the public edge's certificate from an ACME certificate authority,
validated with TLS-ALPN-01 on the :443 the edge already opens. The CA is Let's Encrypt unless
`acme:` names another one; nothing in the code names a domain. Source: `src/bin/sy_admin/acme.rs`;
design: [docs/acme-edge-certificates-v1.md](../acme-edge-certificates-v1.md) (§3, §6, §7).

**Configuration** (`AcmeConfig::from_hive`, `certified_name`). The motherbee's hive.yaml:

```yaml
admin:
  public_edge_node: "SY.edge@ingress1"
  public_base_url: "https://hive-k3m9x7q2.example.com"   # its host is the certified name
acme:
  terms_agreed: true                    # required: the operator accepts the CA's terms
  directory_url: "https://acme-v02.api.letsencrypt.org/directory"            # default
  staging_directory_url: "https://acme-staging-v02.api.letsencrypt.org/directory"   # default
  profile: "classic"                    # optional: the CA's default when absent
  contact: "mailto:ops@example.com"     # optional
  trust_anchor: "/etc/fluxbee/acme-ca.pem"   # optional, only for a private CA
web:
  archi: true                           # optional: archi.<the main name> joins the certificate
```

- The main certified name is the host of `admin.public_base_url` (`SY_ADMIN_PUBLIC_BASE_URL`
  wins), lowercase, with no trailing dot. The web hosts `web:` declares follow it
  ([The web hosts](#the-web-hosts)); one certificate carries every name. They are read at boot: a
  new name takes effect when SY.admin restarts, and certifying it is an explicit
  `POST /acme/renew` (after `POST /acme/test`). Until then the served certificate only raises
  `name_not_covered`. A name taken out goes at the next renewal.
- **Without `acme:`** nothing is ordered; the expiry checks and their alerts still run.
- **A section that cannot work turns ACME off** (`config.state: off` and `config.reason` in
  `GET /acme/status`, an ERROR line in the log): `terms_agreed` not `true`; `public_base_url`
  missing, not https, on a port other than 443, or with a host that is an IP literal, a wildcard
  or has no dot; a directory URL that is not https; a `trust_anchor` that cannot be read or holds
  no certificate; a key that is not one of the six above; the profile `shortlived` (six-day
  certificates do not fit the 24 h backoff cap); a profile the directory does not offer (found at
  the first check, off until a restart).
- The edge is `admin.public_edge_node`, else the one connected ingress of the hive registry
  (`resolve_public_edge_node_from`). Without either, `edge.not_configured` says why and nothing
  is checked.
- The edge's own `edge.tls_vault_key` (on the ingress) names the secret; SY.admin asks the edge
  for it and writes only a name that starts with `edge_tls`.

**The account.** One per directory URL, created on first use with the terms agreed and the
contact (`AcmeService::account`). Its credentials (the library's JSON, which holds the account
key) go to SY.vault under `acme_account:<first 16 hex of SHA-256(directory_url)>`
(`account_vault_key`): `resource_type: acme_account`, owner SY.admin, root tenant, the directory
URL as the description. `acme.json` records the account URL; a vault credential whose URL
differs from that record is refused. Neither the admin API nor Cloud writes these keys or reads
their value: a `vault_put`, `vault_rotate`, `vault_rollback`, `vault_delete`, `vault_assign` or
`vault_get` of an `acme_account*` key answers `403 RESERVED_KEY` (`refuse_acme_account_access`);
`vault_get_metadata` stays open.

**The CA's TLS.** Every connection to the CA (the directory and ARI reads, and instant-acme's own
calls) trusts the system's roots (`rustls-native-certs`, the `ca-certificates` package) plus
`acme.trust_anchor` when it is set (`InstantAcmeCa::new`, `ca_roots`).

**One order at a time.** The check, `POST /acme/renew`, `POST /acme/test` and `POST /acme/reload`
share one in-flight slot; a busy one answers `409 ACME_BUSY` with what is running.

**An order** (`AcmeService::order_flow`; the CA behind the `AcmeCa` trait, instant-acme 0.8.5 in
`InstantAcmeCa`):

1. **Look.** `EDGE_TLS_STATUS`. The edge must be in `vault` mode with a secret named `edge_tls*`;
   when the secret exists its metadata must be `resource_type: tls`, root tenant, owned by that
   edge (`check_edge_secret_metadata`). SY.vault must answer before any CA call. **SY.admin writes
   only onto what the edge serves**: the vault's current write, named by its version and
   `updated_at` (a rollback swaps versions and reuses their numbers), must be the one the edge
   reports (`vault_version`, `vault_updated_at`); an edge that serves nothing from the vault
   (awaiting, `not_found`) must have no secret at all. When they differ the edge is reloaded once
   (a metadata-only write, such as `vault_assign`, moves `updated_at` without changing the
   certificate) and compared again (`writable`); a difference that stays, or an edge that now
   serves another leaf, stops the run before any CA call.
2. **Order**: one order with every name as an identifier, the main one first, with the profile.
   `replaces` names the served leaf (its ARI identifier) only when `acme.json` records it as issued
   by this account of this directory; an error that points to it is retried once without it. The
   order URL is recorded at once with its names, and after a restart the order is resumed
   (`Account::order`), not placed again, when it carries the same names; one past finalize is
   replaced, since its key died with the run. An abandoned order's authorizations whose challenge
   was never marked ready are deactivated (`deactivate_unready`); one the CA validated, or still
   validates, stays.
3. **Challenge**, for each authorization (one per name): `EDGE_ACME_CHALLENGE` arms each pending
   one's TLS-ALPN-01 digest for 600 s (the edge keeps one challenge per name), each is marked
   ready, and the order is polled for up to 5 min. An order that does not end ready, or a partial
   mark, is left to settle (`settle`): a CA can mark the order invalid as soon as one name fails,
   while it still validates another, so the authorizations are read again every second, for up to
   a minute, until none is still being validated. Only then are the failed names named, with
   their errors, and every armed challenge cleared, whatever happened.
4. **Finalize** with a fresh P-256 key and a CSR (empty subject, SAN = every name, the main one
   first; rcgen 0.13).
5. **Check before storing** (`check_material`): the edge's own verdict
   (`json_router::tls_material::inspect`), every name, a chain of at least 2 that verifies to the
   platform roots (`rustls-native-certs`; `ca-certificates`) or `acme.trust_anchor` for each name,
   and more than 2 days left.
6. **Store**: `VAULT_PUT` of `{cert, key}` under the edge's secret with explicit metadata
   (`resource_type: tls`, owner the edge, root tenant), right after checking again that the
   vault's current write is still the one the edge served (same reload-once rule). A vault that
   does not answer is retried for 10 min with the material in memory; then the certificate is
   dropped. A put SY.vault applied whose answer was lost is recognized at the retry (the next
   version, written by SY.admin). The write is read back (version and `updated_at`) and recorded.
7. **Reload**: `EDGE_TLS_RELOAD` (75 s); the edge must answer with the new leaf's fingerprint. A
   refusal of the material (`invalid_material`; `not_found` or `forbidden` after SY.admin's put)
   rolls the secret back at once and holds every order until the operator acts
   (`install_refused`). The rollback is made only while SY.admin's write is still the current
   one, so it never runs twice (`rollback_if_current`): one that does not go through is retried at
   each check, which first checks the write again, and a renewal waits for it. A refused first
   certificate has nothing to roll back to: delete the secret before renewing. A `transient`
   refusal, or no answer, keeps the certificate in the vault: the next check reloads it, never
   orders a new one.
8. **Record** the attempt in `acme.json`.

The private key exists in SY.admin's memory and in the vault only: it is never logged and never
written to disk.

**The main name is never held hostage** by an added one (`Run::main_alone_after`). An order that
fails only on added names, the main name's authorization validated, is abandoned and followed at
once by an order for the main name alone, when the run may fall back (a check's order, or
`POST /acme/renew` when the edge serves no leaf for the main name or the served one is past its
renewal point, `main_needs_certificate`; never a test order) and the edge loses nothing it serves
or cannot wait (`main_alone_loses_nothing`: no served leaf covers the main name, the served leaf
already lacks every name dropped, or it is in the last sixth of its lifetime). The attempt is then
`installed` with a detail that says why, and `name_not_covered` stays open for the missing name.
Otherwise the attempt fails and the backoff retries every name: a passing failure never replaces a
certificate for every name. A renewal 48 h past its point (the `renewal_overdue` mark), after an
order that failed only on added names (`AttemptRecord::failed_names`), orders the main name alone.
The next renewal tries every name again.

**The check** (`AcmeService::check_once`) runs 2 min after the start, then every 6 h plus up to
30 min. It reads `EDGE_TLS_STATUS` and decides (`decide`):

| The edge | `acme:` on | `acme:` absent or off |
| --- | --- | --- |
| no answer, or its router answers UNREACHABLE (`VPN_BLOCKED` included) | no order, no CA backoff; again in 10 min; `edge_unreachable` after 1 h; expiry judged from the last status seen | the same |
| answers `status: error` (no public listener, another origin) | a configuration error, in `edge.config_error`: no order, the regular schedule, no alert | the same |
| `awaiting`, `not_found` | order now | `edge_without_certificate` |
| `awaiting`, another reason | `EDGE_TLS_RELOAD`, then the alert; no order | `edge_without_certificate` |
| serves a leaf this account issued for the main name | renew at a random point of the CA's ARI window (asked again after its Retry-After), else at two thirds of the lifetime, with every name (a leaf that misses an added name too: `name_not_covered` meanwhile); 48 h past that point, after an order that failed only on added names, the main name alone | expiry checks only |
| `acme.json` holds a newer certificate than the served one | `EDGE_TLS_RELOAD`, never a new order | |
| serves a hand-seeded leaf, or one for another main name | alerts only; `POST /acme/renew` replaces it | expiry checks only |
| `disk` or `plaintext` mode | left alone | expiry checks only |

- **Backoff**: a failed order waits 1 h, 2 h, 4 h and so on, up to 24 h; a `rateLimited` answer
  waits until the time the CA names, or 24 h. Both are in `acme.json`, so a restart never spends
  an order.
- **The cap**: at most one issuance per third of the last certificate's lifetime, unless forced
  (`POST /acme/renew`). It counts the certificates stored and served or waiting to be served; a
  failure after issuance is bounded by the backoff, and never holds an edge with no certificate.

**Conditions** (`evaluate_conditions`), shown in `GET /acme/status`. Each writes one drift alert
when it opens and one `info` row when it closes, through `record_drift_alert` to
SY.orchestrator@motherbee (category `certificate`, the edge's hive); a standing condition writes
nothing more, and an alert the orchestrator did not take is retried at the next check. Queued
closures go out before opens, and an answer `recorded: false` (the orchestrator's newest row for
that hive, kind and state is the same) counts as delivered.

| Condition | Severity | When |
| --- | --- | --- |
| `renewal_failed` | warning | the last attempt failed |
| `renewal_overdue` | warning | 48 h past the renewal point, and the edge still serves that leaf |
| `expiring` | critical | less than a sixth of the served leaf's lifetime is left |
| `name_not_covered` | warning | the served leaf does not cover one of the names (the detail lists the missing ones) |
| `foreign_certificate` | warning | a hand-seeded leaf passed its renewal point (two thirds) |
| `install_refused` | critical | the edge refused a certificate SY.admin issued |
| `edge_without_certificate` | critical | the edge is `awaiting` |
| `edge_unreachable` | warning | no status for 1 h |

`expiring`, `edge_without_certificate` and `edge_unreachable` run with or without `acme:`; the
others need it on.

**Actions:**

| Action | Route | Answer |
| --- | --- | --- |
| `acme_status` | `GET /acme/status` | the configuration (and why it is off; `config.name` is the main name, `config.names` every name), the terms URL, the directories read, the accounts, the edge (`live`: a fresh `EDGE_TLS_STATUS`; `last_seen`: what the last check judged by; `names_covered` and `names_missing`: the names its leaf covers), the run in flight, the order recorded, a certificate waiting to be served, the hold and the pending rollback, the last check, attempt and test, the backoff, the next check, the renewal plan, the last 20 issued certificates (each with its `names`) and the open conditions. Times are RFC 3339. |
| `acme_renew` | `POST /acme/renew` | `202 accepted`; needs confirmation. Forced: no backoff and no cap, never past the CA's rate limit. Lifts an `install_refused` hold. Refused with `409`: `ACME_OFF`, `EDGE_NOT_CONFIGURED`, `ACME_BUSY`, `ACME_RATE_LIMITED`, `CERTIFICATE_NOT_SERVED` (reload first), `ROLLBACK_PENDING` |
| `acme_test` | `POST /acme/test` | `202 accepted`; needs confirmation. Steps 1–5 against `staging_directory_url` with its own account and no `replaces`; nothing is stored or installed, and the staging root is reported untrusted (`last_test`). `409 ACME_TEST_SAME_DIRECTORY` when staging equals production |
| `acme_reload` | `POST /acme/reload` | `EDGE_TLS_RELOAD`, after a manual change or a vault rollback: `200` with what the edge serves, `422 EDGE_REFUSED` (the edge kept its certificate, `refused.reason` and `refused.detail`), `503 EDGE_VAULT_UNAVAILABLE` (a `transient` refusal), `504 EDGE_NO_ANSWER`, `502 EDGE_ERROR` |

None takes a parameter: a body with a field answers `400 INVALID_REQUEST`. The three POSTs are
in the audit log. SY.architect's read tool translates `GET /acme/status`.

## The web hosts

The motherbee's `web:` section declares the hosts the public edge serves only through io.web,
never through `/e/` or `/public/` ([docs/io-web-v2.md](../io-web-v2.md) §11.1). Source:
`WebConfig` and `WebRoutes` in `src/bin/sy_admin.rs`.

- **The declaration** (`WebConfig::from_hive`): each key is a host label, declared with `true`;
  `archi` is the only one. The host is the label before the main certified name (`archi: true`
  declares `archi.<host of admin.public_base_url>`), so the domain never comes from code. A bare
  `web:`, `{}` or `archi: false` declares nothing. An unknown key, a value that is not a boolean, or
  a `public_base_url` with no certifiable host declares nothing, with an ERROR line. It is read at
  boot, which logs the declared hosts. The ACME client certifies them after the main name
  ([The ACME client](#the-acme-client)).
- **The push** (`WebRoutes::run`): at the start and every 60 s, SY.admin's own loop (the ACME check
  runs every 6 h) sends `EDGE_WEB_ROUTES {routes: [{host, upstream: "io.web"}]}` to the public edge
  as the ACME client resolves it (`admin.public_edge_node`, else the one connected ingress), with
  a 20 s timeout. The edge takes it on its TLS command channel. Each push carries the full set: an
  empty one without `web:`, so a host taken out leaves the edge at the first push after the
  restart.
- **The answer** is kept in memory: the edge, `acknowledged`, `refused`, `no_answer` or `no_edge`
  with its detail, since when, and when the edge last acknowledged the set. An acknowledgement
  also carries the edge's proxy to io.web (`upstream`: `ready` or `unavailable` with its reason,
  and the last request io.web did not answer, its time and kind), kept with that push
  (`EdgeWebUpstream`). Only a change of edge or of answer is logged, the detail included (a
  timeout reads `no answer in 20s`, so repeated ones are no change), never a change of upstream:
  INFO when acknowledged, and for no edge while no host is declared; WARN otherwise, with the last
  acknowledgement. An edge older than 0.1.84 does not take the command, which reads as no answer.

## The web door's status

`GET /web/status` (`web_status`) is the operator's view of the web door
([docs/io-web-v2.md](../io-web-v2.md) §11.2): what SY.admin holds, plus two reads of io.web. It
takes no body and always answers 200: no `web:`, no edge, and a missing or silent io.web are
states, named in the answer. Source: `handle_web_status` and `io_web_status` in
`src/bin/sy_admin.rs`, `AcmeService::web_certificate` in `src/bin/sy_admin/acme.rs`.

| Part | What it says | From |
| --- | --- | --- |
| `web` | `state`: `declared` (with `hosts`), `absent` (no section, or no key set) or `invalid` (`reason` says why) | `web:` as read at boot |
| `certificate` | `acme` (the client's `config.state`); `names`, the names it certifies (the main one, then the web hosts); `served`, the `names` and `not_after` of the leaf the edge served at the client's last look; `last_answer_at`, the edge's last answer to the client; `hosts_missing`, the web hosts that leaf does not cover (null without a leaf) | the ACME client, with no call to the edge: `config.names` and `edge.last_seen` of `GET /acme/status`, which asks the edge afresh |
| `routes` | `state`: `acknowledged`, `refused`, `no_answer`, `no_edge`, or `not_pushed` before the first push; `edge`, `set` (the full set every push carries), `detail`, `since_at` and `acknowledged_at`; `upstream`, the edge's way to io.web as its acknowledgement reported it (null for any other answer): `state` `ready` or `unavailable` (then every web host answers 502), `reason` (what the edge found missing at its boot), `last_failure` (`at`, `kind` of the last request io.web did not answer, or null) | the last push ([The web hosts](#the-web-hosts)) |
| `io_web` | `node` (`IO.web@motherbee`), `state`; `lifecycle_state` and `runtime_version`, the orchestrator's view; `answer`, the node's own `NODE_STATUS_GET` answer; `view_error` and `answer_error` | `get_node_status` to `SY.orchestrator@motherbee` (5 s) and `NODE_STATUS_GET` to `IO.web@motherbee` (3 s), sent together |

`io_web.state`:

- `running`: it answered `NODE_STATUS_GET`, whatever the orchestrator says; `answer.health_state`
  is the health it reports;
- `silent`: the orchestrator sees it `RUNNING`, but it did not answer in 3 s;
- `starting`, `stopped`, `failed`: the orchestrator's lifecycle;
- `absent`: the orchestrator has no `IO.web@motherbee` (`NODE_NOT_FOUND`), never launched or
  purged. io.web starts only with an explicit `run_node`, in the root tenant;
- `unknown`: neither read says (`view_error`, `answer_error`).

Times are RFC 3339. A read, so it is not in the audit log; over the mesh only Archi may ask for it
(`mesh_access`).

## Config

`/etc/fluxbee/hive.yaml` (`HiveFile`):

| Key | Use |
| --- | --- |
| `hive_id`, `role` | role checks |
| `admin.listen` | HTTP address (env `JSR_ADMIN_LISTEN` wins) |
| `admin.public_edge_node`, `admin.public_base_url` | public artifacts and the ACME client; the host of `public_base_url` is the certified name (env `SY_ADMIN_PUBLIC_EDGE_NODE`, `SY_ADMIN_PUBLIC_BASE_URL` win) |
| `acme` | the ACME client ([The ACME client](#the-acme-client)) |
| `web` | the web hosts, pushed to the public edge and certified after the main name ([The web hosts](#the-web-hosts)) |
| `ai` | executor provider and model |
| `blob.path` | blob root, default `/var/lib/fluxbee/blob` |
| `storage.path` | root of the `/modules` store, default `/var/lib/fluxbee` |

Environment: `JSR_LOG_LEVEL`, `JSR_ADMIN_LISTEN`, the timeout overrides above.

The list of hives is not in hive.yaml: it is the hive registry, `/var/lib/fluxbee/hives/<hive>/`,
which `add_hive` creates and `remove_hive` deletes (FINDINGS A-48). The hives a policy write
waits for, and the ones an OPA read reports as expected (`expected_hives_policy`), are the
motherbee plus every registry entry whose `info.yaml` says `status: connected`
(`opa_rollout_hives`, `expected_hive_sets`). The A-62 table reads a mesh caller's hive role from
the same entries (`caller_hive_role`).

## State on disk

- `/var/lib/fluxbee/state/sy-admin/command-log/`: the audit log.
- `/var/lib/fluxbee/state/sy-admin/publications.json`: the publication ledger.
- `/var/lib/fluxbee/state/sy-admin/acme.json`: the ACME client's state, written atomically (no
  key): the account URL per directory, the order in flight, a certificate waiting to be served,
  the last attempt and test, the last 20 issued certificates, the backoff, the renewal plan, the
  last edge status seen and the conditions. An unreadable file is moved aside
  (`acme.json.unreadable-<time>`) and ACME starts empty.
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
- **ACME**: a section that cannot work turns ACME off; a CA, edge, vault or orchestrator that
  fails only fails the order or the alert, which the backoff and the next check retry. Nothing
  in the ACME client stops SY.admin.
- **Web routes**: a `web:` section that cannot work declares nothing; an edge that refuses the
  routes or does not answer is logged when that starts, and the full set goes again every minute.

## Health

- `curl -s http://127.0.0.1:8080/health` → `{"status":"ok"}` (on the motherbee).
- `curl -s http://127.0.0.1:8080/hive/status` goes through the orchestrator, so it also shows
  whether the mesh answers.
- `systemctl status sy-admin`; the log line `sy.admin http listening` marks the HTTP server up.
- `GET /hives/motherbee/commands?limit=20` shows what was run lately and how it ended.

## Action table

Generated from `INTERNAL_ACTION_REGISTRY` (registry version 16, 106 actions, in registry order),
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
| `publish_artifact` | — (mesh only) | Publish an existing BlobRef through SY.edge. Mesh-only: the router-stamped AI.*, IO.* or WF.* caller, on a worker or the motherbee, becomes the publication owner. |
| `unpublish_artifact` | — (mesh only) | Revoke a public artifact the calling AI.* producer published. Mesh-only and identity-bound; the operator revokes any with POST /artifacts/unpublish. |
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
| `teardown_node` | `POST /hives/{hive}/nodes/{node_name}/teardown` | Remove what a torn-down node leaves in the stores: the secrets dedicated to its ILK, then the ILK itself (marked, then purged). |
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
| `inventory` | `GET /inventory`<br>`GET /inventory/summary`<br>`GET /inventory/{hive}`<br>`GET /hives/{hive}/inventory/summary`<br>`GET /hives/{hive}/status` | Read global or per-hive inventory, including system-wide node visibility. |
| `opa_compile_apply` | `POST /opa/policy` | Compile the user OPA policy on the motherbee and apply it on every hive (one global policy). |
| `opa_compile` | `POST /opa/policy/compile` | Compile OPA policy. |
| `opa_apply` | `POST /opa/policy/apply` | Apply the staged user OPA policy on every hive. |
| `opa_rollback` | `POST /opa/policy/rollback` | Roll every hive back to the previous user OPA policy. |
| `opa_clear` | `POST /opa/policy/clear` | Remove the user OPA policy on every hive (current, staged, backup): back to a fresh install; routers unload it. |
| `opa_check` | `POST /opa/policy/check` | Validate OPA policy text without staging or applying it. |
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
| `acme_status` | `GET /acme/status` | Read the ACME client of the public edge's certificate: its configuration (and why it is off), the terms URL, the accounts, the edge's live status, the order in flight, the last attempt and test, the next check and renewal point, the last issued certificates and the open certificate conditions. |
| `acme_renew` | `POST /acme/renew` | Order the public edge's certificate now (forced: no backoff, no cap). |
| `acme_test` | `POST /acme/test` | Run a test order against the staging directory with the real names: issued and checked, never stored or installed. |
| `acme_reload` | `POST /acme/reload` | Make the public edge re-read its certificate from SY.vault (EDGE_TLS_RELOAD), after a manual change or a vault rollback. |
| `web_status` | `GET /web/status` | Read the web door: the hosts the web section declares, the certificate's names, the routes pushed to the public edge with its last answer, and io.web's state. |

### HTTP routes outside the catalog

Served by `handle_http`, `handle_hive_paths` and `handle_modules_paths` but not in the registry:

| Route | What it does |
| --- | --- |
| `GET /health` | Liveness, answered locally |
| `GET /admin/executor/functions` | The executor's function catalog |
| `POST /channels/externalize`, `POST /channels/unexternalize`, `GET /channels/externalized?edge_node=` | Edge channels as the operator (`ADMIN_ROUTES_OUTSIDE_ACTION_REGISTRY`) |
| `POST /artifacts/unpublish` | Revoke any publication, body `{"publication_id": "pub:<uuid>"}` |
| `GET /config/storage/metrics` | SY.storage inbox metrics |
| `GET /inventory/nodes` | Global inventory, nodes |
| `GET /versions?hive=`, `GET /deployments?hive=`, `GET /drift-alerts?hive=` | The per-hive form of the global route |
| `GET /opa/policy?hive=`, `GET /opa/status?hive=` | Hive-less forms of the OPA reads |
| `POST /wf-rules`, `POST /wf-rules/{compile,apply,rollback,delete}`, `GET /wf-rules`, `GET /wf-rules/status` | Hive-less forms of the wf-rules actions (hive from the body or query) |
| `GET /modules`, `GET /modules/{name}`, `GET /modules/{name}/{version}`, `POST /modules/{name}/{version}` | The module store: list, versions, download (tar or file), upload a tar |

## Links

- [SY.orchestrator](sy-orchestrator.md): where node and hive actions land.
- [SY.architect](sy-architect.md): writes the executor plans.
- [SY.edge](sy-edge.md): `externalize`, public artifacts, and the TLS commands the ACME client sends.
- [ACME certificates for the public edge](../acme-edge-certificates-v1.md): the ACME client's design.
- [SY.vault](sy-vault.md): the vault actions and their rules.
- The other targets: [SY.identity](sy-identity.md), [SY.config.routes](sy-config-routes.md),
  [SY.opa.rules](sy-opa-rules.md), [SY.wf-rules](sy-wf-rules.md), [SY.timer](sy-timer.md),
  [SY.storage](sy-storage.md), and [the router](rt-gateway.md) for the system policy.
- [Admin internal gateway spec](../history/admin-internal-gateway-spec.md): the `ADMIN_COMMAND` contract.
- [IO.cloud API](../io-cloud-api.md): the Cloud side of the relay.
- [ILK and tenant model](../ilk-tenant-model-v1.md): tenants, Cloud rules.
- [Host posture spec](../host-posture-and-exposure-spec-v1.md): why SY.admin stays on loopback.
- [Control-path hardening](../control-path-hardening-v1.md): the A-62 table and `teardown_node`.
- `lab/logbook/FINDINGS.md`: A-46 (no HTTP auth), A-62 (mesh callers), A-68 (one task per command).
