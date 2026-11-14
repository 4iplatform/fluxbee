# SY.admin

SY.admin is the control-plane gateway of a Fluxbee mesh. The operator's actions, and the admin
actions that nodes ask for, reach it two ways: an HTTP API on the motherbee's loopback, and the
`ADMIN_COMMAND` message over the mesh. SY.admin does little work itself. For each action it finds
the node that owns it (SY.orchestrator, SY.config.routes, SY.identity, SY.vault, SY.opa.rules,
SY.wf-rules, SY.timer, SY.storage, SY.cognition, SY.edge, IO.blob), forwards the request, and
returns one envelope. It also runs the executor, which carries out executor plans one step at a
time with an LLM.

Source: `src/bin/sy_admin.rs`. The Fluxbee Cloud allowlist is `CLOUD_EXPOSED_ACTIONS` in
`crates/fluxbee_sdk/src/cloud.rs`.

**Live schema.** `GET /admin/actions` returns every action with its routes, request contract,
executor schema and an example; `GET /admin/actions/{action}` returns one
(`admin_actions_catalog_envelope`, `build_admin_action_help_response`). Over the mesh, and to the
executor, `list_admin_actions` answers the compact catalog instead, each action's `name`,
`description` and `read_only` (`view: compact`, `GET /admin/actions?view=compact` over HTTP): the
full one, some 330 KB, is larger than one mesh frame (128 KiB), so a mesh caller that asks
`view: full` is refused with `CATALOG_TOO_LARGE_FOR_MESH` rather than left waiting on a reply the
frame refuses (`admin_catalog_view`, FINDINGS A-197). Archi asks the compact one for its plan
compiler and its reads, and one action's help for its schema. The
[action table](#action-table) at the end of this page is generated from the same functions. The
AI node config examples (`set_node_config`, `node_control_config_set`) name no
`behavior.provider`, so a copied one runs on the hive's `ai.default_provider`; their notes say
to name a provider only when the node's tenant holds a key of it, since without one every message
fails with `missing_ai_api_key`.

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
  `SY.admin runs only on the motherbee; role != motherbee` and exits 0; with `role: motherbee` the
  `hive_id` must be `motherbee` (`PRIMARY_HIVE_ID`), and `hive_id: motherbee` on another role is
  an error.
- The packaged unit carries the same check as an `ExecCondition` (`unit_role_gate` in
  `packaging/build-deb.sh`), so a start pulled in on another role is skipped, not looped.
- It is a vault consumer: the release's list for the motherbee (`src/release_settings.rs`) starts
  `SY.admin` before `SY.vault`, as its tests (`check_system_nodes`) require.

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
7. Logs once where the executor's `ai` comes from (`log_admin_ai_source`): the installation file,
   with its version and hash, or hive.yaml while the hive holds none
   ([hive-config-update-v1](../hive-config-update-v1.md) §4). Settings it cannot use (an
   installation file no reader can use, which is never a fallback to hive.yaml, or an invalid
   hive.yaml `ai` while the hive holds no file) never stop the start: a warning with the reason,
   which the executor then answers with; the orchestrator's `/versions` shows the file's state
   (`config`). hive.yaml's `ai` is read untyped, and typed only while the hive holds no file. The
   executor's AI key is read in a task of its own, started with the loops of
   step 9 (`boot_admin_executor_ai_runtime`, see [The executor](#the-executor)): on an upgrade the
   whole hive restarts at once, so the read waits for a vault that does not answer yet, up to 60 s
   (`resolve_resource_awaiting_vault`, `VAULT_BOOT_WAIT`; FINDINGS A-141), and the admin serves
   meanwhile. Until the key is read, and with no key in the vault, the executor is unavailable.
8. Reads the `web:` section (`WebConfig::from_hive`) and logs the web hosts it declares, then the
   `acme:` section and the certified names (`AcmeConfig::from_hive`): the main name, then those
   hosts. A section that cannot work declares nothing (`web:`) or turns ACME off (`acme:`) with a
   reason, in the log (and in `GET /acme/status`); neither stops the start.
9. Starts the HTTP server, the `NODE_STATUS_GET` loop, the `ADMIN_COMMAND` loop, the system loop
   (`CONFIG_GET`, `CONFIG_SET`, `VAULT_SECRET_CHANGED`), the publication expiry sweep (every
   600 s, `run_publication_expiry_sweep`), the ACME check (2 min after the start, then about
   every 6 h, `AcmeService::run`; see [The ACME client](#the-acme-client)), the web routes push
   (at once, then every 60 s, `WebRoutes::run`; see [The web hosts](#the-web-hosts)) and the
   installation update's background task, which picks an open update up from disk
   (`installation_update::run_background`; see
   [The installation's update](#the-installations-update)).

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
  `sync_pending` becomes 202 (`build_admin_http_response`), and so do `set_installation_ai`'s and
  `update_installation`'s `partial` ([The installation's ai](#the-installations-ai),
  [The installation's update](#the-installations-update)).
- **Unknown route.** `404 UNKNOWN_ROUTE`, or `405 METHOD_NOT_ALLOWED` with `allowed_methods` when
  the path exists with another method; both carry `"see": "GET /admin/actions"`
  (`admin_unknown_route_response`).
- **A handler that fails before it answers** (a body that does not parse, a field of the wrong
  type, a message the SDK refuses) still answers, in the same envelope, and the log keeps the
  reason (`http handler error: ...`; FINDINGS A-229): `413 MESSAGE_TOO_LARGE` when the message
  is larger than a mesh frame (128 KiB), with its size and the most it may be, since nothing was
  sent; `500 INTERNAL_ERROR` otherwise. The detail never repeats the request
  (`serve_http_connection`, `http_handler_error_response`). Before 0.1.125 the connection closed
  with no answer.
- **A body past 1 GiB** (`HTTP_BODY_MAX_BYTES`, above the largest body an action takes, a
  `bundle_upload`'s 512 MiB zip): a request whose `Content-Length` declares more answers
  `413 BODY_TOO_LARGE` with the size it declared, before its body is read and with nothing
  allocated for it (`read_http_request`, FINDINGS A-234).
- **Headers past 64 KiB** (`HTTP_HEADER_MAX_BYTES`, the request line and headers up to the blank
  line that ends them): `431 HEADERS_TOO_LARGE`, without reading on (`read_http_request`,
  FINDINGS A-234).
- **After either of those two answers** the request has not been read to its end, and closing the
  socket with the client's bytes unread would send a reset, which a client still sending sees
  instead of the answer. So SY.admin ends its side after the answer, then reads and drops what the
  client still sends until the client ends its own, for 2 s and 8 MiB at most
  (`HTTP_UNREAD_DRAIN_TIME`, `HTTP_UNREAD_DRAIN_BYTES`), and only then closes
  (`close_after_unread_request`).
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

### Replies that do not fit a frame

The SDK refuses a mesh frame over 128 KiB (`MAX_FRAME_SIZE`) before it sends it, and past the
SDK the router adds to the frame (the sender's stamped `src_l2_name`, a missing
`meta.action_class`) and drops one past the frame without telling the sender (FINDINGS A-202).
Before 0.1.123 the caller then waited to its own timeout, with no reason (FINDINGS A-198). Now:

- **Every reply is answered.** SY.admin measures each `ADMIN_COMMAND_RESPONSE` serialized against
  the frame less the router's 1 KiB reserve (`mesh_reply::REPLY_MAX_BYTES`,
  `mesh_reply::oversized`), and takes the SDK's `MessageTooLarge` the same way. A reply past it
  goes as `REPLY_TOO_LARGE`, logged once with the action, the caller and the size
  (`send_admin_command_response`, `reply_too_large_message`). The envelope is `status: "error"`,
  `error_code: "REPLY_TOO_LARGE"` (HTTP status 413), and an `error_detail` object:
  - `message`, `action`, `size` (the reply's bytes) and `max` (the most a reply may take);
  - `paging` for an action that pages (`admin_action_paging`): its `params`, the field that names
    the next page (`next`) and the field of its rows (`rows`), with `suggested_limit`, about how
    many rows fit half a frame at the size the reply's rows had;
  - `http`, for a read that does not page: its route, since HTTP on the motherbee has no frame.
    Never to Fluxbee Cloud's relay (`IO.cloud@motherbee`), which reaches SY.admin only through
    IO.cloud: it is told the pages, or that the answer has no smaller form over the mesh
    (`admin_reply_hint_reach`);
  - `outcome`: the action's own `status` and `error_code`, and the ids its payload named
    (`request_id`, `execution_id`, `ilk_id`, `tenant_id`, `node_name`, `hive_id`, `key`,
    `version`, ...; `mesh_reply::reply_outcome`).
- **An action that changed something says it ran.** For an action the registry does not mark
  `read_only`, and the mesh-only `executor_execute_plan`, `externalize` and `unexternalize`
  (`admin_reply_reports_a_change`), `error_detail.action_ran` is `true`, `read_only` is `false`,
  the message says it ran and must not be run again to see its answer, and the reply's
  `meta.action_result` is the action's own, from its kept `outcome` (`applied` for an `ok`). An
  executor run's `outcome` also keeps its `plan_status`, `completed_steps`, `total_steps`,
  `failed_step_id` and `log_path`, its full log (`admin_reply_outcome_paths`).
- **The nodes behind SY.admin answer the same way.** SY.identity, SY.orchestrator and SY.vault
  measure their own answers the same way and send `REPLY_TOO_LARGE` in place of one past it, in
  their own payload shape (`status`, `error_code`, `message`, `size`, `max`, `outcome`, and `rows`,
  the rows of a listing it held; `mesh_reply::reply_too_large_payload`). SY.admin answers its
  caller in its own shape (`source_reply_too_large_detail`): the action's pages and
  `suggested_limit`, `action_ran` from the source's outcome, the outcome, and the source's text as
  `source`; never an HTTP route, since its HTTP route takes the same hop. Their listings come in
  pages, so the hop between them and SY.admin stays within the frame (below).
- `CONFIG_GET` and `CONFIG_SET` answers, the executor's config, have a fixed shape well under a
  frame and are sent as they are.

### Pages

The listings that grow with the installation answer in pages. Each page names where the next one
starts, and `total` says how many rows there are in all (`timer_list` answers none, below):

| Action | Ask a page with | Next page | Rows | Order |
| --- | --- | --- | --- | --- |
| `list_ilks`, `list_tenants` | `limit`, `cursor` | `next_cursor` | `ilks`, `tenants` | by id |
| `list_nodes` | `limit`, `cursor` | `next_cursor` | `nodes` | by name |
| `list_tenant_nodes` | `limit`, `cursor` | `next_cursor` | `nodes` | by hive, then name |
| `inventory` (`global`, `hive`) | `limit`, `cursor` | `next_cursor` | `nodes` | by hive, then name |
| `list_versions` | `limit`, `cursor` | `next_cursor` | `hives` | the motherbee, then the others by id |
| `list_recent_commands` | `limit`, `cursor` | `next_cursor` | `commands` | newest first, by `seq` |
| `list_published_artifacts` | `limit`, `cursor` | `next_cursor` | `publications` | newest first, then by id |
| `vault_list` | `filter.limit`, `cursor` | `next_cursor` | `secrets` | newest write first |
| `get_installation`, `get_versions`, `list_versions` | `versions_limit`, `versions_before` | `versions_next_before` | `versions` | oldest first, the newest 20 |
| `get_llm_usage` | `limit`, `after` | `next` | `records`, `groups` | SY.storage's, under 64 KiB a page |
| `timer_list` | `limit`, `cursor` | `next_cursor` | `timers` | by fire time |

- **No caller's default changed.** Without a `limit` a listing answers every row, as before, but
  for the three that never did: `vault_list` answers its filter's limit (200 without one, at most
  1000), `list_recent_commands` its `limit` (50 without one, at most 500), and `timer_list` the
  one page SY.timer answers: `limit` timers (100 without one, at most 1000), or fewer when the
  frame cuts the page first, with `next_cursor` while timers follow (since 0.1.133, FINDINGS
  A-266; before, only a page the frame cut gave one). Over HTTP the query parameters are the same
  names (`?limit=&cursor=`, `?versions_limit=&versions_before=`), and an empty `?cursor=` is none.
  A caller whose answer outgrows the frame gets `REPLY_TOO_LARGE` with the params to page and
  `suggested_limit`: it asks again with that limit and follows the next page until the answer
  names none.
- **The one exception is the installation's version history.** The motherbee's `config.versions`
  in `/versions` and `get_installation`'s `versions` list the newest 20 versions recorded, oldest
  first, with `versions_total`, `versions_latest` (the newest version recorded, whichever page is
  listed) and, while older ones remain, `versions_next_before`: pass it as `versions_before` (and
  `versions_limit`, 1 to 100) to list them. `get_installation`'s `expected_version` for a file no
  reader can use is `versions_latest`, so a page of older versions names it too; the newest
  version is always on the default page, which is all `set_installation_ai` reads.
- **SY.admin pages the hops behind it** (`assemble_list_pages`). SY.identity (`ILK_LIST`,
  `TNT_LIST`), SY.orchestrator (`list_nodes`, `list_tenant_nodes`, `INVENTORY_REQUEST`,
  `list_versions`) and SY.vault (`VAULT_LIST`) answer a page within a frame: half a frame of rows
  (`LIST_PAGE_MAX_BYTES`, 64 KiB), and for SY.vault 96 KiB, since other nodes read it without
  pages. SY.admin asks page after page and assembles them into the answer its caller asked for:
  every row without a `limit`, at most `limit` rows with one, and the source's `next_cursor` while
  rows follow. A source older than the pages answers every row at once and names no next page,
  which ends the listing. `list_versions`' pages each compare their hives with the motherbee's
  snapshot, and SY.admin joins them (`merge_versions_comparisons`): the hives' verdicts add up and
  the worst wins (`differs`, then `unknown`, then `match`). SY.admin's own reads of SY.vault, the
  provider keys a reader may read (`list_provider_keys`) and the secrets that block a tenant's
  purge (`purge_tenant_vault_block`), follow every page the same way (`vault_list_pages`).
- **A listing takes its caller's time, no more.** The whole assembly is bounded by the time the
  action gives one request (`admin_action_timeout`; `inventory`'s 10 s): each page is asked with the
  time left, and a listing not done by then answers `TIMEOUT`, naming its pages. Ask such a
  listing in pages.
- **A listing is no snapshot.** Each page is the source's at the time SY.admin asks it, so a row
  written, changed or removed while SY.admin pages may be in the listing or not, though never
  twice. A secret written meanwhile moves to the front of `vault_list`'s order.
- **Cursors are opaque.** Pass `next_cursor` back as it came. A cursor names a place in the order,
  not a row, so a row removed since keeps the place, and a row added before it does not move the
  next page. A cursor the listing did not give (another listing's, or none at all) answers
  `INVALID_CURSOR` (HTTP status 400), never an empty page; `timer_list`'s is SY.timer's to check
  (below). `list_recent_commands`' place is each entry's `seq`, given under the log's lock in the
  order the entries were written, so a clock that steps back neither hides nor repeats an entry;
  entries written before 0.1.123 have none and page after every entry with one. Its pages also
  stay within half a frame, whatever the entries' params.
- **`timer_list` is SY.timer's page, as SY.timer answers it** ([SY.timer §7.6](../sy-timer.md)),
  so the last two points hold for it only in part. SY.admin asks one `TIMER_LIST` and answers it,
  with no `total`. A timer whose fire time moves between two pages (a recurring timer's fire, a
  `TIMER_RESCHEDULE`) takes its new place in the order, so it can come twice or not at all. A
  cursor that is not one is refused with `TIMER_INVALID_PAYLOAD` (HTTP status 400), and a
  well-formed one SY.timer never gave lists the timers after the place it names.

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
| `SY.architect@motherbee` | every action `mesh_access` lists, `get_llm_usage` of all tenants included, but those closed to the mesh (`cognition_clear`, `reset_base_runtime`, `set_installation_ai`, `update_installation`, `set_hive_egress`, `clear_hive_egress`) | `authorize_llm_usage_scope` |
| `IO.cloud@motherbee` | the Cloud-exposed actions (`CLOUD_EXPOSED_ACTIONS`); payload rules apply (next section) | `enforce_cloud_relay_content`, `stamp_relay_facts` |
| `SY.wf-rules@H` | the node actions `run_node`, `restart_node`, `kill_node`, `get_node_config`, `set_node_config`, `get_node_status`, only on `WF.*@H`, binding or launching `wf.<name>` on `wf.engine`; the package actions and `sync_hint` (below) | `authorize_wf_rules_node`, `authorize_wf_rules_package` |
| `SY.orchestrator@H` | `teardown_node` for a node `*@H` ([Node teardown](#node-teardown)) | `authorize_orchestrator_teardown` |
| `IO.*@H` (IO.cloud included) | `externalize` and `unexternalize` of a channel it owns; `list_externalized` shows only its rows | `authorize_channel_command`, `filter_externalized_channels_for_caller` |
| `AI.*@H`, `IO.*@H` (IO.cloud included), `WF.*@H` | `publish_artifact` (the caller needs a complete identity registration; its tenant owns the publication), and `unpublish_artifact` of its own publications only (FINDINGS A-243, since 0.1.137, where only `AI.*@H` might) | `authorize_artifact_publisher`, `resolve_artifact_publisher`, `handle_unpublish_artifact` |

- **The table lives next to the catalog.** `mesh_access`, right after `INTERNAL_ACTION_REGISTRY`,
  gives every action of the catalog and every mesh command off it its row. An action it does not
  list is closed to the whole mesh, Archi included, and the unit test
  `every_action_has_a_mesh_decision` fails until it is listed. One closed on purpose is listed as
  `Nobody`.
- **A hive's role comes from the hive registry**, `/var/lib/fluxbee/hives/<hive>/info.yaml`
  (`caller_hive_role`, over the SDK's strict reading `HiveRegistry::role`): `status: connected`,
  its join done, and `role` worker, ingress or egress.
  It fails closed: no entry, no role, a hive still joining or a removed one gets nothing. The
  local hive is the motherbee. At start SY.admin logs the role of each registered hive, and a
  warning for each one that gets none (`log_registry_caller_roles`).
- **Only Archi and the HTTP API** ask for `executor_validate_plan`, `executor_execute_plan` and
  `publish_cloud_endpoint`. The executor then runs every step of the plan as the operator.
- **Closed to the whole mesh,** Archi included: `cognition_clear`, `reset_base_runtime`,
  `set_installation_ai`, `update_installation`, `set_hive_egress` and `clear_hive_egress`
  (`MeshAccess::Nobody`). The operator asks for them on loopback, or Archi stages them in a plan
  the operator confirms and the executor runs
  ([A hive's cognition](#a-hives-cognition-the-clear),
  [The installation's ai](#the-installations-ai),
  [The installation's update](#the-installations-update),
  [A worker behind the egress](#a-worker-behind-the-egress)). Their reads, `get_installation` and
  `get_installation_update`, are Archi's.
- **`remove_runtime_version` keeps a base runtime's `package_version`** (`RUNTIME_PACKAGE_VERSION`):
  it is the way back with `reset_base_runtime` (FINDINGS A-122).
- **SY.wf-rules@H's packages**, with only the fields its deploys and deletes send
  (`go/sy-wf-rules/node/admin.go`):
  - `publish_runtime_package` `{source: {kind, files}, set_current}`: only an inline package
    whose `package.json` (read with the publisher's own `PackageMetadata`) says `type: workflow`,
    `runtime_base: wf.engine` and `name: wf.<x>`, never `wf.engine` itself; so no `sync_to`,
    `update_to` or bundle upload, and each file under one plain path;
  - `remove_runtime_version` `{runtime, runtime_version}`: a version of a `wf.<x>`, named in
    lowercase. A `test_hold_ms` would hold the manifest lock every publish and deploy waits for;
  - both are refused (`403 UNAUTHORIZED`) while another hive holds a `WF.<x>`, connected or not
    (`admit_workflow_package`; FINDINGS A-304, the operator's call of 2026-11-12, since 0.1.139):
    the package is the same on every hive. First the nodes connected now, read from shared
    memory, asking no one (`connected_nodes_from_shm`, `workflow_nodes_elsewhere`): the
    motherbee's own from the regions of its routers, every other hive's from the LSA, as the
    router reaches them, and what the LSA last saw of each hive it holds stale, a hive cut off,
    while the hive registry still lists it (`last_seen_node_names`, `nodes_of_listed_hives`: the
    router keeps a removed hive's last LSA until it restarts, and a hive gone for good holds
    nothing up, as in a teardown). Then the node's config on every other hive that may run one
    (the motherbee and each worker whose join is done), each asked at once through the
    motherbee's orchestrator (`get_node_config` of `WF.<x>@<hive>`, 3 s:
    `read_workflow_node_config`): a hive that holds it refuses, its node stopped, failed or not
    connected. A hive that does not answer within the 3 s is judged by what the LSA last saw of
    it, and SY.admin's log names it (a warn line; no answer carries it): nothing is refused for
    one hive that fails, and a removal still waits for that hive's read of the version's use
    (A-281). What the LSA last saw lives in the router's memory from its last start (a deploy
    restarts it), so a `WF.<x>` on a hive that does not answer is not seen when it is stopped,
    nor, stopped or running, when that hive has been cut off since before the router's start. Before 0.1.139 only a connected `WF.<x>` refused. A
    workflow delete's purge meets the same refusal at the first version it asks to remove, and
    stops there: its `packages_left` names that version `UNAUTHORIZED` and each after it
    `PURGE_STOPPED`, none retryable, since every version may be the other hive's, and they stay
    until the other hive's workflow is deleted. A deploy's publish refused so answers
    `PACKAGE_PUBLISH_FAILED`, and changes nothing on its hive (SY.wf-rules publishes before its
    slots move, since 0.1.139's integration review).
    Regions that cannot be read answer `503 INVENTORY_UNAVAILABLE`, to retry (its
    `payload.retryable` true since 0.1.139, FINDINGS A-305), never `UNAUTHORIZED`;
  - `sync_hint` `{channel, folder_id, wait_for_idle, timeout_ms}`: only a scan of
    `fluxbee-dist-runtimes` (channel `dist`) on the motherbee, as a worker's deploy asks for one
    (`announceRuntimes`), with `timeout_ms` given and at most 5000. A longer wait would hold the
    motherbee orchestrator's system worker, which takes every message but a read one at a time.

Other checks:

- `CONFIG_GET`/`CONFIG_SET` addressed to SY.admin itself are accepted only from
  `SY.admin@motherbee` (`admin_origin_authorized`), otherwise `FORBIDDEN`. The router delivers
  them from no one else (rule (2) of `policy/system.rego`), so what reaches SY.admin is its own
  `node_control_config_get`/`node_control_config_set` on `SY.admin@motherbee`, which reads and
  sets the executor catalog. Over the mesh only Archi asks for those two and for
  `send_node_message`.
- Fanout `externalize` (with `fanout_family`) is operator-only (`handle_externalize`).
- **The secrets only an edge reads are never read back** (FINDINGS A-316, since 0.1.139): an
  externalized endpoint's bearer (`edge_channel_secret:<ich>`, the Cloud's door among them), the
  edge's TLS key (`edge_tls*`) and a fanout's verify token (`edge_fanout_verify:<ich>`), each owned
  by its edge. `vault_get` and `vault_assign` of one answer `403 RESERVED_KEY` before anything
  reaches SY.vault (`refuse_edge_secret_read_back`), whoever asks: the operator, the executor,
  Archi, Cloud. A `vault_assign` naming SY.admin's own ILK, then a `vault_get` through SY.admin, read
  the Cloud's bearer back, and through Archi it reached the AI provider (A-124). Their metadata
  stays readable; the operator still writes the edge's TLS key (put, rotate, the rollback of
  [The ACME client](#the-acme-client)) and deletes any of them, and SY.admin stores the bearers
  and verify tokens itself.
- The operator's `unpublish_artifact` may revoke any tenant's publication
  (`handle_unpublish_artifact` with no caller): over HTTP, `POST /artifacts/unpublish`, or as a
  step of an executor plan. Archi takes a page down that way, with a plan the operator confirms,
  never with an `ADMIN_COMMAND` of its own, which the handler refuses: Archi is no producer
  (FINDINGS A-120).
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
  `web_exposure_key:` since 0.1.145 (planned); `is_cloud_reserved_vault_key`).
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
| node lifecycle, node config/state/status, a node's self-description (`node_help`), hives, versions, runtimes, deployments, drift alerts, storage path, `core_rollback`, `set_hive_egress`, `clear_hive_egress` (`action_routes_via_local_orchestrator`) | `SY.orchestrator@motherbee`, with `payload.target` = the hive; `node_name@hive` overrides `target` (`normalize_admin_payload`) | `msg_type: admin`, `meta.action` |
| routes, VPNs, taps | `SY.config.routes@<hive>` | `msg_type: admin` (`build_admin_request`) |
| `inventory` | `SY.orchestrator@motherbee` | `INVENTORY_REQUEST` (`handle_inventory_http`) |
| identity actions | `SY.identity@motherbee`, the primary, whatever hive the path names, writes and reads alike: a spoke's replica refuses SY.admin of another hive (F-07; writes A-143, reads 2026-11-07), and every replica holds the primary's identities | `ILK_*`, `TNT_*` (`handle_identity_command`, `identity_command_hive`, `handle_identity_query`) |
| `retire_hive_ilks`, and `remove_hive` once the hive is gone | `SY.identity@motherbee` | `ILK_RETIRE_HIVE` ([A removed hive's system ILKs](#a-removed-hives-system-ilks)) |
| vault actions | `SY.vault@motherbee`; another hive is refused with `400 INVALID_REQUEST` ("SY.vault runs only on the motherbee") | `VAULT_*` (`handle_vault_command`) |
| `teardown_node` | `SY.identity@motherbee`, then `SY.vault@motherbee`, then SY.identity again | `ILK_GET`, `VAULT_LIST`, `VAULT_DELETE`, `ILK_DELETE`, `ILK_PURGE` ([Node teardown](#node-teardown)) |
| `update`, `sync_hint` | `SY.orchestrator@<hive>` | `SYSTEM_UPDATE` (`category` runtime, core, vendor or config; default runtime), `SYSTEM_SYNC_HINT` |
| `get_installation` | `SY.orchestrator@motherbee` | `list_versions` (`msg_type: admin`), shown as the installation's view (`installation_view`) |
| `set_installation_ai` | `SY.orchestrator@motherbee` for its `/versions`, the nodes' configs (A-11), the write and the scan; then every connected hive's `SY.orchestrator` at once | `get_versions`, `list_nodes`, `get_node_config` (`msg_type: admin`); `SYSTEM_UPDATE` category config with `write`; `SYSTEM_SYNC_HINT` of `fluxbee-dist-installation`; `SYSTEM_UPDATE` category config ([The installation's ai](#the-installations-ai)) |
| `update_installation` (and its background task) | `SY.orchestrator@motherbee` for each hive's `/versions`; then each hive's `SY.orchestrator` | `get_versions` (`msg_type: admin`); `SYSTEM_UPDATE` category core, then category config ([The installation's update](#the-installations-update)) |
| `get_installation_update` | answered by SY.admin from its record | |
| `opa_*` writes | `SY.opa.rules@motherbee`, then waits for every hive's answer | `CONFIG_CHANGED`, subsystem `opa` (`send_opa_config_changed`) |
| `opa_check` | `SY.opa.rules@motherbee` only; nothing is announced | `CONFIG_CHANGED`, action `check`, `version: 0` (a check takes no number from the counter); the answer is matched on the `config.check_id` it echoes (`send_opa_check`) |
| `wf_rules_*` | `SY.wf-rules@<hive>` | the writes as `CONFIG_SET` with `operation` (`send_wf_rules_config_set`); `delete_workflow` and the reads as `command` / `query` messages (`send_l2_action_request`) |
| `wf_rules_check` | `SY.wf-rules@<hive>` only; it stages nothing | `CONFIG_SET`, `operation: check`, a fresh `check_id` and no version; only the answer that echoes that `check_id` counts (`send_wf_rules_check`) |
| `wf_list_instances`, `wf_get_instance`, `wf_cancel_instance` | the WF node, `WF.<workflow>@<hive>` | `WF_LIST_INSTANCES`, `WF_GET_INSTANCE`, `WF_CANCEL_INSTANCE` → `WF_*_RESPONSE` (`handle_wf_instance_command`) |
| `timer_*` | `SY.timer@<hive>` | `TIMER_*` → `TIMER_RESPONSE` (`handle_timer_rpc`); a refusal keeps SY.timer's code, its HTTP status by what it says: `TIMER_INVALID_PAYLOAD` and `TIMER_INVALID_TIMEZONE` 400, `TIMER_NOT_FOUND` 404, `TIMER_STORAGE_ERROR` 502, `TIMER_UNKNOWN_VERB` 501 (FINDINGS A-251) |
| `list_streams` | every `RT.<router>.stream@<hive>` in the LSA | `STREAM_STATUS` (`handle_list_streams`) |
| `get_llm_usage`, `GET /config/storage/metrics` | `SY.storage@motherbee` | `USAGE_QUERY`, `STORAGE_METRICS_GET` |
| `cognition_clear` | `SY.cognition@<hive>` | `COGNITION_CLEAR` → `COGNITION_CLEAR_RESPONSE` (`handle_cognition_clear`) |
| `node_control_config_get/set`, `send_node_message` | the node; a bare name is a node of the path's hive, and a name of another hive is refused (`node_name_on_hive`, FINDINGS A-127) | `CONFIG_GET`, `CONFIG_SET`; the message as given, with no answer awaited |
| `externalize`, `unexternalize`, `list_externalized` | `SY.edge@<hive>` | `EDGE_OPEN_URL`, `EDGE_CLOSE_URL`, `EDGE_LIST_URLS` |
| `publish_cloud_endpoint` | `SY.orchestrator@motherbee` (the record), then `SY.vault@motherbee` and the edge (its externalize, the token's mint), then `IO.cloud@motherbee`, then, when the door moved, the edge it moved from | the record first (FINDINGS A-317, since 0.1.139): `get_node_config` of IO.cloud, then `set_node_config` of its `io` with `io.edge_node`, the rest of it as read (`msg_type: admin`; each tried once more at once, `record_cloud_edge`); then `VAULT_*`, `EDGE_OPEN_URL`; then `CONFIG_GET` and, unless IO.cloud trusts that edge already, a `CONFIG_SET` of its live config with the edge at its `config_version` + 1; a `CONFIG_SET` refused as stale or not answered is tried once more on the config read again; 30 s in all (`apply_cloud_edge`, FINDINGS A-255); last, when the config named another edge before and the running IO.cloud trusts the new one, `EDGE_CLOSE_URL` of IO.cloud's channel to that edge, tried once more at once (`settle_moved_cloud_door`, `close_moved_cloud_door`). One run at a time (`cloud_door_lock`, in SY.admin's context): one asked while another runs is refused at once, `409 BUSY` with `payload.retryable: true`, nothing read, recorded or minted (`cloud_door_busy`, since 0.1.139's integration review; it waited for the lock with no bound, past what a plan's run waits, and then answered a token no one read). `edge_node` must be `SY.edge@<hive>`, and a `cloud_node` that is not IO.cloud is refused, before anything is read or recorded. The answer says `edge_applied`; `false` comes with a `warning` that says why: the edge recorded but not applied yet, SY.admin unable to check what IO.cloud trusts, or another config that reached IO.cloud meanwhile and may have replaced the record. A door moved to another edge: `previous_edge_node` names the edge it moved from and `previous_edge_closed` says whether its door closed; one that did not close is in the `warning` too, with the `unexternalize` that closes it once that edge answers. When IO.cloud did not take the new edge, or SY.admin could not tell (`edge_applied: false`), the old door is left open, not asked to close (A-317's review): IO.cloud may still trust the old edge, whose door is then the Cloud's only way in; the `warning` says to close it once IO.cloud trusts the new edge and the Cloud has the new URL and token. The previous token stops at once on a move; on a rotation on the same edge it works for up to 10 minutes (SY.edge's grace, in its memory: the edge keeps one previous token, and a restart or another run ends it sooner). Nothing extends it. A record whose read or write fails twice answers `502 CONFIG_RECORD_FAILED` before anything is minted (FINDINGS A-273, A-317): no token, no door opened or closed, IO.cloud not asked, so a token the Cloud holds works as before. Only when a try of the write got no answer (HTTP 504, or none at all) does the detail say it may have been written all the same, and on a move to put `io.edge_node` back to the old edge first (`cloud_edge_maybe_written`): IO.cloud would otherwise trust, from its next start, an edge with no door. The cause is fixed first, then `publish_cloud_endpoint` runs again, on the motherbee's HTTP API (`POST /hives/motherbee/cloud/endpoint`) or through Archi, which since 0.1.139 shows the token once and keeps it masked (FINDINGS A-124; before, never through Archi). Up to 0.1.138 the record came after the token, and that answer left the door open with a token no one received. A door that does not open (the externalize's error) puts the record back as it was, tried once more at once (`put_back_cloud_edge_record`), and the error's detail says so, or that IO.cloud's config still names the edge. When the edge refused the open or did not answer it, the vault had already taken the token this run minted, which no one received: SY.admin first rolls `edge_channel_secret:<ich>` back to the token before it (`VAULT_ROLLBACK` to SY.vault, 5 s, `roll_back_cloud_token`; the operator's call of 2026-11-12 on FINDINGS A-317), so the vault holds the token the door served before the run, and the detail says it rolled back (`cloud_token_said`): on a rotation on the same edge, when the edge refused the open or the router did not reach it, the token that edge serves and reads again when it restarts; on a move, the old edge's, though the key stays the new edge's (this run's put made it the owner, and a rollback does not change it), so the old edge reads none once it restarts and the run goes again on an edge that answers. An edge that did not answer the open (`StoredChannelToken`'s `open_unanswered`: any error of `EDGE_OPEN_URL` but the router's `UNREACHABLE` or `TTL_EXCEEDED`, `never_reached`; a 15 s timeout above all) may have taken it all the same: SY.edge answers an open, then reads its token from the vault, and the rollback reaches the vault, not the edge's memory (a `VAULT_SECRET_CHANGED` re-reads only an entry with no token). It then serves this run's token until it restarts, and on a rotation the Cloud's beside it only for up to 10 minutes after the open (its grace); the detail says so, and that the run goes again on an edge that answers (0.1.139's integration review). A rollback SY.vault refused, or that did not reach it, changed nothing and is asked once more at once; one SY.vault did not answer in time is not asked again, since a rollback swaps the token and the one before it; `NO_PREVIOUS_VERSION` means no token before this run. Each of these is logged, nothing asks again later, and the detail says the vault then holds (or, unanswered, may hold) the token no one received, which the edge the door was on reads when it restarts (after a move it reads none, the key being the new edge's). Such a run takes up to 235 s by its bounds, within Archi's 240 s. A token goes out once, only in an ok answer, and in no error or log line |
| `publish_artifact` | the producer's hive's `SY.orchestrator` when the blob is not on the motherbee yet, then `IO.blob@motherbee`, then the public edge | `SYSTEM_SYNC_HINT`, `BLOB_CURATE`, `EDGE_PUBLISH_BLOB` |
| `unpublish_artifact` | the publication's edge, then `IO.blob@motherbee` | `EDGE_UNPUBLISH_BLOB`, `BLOB_RELEASE` ([Publications](#publications-and-the-audit-log)) |
| `list_published_artifacts` | answered by SY.admin from its publication ledger | |
| `acme_status`, `acme_renew`, `acme_test`, `acme_reload` | answered by SY.admin's ACME client, which asks the public edge, SY.vault, the CA and SY.orchestrator | `EDGE_TLS_STATUS`, `EDGE_TLS_RELOAD`, `EDGE_ACME_CHALLENGE`, `VAULT_*`, `record_drift_alert` ([The ACME client](#the-acme-client)) |
| `web_status` | answered by SY.admin from what it holds, plus two reads of io.web: `SY.orchestrator@motherbee` and `IO.web@motherbee` | `get_node_status` (`msg_type: admin`), `NODE_STATUS_GET` ([The web door's status](#the-web-doors-status)) |
| catalog, help, `list_recent_commands` | answered by SY.admin | |

Timeouts (`admin_action_timeout`; seconds, with environment overrides): `add_hive` 30
(`JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS`; the join itself runs in the background), `update` 60,
`set_installation_ai` 210, its ceiling (twice the 30 of an orchestrator's answer, for the node
check and the write, plus each hive's 90 of `sync_pending`,
`JSR_ADMIN_INSTALLATION_APPLY_TIMEOUT_SECS`, plus an update's 60), `update_installation` 550, its
ceiling (30 for the motherbee's `/versions`, then each hive's pass: 120 from the run's start for its
answer, `JSR_ADMIN_INSTALLATION_ANSWER_TIMEOUT_SECS`, two reads of its `/versions`, 50 each,
`JSR_ADMIN_INSTALLATION_READ_TIMEOUT_SECS`, and a core and a config update, each 90 and 60),
`sync_hint` 45, `core_rollback` 300, `set_hive_egress` and `clear_hive_egress` 60
(`JSR_ADMIN_EGRESS_TIMEOUT_SECS`; the motherbee forwards the switch with 30 and the confirm with
10), `get_llm_usage` 20, `acme_reload` 90 (the edge has 75 s),
`node_help` 10 (the node's hive gives the node 3 s, the motherbee gives that hive 7 s),
`kill_node` and `remove_node_instance` 180 (a purge waits for the node's teardown: the
orchestrator gives `teardown_node` 120 s, and the motherbee gives a spoke's purge 150 s),
catalog/help/timer/`hive_status`/`list_streams`/`web_status` 5 (`JSR_ADMIN_TIMEOUT_SECS`; for
`web_status`, its read of the orchestrator's view of io.web), everything else 30
(`JSR_ADMIN_ORCH_TIMEOUT_SECS`). The `wf_rules_*` actions, the check included, wait 75 for
SY.wf-rules (`JSR_ADMIN_WF_RULES_TIMEOUT_SECS`, `wf_rules_request_timeout`). The routes that ask a
node itself (`node_control_config_get/set`, the instance actions and `cognition_clear`) wait 5 less
than their action, 25, so they answer before a caller that waits 30.

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
   the node, or `503 INVENTORY_UNAVAILABLE` when they cannot be read, or when the node's hive,
   which the hive registry still lists, is stale in the LSA or not in it, and nothing is removed
   (`teardown_live_node_refusal`); its `payload.retryable` is true (FINDINGS A-305, since
   0.1.139). Otherwise a node redeployed onto the ILK of a teardown that
   failed half-way could be purged alive. A node of a hive cut off is not connected, and may run:
   before 0.1.133 it read as gone, and its secrets and its ILK went while it ran. A hive the
   registry no longer lists holds nothing up. **The node's public pages refuse nothing** (FINDINGS
   A-242): they stay up until they expire, and the ok answer names them
   ([the pages](#publications-and-the-audit-log)).
2. **The ILK is the node's**: `ILK_GET` to SY.identity, whose `identification.node_name` must be
   the node, else `409 NODE_ILK_MISMATCH` and nothing is removed. An ILK SY.identity does not
   hold is reported `not_found` and skips step 4. Over the mesh it skips step 3 too, since
   nothing ties those secrets to the node any more; the operator's teardown (HTTP, the executor)
   still deletes them.
3. **Its secrets**: `VAULT_LIST` with `filter.ilk`, then `VAULT_DELETE` of each in its own tenant,
   through `handle_vault_command` like every admin vault call.
4. **The ILK**: `ILK_DELETE` (the mark), then `ILK_PURGE`; `ILK_NOT_FOUND` counts as done. Both go
   to SY.identity as such (`send_identity_command`): step 2 read the ILK, and `purge_ilk`'s own
   read of it (`purge_ilk_pages_of`) would only name its pages again.

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
says `skipped`. The ok answer adds `pages_left_up`, the node's public pages that are up
([the pages](#publications-and-the-audit-log)), to every caller but the node's own orchestrator,
which reads none of it: the purge that called it, which SY.admin answers, names them.

## A removed hive's system ILKs

A hive's SY nodes have deterministic system ILKs, `SY.<node>@<hive>`, which SY.identity seeds. When
the hive goes, they are **retired**, never deleted (FINDINGS A-191; the operator's call of
2026-11-11: an ILK that made history is disabled, never deleted): SY.identity marks them as a
delete marks, with the reason `retired: hive`, so they leave every lookup and the identity SHM
while their rows stay ([SY.identity](sy-identity.md#delete-marks-purge-removes)).

- **`retire_hive_ilks {hive_id}`** (`POST /hives/{hive_id}/system-ilks/retire`,
  `handle_retire_hive_ilks`): only for a hive that is gone (`hive_removed`). The motherbee, and a
  hive the registry still lists (`HiveRegistry::lists`, a join in flight included), answer
  `409 HIVE_NOT_REMOVED`; a registry SY.admin cannot read, `503 HIVE_REGISTRY_UNREADABLE`; a name
  that is no hive id, `400 INVALID_REQUEST`. None repeats the value sent. Otherwise SY.admin sends
  `ILK_RETIRE_HIVE {hive_id}` to `SY.identity@motherbee`, which takes it from `SY.admin@motherbee`
  alone, and its answer is the action's: `{hive_id, count, ilk_ids, already_retired}`. Running it
  again marks none. It is a write the operator confirms; Archi reaches it like every action open
  to it and stages it in a plan. Archi's chat has no SCMD for it: SY.architect's `translate_scmd`
  maps no such path, and a release that maps one lists the action in `admin_action_allows_ai_write`
  too, or `handle_scmd` would run it untracked, as a read.
- **`remove_hive` runs it** once the orchestrator removed the hive (`with_system_ilks_retired`),
  and its ok answer names the result in `payload.system_ilks_retired`. A request that does not
  reach SY.identity, which the removal of an unreachable hive restarts (`identity_restart`), is
  asked again, up to 3 times 2 s apart (`RETIRE_AFTER_REMOVAL_ATTEMPTS`). A retire that fails leaves
  the removal ok, since the hive is gone and removing it again would only answer `NOT_FOUND`:
  `system_ilks_retired` is `{count: null, error_code, error_detail}` and `payload.warning` says to
  run `retire_hive_ilks`, after any warning of the orchestrator's. The executor keeps
  `system_ilks_retired` in a step's essentials, so Archi reads it whatever the frame drops.
- A hive joined again under the same name gets its ILKs back unmarked from its own SY.identity.
  A removed hive's SY.identity that outlives the removal (its cleanup did not reach it, or has not
  stopped it yet) cannot: the primary drops every publish of a hive the registry no longer lists
  ([SY.identity](sy-identity.md#sync-between-hives-tcp-9100)). The ILKs of the SY nodes a release
  no longer runs on a hive are retired by SY.identity itself at its start (`retired: node`).
- **Only the operator purges a retired system ILK** (`purge_ilk`), once it made no history; one that
  made history stays retired. `delete_ilk` and `restore_ilk` refuse a system ILK
  (`SYSTEM_ILK_PROTECTED`).

## A node's self-description

`GET /hives/{hive}/nodes/{node_name}/help` (`node_help`) asks one node to describe itself and
returns what it says ([agent-tools-v1](../agent-tools-v1.md) §3.3). An AI node answers with the
cards of its tools. Every AI agent loads the same base tools, so any running AI node gives the
whole catalog. Before it designs a role or a skill, Archi reads the same cards first from
`get_runtime` on `ai.generic` (`GET /hives/motherbee/runtimes/ai.generic`, its `tool_catalog`),
which needs no running node, and from a running AI node with `node_help`. The operator asks
Archi, and over the mesh only Archi may ask for it (`mesh_access`).

- **The way**, as `get_node_status`'s:
  1. SY.admin sends `node_help` to `SY.orchestrator@motherbee` (`msg_type: admin`);
  2. for a node of another hive, that orchestrator forwards `NODE_HELP` to the hive's
     orchestrator and waits 7 s (`NODE_HELP_FORWARD_TIMEOUT_SECS`);
  3. the node's own hive sends `NODE_HELP` to the node and waits 3 s
     (`NODE_HELP_PROBE_TIMEOUT_SECS`, `node_help_flow` in `src/bin/sy_orchestrator.rs`), only
     when `ai.generic` runs the node (its package's base runtime included). Any other node, a core
     node included, is answered at once.

  Each orchestrator runs it in a task of its own, so the wait holds no other admin or system
  request ([SY.orchestrator, messages](sy-orchestrator.md#messages)). SY.admin waits 10 s. The
  router delivers `NODE_HELP` only from an orchestrator (system policy rule (3), a read) or the
  same hive's admin (rule (4)).
- **The answer**, always 200 for a node that exists: `{status: "ok", node_name, help, reason}`.
  - `help` is the node's `NODE_HELP_RESPONSE`, `{status: "ok", node_name, runtime,
    runtime_version, tools: [card, ...]}`, and `reason` is null;
  - or `help` is null, and `reason` says why: what runs the node does not answer `NODE_HELP`
    (today only `ai.generic` does), the node is not connected (its lifecycle is named), it did not
    answer within 3 s, or it answered with an error. An AI node on `ai.generic` 0.1.136 or later
    answers at once however busy its conversations keep it (FINDINGS A-257): one that does not
    answer in 3 s is stuck or restarting, or its 64 places for control requests were all taken
    (its journal says which). One on an older version (a node pinned to one) may still miss the
    3 s while its workers are busy: its `NODE_HELP` waits behind the turns that came before it.
- **A card:** `{name, summary, when_to_use, limits: [...], present_when, input: <JSON schema>,
  output: <JSON schema>, errors: [{code, meaning}]}`. `present_when` says when the tool is there.
  The same cards make the generated catalog, `docs/reference/agent-tools.md`.
- **Errors:** a node that does not exist answers `404 NODE_NOT_FOUND`, as `get_node_status`
  does; a hive whose orchestrator does not answer, `500 NODE_HELP_FAILED`.

SY.timer describes itself with `timer_help` (`TIMER_HELP`) and may answer `NODE_HELP` later.

## A node's config.json

`GET /hives/{hive}/nodes/{node_name}/config` (`get_node_config`) reads the `config.json` of a node
`run_node` launched, and `PUT` on the same path (`set_node_config`) writes it. The orchestrator of
the node's hive does both (`get_node_config_flow`, `set_node_config_flow` in
`src/bin/sy_orchestrator.rs`); the write tells the running node nothing, and the node reads it at
its next start.

- **The body is the config patch itself** (FINDINGS A-346), its keys at the top level as
  `get_node_config` shows the node's config: `{"behavior": {"kind": "ai_chat"}}`, never wrapped in
  a `config` key. SY.admin sends it as the payload's `config` (`handle_hive_paths`). A patch
  wrapped in `config` would put a key named `config` in `config.json`, which the node, at its next
  start, takes for its whole config (the Rust SDK's `extract_effective_config`, the Go SDK's
  `ExtractEffectiveConfig`). Up to 0.1.143 the help named a `config` and a `replace` field of the
  body, and its example wrapped the patch.
- **A patch** replaces each top-level key it names whole, a nested object included, and keeps the
  keys it does not name. **`?replace=1`** (or `true`) makes the body the whole config, all but its
  `_system`. A body that names `_system` is refused (`INVALID_REQUEST`); a node with no
  `config.json` answers `NODE_CONFIG_NOT_FOUND`.
- **The binding fields** (`NODE_BINDING_FIELDS`: `runtime`, `runtime_version`,
  `requested_runtime_version`, `runtime_base`, `package_path`) at the top level of the body are
  lifted out of the patch, and the orchestrator writes them to `_system`, never as config keys.
- **The args** of the same action, which the executor's plans and a mesh caller send (and
  SY.wf-rules, rebinding its WF nodes with an empty `config` and the binding fields), name the
  patch `config` and the replacement `replace`, beside `node_name` and the binding fields. The
  help gives each shape its own fields (`admin_action_http_body`): `request_contract` the HTTP
  body, with `body.description` and `query_params`, and `executor_contract` the args.
- Archi's SCMD of this route takes the body as the patch too, but always merges and leaves the
  binding fields in the patch (FINDINGS A-348).

## A runtime package's publish

`publish_runtime_package` (`POST /admin/runtime-packages/publish`,
`handle_publish_runtime_package`) installs a package into the motherbee's dist and runtimes
manifest, from an `inline_package` or a `bundle_upload`; each hive's mirror brings it.

- **The version.** The package's check is the one `fluxbee-publish` runs (`validate_package` in
  `src/runtime_package.rs`): its `package.json`'s `version` is `MAJOR.MINOR.PATCH`, with an
  optional `-prerelease` (`1.2.0-rc.1`). A version with build metadata (`1.2.0+b1`) is refused
  with `400 INVALID_REQUEST` before anything is installed (FINDINGS A-312, since 0.1.139): no
  reader of a version after its publish takes a `+` (`remove_runtime_version`'s
  `valid_runtime_token`, the orchestrator's launch and reads), so such a version ran no node of a
  config-only or workflow package on it (`BASE_RUNTIME_NOT_AVAILABLE`) and could not be removed.
- **The manifest's lock.** A publish waits for SY.admin's lock of the manifest
  (`take_runtime_manifest_lock`) and for its file lock up to 60 s each
  (`RUNTIME_MANIFEST_LOCK_WAIT`); past either it answers `409 RUNTIME_MANIFEST_BUSY` with
  `payload.retryable: true`, nothing published (FINDINGS A-306, since 0.1.139): ask again once the
  other writer is done. Before, it waited for SY.admin's lock with no bound, behind a removal that
  held it through its read of every hive's use, and a file lock held too long answered
  `SERVICE_FAILED`. A publish with follow-ups (`sync_to`, `update_to`) holds SY.admin's lock
  through them. A `SY.wf-rules@<hive>` deploy's publish waits for SY.admin's lock 4 s at most
  (`WF_RULES_PUBLISH_LOCK_WAIT`, `publish_lock_wait`, since 0.1.139's integration review): that
  caller waits 10 s for the answer (`packagePublishTimeout`), after the A-62 admission's 3 s, so
  the refusal reaches it. It waited the 60 s while SY.wf-rules gave up after 5 s: the deploy
  failed as a timeout, and the version was published as current once the lock came. The file
  lock, which only a writer outside SY.admin holds long (the package's seed, a hand publish),
  is still waited for 60 s.

## A runtime version's removal

`remove_runtime_version` (`DELETE /hives/motherbee/runtimes/{runtime}/versions/{version}`,
`handle_remove_runtime_version`) removes a version from the motherbee's dist and runtimes manifest;
each hive's receive-only mirror deletes it when it syncs. Before it writes, it reads the version's
use on every hive from the motherbee's orchestrator (`get_runtime` with the version, its
`usage_global_visible`; `read_runtime_version_use`): `running_nodes`, the nodes of every hive
pinned to the version whose unit may run, or that the orchestrator relaunches on it within a minute
(`RELAUNCH_DUE`, since 0.1.139, FINDINGS A-300: listed by the node's own hive, so by none whose
core is older), each with its `hive` and `lifecycle`, and
`skipped_hives`, the hives whose nodes the orchestrator did not count
([SY.orchestrator](sy-orchestrator.md)). Only a read that counted every hive that may run a node on
a runtime, and lists no node, lets the version go (`runtime_version_removable`). The manifest's
checks come first and the read of the use after them, with no lock held (FINDINGS A-306, since
0.1.139); then the removal takes SY.admin's lock and the file lock, checks the manifest again
(`runtime_removal_view`) and, only when the runtime's entry changed meanwhile (a publish of it,
say), reads the use again under the locks before it writes. Before, it held both locks through
the read, up to 30 s while a hive did not answer, and every publish, SY.wf-rules' deploys among
them, waited behind it. An ingress or an
egress, by the role its registry entry records whatever the state of its join
(`registered_hive`), receives no runtime: the orchestrator links only the workers into
`fluxbee-dist-runtimes`. The motherbee's orchestrator asks neither and lists them apart
(`hives_without_runtimes`) since 0.1.133; one before it put each in `skipped_hives` on every read,
for its `RUNTIME_MANIFEST_MISSING` (FINDINGS A-24), and SY.admin never takes one there for a hive
not read. Otherwise, with nothing removed:

- **`409 RUNTIME_IN_USE`** while a node is listed, whatever else was not counted. The detail names
  each node, the first 20 of them, with its hive and lifecycle: `RUNNING`, `STARTING` (its unit
  runs and the router does not see it yet), `UNKNOWN` (systemd did not answer whether its unit
  runs; it is listed until systemd answers that it does not: [SY.orchestrator](sy-orchestrator.md),
  FINDINGS A-271) or `RELAUNCH_DUE` (its unit does not run, failed or between two restarts of a
  crash loop, and the orchestrator relaunches it on this version within a minute,
  `relaunch_on_boot`: FINDINGS A-300, since 0.1.139). Before 0.1.133 it said "still running" of
  each. `payload.usage_global_visible.running_nodes` lists them all, and `payload.unanswered_hives`
  the hives not counted besides. Stop each node that runs on the version (`kill_node`, which also
  clears `relaunch_on_boot`), or move it to another, then ask again. A node stopped through
  `kill_node` is not listed, nor one that follows `current`, which a relaunch puts on the current
  version; before 0.1.139 a pinned node due for a relaunch was not listed either, and failed to
  start once its version was gone.
- **`409 RUNTIME_USAGE_UNKNOWN`** when the use could not be read on every hive (FINDINGS A-281,
  since 0.1.133). `payload.unanswered_hives` names each hive not read, `{hive, reason, detail}`,
  the detail names them with their reasons, and `payload.retryable` says whether asking again,
  nothing changed first, may pass: false for a `USAGE_READ_FAILED` and while a join that ended is
  not read (both below), true otherwise:
  - the motherbee alone, when the read failed there, and the detail says that no hive's nodes were
    counted: its orchestrator did not answer the read (`TIMEOUT`: no answer within SY.admin's
    30 s, that orchestrator stuck or restarting, or its read waiting behind other reads of a
    runtime's use, which it runs 4 at a time; `TRANSPORT_ERROR`), refused it (its code), could not
    make it (`USAGE_READ_FAILED`: its usage says `status: error`, its own nodes or its registry
    unread, a node's `config.json` that cannot be read while the node may run among them; every
    read fails so, for every runtime, until that is fixed, and the detail says what to fix: stop
    that node with `kill_node` or fix its config, or make the folder or the registry readable) or
    gave no usage of every hive (`NO_USAGE`: none, or one without `running_nodes` or
    `skipped_hives`);
  - a worker of its `skipped_hives`, or a hive whose role its registry entry does not record, each
    asked at once for its nodes alone (`GET_RUNTIME` with `usage_only`): one that gave no answer
    within 10 s (`TIMEOUT`: its orchestrator stuck or restarting while the LSA shows it); one the
    request never reached (`UNREACHABLE`: down, or stale in the LSA); one that answered an error
    (its code; only a worker before 0.1.133 does, which reads its copy of the manifest first:
    `RUNTIME_NOT_FOUND` while that copy does not list the runtime yet, `RUNTIME_MANIFEST_MISSING`
    without one; a 0.1.133 worker answers its nodes whatever its copy lists); or one that could
    not read its own nodes (`USAGE_UNREADABLE`: its nodes' folder, or the `config.json` of a node
    whose unit runs, gives systemd no answer or, since 0.1.139, is on its way up or down in
    systemd, as a crash loop's unit is between two restarts (FINDINGS A-310), which the detail
    names: stop that node or fix its config). A hive whose join the registry does not record as done also gets its `status`, its
    `join_phase` and `next`: finish its join (`add_hive` again) or remove it (`remove_hive`). One
    whose join ended (its status `failed` or `interrupted`, or its record's phase `done`, as a
    failed join and the boot sweep of a cut one stamp it: `UnfinishedJoin::ended`) is read only
    then, so the refusal is not retryable while it is not read; a join under way ends by itself.

  Ask again once each hive answers. A hive gone for good is removed with `remove_hive` first: the
  orchestrator asks only the hives of the registry. Before 0.1.133 a hive not counted read as one
  with no node, and a usage missing from the answer as one not in use (`in_use: false`): with
  worker1's orchestrator restarting, a version a node of worker1 ran on was removed, its mirror
  deleted it when it came back, and the node failed at its next start. A read the motherbee's
  orchestrator did not answer was `RUNTIME_REMOVE_FAILED`.

The other refusals: `400 INVALID_REQUEST` for a missing or invalid runtime or version,
`409 RUNTIME_CURRENT_CONFLICT` for the runtime's current version while other versions remain,
`409 RUNTIME_PACKAGE_VERSION` for the version the installed package brought (FINDINGS A-122),
`404 RUNTIME_NOT_FOUND` or `RUNTIME_VERSION_NOT_FOUND`, and `409 RUNTIME_HAS_DEPENDENTS` while
packages are built on the runtime. The manifest alone decides that last one, before the read of the
use since 0.1.133: a removal that could never pass no longer waits for every hive, nor is refused
`RUNTIME_USAGE_UNKNOWN` while one does not answer. `502 SERVICE_FAILED` when the manifest it wrote
cannot be read back: the version was removed.

SY.wf-rules removes its workflows' old versions through this action
([SY.wf-rules](sy-wf-rules.md)), waiting up to 35 s for each removal (this read of every hive's use
comes first) and, in a deploy or a rollback, up to 15 s for the whole purge: a deploy or a rollback
whose removal is refused, or not answered in time, goes on, logs what it left, and the next one
tries those versions again. A workflow's delete removes its versions last, after the workflow, its
node and its state are gone, within what is left of its request's 60 s, and stops at the first
removal that fails; since 0.1.133 it answers ok all the same: `payload.packages_left` names each
version left, `{runtime, version, error_code, detail, retryable, unanswered_hives}` (the one it
stopped at with its failure, each after it `PURGE_STOPPED`; `[]` when every version it knew of
went; one entry with no version, `DIST_UNREADABLE`, when its hive's dist could not be listed),
and `payload.warning` says what removes them. A workflow created again under that name numbers its
versions above every version the dist holds, so it never publishes over one left there, and its
deploy's purge removes them; or remove each with `remove_runtime_version` once every hive answers,
in `packages_left`'s order, the runtime's current last (`RUNTIME_CURRENT_CONFLICT` otherwise).
After `DIST_UNREADABLE` neither holds until that hive's `dist/runtimes/wf.<workflow>` can be listed
again: a compile of the name that names no version is refused meanwhile (`WFRULES_ERROR`), and the
versions to remove are those `GET /hives/motherbee/runtimes/wf.<workflow>` lists.
`fluxbee-factory-reset` prints each version left under its workflow and counts them in its log line
(`packages_left=`); a second run does not remove them, since the workflow is no longer listed.

## Workflows: the check and the instances

What Archi needs to write and operate a WF ([wf-orchestration-v1](../wf-orchestration-v1.md)
§5.5, topics 2 and 4). Over the mesh only Archi asks for these (`mesh_access`).

### Checking a definition

`POST /hives/{hive}/wf-rules/check` (`wf_rules_check`), body `{workflow_name, definition}`, is to
a workflow what `opa_check` is to a policy: a read. The hive's SY.wf-rules validates the definition
as a compile does, with the same limits, and only answers: it takes no version and stages,
publishes and stores nothing, so the staged and the current definitions stay as they are.

- **The request** (`send_wf_rules_check`): a `CONFIG_SET` to `SY.wf-rules@<hive>` whose `config`
  is the request's fields with `operation: "check"` and a fresh `check_id` (a UUID), without a
  `version` or a `launched_by` (`wf_rules_check_config`).
- **The answer:** only the one that echoes the `check_id` counts. An answer without it, or with
  another, ran no check (an SY.wf-rules that knows no check compiles and stages instead), so it
  is `502 TRANSPORT_ERROR`, never a pass (`wf_rules_check_answer`).
  - A pass: `200`, payload `{ok: true, node_name, operation: "check", check_id, workflow_name,
    guard_count, state_count}`.
  - A refusal answers as a refused `wf_rules_compile` does: `status: error`, `error_code` and
    `error_detail` from SY.wf-rules' `error` (`COMPILE_ERROR`, 422, names the exact path), and
    SY.wf-rules' payload kept.

```json
{
  "status": "error", "action": "check",
  "payload": { "ok": false, "state": "error", "operation": "check", "check_id": "…",
               "error": { "code": "COMPILE_ERROR", "detail": "states[0].transitions[1].guard: …" } },
  "error_code": "COMPILE_ERROR", "error_detail": "states[0].transitions[1].guard: …"
}
```

### A change refused BUSY

SY.wf-rules takes one request at a time, in the order they came. A change of a workflow
(`wf_rules_compile_apply`, `wf_rules_compile`, `wf_rules_apply`, `wf_rules_rollback`,
`wf_rules_delete`) whose turn comes with less than 20 s left of SY.admin's 75 s wait for it
(`wf_rules_request_timeout`) is refused at once, nothing changed: `409 BUSY`, SY.wf-rules' payload
kept, its `retryable: true` included, and `error_detail` saying how long it waited (FINDINGS
A-307, since 0.1.139; `WF_RULES_BUSY_NOTE`). Ask again. `wf_rules_check` and the reads are never
refused so. Before, such a change ran behind the requests before it, and SY.admin could answer
`TIMEOUT` while it went on.

A change whose turn comes with 20 s or more left still runs with its 60 s from its turn, and can
end past SY.admin's wait: `504 TIMEOUT` is no refusal, and the change may still complete. Before
staging it again, read `wf_rules_get_status`, which SY.wf-rules answers once the change has ended
(its requests in the order they came): its `current_version`, `staged_version` and `wf_node` say
what the change did, and `WORKFLOW_NOT_FOUND` that a delete went through (`WF_RULES_TIMEOUT_NOTE`,
in the help of each change).

### Operating instances

Three actions on a WF node, each the engine's own verb sent to the node and its answer as it is
(`handle_wf_instance_command`). The engine answers these verbs only to `SY.admin@motherbee` and
to its hive's SY.wf-rules (`controlCallerAuthorized`, go/nodes/wf/wf-generic/node/dispatch.go).

| Action | Route | Sends |
| --- | --- | --- |
| `wf_list_instances` | `GET /hives/{hive}/nodes/{node_name}/wf/instances?status=&limit=` | `WF_LIST_INSTANCES {status, limit}` |
| `wf_get_instance` | `GET /hives/{hive}/nodes/{node_name}/wf/instances/{instance_id}?log_limit=` | `WF_GET_INSTANCE {instance_id, log_limit}` |
| `wf_cancel_instance` | `POST /hives/{hive}/nodes/{node_name}/wf/instances/{instance_id}/cancel`, body `{reason}` | `WF_CANCEL_INSTANCE {instance_id, reason}` |

- **The node:** a bare `WF.<workflow>` is a node of the path's hive (over the mesh, of the
  target, or a `node_name@hive` wins). A name of another hive, or of a node that is not `WF.*`, is
  refused with `400 INVALID_REQUEST` before anything is sent (`wf_instance_request`).
- **The fields**, checked before anything is sent:
  - `status`: `running`, `completed`, `cancelled` or `failed`; without it, every instance;
  - `limit`: 1 to 200, 50 without it (the engine would take none as every instance);
    `log_limit`: 1 to 200, the engine's 20 without it. A value out of range is refused, never
    cut;
  - `reason` is required for a cancel: the instance's log keeps it.
- **The answer** is the engine's payload as it is. `wf_list_instances` answers `{ok: true, count,
  instances: [{instance_id, thread_id, workflow_type, definition_hash, status, current_state,
  created_at_ms, updated_at_ms, terminated_at_ms}]}`, newest first; `wf_get_instance` answers
  `{ok: true, instance: {…, input, state_variables, recent_log}}`; `wf_cancel_instance` answers
  `{ok: true, instance_id, status: "cancelled"}`.
- **A refusal**, `{ok: false, error: <code>, detail}`, keeps that payload and gives `error_code`
  and `error_detail` (`node_verb_answer`): `INSTANCE_NOT_FOUND` 404,
  `INSTANCE_ALREADY_TERMINATED` and `node_not_configured` 409, `INVALID_REQUEST` 400,
  `STORE_ERROR` 502. A node that does not answer is `TIMEOUT` or `TRANSPORT_ERROR`.
- **The cancel** changes a running conversation: it is the one mutating action of the three, with
  `confirmation_required`, so Archi asks the operator first. The engine runs the cancel transition
  (the instance's exit actions and the `cancelled` state's entry actions when the definition has
  one), cancels its timers and ends it `cancelled`. Nothing it already did is undone.

```json
{
  "status": "ok", "action": "wf_list_instances",
  "payload": { "ok": true, "count": 1, "instances": [ { "instance_id": "wfi:…", "thread_id": "…",
    "workflow_type": "invoice", "definition_hash": "…", "status": "running",
    "current_state": "waiting_team", "created_at_ms": 1775577000000,
    "updated_at_ms": 1775578000000, "terminated_at_ms": null } ] },
  "error_code": null, "error_detail": null
}
```

## A hive's cognition: the clear

`POST /hives/{hive}/cognition/clear` (`cognition_clear`) has the hive's SY.cognition drop every
communication and every episode it keeps, in memory, in its file and in its memory region, and
answer what it removed (FINDINGS A-160; [cognition §5.1](../reference/cognition.md)). It cannot be
undone: the hive's agents lose the memory of every conversation, and the hive every episode it
kept, its own and those other hives sent it. Each hive keeps its own copy of an episode, so the
other hives keep theirs.

- **The request** (`handle_cognition_clear`): no body, or `{}`. A field is refused with
  `400 INVALID_REQUEST` before anything is sent. SY.admin sends `COGNITION_CLEAR {}` to
  `SY.cognition@<hive>` and waits 25 s for its `COGNITION_CLEAR_RESPONSE`.
- **Who may ask:** the operator on loopback, or a step of an executor plan, which Archi stages
  with its plan compiler and the operator confirms (`confirmation_required`). It is closed to the
  whole mesh, Archi's own `ADMIN_COMMAND` included (`MeshAccess::Nobody`). SY.cognition takes the
  message only from `SY.admin@motherbee`, as the router names it.
- **The answer** is SY.cognition's payload as it is (`node_verb_answer`): `{ok: true, node_name,
  removed: {file: {communications, episodes}, memory: {communications, episodes}}, region}`,
  `region` being `written_empty`, or `none` for a cognition that opened no region. A refusal
  gives `error_code` and `error_detail`: `UNAUTHORIZED` 403, `STORE_ERROR` 502 (its file could not
  be cleared, and nothing was), `REGION_ERROR` 502 (its file and memory are clear, its region
  could not be written). A hive without SY.cognition (an ingress, an egress) answers
  `TRANSPORT_ERROR`, `NODE_NOT_FOUND` in the detail; a cognition that does not answer, `TIMEOUT`.
- **After it** the cognition keeps running, empty: its `STATUS` shows its file and region empty,
  and the clear in `clears.total` and `clears.last_at`.
- **The factory reset** runs it on every hive that runs SY.cognition, the motherbee included
  (`clear_cognitions` in `packaging/fluxbee-factory-reset`).
- **Only this action clears:** `send_node_message` refuses a `COGNITION_CLEAR`, which would pass as
  SY.admin's own message without its name or its confirmation.

```json
{
  "status": "ok", "action": "cognition_clear",
  "payload": { "ok": true, "node_name": "SY.cognition@worker1",
    "removed": { "file": { "communications": 12, "episodes": 3 },
                 "memory": { "communications": 5, "episodes": 3 } },
    "region": "written_empty" },
  "error_code": null, "error_detail": null
}
```

## A worker behind the egress

`PUT /hives/{hive}/egress` (`set_hive_egress`) puts a worker that joined before the motherbee
declared its egress behind it; `DELETE /hives/{hive}/egress` (`clear_hive_egress`) takes a worker
back ([edge-egress-nat-spec §15](../edge-egress-nat-spec.md), FINDINGS A-219). Both are the
operator's: closed to the mesh, Archi included, so Archi stages them, one step per worker, and the
executor runs them once the operator confirms.

- **The request:** `{}`, or `{"access_from": ["<IPv4 address or network>", ...]}`: what the operator
  manages the worker from, besides its live SSH sessions. A clear may name `gateway_ip`, the gateway
  to go back to, only for a worker whose record keeps no route from before the egress. It names no
  gateway otherwise: a set's is the motherbee's declaration.
- **What SY.admin checks** before it sends anything (`handle_hive_egress_command`): the body names
  no other hive than the path's, in `target` or `hive_id` (`hive_egress_payload`,
  `400 INVALID_REQUEST`); the hive is a worker of the registry whose join is done
  (`hive_egress_worker_refusal`, `409 NOT_A_WORKER` for the motherbee, an ingress, an egress, a hive
  whose join is not done, or none of the registry's); no other egress action runs, on any worker
  (`409 EGRESS_SWITCH_OPEN`, naming it: one at a time in the installation); no installation update
  run is under way, nor a task of the update acting on that worker
  (`409 INSTALLATION_UPDATE_RUNNING`): its core update restarts the worker's orchestrator. It holds
  the run's mark while the action runs, as `<action> on <hive>`, as `set_installation_ai` holds
  it, so an `update_installation` started meanwhile is refused, naming the egress action. Each
  refusal says `outcome: refused` and the hive in its `payload`.
- **Then** it sends the action to `SY.orchestrator@motherbee`, `payload.target` the worker the path
  names, and waits 60 s. The motherbee's checks, the worker's guards, the switch, the confirm and the
  commit or the revert are the orchestrators'
  ([SY.orchestrator](sy-orchestrator.md#a-worker-behind-the-egress-a-219)). While a switch is open
  on a worker, its orchestrator refuses a core update or a core rollback sent to it
  (`update`, `update_installation`, `core_rollback`): `409 EGRESS_SWITCH_OPEN`, nothing done.
- **The answer:** `status` `ok` (`outcome` `switched`, `cleared` or `unchanged`) or an error with
  `error_code`, `step` and `outcome` (`refused`, `reverted`, `not_answered`), and the worker's
  `gateway_ip`, `previous_gateway`, `ipv6_blocked`, `internet_reachable`, `probe`,
  `ssh_reachable_from`, `switch_id` and `timings_ms` per step. A refusal before anything changed is
  409 (`DEFAULT_ROUTE_MULTIPATH` among them; `503` for `HIVE_UNREACHABLE` and
  `EGRESS_GUARD_UNREADABLE`, `422` for `EGRESS_DECLARATION_INVALID`); a step that failed and reverted
  502 (`revert_problems` names each step the revert could not do), and so is
  `EGRESS_SWITCH_NOT_FOUND`; `EGRESS_CONFIRM_MISSED` and `EGRESS_NOT_ANSWERED` 504.
  `EGRESS_NOT_ANSWERED` never means "not applied", whatever its `step` (`switch`, `confirm`, or
  `commit`: the worker took the confirm and did not answer it within 7 s); nor does an answer
  SY.admin did not get (`TIMEOUT`, `TRANSPORT_ERROR`), which says `outcome: not_answered` too. `GET
  /hives/{hive}/versions` says where the worker stands (`egress.state`, `egress.last_switch`).
- The command log keeps each run under the worker's hive (`GET /hives/<worker>/commands`).

## The installation's ai

The installation file holds the installation's `ai`, the default provider and each provider's
model, one version on every hive; the motherbee's orchestrator is its only writer
([hive-config-update-v1](../hive-config-update-v1.md) §4, "As built (stage 3c)";
[SY.orchestrator](sy-orchestrator.md#the-installation-file)). Two actions read and change it
(`InternalActionRoute::Installation`), and `update` takes category `config` for one hive.

**`GET /installation` (`get_installation`)**, a read, not in the command log; Archi may ask for it
over the mesh. It takes only `versions_limit` and `versions_before` (`?versions_limit=&versions_before=`
over HTTP). SY.admin reads the motherbee's `/versions` (`list_versions`, in pages of hives) and
answers its view (`installation_view`):

- `motherbee`: its file, `file`, `version`, `sha256`, `state`, `ai` (the values its readers run
  on), and `not_written` when its start wrote none;
- `expected_version`: the version a change names, the file's, or the last one recorded for a file
  no reader can use (`installation_expected_version`); null while the motherbee holds no file;
- `versions`: the newest 20 versions recorded, oldest first, `{version, sha256, written_at_ms,
  source, ai}`, `source` being `migration` (version 1) or `operator`. `versions_total` counts every
  version recorded, and `versions_next_before`, while older ones remain, is the `versions_before`
  that lists them; `versions_limit` (1 to 100) asks another count (FINDINGS A-198, [Pages](#pages));
- `hives`: each hive's copy, `{state, version, sha256, applied}`, `applied` being the config update
  it applied last (`{version, sha256, applied_at_ms, ai, restarted}`, null before its first). A
  hive whose `/versions` failed is `unknown`, with the error; one on an orchestrator older than
  0.1.121 reports no copy.

**`PUT /installation/ai` (`set_installation_ai`)**, the operator's word: on loopback, or a step of a
plan Archi stages and the operator confirms (`confirmation_required`). Closed to the whole mesh,
Archi's own `ADMIN_COMMAND` included (`MeshAccess::Nobody`). The command log keeps it. Body
`{ai, expected_version, replace_hand_edit?}` (`parse_installation_ai_change`):

- `ai` is the whole new `ai`, taken exactly (`ai_settings::parse_operator_ai`): `default_provider`
  and `providers`, each provider with its `model`; any other field, an empty model, a provider
  named twice or values a reader cannot use is refused (`400 INVALID_REQUEST`, `422
  INVALID_AI_SETTINGS`). A provider given as null is not listed, as `get_installation` shows one;
- `expected_version` is `get_installation`'s; `replace_hand_edit` (false by default; null is
  absent) replaces a hand edit of the motherbee's file, the operator's choice.

What it does (`handle_set_installation_ai`):

1. **The node check (A-11).** Against the values the motherbee's file holds now (its `/versions`,
   `installation_current_values`), and only when the change switches the default provider or drops
   a provider (`ai_change_needs_node_check`). It reads, each hive at once, the managed nodes of the
   motherbee and of each connected worker (`installation_node_check_hives`, `list_nodes` with
   `managed`), then each `AI.*` node's config (`get_node_config`: `behavior.provider`,
   `behavior.model`), and SY.architect's own `config.ai.model` (`scan_node_ai_names`). It refuses,
   writing nothing:
   - `409 NODES_LEFT_WITHOUT_ENGINE`, naming each node and why (`nodes_left_without_an_engine`): a
     node that names its model and not its provider, Archi's own model included, when the default
     provider switches; a node that names its provider and not its model, when that provider is
     dropped. The payload lists `nodes` (`{node, why}`) and `unchecked`;
   - `503 HIVES_NOT_CHECKED`, naming each hive (or SY.architect's config) it could not read.

   A worker whose join did not finish is named in `node_check.skipped`, never a refusal. When the
   motherbee's values are not known (its file is one no reader can use, and its last version
   recorded none), the check is skipped and `node_check` says so.
   **The key check**, when the change switches the default provider (stage 3c's review, F3): every
   reader of the default needs a key of the new provider. The readers (`installation_key_readers`)
   are each AI node that names no provider of its own, in its own tenant, and SY.admin's executor,
   SY.architect, SY.frontdesk.gov and each SY.cognition, in the root tenant, each as itself.
   SY.admin lists each tenant's secrets of that provider from SY.vault, by names only, never
   values (`VAULT_LIST` with `tenant_id` and `resource_type`, `list_provider_keys`), and each reader
   must find one it reads: its own, one assigned to it, or its tenant's general one
   (`readers_without_the_key`). It refuses, writing nothing:
   - `409 PROVIDER_KEYS_MISSING`, naming each node, its tenant and the `resource_type` (payload
     `missing`): put the key in SY.vault first, or give the AI node its own provider;
   - `503 KEYS_NOT_CHECKED`, naming each tenant whose keys could not be listed.
2. **The write.** `SYSTEM_UPDATE {category: "config", write: {ai, expected_version,
   replace_hand_edit}}` to `SY.orchestrator@motherbee`, 30 s. Its refusal passes through with its
   code and payload: `409` `JOIN_IN_PROGRESS`, `INSTALLATION_EDITED_BY_HAND`, `VERSION_CONFLICT`,
   `INSTALLATION_UNCHANGED`, `INSTALLATION_NOT_WRITTEN` or `NOT_PRIMARY`; `422 INVALID_AI_SETTINGS`;
   `502` `INSTALLATION_RECORDS_UNREADABLE`, `INSTALLATION_UNREADABLE` or
   `INSTALLATION_WRITE_FAILED`.
   No answer is `TIMEOUT` (or `TRANSPORT_ERROR`), and not "nothing written": the motherbee's
   orchestrator takes its system messages but the reads one at a time, and one ahead of the write
   (a node's purge, up to 120 s) can hold it past the window. The answer says the write may still
   land (`installation_write_unanswered`, payload `{write_may_still_land, expected_version}`): read
   `get_installation`, and when its `expected_version` moved past the one sent, run `update` with
   category `config` on every hive, since this change applied it on none.
3. **The scan.** The motherbee's Syncthing scans `fluxbee-dist-installation` (`SYSTEM_SYNC_HINT`,
   3 s, waited 5 s; `scan_installation_folder`), as a policy's write does: a hint, never a gate.
4. **The apply.** Every hive at once, the motherbee and each one the registry calls connected
   (`connected_hives_with`): `SYSTEM_UPDATE {category: "config", manifest_version,
   manifest_hash}` (`apply_installation_version`). A `sync_pending` hive is asked again 0.5 s
   later (2 s before 0.1.123), then half again as long each time up to 5 s, for up to 90 s
   (`JSR_ADMIN_INSTALLATION_APPLY_TIMEOUT_SECS`, `apply_installation_on_hive`); a hive it cannot
   reach is asked again within that bound, and a timeout (60 s, an update's) ends the asking. Each
   hive's apply ends by the action's ceiling (210 s). Each restarts only its readers whose engine
   changed, or that became active since its record of the update applied last; SY.admin and
   SY.architect are never restarted. A hive the registry holds whose join did not finish (its
   status not `connected`) is not asked: it is named in `skipped` (stage 3c's review, F4).

The answer's payload is `{version, sha256, previous_version, previous_sha256, hives, skipped,
node_check, timings_ms}`:

- `hives.<hive>`: `status` `ok` (with `restarted` and `unchanged`), `sync_pending` (its copy had
  not arrived at the bound; `local_version`, `local_sha256`), `error` (`error_code`,
  `error_detail`: `VERSION_MISMATCH` for a copy past the version, `INSTALLATION_HASH_MISMATCH` for
  a copy of the version with another hash, `RESTART_FAILED`, `CONFIG_RECORD_FAILED` (its readers
  took the version, and its record could not be written), or `MANIFEST_INVALID` from an
  orchestrator before 0.1.122, which takes no category `config`), `not_answered` (`detail`) or,
  since 0.1.124, `left_to_task` (`detail`: another task of the installation's update held the hive
  until too little of the change's time was left to apply it, so the change sent it nothing;
  below), with `attempts` and `elapsed_ms`; a hive not ok gets `next`, what finishes it by its
  error code (`installation_next_step`): the same update for `sync_pending`, no answer,
  `RESTART_FAILED` and `CONFIG_RECORD_FAILED`; the core first for `MANIFEST_INVALID`; the copy
  restored for `INSTALLATION_HASH_MISMATCH`; the later version for `VERSION_MISMATCH`;
  `update_installation` once that task ends, or the per-hive update, for `left_to_task`
  (`installation_hive_left_json`). Never "not applied" (P-13): a hive that did not answer may hold
  the version already;
- `skipped`: each hive the registry holds whose join did not finish, `{hive, role, status,
  join_phase, next}` (`installation_skipped_hives`): its join may have copied the file and started
  SY.cognition; finish its join or remove it, then `update` it with category `config`. It does not
  make the answer `partial`, as `/versions` leaves such a hive out;
- `node_check`: `{ran: true, hives, nodes, skipped, keys}`, or `{ran: false, reason}`;
- `timings_ms`: `check`, `write`, `scan`, `apply`, `total`, and `hives`, each hive's time to its
  final answer.

The status is `ok` (200) when every connected hive is ok, and `partial` (202) otherwise: the
version is written, `pending` names the hives that are not ok and `message` says what finishes
them (and names the skipped ones). Since 0.1.123 the change also opens the installation's update
([The installation's update](#the-installations-update)), and is one of its explicit runs (a
second while one runs is refused first, `409 INSTALLATION_UPDATE_RUNNING`, writing nothing):

- its target is the version it wrote, as the file, and the motherbee's core, the reference. So a
  hive that comes back behind on its core gets its core too, once, by the automatic step, then the
  version;
- the target differs from an open update's, so the change supersedes it, and the new update starts
  with every hive `unknown`: the background task, woken once the apply ended, reads each one again
  (only the motherbee, the reference, is recorded on its core at once);
- its apply is the update's pass (`installation_update::apply_change_on_hive`, since 0.1.124), and
  `update: {id, state}` is in the answer:
  - each hive is claimed while it applies. A task holding it, one that reads it or one that acts on
    it (the follow applying its config, the automatic step or an explicit run's pass, of the update
    the change superseded), is waited for, and the hive applied once it lets go: the operator's
    change reaches every hive in one go whenever it can. The wait ends while the apply still has
    its own time before the change's ceiling, an update's answer (`JSR_ADMIN_UPDATE_TIMEOUT_SECS`,
    60 s) and never less than 10 s, as a hive applied with less would read `not_answered`; one that
    holds the hive longer keeps it, and the change leaves the hive to it, recorded, its line
    `left_to_task`. No read of the background task is recorded over a hive while it applies
    (FINDINGS A-205: a read taken just before the apply, its answer back after the apply's outcome,
    set the hive back to `pending`);
  - each hive's outcome is recorded as its own apply ends, from its own start to its own end: its
    `elapsed_ms` is the answer's `timings_ms.hives` (A-204: once every apply had ended, every hive
    got the slowest one's time);
  - a hive that did not answer stays pending, and the automatic step finishes it once when it
    answers again;
- a hive that joins after the change takes the file with its join and has applied no version: it
  gets the version at the next explicit run (`update_installation`), whose config update restarts
  its readers once; the background task only reads it;
- a hive registry it cannot read refuses the change first, before anything is read or written,
  `503 HIVE_REGISTRY_UNREADABLE` (F1, as `update_installation`; since 0.1.124, before which it
  applied the version on the motherbee alone and answered ok). When the motherbee's own core is
  rolled back, or the registry fails only when the update opens, no update opens: the version is
  still written and applied, and the answer says why (`update: {id: null, not_opened}`).

```json
{
  "status": "partial", "action": "set_installation_ai",
  "payload": { "version": 2, "sha256": "…", "previous_version": 1, "previous_sha256": "…",
    "hives": {
      "motherbee": { "status": "ok", "restarted": [], "unchanged": ["sy-cognition", "sy-frontdesk-gov"],
                     "attempts": 1, "elapsed_ms": 140 },
      "worker1": { "status": "sync_pending", "local_version": 1, "local_sha256": "…",
                   "attempts": 20, "elapsed_ms": 89620, "next": "GET /versions shows its copy; …" } },
    "node_check": { "ran": false, "reason": "the change keeps the default provider and every provider listed" },
    "timings_ms": { "check": 12, "write": 35, "scan": 410, "apply": 89625, "total": 90090,
                    "hives": { "motherbee": 140, "worker1": 89620 } },
    "pending": ["worker1 (sync_pending)"], "message": "version 2 is written, and not every hive …" },
  "error_code": null, "error_detail": null
}
```

**`POST /hives/{hive}/update` with category `config`** (`handle_hive_update_command`) applies one
version on one hive: `{"category": "config", "manifest_version": <n>, "manifest_hash": "<sha256>"}`,
both from `get_installation`. It answers as the other categories do: `sync_pending` (202) until the
hive's copy is that version, then `restarted` and `unchanged`; the same version again restarts
nothing.

## The installation's update

Since 0.1.123 SY.admin brings every hive to what the motherbee holds, its core and its
installation file, with one update at a time that it keeps on disk
([hive-config-update-v1](../hive-config-update-v1.md) §6, "As built (stage 4)";
`src/bin/sy_admin/installation_update.rs`). Two actions, both `InternalActionRoute::Installation`.

**`POST /installation/update` (`update_installation`)**, the operator's word: on loopback, or a step
of a plan Archi stages and the operator confirms (`confirmation_required`). Closed to the whole
mesh, Archi's own `ADMIN_COMMAND` included (`MeshAccess::Nobody`). The command log keeps it. Body
`{reason?, include_rolled_back?}` (`parse_update_request`; null is absent): `reason` in a few words
("deploy 0.1.123"), and `include_rolled_back`, the hives rolled back by the operator whose core this
run moves too, each one the update reaches (`400 INVALID_REQUEST` otherwise).

What it does (`handle_update_installation`, `run_explicit`):

1. **One explicit run at a time:** a second, this action or `set_installation_ai`, is refused
   before it reads anything, `409 INSTALLATION_UPDATE_RUNNING`, its payload naming the running one
   (`running: {origin, since_ms, update}`).
2. **The hives:** the motherbee and each hive the registry calls connected (`registry_hives`); the
   others are named in `skipped`. A registry that cannot be read is no list (the run would close
   the update on the motherbee alone): `503 HIVE_REGISTRY_UNREADABLE`, nothing changed. A registry
   directory that does not exist yet (no hive ever added) is the motherbee alone.
3. **The target**, from the motherbee's `/versions` (`get_versions`, `target_of`): its core, the
   reference (the manifest hash it runs: `core.installed.manifest_hash`, else dist's; and its
   version), and its installation file's version and sha256 while the file is `current`. A
   motherbee whose own core is rolled back (`core.installed.source` `core_rollback`) runs a hash
   its dist's version does not name: `409 MOTHERBEE_ROLLED_BACK`, install a release on the
   motherbee first. A core it cannot name refuses the run, `502 INSTALLATION_TARGET_UNKNOWN`; a
   file edited by hand names no config, and every hive's config is left as it is, with why.
4. **Every hive read once, at once.** With no update open and every hive on the target it answers
   `ok` with `nothing_to_do`, and records nothing.
5. **Opened or resumed** in `/var/lib/fluxbee/state/sy-admin/installation-update.json`: the same
   target resumes the open update (its `stale_target` cleared, `last_run_at_ms` the run's start);
   another closes it as `superseded` and opens a new one. A task of a closed update (superseded,
   or done) sends nothing more (since 0.1.124, `still_open`): the core update and the config apply
   each check, before they send, that their update is still the open one, and the automatic step
   sends through them; a send already under way finishes, its outcome not recorded.
6. **Each hive's pass, at once** (`run_hive`), decided by `decide` from its `/versions`:
   - a hive another task holds: one that only reads it, or one of a closed update (superseded,
     done), which sends nothing more, is waited for, up to a read and the send it may have under
     way (`Bounds::holder_wait`, 205 s), then the pass acts; one that acts on it for this update
     (the follow applying its config, the automatic step) keeps it, and the hive is recorded as
     left to that task, naming it, the update and how long it has been under way, with `next`.
     Never left unrecorded. A holder that ended meanwhile is none: the hive is claimed
     (`claim_for_run`; `set_installation_ai`'s apply shares it, and waits for a task that acts on
     the hive too, while it keeps its own time);
   - it waits until the hive's orchestrator answers (120 s from the run's start,
     `JSR_ADMIN_INSTALLATION_ANSWER_TIMEOUT_SECS`);
   - its core, when not on the target: `SYSTEM_UPDATE {category: "core", manifest_version: 0,
     manifest_hash}` to `SY.orchestrator@<hive>`, asked again while `sync_pending` or unreachable,
     first after 0.5 s, then half again as long up to 5 s, for up to 90 s
     (`JSR_ADMIN_INSTALLATION_APPLY_TIMEOUT_SECS`). A core that answers `restarting` is left in
     flight: the background task follows it to its settle (300 s,
     `JSR_ADMIN_INSTALLATION_SETTLE_TIMEOUT_SECS`), then applies its config. A core update with no
     answer in time (`TIMEOUT`, an update's 60 s) is followed the same way, since the hive may be
     applying it: it is `not_answered` only when its `/versions` never shows the update within the
     settle bound. A core that did not settle, its record unchanged since, gets its core update
     again (it installs what is pending);
   - its config, when its `config.applied` is not the target's: `SYSTEM_UPDATE {category: "config",
     manifest_version, manifest_hash}`, the same way, after its core;
   - left as it is, with its reason and `next`, the per-hive action that moves it: a hive the
     operator rolled back (`core.installed.source` `core_rollback`) keeps its core unless
     `include_rolled_back` names it, its config still applied; a copy edited by hand gets no
     config; a hive whose orchestrator takes no category config (before 0.1.122) gets its core
     first, and nothing when its core is left as it is. The per-hive `update` moves any of them.
7. It waits for the passes up to its ceiling (`admin_action_timeout`, 550 s); one still running
   goes on, its outcome landing in the record.

The update's states: `open`; `held`, open while the only hives off the target are those the
operator rolled back, each with its config on the target (A-5's one allowed divergence: the per-hive
`update` that moves one back closes the update `done`); `done`; `superseded`. Anything else off the
target keeps it `open`: a copy edited by hand, a motherbee file that is not a recorded version, an
error, a pending hive, one that did not answer. While the motherbee holds another target than the
update's (a downgrade, a new file), the update is never `held` or `done`: it carries
`stale_target`, and nothing acts on it until an explicit run supersedes it.

Its answer: `ok` (200) when the update is done, or held (with `held`, the hives the operator rolled
back, and a message naming them); `partial` (202) while it stays open. Payload `{update, opening,
superseded, include_rolled_back, elapsed_ms, message, pending, held?}`:

- `update`: `{id, state, opened_at_ms, closed_at_ms, last_run_at_ms, origin, reason, superseded_by,
  target: {core: {manifest_hash, version}, core_note, config: {version, sha256}, config_note},
  stale_target, held, hives, pending, skipped}`;
- `hives.<hive>`: `{role, core, config, automatic}`, each category `{state, reason, exclusion,
  outcome, next}`. `state` is `on_target`, `pending`, `in_flight` (a core restarting or settling,
  an update being sent), `excluded` (`exclusion`: `rolled_back`, `ahead`, `hand_edited`,
  `old_orchestrator`, `no_target`) or `unknown`. `outcome` is `{status, by, started_at_ms, at_ms,
  elapsed_ms, attempts, error_code, detail, answered_ms, installed_ms, settled_ms, restarted,
  seen}`: `status` `sent`, `ok`, `restarting`, `settled`, `sync_pending`, `error`
  (`CORE_UPDATE_ROLLED_BACK`, `CORE_NOT_SETTLED`, `CORE_NOT_INSTALLED`, `CORE_NOT_ON_TARGET`, or the
  hive's own code), `not_answered` (never read as not applied) or `excluded`; `by` `explicit`,
  `automatic` or `background` (a config the background task sent after a core it found under way,
  not one a run sent); `seen: true` (only then present) marks a read, by `by`, that found the
  category on the target its last outcome said it was not: no attempt of its moved it (below);
- `pending`: each hive not on the target, as `worker1 (core in_flight (restarting), config
  pending)`.

**The background task** (`run_background`), while an update is open, looks at each hive when its
time comes, in a task of its own, and sleeps until the next one is due:

- **one read of a hive at a time, everywhere, and every hive at once:** the explicit run, the
  follow, the re-read and the automatic step share each hive's read, so at most one
  `GET_VERSIONS` of a hive is outstanding, and the hives' reads go beside each other: the
  background task reads every hive at once, each read under its own bound, since 0.1.137
  (FINDINGS A-263), at most 8 at a time since 0.1.139 (`BACKGROUND_READS_AT_ONCE`, FINDINGS
  A-319): past about 16 in flight the motherbee's orchestrator queued the rest (16 forwarding
  places, 4 per target hive) while each read's bound ran. A look takes its place
  (`background_claim`) before it claims its hive and lets it go once its read ends, so a hive
  waiting for a place is free for an explicit run; while its look waits, no tick spawns another
  look at that hive (`Updates::waiting`), where each tick past the hive's cadence queued one more,
  each reading it again once the one before let it go; the explicit run's own reads, and the
  motherbee's, which its orchestrator answers itself, take none. Before 0.1.137 it read one hive at
  a time, so a read that a spoke's self-restart lost held up the reads of the hives after it for
  its whole bound, and an explicit run's pass waited behind them. The motherbee's orchestrator forwards a spoke's read within its own bound (45 s,
  `JSR_ORCH_SYSTEM_FORWARD_TIMEOUT_SECS`), each in a task of its own since 0.1.127 (FINDINGS
  A-199): a read waits 50 s (`JSR_ADMIN_INSTALLATION_READ_TIMEOUT_SECS`), and one that timed out
  anyway keeps that hive unread for another 50 s, so a second never runs beside it. A re-read
  (`recheck`) claims its hive only for its own read, so an explicit run's pass on a hive waits
  only for the read of that hive under way;
- **a read of a hive in its restart** (since 0.1.131, FINDINGS A-250): from SY.admin's `restarting`
  outcome for a hive's core (its core update's answer, or a core found under way) to its `settled`,
  or to the follow's verdict at the settle bound, a read of that hive waits 10 s
  (`JSR_ADMIN_INSTALLATION_RESTART_READ_TIMEOUT_SECS`), and one that times out leaves no hold and
  passes one it finds: the hive's orchestrator restarts itself for the new binary, and a read that
  reaches it in the few seconds it is off its router, or just before it stops, is never answered.
  The follow asks it again at once, in a look of its own (each look holds the hive for one read,
  and the hive's reads still wait for each other), and logs `installation update: a read of a hive
  in its restart got no answer within its bound: asked again at once`. The read given up runs on
  at the motherbee's orchestrator to its 45 s, in one of the 4 places it keeps for each target
  hive, and SY.admin drops its late answer. A read that is answered ends a hold. Outside a
  restart, a core update that did not answer in time included, the 50 s and the hold stay: they
  protect a hive that is slow. The spoke reporting its settle itself is the design for later
  ([hive-config-update-v1](../hive-config-update-v1.md) §6, "Later: the spoke reports its settle");
- a hive in flight is read every 3 s (`JSR_ADMIN_INSTALLATION_FLIGHT_CHECK_SECS`) until it settles
  (a read lost in its restart at once, above), then its config goes, `by` the run that sent its
  core (`background` for a core it found under way). Before that config, and before the automatic
  step, the task checks that the motherbee still holds the target (`stale_target` otherwise, said
  once in the log);
- a hive that did not answer, or not read yet (`unknown`): a check of shared memory every 30 s
  (`JSR_ADMIN_INSTALLATION_RETRY_CHECK_SECS`), which asks no one; it is read only once shared
  memory shows its orchestrator (`connected_nodes_from_shm`), and read anyway when shared memory
  cannot be read;
- a hive left as it is (excluded), in error, `sync_pending` at its bound, or pending: read every
  10 min (`JSR_ADMIN_INSTALLATION_SLOW_CHECK_SECS`). A read sets its states as a run would decide
  them, nothing sent: a hive the operator moved with a per-hive `update` counts. A core that did
  not settle (`CORE_NOT_SETTLED`, `CORE_NOT_INSTALLED`) stays pending while its record is the one
  it saw then: its `next` says to run the update again or the per-hive `update`, and it is in
  flight again only once its record changes;
- **what such a read records** (`recheck`, for this look and the one above), category by category,
  since 0.1.124: nothing when the category's last outcome came in after the read began, which
  stands (FINDINGS A-205: an apply that answered while the read was under way is newer than what
  the read shows); and for a category it finds on the target, its last outcome saying otherwise
  (`excluded`, an error, no answer), an outcome of the read: the category's own status on the
  target (`settled` for a core, `ok` for a config), `seen: true`, `attempts` 0, the read's times,
  and a `detail` naming the outcome it replaces. Every read that decides does the same, `by` its
  own kind (`record_step`): this one, an explicit run's pass, the automatic step, and the follow's
  config step once the core settled (A-204: a hive moved by its own per-hive `update` kept the
  last outcome a run had left, `excluded` by the automatic step on 8.x; a hive whose apply
  answered too late kept `not_answered` once its automatic step found it applied);
- **each hive by the open update's cadence:** the task's first look at a new update (a run's, a
  change's) forgets when the looks of the update it superseded had each hive due next (since
  0.1.124: the due time is kept per hive, and a hive the superseded update read every 10 min, which
  the new run recorded `not_answered`, waited out that cadence before its 30 s check);
- **an immediate look,** which never blocks the action: a per-hive `update` or `core_rollback` on a
  hive, the end of `add_hive`'s join and `remove_hive` make the task look at that hive at once
  (`add_hive`'s and `remove_hive`'s is the payload's `hive_id`, whatever the command's target,
  which Archi sets to its own hive: `moved_hive_of`); a read of `get_installation_update` or of
  `/versions` makes it look at each hive off the target it has not looked at for 30 s;
- **the automatic step:** a hive whose outcome was `not_answered`, and that answers again, gets the
  update finished, once per hive and target, core then config, with the same bounds. A core it
  finds under way is followed instead, and the attempt is not spent on it. The attempt is recorded
  before it acts (`automatic: {started_at_ms, finished_at_ms, result}`; `result` says `followed`
  while what it started is in flight), its outcomes `by: automatic`. It never acts on a hive rolled
  back by the operator, one ahead of the motherbee (its core a newer release SY.admin recorded as a
  target), or one with a copy edited by hand: each is recorded `excluded`, saying why, and a hive
  that did not answer is promised the step "unless it leaves the hive alone". An error, or
  `sync_pending` at its bound, is left for the operator with its `next`;
- a hive registry that cannot be read is said once in the log: that look neither refreshes the
  update's hives nor closes it;
- the update closes as `done` once every hive is on the target.

SY.admin's start picks the open update up from disk: the task first checks its target against the
motherbee's; a category in flight stays in flight and is followed, a core sent with no answer
recorded included (it becomes `not_answered` only when its `/versions` never shows the update within
the settle bound); a config apply sent with no answer is sent again (the same version restarts
nothing).

Every outcome is one line of the log, `installation update: outcome`, with `update`, `hive`,
`category`, `outcome`, `by` and `elapsed_ms`; the opening, the supersede, a stale target, the held
and done states and the automatic step (`deciding what it finishes`, `decided`, with each
category's step and what it leaves alone, and `ended its pass`, with its result) are lines of their
own (`installation update: ...`).

**`GET /installation/update` (`get_installation_update`)**, a read of SY.admin's own record, not in
the command log; Archi may ask for it over the mesh. `?history=true` (`history: true`) adds the
closed updates, newest first, at most 20, each with its target and the hives it left off it. The
answer, `{update, open, history?}`, stays well inside one mesh frame at the history's bound (a test
holds it under 96 KiB for 12 hives with every text at its longest). `GET /versions`
(`installation_update`) and `get_installation` (`update`) carry its summary: id, state, target,
`stale_target`, `held` and each hive's `{core, config}` state.

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
- **Execution** (`execute_admin_executor_plan`): each step's `$steps.<step_id>.<path>`
  references are resolved first, from the earlier steps' results.
  - **A step with nothing to fill**, with no `executor_fill` (or one that allows no field), is
    dispatched as the plan wrote it, with no model (`executor_step_has_nothing_to_fill`, since
    0.1.95). The model could only repeat its args, since a change is refused. Repeating a long one,
    a workflow's definition, outgrew the model's answer at WF Case 5's gate. Its events say "as
    planned". The dispatch is the one the model's tool makes (`dispatch_internal_admin_command`),
    so it routes every action the executor may run (0.1.97; 0.1.96's went through
    `handle_admin_command_with_origin`, which answered `UNKNOWN_ACTION` for the wf-rules ones).
  - **A step with fields to fill** gets an LLM run, whose only tools are that step's action (and
    `get_admin_action_help` if `allow_help_lookup`). The prompt tells it to call the action with
    exactly the step's args (`admin_executor_prompt`). If the model calls no tool, SY.admin runs
    the step directly (`execute_admin_executor_step_fallback`, "fallback dispatch").
  - With `stop_on_error` the run stops at the first failure. `dry_run` validates only.
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
  `/var/lib/fluxbee/state/admin-executor/<hive>/executions/<execution_id>.json`. Of an
  `unpublish_artifact` step it keeps the args as the command log does, a link's key only as
  `key_sha256` (`executor_log_step_args`).
- **Over the mesh the result is cut no more than the frame needs** (FINDINGS A-198,
  `fit_executor_answer_for_mesh`). The plan has run when its answer is sent, so SY.admin measures
  the answer serialized, with room for the router's reserve and the message around it, and goes
  step by step:
  1. **Whole.** An answer that fits goes as it is.
  2. **The largest outputs cut first.** The steps' outputs (`result_preview`) and the args the
     answer echoes (`tool_args_preview`) past one share are cut to it, the share the largest under
     which the answer fits, so the smaller ones stay whole (`cut_executor_previews`,
     `mesh_reply::bounded_value`, which cuts by serialized length, escapes included). A cut value
     keeps its shape: its small fields whole (a status, an id, a short list: a deploy's `wf_node`,
     a change's `version` and `pending`, which Archi reads), its large ones cut, a long text with
     how many bytes it lost, a long list with `{"truncated_items": n}`, the fields with no room
     dropped and counted (`truncated_keys`). An error text past 2 KiB is cut too. A share under 256
     bytes is no use, and the next step follows. The output of a step that answers a secret
     (`publish_cloud_endpoint`, `externalize`, `vault_get`: `EXECUTOR_ACTIONS_ANSWERING_A_SECRET`)
     is never cut, since a token cut short would be shown as the token: it stays whole, or the
     next step follows (since 0.1.139's integration review, FINDINGS A-124).
  3. **The outputs dropped.** Every output and args goes; each event keeps its step, status, tool,
     error code and error text (cut to 256 bytes), and of its output its status, its error code,
     its ids and the fields Archi reads (`wf_node`, `warning`, `pending`, `version`, `held`,
     `nothing_to_do`, a removal's `pages_left_up` cut to 1 KiB, a workflow delete's
     `packages_left` in 1 KiB, each version's `runtime`, `version`, `error_code` and `retryable`,
     the first one's `detail` cut to 256 bytes and each hive not read with its `reason` and `next`
     (`packages_left_essentials`), a refusal's `retryable`, its `unanswered_hives` in 1 KiB, each
     hive with its `reason` and `next` (`unanswered_hive_essentials`), and its `next` cut to 256
     bytes (FINDINGS A-328, since 0.1.139: Archi's report of a run says from them whether asking
     again may pass), and of an installation update its `id`, `state`, `held`,
     `stale_target` and `superseded_by`, never its hives: Archi reads an update whose answer was
     cut again; `executor_result_essentials`), and a step that answers a secret its secret whole,
     its `token`, `verify_token` or vault-shaped `value` (`executor_answer_secrets`, since 0.1.139's
     integration review: the operator gets it in this answer only, and Archi shows it once). Its
     run id, time and summary line go. If the answer still does not fit, each step keeps only its
     last event (`collapse_executor_events`).
  4. **REPLY_TOO_LARGE.** An answer past all of it goes as `REPLY_TOO_LARGE`, with `action_ran`
     and an `outcome` that keeps the run's `execution_id`, `plan_status`, `completed_steps`,
     `total_steps`, `failed_step_id` and `log_path`.

  An event that was cut or dropped says so: `output_truncated` with `output_bytes`,
  `args_truncated` with `args_bytes`, the sizes before. A cut answer names how in
  `outputs_bounded`: its `mode` (`cut`, `outputs_dropped`, `events_collapsed`), the share
  (`max_bytes_each`, in `cut`) and `full_log`, the run log above (`summary.log_path`), which keeps
  every output whole, secrets redacted. No action reads it back: it is read on the motherbee, and
  each step's outcome is also in the command log (`list_recent_commands`, origin `executor`).
- **Model and key**: provider and model from the hive's `ai` settings (`hive_ai_settings`): the
  installation file once the hive holds one, hive.yaml's `ai` while it holds none (the fallback
  provider without either); CONFIG_GET's `config.ai.source` says which, with the file's version
  and sha256. The key from SY.vault, resource type = the provider, root tenant
  (`build_admin_executor_ai_runtime`). At boot the read waits for the vault, up to 60 s, in a task
  of its own (`boot_admin_executor_ai_runtime`); a later read asks once. Without a key
  `executor_execute_plan` answers `EXECUTOR_UNAVAILABLE`; with settings it cannot use, it answers
  with their reason (`admin_executor_unavailable_reason`), and CONFIG_GET fails with it. A
  `VAULT_SECRET_CHANGED` for that provider rebuilds the runtime in place
  (`handle_vault_secret_changed_admin`).
- **A new version of the installation file** is taken at the executor's next use, never by a
  restart: no config update restarts SY.admin, which may be running the very change
  ([The installation's ai](#the-installations-ai)). Each verdict records the stamp of the file it
  read, the sha256 of its bytes (`ai_settings::InstallationBuiltFrom`); before each plan it runs or
  reviews, the executor reads that one small file and hashes it (`executor_takes_new_installation_ai`).
  A changed file whose engine (the default provider and its model) is the one the runtime runs
  keeps the runtime, with no key read: the stamp is recorded and nothing else happens. Only
  another engine rebuilds it, its key read once from the vault (`ai_settings::provider_key`), and
  the log then says once where its `ai` comes from. A vault that did not answer is no verdict: the
  runtime it had stays, no stamp is recorded, a warning is logged and the next use asks again. A
  key the vault does not hold for the new provider, or an `ai` no reader can use, takes the
  runtime down with the reason, as at start (stage 3c's review, F1). Until a verdict is recorded
  (a boot that waited on the vault), every use asks.
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
  (`PUBLICATION_*_SECS`). The sweep retires expired ones every 600 s; it asks no edge whose hive
  the hive registry no longer lists, and releases only the page's copy, as a take-down does
  (`publication_edge_hive_gone`, FINDINGS A-242). It holds the publication lock only to read a
  page's row and to write each of its states, never across the edge's or IO.blob's call (30 s
  each), and writes a state only while the row is still at the one it read, so a state another
  writer recorded meanwhile stays (`retire_expired_publication`, `advance_publication`, FINDINGS
  A-260): the sweep's own calls, even to an edge that is listed but down or to a slow IO.blob,
  hold up no publish or take-down. A publish still holds the lock across its curate and the
  edge's publish (up to 110 s; a failed one lets it go before its clean-up, FINDINGS A-280,
  below), and each of the sweep's reads and writes waits for it. A take-down holds the lock as
  the sweep does (FINDINGS A-274, below), so the two may run on one page at once, and neither
  writes over the other's state. The ledger keeps a page's row for 30 days once the page is down,
  `expired` or `unpublished` (since its `released_at`, or its `expires_at` for a row without one);
  each sweep first drops the rows down for longer, and keeps every other row
  (`drop_old_down_publications`, `PUBLICATION_LEDGER_KEEP_DOWN_SECS`, FINDINGS A-231).
- **A producer on a worker** (FINDINGS A-81) promotes its blob there, and Syncthing brings it to
  the motherbee about 10 s later. Before the publication lock, unless the blob is in the
  motherbee's `active/` already, SY.admin asks the producer's hive to scan its blob folder
  (`SYSTEM_SYNC_HINT` to its `SY.orchestrator`, `fluxbee-blob`, `wait_for_idle: false`, 10 s at
  most, in a task of its own) while it looks for the blob every half second for up to 30 s
  (`await_producer_blob`). A producer on the motherbee gets the looks only. Either way the curate
  answers for the blob. Any curate error, a lost answer included, releases what `IO.blob` may
  have curated, after the answer (`fail_publish`, below).
- **One budget of 110 s** covers a whole `publish_artifact`, under the 120 s ai.generic waits for
  its answer: the wait for the blob, the publication lock, the curate (30 s at most) and the
  edge's publish (75 s at most) each get what is left. A step with nothing left answers
  `TIMEOUT`. A publish that fails once it asked for the curate answers at once and lets the
  publication lock go (`fail_publish`, FINDINGS A-280, since 0.1.137); its clean-up runs after
  the answer, in a task of its own: IO.blob's release of what it may have curated (`BLOB_RELEASE`),
  after the edge's unpublish (`EDGE_UNPUBLISH_BLOB`) when the edge took the page and the last
  ledger write failed, each within its own 30 s, a failed step logged. It ends with one info line,
  `a failed publish's clean-up ended`, whatever its steps did: the `publication_id`, the `edge`
  and its `edge_unpublish` (`not_asked`, `ok` or `failed`), IO.blob's `release` (`ok` or
  `failed`) with its `released` and `ref_count`, its `ledger_row` (below), and `elapsed_ms`, the
  clean-up's own time (FINDINGS A-280's review). Before, the clean-up ran under the lock and
  before the answer: up to 60 s more against every take-down and sweep, and an answer past
  ai.generic's 120 s. The row the failed publish wrote stays `curated` when it lets the lock go,
  and the clean-up removes it once its steps went through, under the publication lock and only
  while it is still `curated` (`drop_failed_publish_row`, FINDINGS A-318, since 0.1.139):
  `ledger_row` `removed`; `kept` when a step failed, the row left for a take-down or the expiry
  sweep, which finish it as any page stopped half-way; `none` when the publish wrote no row or a
  take-down moved it on first; `failed` when the ledger could not be written. In 0.1.137 the
  publish removed its row before its answer, so a stop of SY.admin during IO.blob's release (up
  to 30 s, a deploy say) left a copy no row names, which nothing collects. A publish that succeeds
  is unchanged.
- **A page outlives its publisher** (FINDINGS A-242). A purge of the node while its publish is
  under way neither waits for the publish nor stops it, and the page, once up, stays up until it
  expires or is taken down, as every page the node published before.
- **Taking a page down.** `unpublish_artifact` (`handle_unpublish_artifact`) names the
  publication by exactly one of `publication_id` (`pub:<uuid>`) or `key`, the last segment of its
  `/public/<key>` link: 64 lowercase hex characters, as `mint_public_capability` makes it. A
  `publication_id` is read in the form the ledger keeps, lowercase and hyphenated, whatever form
  of the UUID it comes in (`canonical_publication_id`). Over the mesh each publisher takes its own
  pages down: an `AI.*`, `IO.*` or `WF.*` node may ask (A-62; FINDINGS A-243, since 0.1.137, where
  only an `AI.*` node might), and only for its own publication: the record's publisher must be its
  exact router-stamped name, and the record's tenant the one identity's shared memory charges
  that name to now (`resolve_artifact_publisher`, the read publish makes), else
  `403 UNAUTHORIZED`. A page outlives its publisher (FINDINGS A-242), so a node launched later
  under the same name in another tenant does not take it down. The operator takes down any
  publication by either field (FINDINGS A-120): `POST /artifacts/unpublish`, or a step of an
  executor plan, which Archi's plan compiler stages and which runs only once the operator
  confirms it. The action is `confirmation_required`, and takes no hive: the publication names
  its edge. It reads the page's row under the publication lock:
  - a publication the ledger does not hold answers `404 NOT_FOUND`;
  - one already down answers ok, `unpublished: false`, and asks no one: `unpublished`, `expired`,
    or past its `expires_at`, which the edge no longer serves. That last one keeps its row for the
    expiry sweep, which takes it down and records it `expired`;
  - otherwise `EDGE_UNPUBLISH_BLOB` to the publication's edge (the row goes to `edge_removed`),
    then `BLOB_RELEASE` to `IO.blob@motherbee` (the row goes to `unpublished`), each with 30 s,
    and the answer is `unpublished: true`. A failed step answers `EDGE_UNPUBLISH_FAILED` or
    `BLOB_RELEASE_FAILED`; the same request again finishes it;
  - a row at `edge_removed`, a take-down past its edge (stopped there, or under way), has had its
    edge's step, so the edge is not asked again: only the `BLOB_RELEASE`, as the expiry sweep
    does, and the answer carries no `edge_skipped`. An edge that is down fails no retry after a
    `BLOB_RELEASE_FAILED` (FINDINGS A-274's review);
  - an edge on a hive the hive registry no longer lists (removed, or renamed: its directory is
    gone) cannot be asked, so its step is skipped: only the `BLOB_RELEASE`, and the answer says
    why in `edge_skipped` (`publication_edge_hive_gone`). Otherwise such a page could not be
    taken down, and would stay `published` until it expires; the expiry sweep skips that edge the
    same way. This admin's own hive, and a registry whose directory is missing, always have the
    edge asked.

  A take-down holds the publication lock only to read the row and to write each of its states,
  never across the edge's or IO.blob's call, as the expiry sweep (FINDINGS A-274): its own calls,
  even to an edge that is listed but down or to a slow IO.blob, hold up no publish, no other
  take-down and no sweep, and neither do the factory reset's take-downs. A publish still holds the
  lock across its curate and the edge's publish (up to 110 s; a failed one lets it go before its
  clean-up, FINDINGS A-280): each of the take-down's holds, its read and each of its writes,
  waits for the publish under way and for those queued before it. Each state is written on the
  ledger loaded again, and only while the row is still at the one the take-down read
  (`take_down_publication`, `advance_publication`), so a page published meanwhile keeps its row
  and a state another writer recorded meanwhile stays. When another take-down, or the sweep, got
  past the edge meanwhile (`edge_removed`), this one goes on to the release; when one of them
  recorded the page down meanwhile (`unpublished`, `expired`), this one asks no one more, writes
  nothing, and answers ok with `unpublished: false`, as it would have after that one.

  The answer carries the `publication_id` either way. A request can carry a live link in any
  field, so the command log keeps only a `publication_id` that is one, in its canonical form, and
  a `key` as `key_sha256`, the form the edge keeps (A-82). Of anything else, a `publication_id`
  that is not one included, it keeps the field's name with the value `***REDACTED***`, and of
  params that are neither an object nor null only `***REDACTED***`
  (`unpublish_artifact_log_params`). So the command log carries no live link, refused requests
  included (A-95), and neither does the executor's run log of a plan that took a page down.
- **The pages that are up** (FINDINGS A-212). `GET /artifacts` (`list_published_artifacts`,
  `handle_list_published_artifacts`) lists the ledger's pages that are neither `unpublished` nor
  `expired` nor past their `expires_at`, newest first and by id within a second
  (`live_publication_rows`). A row is built field by field (`published_artifact_row`):
  `publication_id`, `status` (`published`; `curated`, a publish under way or stopped half-way,
  which the edge may serve; `edge_removed`, a take-down under way or stopped half-way, which the
  edge no longer serves and whose copy IO.blob still holds), `filename_original`,
  `content_type`, `presentation`, `size`, `tenant_id` and `publisher_l2_name` (the producer and
  its tenant when it published), `edge_node`, `created_at` and `expires_at` (epoch seconds).
  **No row carries a link's key, whole or in part:** the key is the permission to read the page,
  and the list goes to Archi, its model and its journal; a page is taken down by its
  `publication_id`, which `unpublish_artifact` takes, and whoever holds a link can still name it
  by its key. Pages (FINDINGS A-198): `limit` and `cursor`, `next_cursor` (`<created_at>|<id>`,
  `publication_place`) while pages follow, `total`; a cursor the list did not give answers
  `INVALID_CURSOR`. It reads the ledger without the publication lock, which a publish holds for up
  to 110 s (the file is replaced whole), asks no one, and, a read, is not in the command log.
  Archi reads it (`GET /artifacts` through `fluxbee_system_get`); the Cloud's relay does not.
- **No removal is refused for a page** (FINDINGS A-242, the operator's call of 2026-11-10). A
  page has a fixed expiry that never renews, past which the edge serves it no more and the sweep
  retires it, so it outlives its node, its tenant and its hive until then; the operator takes it
  down sooner (`unpublish_artifact`, whoever published it). A removal goes as asked, and its ok
  answer names the pages it leaves up in `payload.pages_left_up`: `{count, pages, more}`, the pages
  that are up (as `GET /artifacts` lists them), soonest to expire first, at most 20 of them in at
  most 8 KiB (`more` counts the rest), each `{publication_id, expires_at, status,
  filename_original (cut to 96 bytes), publisher_l2_name, tenant_id, edge_node}` and never a
  link's key (`pages_left_up`, `with_pages_left_up`). A ledger that cannot be read gives
  `{count: null, error}` and refuses nothing; a refusal or a failure names none. Whose pages:
  - a node's purge (`kill_node` with `purge_instance`, `remove_node_instance`, the node as the
    orchestrator reads it, `purged_node_name`), its `teardown_node` (to every caller but its own
    orchestrator) and a workflow's delete (`wf_rules_delete`, the WF node SY.wf-rules names in
    `wf_node.node_name`): the pages whose `publisher_l2_name` is the node;
  - `purge_ilk`: those of the ILK's node, its `identification.node_name`, which goes with the
    purge, so SY.admin reads it first (`ILK_GET`), and only while some page is up
    (`purge_ilk_pages_of`); an ILK of no node names none, and one that cannot be read gives
    `{count: null, error}`;
  - `purge_tenant`: those whose `tenant_id` is the tenant (its publisher's when it published),
    once SY.vault holds no secret of the tenant (`purge_tenant_vault_block`, still a refusal);
  - `remove_hive`: those a node of the hive published and those its edge serves (`edge_node` on
    the hive), the hive being the payload's `hive_id` the orchestrator removes, never the
    command's target (`removal_pages_of`). The hive's cleanup stops its edge (FINDINGS A-236), so
    its pages are served no more once it ran, and taking one down releases only its copy.

  A publish of the node under way at its purge is not waited for: its page may go up within
  110 s, unnamed. Over the mesh the answer stays within its frame (SY.wf-rules and Archi read
  it), and an executor answer cut to fit keeps a step's `pages_left_up`, its `count` and `more`
  whole and the pages that fit in 1 KiB (`executor_result_essentials`).
- **The factory reset takes every page down** through these two actions, first and once more at
  the end: no removal waits for a page, and a publish under way when its node went may put its
  page up within 110 s, so once a node was removed the reset waits until 120 s after its last
  removal and takes down every page that is up again (`unpublish_pages` and
  `take_down_late_pages` in `packaging/fluxbee-factory-reset`; see
  [operations](../reference/operations.md#factory-reset)).
- **Audit log.** Commands are appended to
  `/var/lib/fluxbee/state/sy-admin/command-log/commands.jsonl`: one JSON line with time, action,
  hive, origin (`http`, `l2:<caller>` or `executor`), redacted params (cut at 2048
  bytes), status, error code, HTTP status, duration and `seq`, one past the newest entry's, given
  under the log's lock (FINDINGS A-198): the order the entries were written in, whatever their
  clocks. The file rotates every 100 entries and keeps 3 rotations; the log itself is the counter,
  so a restart goes on from it. `GET /hives/{hive}/commands` (`list_recent_commands`) reads it,
  newest first by `seq`: 50 a page by default, at most 500 and at most half a frame (64 KiB), with
  `next_cursor` while older entries follow; filters `action`, `status`, `since_ms`. Not logged:
  `update` and `sync_hint` (comment above `ADMIN_COMMAND_LOG_ROTATE_ENTRIES`).

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
- `unknown`: neither read says (`view_error`, `answer_error`), or the orchestrator's
  `lifecycle_state` is `UNKNOWN`: the hive's systemd did not answer for io.web's unit (FINDINGS
  A-271, since 0.1.133). Read it again.

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
| `ai` | executor provider and model while the hive holds no installation file; ignored once it holds one |
| `dist.path` | where the installation file is: `<dist.path>/installation/installation.json`, default `/var/lib/fluxbee/dist` |
| `blob.path` | blob root, default `/var/lib/fluxbee/blob` |
| `storage.path` | root of the `/modules` store, default `/var/lib/fluxbee` |

Environment: `JSR_LOG_LEVEL`, `JSR_ADMIN_LISTEN`, the timeout overrides above.

The list of hives is not in hive.yaml: it is the hive registry, `/var/lib/fluxbee/hives/<hive>/`,
which `add_hive` creates and `remove_hive` deletes (FINDINGS A-48). The hives a policy write
waits for, and the ones an OPA read reports as expected (`expected_hives_policy`), are the
motherbee plus every registry entry whose `info.yaml` says `status: connected`
(`opa_rollout_hives`, `expected_hive_sets`). The A-62 table reads a mesh caller's hive role from
the same entries (`caller_hive_role`).

Every read of the registry's entries reads them as the orchestrator does (FINDINGS A-311, since
0.1.137):

- A directory whose name is not a hive id (a `lost+found`, a `worker1.bak` left there) is no
  hive, and is warned of once while SY.admin runs (`registry_hive_entries`). No join files one
  under such a name, and `add_hive` and `remove_hive` refuse it (`INVALID_HIVE_ID`). Read as a
  hive, `set_installation_ai` named it in `skipped` as a join to finish or remove.
- A connected entry stands for a hive only under its own name: its `hive_id` is the directory's
  name, as the strict reading requires (`connected_registry_hive`). Read by the `hive_id` it
  records, a copy of a removed hive's entry kept that hive pending in every policy rollout and in
  the installation's changes and update, and a copy of a removed ingress's made the public edge
  ambiguous (`resolve_public_edge_node`: two connected ingress hives).

## State on disk

- `/var/lib/fluxbee/state/sy-admin/command-log/`: the audit log.
- `/var/lib/fluxbee/state/sy-admin/publications.json`: the publication ledger; a page's row stays
  30 days after the page went down.
- `/var/lib/fluxbee/state/sy-admin/acme.json`: the ACME client's state, written atomically (no
  key): the account URL per directory, the order in flight, a certificate waiting to be served,
  the last attempt and test, the last 20 issued certificates, the backoff, the renewal plan, the
  last edge status seen and the conditions. An unreadable file is moved aside
  (`acme.json.unreadable-<time>`) and ACME starts empty.
- `/var/lib/fluxbee/state/sy-admin/installation-update.json`: the installation's update, written
  atomically: the open update with each hive's state, outcome and next step, and the last 20
  closed ones ([The installation's update](#the-installations-update)). An unreadable file is moved
  aside (`installation-update.json.unreadable-<ms>`) and the state starts empty.
- `/var/lib/fluxbee/state/admin-executor/<hive>/executions/`: executor run logs.
- `/var/lib/fluxbee/state/runtime-package-staging/`: staging for `publish_runtime_package`.
- `/var/lib/fluxbee/nodes/SY/SY.admin@motherbee/config.json`: executor catalog config.
- `<storage.path>/modules/`: the `/modules` store.
- `publish_runtime_package`, `remove_runtime_version` and `reset_base_runtime` rewrite the
  motherbee's `dist/runtimes` manifest under one lock (`runtime_manifest_lock`); each refuses to
  run off the motherbee (`NOT_PRIMARY`). Each also holds the manifest's file lock,
  `dist/runtimes-manifest.lock` (in the dist root, outside the runtimes folder Syncthing shares),
  from its read of the manifest to its write (`lock_runtime_manifest`, FINDINGS A-171), the flock
  every writer outside SY.admin takes too
  (`fluxbee-publish`, `scripts/publish-runtime.sh`, the .deb's seed): `remove_runtime_version`
  and `reset_base_runtime` before they read it (`lock_local_runtime_manifest`),
  `publish_runtime_package` in the shared install. A removal reads the version's use before it
  takes either lock, and reads it again under them only when the runtime changed meanwhile
  (FINDINGS A-306, since 0.1.139; [A runtime version's removal](#a-runtime-versions-removal)).
  A writer waits for another up to 60 s (`RUNTIME_MANIFEST_LOCK_WAIT`), for SY.admin's lock as for
  the file's (`take_runtime_manifest_lock`, since 0.1.139: it waited with no bound): past that a
  removal answers `RUNTIME_REMOVE_FAILED`, a reset `RUNTIME_RESET_FAILED` and a publish
  `RUNTIME_MANIFEST_BUSY` (`SERVICE_FAILED` before 0.1.139), with the manifest untouched. Each
  says `payload.retryable: true` (FINDINGS A-305, A-306): asked again once the other writer is
  done, it may pass. A lock whose file cannot be opened or locked is the dist's to fix: a removal
  or a reset answers its code with `payload.retryable: false` (`manifest_lock_refusal`), and a
  publish `SERVICE_FAILED` with no payload (`publish_runtime_package_failure`).

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

Generated from `INTERNAL_ACTION_REGISTRY` (registry version 26, 121 actions, in registry order),
`admin_action_path_patterns` and `admin_action_summary`. A few long descriptions are cut to their first
sentence; `GET /admin/actions/{action}` has the full text, the fields and an example.

| Action | HTTP route(s) | Description |
| --- | --- | --- |
| `hive_status` | `GET /hive/status` | Read the local hive status summary. |
| `list_hives` | `GET /hives` | List all known hives. |
| `list_admin_actions` | `GET /admin/actions` | List the admin action catalog: in full over HTTP, compact (each action's name, description and read_only) over the mesh. |
| `list_cloud_actions` | — (no HTTP route) | List the provisioning actions IO.cloud (Fluxbee Cloud) may invoke — the exposed subset. |
| `get_admin_action_help` | `GET /admin/actions/{action}` | Return help metadata for one admin action. |
| `publish_runtime_package` | `POST /admin/runtime-packages/publish` | Publish one runtime package into dist/manifest on motherbee. |
| `publish_artifact` | — (mesh only) | Publish an existing BlobRef through SY.edge. Mesh-only: the router-stamped AI.*, IO.* or WF.* caller, on a worker or the motherbee, becomes the publication owner. |
| `unpublish_artifact` | `POST /artifacts/unpublish` | Take a public page down before it expires, by its publication_id or the key of its link. The operator takes down any page; over the mesh an AI.*, IO.* or WF.* node takes down only its own. |
| `list_published_artifacts` | `GET /artifacts` | List the public pages that are up, newest first: each one's publication_id (what unpublish_artifact takes), status, file name, content type, size, tenant, publisher, edge, and when it went up and expires. Never a link's key. |
| `list_versions` | `GET /versions` | List core and runtime versions across hives, and each hive's installation file. |
| `get_versions` | `GET /hives/{hive}/versions` | Read core and runtime versions for one hive, and its installation file. |
| `get_installation` | `GET /installation` | Read the installation file: the motherbee's (its version, hash, state and ai: the default provider and each provider's model), the versions recorded with their values (the newest 20 unless asked), and each hive's copy with the config update it applied last. |
| `set_installation_ai` | `PUT /installation/ai` | Change the installation's ai (the default provider and each provider's model) on every hive: the motherbee's orchestrator writes the next version of the installation file, and each hive restarts only the readers whose engine changed. Closed to the mesh, confirmed by the operator ([The installation's ai](#the-installations-ai)). |
| `update_installation` | `POST /installation/update` | Bring every hive to what the motherbee holds, its core and its installation file: one update at a time, kept on disk and resumed after SY.admin restarts. Closed to the mesh, confirmed by the operator ([The installation's update](#the-installations-update)). |
| `get_installation_update` | `GET /installation/update` | Read the installation's update: the current or last one, its target, and each hive's state, last outcome and next step; with history, the closed ones. |
| `list_runtimes` | `GET /hives/{hive}/runtimes` | List runtimes for one hive. |
| `get_runtime` | `GET /hives/{hive}/runtimes/{runtime}` | Read one runtime definition in a hive; `tool_catalog` is the tool catalog its version ships (ai.generic's base tools), or null. |
| `remove_runtime_version` | `DELETE /hives/{hive}/runtimes/{runtime}/versions/{runtime_version}` | Delete one published runtime version from motherbee dist/manifest. |
| `reset_base_runtime` | `POST /hives/{hive}/runtimes/{runtime}/reset` | Return a base runtime to the version the installed package brought, its `package_version`: `current` becomes that version, and every node that follows `current` moves onto it. Motherbee-only, closed to the mesh, confirmed by the operator (FINDINGS A-122). |
| `list_routes` | `GET /hives/{hive}/routes` | List route rules for a hive. |
| `list_vpns` | `GET /hives/{hive}/vpns` | List VPN patterns for a hive. |
| `list_nodes` | `GET /hives/{hive}/nodes` | List nodes for one hive only. Use inventory for system-wide node visibility. |
| `list_streams` | `GET /hives/{hive}/streams` | Read the status of every router stream of one hive: backlog, deliveries and drops. |
| `get_llm_usage` | `GET /hives/{hive}/usage` | Read the LLM usage records, one per model call, of one tenant or of every tenant: a period grouped by day, model, node or tenant or record by record, or the records stored since a cursor. |
| `cognition_clear` | `POST /hives/{hive}/cognition/clear` | Make one hive's SY.cognition drop every communication and every episode it keeps (in memory, in its file and in its memory region) and answer what it removed. |
| `get_storage` | `GET /config/storage` | Read storage configuration and status. |
| `add_hive` | `POST /hives` | Create a hive and bootstrap it. |
| `get_hive` | `GET /hives/{hive_id}` | Read one hive definition. |
| `remove_hive` | `DELETE /hives/{hive_id}` | Remove a hive, and retire its system ILKs once it is gone. |
| `retire_hive_ilks` | `POST /hives/{hive_id}/system-ilks/retire` | Retire the system ILKs of a removed hive, its `SY.*@<hive>` identities: marked, reversibly, out of every lookup and of the identity SHM, never purged. Refused for the motherbee and for a hive the registry still lists ([A removed hive's system ILKs](#a-removed-hives-system-ilks)). |
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
| `node_help` | `GET /hives/{hive}/nodes/{node_name}/help` | Ask one node to describe itself (NODE_HELP) and return what it says. |
| `list_tenant_nodes` | `GET /hives/{hive}/identity/tenants/{tenant_id}/nodes` | List the managed nodes charged to one tenant on every hive, with their runtime, state and who launched them. |
| `send_node_message` | `POST /hives/{hive}/nodes/{node_name}/messages` | Send a system message to a node. |
| `list_recent_commands` | `GET /hives/{hive}/commands` | Audit log: list the most recent admin commands executed on this backend (all origins — operator HTTP, internal L2 callers like SY.architect, and executor-plan steps), with redacted params, result status and duration. Read-only. |
| `node_control_config_get` | `POST /hives/{hive}/nodes/{node_name}/control/config-get` | Send CONFIG_GET to a node that participates in live control-plane config and return its CONFIG_RESPONSE. |
| `node_control_config_set` | `POST /hives/{hive}/nodes/{node_name}/control/config-set` | Send CONFIG_SET to a node that participates in live control-plane config and return its CONFIG_RESPONSE. |
| `list_ilks` | `GET /hives/{hive}/identity/ilks` | List identity ilks in a hive. |
| `get_ilk` | `GET /hives/{hive}/identity/ilks/{ilk_id}` | Read one identity ilk. |
| `set_ilk_definition` | `POST /hives/{hive}/identity/ilks/{ilk_id}/definition` | Set the cognitive definition hashes for one identity agent ILK. |
| `delete_ilk` | `DELETE /hives/{hive}/identity/ilks/{ilk_id}` | Mark one identity ILK deleted (reversible: hidden, its keys stay reserved until purge). Refuses well-known SY system ILKs (SYSTEM_ILK_PROTECTED). |
| `restore_ilk` | `POST /hives/{hive}/identity/ilks/{ilk_id}/restore` | Undo the deleted mark of one identity ILK (a person also while their tenant is marked; any other ILK waits for its tenant). Refuses system ILKs (SYSTEM_ILK_PROTECTED). |
| `set_ilk_tenant` | `PUT /hives/{hive}/identity/ilks/{ilk_id}/tenant` | Charge one person to another tenant, its cost center (a temporary only out of a deleted tenant); channels keep their scope. |
| `purge_ilk` | `POST /hives/{hive}/identity/ilks/{ilk_id}/purge` | Physically and finally remove one identity ILK that is ALREADY marked deleted (ILK_NOT_DELETED otherwise); a system ILK only once it is retired (SYSTEM_ILK_PROTECTED otherwise). |
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
| `set_hive_egress` | `PUT /hives/{hive}/egress` | Put a worker behind the motherbee's egress: its hive.yaml gets the motherbee's egress section (gateway_ip, reachability_url), as add_hive writes it into a new worker, and its default route goes to the egress gateway with IPv6 off. Closed to the mesh, confirmed by the operator ([A worker behind the egress](#a-worker-behind-the-egress)). |
| `clear_hive_egress` | `DELETE /hives/{hive}/egress` | Take a worker off the egress: its default route goes back to the one it had before (its record's, or gateway_ip for a worker that joined after the declaration), its egress section leaves hive.yaml, and the IPv6 values it had come back. |
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
| `wf_rules_check` | `POST /hives/{hive}/wf-rules/check` | Validate a workflow definition without staging or applying it. |
| `wf_rules_get_workflow` | `GET /hives/{hive}/wf-rules` (with `?workflow_name=`) | Read the current workflow definition managed by SY.wf-rules for a hive. |
| `wf_rules_get_status` | `GET /hives/{hive}/wf-rules/status` | Read workflow status from SY.wf-rules for a hive. |
| `wf_rules_list_workflows` | `GET /hives/{hive}/wf-rules` | List workflows managed by SY.wf-rules for a hive. |
| `wf_list_instances` | `GET /hives/{hive}/nodes/{node_name}/wf/instances` | List the instances of one WF node, newest first, all or those of one status: each with its thread, its state and status, and when it was created, last changed and ended. |
| `wf_get_instance` | `GET /hives/{hive}/nodes/{node_name}/wf/instances/{instance_id}` | Read one instance of a WF node: its thread, state and status, its input, its state variables and its most recent log entries. |
| `wf_cancel_instance` | `POST /hives/{hive}/nodes/{node_name}/wf/instances/{instance_id}/cancel` | Cancel one running instance of a WF node: its cancel transition runs, and the conversation it carries ends. |
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
| `GET /config/storage/metrics` | SY.storage inbox metrics |
| `GET /inventory/nodes` | Global inventory, nodes |
| `GET /versions?hive=`, `GET /deployments?hive=`, `GET /drift-alerts?hive=` | The per-hive form of the global route |
| `GET /opa/policy?hive=`, `GET /opa/status?hive=` | Hive-less forms of the OPA reads |
| `POST /wf-rules`, `POST /wf-rules/{compile,apply,rollback,delete,check}`, `GET /wf-rules`, `GET /wf-rules/status` | Hive-less forms of the wf-rules actions (hive from the body or query) |
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
