# SY.architect

SY.architect ("Archi") is the operator's agent. It serves a chat UI and API on the motherbee's
loopback. The operator asks in plain language; Archi reads the system through SY.admin, and turns
requested changes into executor plans that SY.admin runs after the operator confirms. It also
runs a design pipeline for larger work, publishes runtime packages, keeps the agent assets
(roles, skills, handbooks, personalities) that AI nodes use, and shows the stored conversation
turns.

Source: `src/bin/sy_architect.rs`, with `src/bin/sy_architect/` (`pipeline_types.rs`,
`reconciler.rs`, `artifact_loop.rs`, `failure_classifier.rs`, `messages_db.rs`).

| | |
| --- | --- |
| Binary | `/usr/bin/sy-architect` (`RUST_BINS` entry `sy-architect:sy_architect`) |
| Node name | `SY.architect@<hive>` (`architect_node_name`) |
| Role | motherbee (by configuration) |
| systemd unit | `sy-architect.service`: `Restart=always`, `RestartSec=5`, `TimeoutStopSec=15`; no role gate (`gen_unit`) |
| Listens on | HTTP `127.0.0.1:3000` by default; reaches the mesh through the local router socket |
| State | `/var/lib/fluxbee/nodes/SY/SY.architect@<hive>/architect.lance`, `/var/lib/fluxbee/state/{pipeline,manifests,cookbook}/`, blob `agent-assets/` |

## Where it runs

- `SY.architect` is in `system_nodes.motherbee.nodes` and in no other role's list
  (`packaging/hive.yaml.example`), before `SY.vault` as a vault consumer.
- The binary does not check the role: `main` reads only `hive_id`, `architect.listen`, `ai` and
  `blob.path` from hive.yaml, and the unit has no `ExecCondition`.

## How it starts

`main`, in order:

1. Exits 1 on a non-Linux target. Log level from `JSR_LOG_LEVEL` (default `info`).
2. Reads `/etc/fluxbee/hive.yaml`; computes its ILK from its L2 name
   (`deterministic_system_ilk_id`).
3. Connects to the router as `SY.architect` (`RouterDispatcher::connect_with_retry`, every 1 s).
   Two command channels: `system` and `incoming` (`build_architect_rpc_profile`).
4. Gets its AI key and the messages database URL from SY.vault, waiting up to 60 s for the vault
   to answer (`VaultWait::UntilReachable`, `VAULT_BOOT_WAIT`).
5. Opens its LanceDB, seeds each cookbook that is missing or empty from the seeds built into the
   binary (`COOKBOOK_SEEDS`, the files of `seeds/cookbook/`; the `.deb` ships no seeds), and marks
   pipeline runs left `in_progress` as `interrupted` (`mark_interrupted_pipeline_runs`).
6. Starts the system and incoming workers and a status refresh every 60 s
   (`STATUS_REFRESH_INTERVAL_SECS`), then binds HTTP: `JSR_ARCHITECT_LISTEN`, else
   `architect.listen`, else `127.0.0.1:3000` (`DEFAULT_ARCHITECT_LISTEN`).

## HTTP API

There is no authentication: the axum router has one layer, `DefaultBodyLimit` (FINDINGS A-46).
Archi must stay on loopback: the orchestrator's watchdog reports a non-loopback bind as drift
(`watchdog_listener_check`).

| Route | What it does |
| --- | --- |
| `GET /` (and any other non-API GET) | the chat UI |
| `GET /healthz`, `GET /api/status` | the cached status (`ArchitectStatus`) |
| `POST /api/chat` | one chat turn `{session_id?, message, attachments[]}` (1 MiB) |
| `GET/POST/DELETE /api/sessions`, `GET/DELETE /api/sessions/<id>` | sessions; `DELETE /api/sessions` deletes all of them |
| `POST /api/sessions/<id>/pipeline-recovery` | resume or discard an interrupted pipeline run (32 KiB) |
| `POST /api/executor/plan` | run an executor plan (4 MiB) |
| `POST /api/package/publish` | publish an uploaded runtime package (1 MiB) |
| `POST /api/software/upload`, `POST /api/software/publish` | upload a file (128 MiB), publish it as a runtime |
| `POST /api/attachments` | chat attachments (up to 8 files, 10 MiB each) |
| `GET/POST/DELETE /api/agent-assets` | agent assets |
| `GET /api/messages`, `GET /api/messages/stream`, `GET /api/messages/<dedupe_key>` | the Messages page |
| `GET /api/identity/ich-options` | channels from the local identity SHM |

The global body limit is 132 MiB (`ARCHITECT_MAX_MULTIPART_UPLOAD_BYTES`); the per-route limits
above are in `dynamic_handler`.

## Chat

`handle_chat_message` looks at each message in this order:

1. `CONFIRM`: runs what is waiting for confirmation (a pipeline checkpoint, or a plan the plan
   compiler prepared, sent with labels `{"origin": "plan_compiler_confirmed"}`). `CANCEL` or
   `ABORT` drops it.
2. While something waits for confirmation, other messages get `status: "blocked"`.
3. `SCMD: <curl-like command>`: an operator command, run at once (see SCMD).
4. A pasted JSON `executor_plan`: validated and run at once.
5. In an impersonation session, the text goes out as a `user` message to the mesh
   (`handle_impersonation_chat`); replies that carry the session in `meta.context.session_id`
   come back on the `incoming` channel.
6. Anything else goes to the LLM (`handle_ai_chat`), with a session summary and the recent turns.

The LLM's tools (`ArchitectAdminReadToolsProvider`): `fluxbee_system_get`, `list_agent_assets`,
`get_agent_asset`, `delete_agent_asset`, `create_agent_role_asset`, `create_agent_skill_asset`,
`create_agent_handbook_asset`, `create_agent_personality_asset`, `fluxbee_start_pipeline`,
`fluxbee_pipeline_action`, `fluxbee_plan_compiler`.

Sessions, messages, operations, pipeline runs and manifest references live in LanceDB at
`/var/lib/fluxbee/nodes/SY/SY.architect@<hive>/architect.lance` (`architect_db_path_for_hive`;
tables `sessions`, `session_profiles`, `messages`, `operations`, `pipeline_runs`,
`manifest_refs`). A new session id is a UUIDv4.

## Reads, SCMD and mutations

**SCMD** is a curl-like line, `SCMD: curl -X GET /hives/motherbee/nodes`. `parse_scmd` reads it
and `translate_scmd` maps the method and path to one admin action, sent as `ADMIN_COMMAND` to
`SY.admin@<hive>` with the `{hive}` of the path as target. The paths are the ones in the
[SY.admin action table](sy-admin.md#action-table) (for example `GET /inventory` → `inventory`,
`GET /hives/{hive}/vault/secrets` → `vault_list`, `POST /hives/{hive}/nodes` → `run_node`,
`DELETE /hives/{hive}/nodes/{name}` → `kill_node`). Two paths stay inside Archi:
`GET /architect/control/config-get` and `POST /architect/control/config-set`
(`handle_local_architect_scmd`). `SCMD: help` lists the forms.

An SCMD typed by the operator runs immediately; a mutating action is recorded in the
`operations` table (`handle_scmd`, `execute_tracked_admin_translation`).

**`fluxbee_system_get`** is the LLM's read tool. It builds an SCMD from `{method, path, body}`
and runs it through the same `translate_scmd` (`ArchitectSystemGetTool::call`), and it runs reads
only (`system_get_may_run`, FINDINGS A-73): a GET whose action changes nothing, and the POSTs that
change nothing: a node's live `CONFIG_GET` (`node_control_config_get`), an OPA policy check
(`POST /opa/policy/check` → `opa_check`: the motherbee's SY.opa.rules compiles the rego in a
scratch directory and only answers, with no version and nothing staged) and the timer's
conversions. Anything else is refused with "stage it with fluxbee_plan_compiler"; PUT and DELETE
forms cannot be reached through it. The sub-agents' `query_hive` tool accepts only the read
actions in `PROGRAMMER_QUERY_ALLOWED_ACTIONS`.

**Mutations the intended way**: `fluxbee_plan_compiler` asks a plan-compiler agent for an
`executor_plan`, pre-validates it with SY.admin (`executor_validate_plan`) and keeps it until the
operator writes `CONFIRM`. Then `execute_executor_plan_with_context` sends
`executor_validate_plan` and `executor_execute_plan` to SY.admin, which runs the steps (see
[SY.admin, the executor](sy-admin.md#the-executor)). Plans sent to `POST /api/executor/plan` or
pasted in the chat run without that confirmation, unless one is already pending in the session.

## Pipelines

`fluxbee_start_pipeline` starts the design pipeline; `fluxbee_pipeline_action` resolves a blocked
one (`discard`, `restart_from_design`, `retry`). Stages (`PipelineStage` in
`pipeline_types.rs`): `design`, `design_audit`, `confirm1`, `reconcile`, `artifact_loop`,
`plan_compile`, `plan_validation`, `confirm2`, `execute`, `verify`, then `completed`, `failed`,
`blocked` or `interrupted`.

- **design / design_audit**: a designer agent writes a solution manifest and an auditor agent
  checks it, up to 3 rounds (`MAX_DESIGN_ITERATIONS`). Manifests are saved under
  `/var/lib/fluxbee/state/manifests/<solution_id>/` and referenced as
  `manifest://<solution_id>/current`. A manifest that puts a workflow on an ingress or egress
  hive is refused (`refuse_workflows_on_dmz_hives`): by the role its topology gives a hive it adds,
  and, for the designer's manifest, by the role in the motherbee's hive registry. So is one that
  declares a `WF.*` node (SY.wf-rules creates it) or a core node (`SY.*`, `RT.*`), a runtime name
  outside the naming rule (lowercase letters and digits in dot-separated parts), a workflow name
  SY.wf-rules rejects, or a route SY.config.routes would not take as declared (action FORWARD
  with a `next_hop_hive`, or DROP, case-sensitive; one route per prefix on a hive).
  - Validation is strict when a manifest is saved and before a run reconciles. Reading one is not:
    `get_manifest_current` returns a manifest saved before a rule changed with its
    `validation_error`, so the designer can fix it, and a run made from it is blocked.
- **reconcile**: the actual state is read through SY.admin and compared with the manifest by
  `reconciler.rs`, which is deterministic (no LLM). The result is a delta and build tasks.
  - The snapshot (`build_actual_state_snapshot`) reads Archi's own hive and every hive the
    manifest names (topology, nodes, routes, workflows), asking `SY.admin@motherbee` for each
    one's runtimes, managed node instances (`list_nodes` with `managed: true`: running or not,
    with their runtime), routes and VPNs, the stored config (`get_node_config`) of each declared
    node with a declared config, and its workflows when workflows are in scope (the manifest
    declares one, or an executed version of the solution did, whose hives are read too). The
    motherbee's hive registry
    (`list_hives`) says which hives exist: a hive it does not list is one the manifest adds, so it
    is not read and holds nothing; the reconciler creates it (`HiveCreate`, the first operation, so
    `add_hive` is the plan's first step) and then what is declared on it. An ingress or egress hive
    is not asked for runtimes or workflows: it has no runtime manifest and no SY.wf-rules. A hive
    whose role is not known is asked for everything, and without the registry's answer every hive
    is read, a hive that does not answer blocking the snapshot.
  - Each answer is read in the shape its producer sends: a runtime by its `current` version (the
    motherbee's where hives differ); a node as existing while its config.json does (run_node
    refuses the name then), its runtime from `runtime.name`, its config unchanged while the stored
    config holds every declared key, at every level, with the declared value (keys only the node
    stores, as ai.generic fills in its `behavior`, do not count); a route by its prefix
    (SY.config.routes keeps one per prefix: another action or next hop is a replace); a VPN rule by
    its hive and pattern (one rule per pattern: another `vpn_id`, match kind or priority is a
    replace, `delete_vpn` then `add_vpn`).
  - VPN rules are declared as SY.config.routes keeps them, in `desired_state.topology.vpns`:
    `{hive, pattern, vpn_id, match_kind, priority, ownership}`, `vpn_id` a number (u32),
    `match_kind` EXACT, PREFIX or GLOB (PREFIX when omitted), `priority` 100 when omitted. The
    manifest is refused with an empty hive or pattern, a pattern over 256 bytes, another match
    kind, a `vpn_id` that is not a u32, or a pattern repeated on a hive.
  - The reconcile never removes a node, route or VPN rule a solution stops declaring: SY.orchestrator
    and SY.config.routes record no owner for them. Workflows and the user policy are removed,
    because Archi knows which solution deployed them. An extra node, route or VPN rule is removed
    through SY.admin's API (`kill_node`, `delete_route`, `delete_vpn`).
  - Workflows: SY.wf-rules lists each one with `current_hash`, the `sha256:` of the definition as
    it stores it (Go's indented JSON of it); `wf_rules_definition_hash` computes the same hash of
    the declared definition, and a difference is an update. wf-rules records no owner: a running
    workflow belongs to the solution whose executed manifest declared exactly that definition last
    as its own (`ownership: solution`; omitted is external), and only that solution removes it,
    with `wf_rules_delete`, once it stops declaring it, as with the user policy. A manifest version
    is executed once a plan made from it ran (`mark_manifest_executed`, a `<version>.executed` file
    next to it); the versions the designer saves along the way own nothing.
- **artifact_loop**: a programmer agent builds runtime packages, audited, up to 3 attempts
  (`MAX_ARTIFACT_ATTEMPTS`); files under `/var/lib/fluxbee/state/pipeline/<run_id>/`
  (`artifact_loop.rs`).
- **plan_compile / plan_validation / confirm2 / execute / verify**: an executor plan, validated
  by SY.admin, run after the operator's `CONFIRM`, then checked. The plan compiler is given the
  declared user policy, workflow definitions, routes and VPN rules, and the plan's steps are
  pinned to the manifest afterwards (`pin_plan_steps`): `opa_compile_apply` carries exactly the
  declared rego, `wf_rules_compile_apply` exactly the declared hive, name and definition (the plan
  compiler adds only `auto_spawn` and `tenant_id`), the `restart_node` after it that workflow's
  node, `wf_rules_delete` exactly the workflow its operation names, without `force`, `add_route`
  and `add_vpn` exactly the declared route or VPN rule, and the `delete_route` / `delete_vpn` of a
  replace exactly its prefix or pattern. A pinned step loses its `executor_fill`, so SY.admin's
  executor cannot add a field back.
- Failures are classified first by rules, then by an LLM if no rule matched
  (`failure_classifier.rs`), and either retried or escalated.

Runs are stored in the `pipeline_runs` table; a restart marks unfinished ones `interrupted`, and
the session's recovery route resumes or discards them.

## Messages page

`messages_db.rs` reads the `turns` table of PostgreSQL database `fluxbee_storage`, the one
SY.storage writes. It only runs `SELECT`s. The connection URL is the root tenant's `postgres`
secret in SY.vault, credentials and host only (a URL with a database name is refused); Archi adds
`fluxbee_storage` itself (`ARCHITECT_MESSAGES_DB_NAME`). Without that secret the routes answer
503 `messages_db_not_configured`. The list is newest first, default 200 rows, at most 500; the
stream (`/api/messages/stream`) polls every second. Every row keeps the whole message as the router
copied it (`turns.message`): the sender, the size and the detail come from it. A message copied
before routers stamped names (the rows backfilled at 0.1.76) shows the sender's ILK, and a
resolved destination, which the message carries as null, the destination ILK.

## Agent assets

Agent assets are JSON documents an `ai.generic` agent's ILK definition points to by hash: a role,
skills, handbooks and a personality.

- Stored as `<blob root>/agent-assets/<sha256>.json` (`persist_agent_asset`); the blob root is
  `blob.path`, default `/var/lib/fluxbee/blob`.
- `GET /api/agent-assets` lists them (`asset_type`) or returns one (`?hash=`); `POST` creates a
  `role`, `skill` or `handbook` (256 KiB); `DELETE` removes one by hash. A personality can only be
  created by the LLM tool `create_agent_personality_asset`.
- Creating an asset does not touch identity. The reply carries the `definition_fragment`
  (`role_hash`, `skill_hashes`, `handbook_hashes`) to apply with `set_ilk_definition` through the
  plan compiler.
- These files are written on the motherbee's blob root, and SY.orchestrator shares
  `blob/agent-assets/` with the workers, where the agents run (Syncthing folder
  `fluxbee-dist-agent-assets`, sent by the motherbee only; FINDINGS A-72). An asset is written as
  `.agent-asset-<sha256>.tmp` in the blob root itself, which is not shared, given mode 0644 and
  renamed in, so Syncthing never sees a partial file.

## Package publishing

- `POST /api/package/publish` sends `publish_runtime_package` to SY.admin with
  `{source: {kind: "bundle_upload", blob_path}, set_current, sync_to, update_to}`
  (`handle_package_publish_request`). With `set_current` (default true) it then redeploys the
  nodes of that runtime on Archi's own hive with the new current version.
- `POST /api/software/publish` publishes an uploaded `.zip`, or packages a single binary first.
- Pipeline artifacts are published inside the executor plan (`inline_package` or
  `bundle_upload`).

## LLM, key and usage records

- **Provider and model**: the hive.yaml `ai` section (`ai.default_provider`,
  `ai.providers.<provider>.model`); with no section, `openai` and `gpt-5.5`
  (`HiveAiConfig::fallback`).
- **Key**: from SY.vault, the root tenant's secret of the provider's resource type, read as
  Archi. `VAULT_SECRET_CHANGED` for that type, or for `postgres`, rebuilds the AI runtime or the
  messages connection. `CONFIG_SET` refuses `ai`, `ai_providers`, `api_key`, `api_key_ref` and
  the messages URL fields (`reject_architect_secret_fields`).
- **Usage records**: every model call is charged to the root tenant (`AiAccount::root`, source
  `root`) with work `{session_id, pipeline_run_id, solution_id, stage}` (`usage_account`). The
  stage is `chat`, `designer`, `design_auditor`, `programmer`, `plan_compiler` or
  `failure_classifier`. Records go to the router stream subject `storage.usage`.

## The handbook

`read_handbook_text` loads `/etc/fluxbee/handbook_fluxbee.md` (or, in a repo checkout, the
relative `docs/archi/handbook_fluxbee.md`) and `build_archi_prompt` appends it as
"Reference handbook". The `.deb` installs the repo file there:
`install -m0644 "docs/archi/handbook_fluxbee.md" "$DEST/etc/fluxbee/handbook_fluxbee.md"`
(`packaging/build-deb.sh`). A missing handbook is skipped without a log line.

## Mesh messages

- **System channel**: `NODE_STATUS_GET`, `CONFIG_GET` and `CONFIG_SET` are answered only to the
  origins the router admits, checked again against the same system policy
  (`architect_origin_authorized` → `authorize_system`, `policy/system.rego`): `NODE_STATUS_GET`
  from SY.admin or SY.vault of the same hive and from the motherbee's or the same hive's
  SY.orchestrator, `CONFIG_GET`
  and `CONFIG_SET` from `SY.admin@motherbee`. Anything else gets `FORBIDDEN`.
  `VAULT_SECRET_CHANGED` is not gated.
- **Incoming channel**: `user`, `chat` and `text` messages, stored to an impersonation session.
- **Sends**: `ADMIN_COMMAND` to `SY.admin@<hive>` (reads, SCMD, the executor, `inventory` for
  the status), vault reads, usage records, impersonation messages. Admin timeouts: 120 s for the
  executor (`JSR_ADMIN_EXECUTOR_TIMEOUT_SECS`), and 35 min more for each `add_hive` step of the
  plan it runs, since SY.admin's executor follows each join to its end, for up to 30 min, before
  the next step; 30 s default (`JSR_ADMIN_ORCH_TIMEOUT_SECS`) (`architect_admin_action_timeout`).
  While a plan that joins hives runs, the chat's execution hint says so (`execution_hint` on the
  pending confirmation).

## State on disk

- `/var/lib/fluxbee/nodes/SY/SY.architect@<hive>/architect.lance`: sessions and pipeline runs.
- `/var/lib/fluxbee/state/pipeline/<run_id>/`, `/var/lib/fluxbee/state/manifests/`,
  `/var/lib/fluxbee/state/cookbook/`, `/var/lib/fluxbee/state/software-upload-staging/`.
- The blob root: attachments and uploads (`staging/`, then `active/`), and `agent-assets/`.
- Pending plans and the status cache are kept in memory only.

## Failure behavior

- **No AI key**: chat answers "AI provider not configured. Chat remains available for SCMD system
  operations."; SCMD still works.
- **SY.admin not answering**: the status turns `degraded` with `admin_available: false`; SCMD
  answers `SCMD_EXECUTION_FAILED`.
- **No messages secret**: the Messages page answers 503; the rest works.
- **Router down at boot**: `connect_with_retry` waits; HTTP is not bound until it connects.

## Health

- `curl -s http://127.0.0.1:3000/healthz` on the motherbee: `status` (`loading`, `ok`,
  `degraded`), `router_connected`, `admin_available`, and the state of the main components.
- `systemctl status sy-architect`; the log line `sy.architect axum listening` marks HTTP up.
- In the chat: `SCMD: curl -X GET /architect/control/config-get` shows whether the AI key and the
  messages database are configured.

## Links

- [SY.admin](sy-admin.md): the actions SCMD maps to, the executor.
- [SY.orchestrator](sy-orchestrator.md): the watchdog's loopback check.
- [SY.vault](sy-vault.md): the AI key and the `postgres` secret.
- [SY.storage](sy-storage.md): writes the `turns` table the Messages page reads.
- [Archi spec](../history/onworking-coa/sy-architect-spec.md): the original design.
- [Host posture spec](../host-posture-and-exposure-spec-v1.md): why Archi stays on loopback.
- [Router stream](../router-stream-v1.md): usage records.
- `lab/logbook/FINDINGS.md`: A-24 (the snapshot), A-46 (no auth), A-72 (agent assets on
  workers), A-73 (the read tool).
