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

- `SY.architect` is in the release's list for the motherbee and in no other role's
  (`src/release_settings.rs`), before `SY.vault` as a vault consumer.
- The binary does not check the role: `main` reads only `hive_id`, `architect.listen`, `ai`,
  `blob.path` and `dist.path` from hive.yaml, and the unit has no `ExecCondition`. `ai` is read
  untyped, and typed only while the hive holds no installation file
  ([LLM, key and usage records](#llm-key-and-usage-records)).

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
   - A seed is a `manual` entry with its seed's `seed_version`; the pipeline appends `observed`
     ones.
   - A cookbook whose manual entries are older than the built-in seeds (no `seed_version` counts
     as 0) gets them replaced by the built-in ones, in their place, and keeps its observed entries
     as they are. The file is rewritten atomically, and the journal gets one INFO line
     (`cookbook seeds upgraded`) naming the entries replaced and both versions.
   - A seed edit raises that seed's `seed_version` (`a_changed_seed_raises_its_seed_version`).
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

Each chat turn first takes a new version of the installation file, when there is one
(`architect_takes_new_installation_ai`, [LLM, key and usage records](#llm-key-and-usage-records)).
Then `handle_chat_message` looks at each message in this order:

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

**SCMD** is a curl-like line, `SCMD: curl -X GET /hives/motherbee/nodes`. `parse_scmd` reads it,
its path bare or in quotes, as SY.admin's `example_scmd` writes a path with a query
(`'/hives/motherbee/timer/timers?owner_l2_name=...'`, FINDINGS A-279), and `translate_scmd` maps
the method and path to one admin action, sent as `ADMIN_COMMAND` to `SY.admin@<hive>` with the
`{hive}` of the path as target. The paths are the ones in the
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

**Pages** (FINDINGS A-198): the reads of lists that grow with the installation take
`?limit=&cursor=` (`/inventory`, `/inventory/{hive}`, `/hives/{hive}/status`,
`/hives/{hive}/nodes`, `/versions`, `/hives/{hive}/identity/ilks`, `/hives/{hive}/identity/tenants`,
`/hives/{hive}/commands`, `/artifacts`, `/hives/{hive}/timer/timers` beside its `owner_l2_name`
and `status_filter`, and `cursor` beside the filter of `/hives/{hive}/vault/secrets`), and the
installation's versions `?versions_limit=&versions_before=` (`/installation`, `/versions`,
`/hives/{hive}/versions`); `translate_scmd` passes them to SY.admin, the numbers as numbers
(`scmd_page_params`, `scmd_vault_list_params`, and `scmd_timer_list_params`, whose `limit` goes
only when it is a number, as on SY.admin's route). Every query value of an SCMD read is
percent-decoded once, where the query is read (`scmd_query_params`, FINDINGS A-279), as SY.admin's
own HTTP routes decode theirs: a cursor written with `%3A`, or an owner with `%40`, reaches SY.admin
as it does written raw; an escape that is none goes as it came, and `+` reads as a space, where
SY.admin's routes keep it. The path's description tells the model to follow `next_cursor`, and on
`REPLY_TOO_LARGE` to ask again with `error_detail.suggested_limit` ([SY.admin](sy-admin.md#pages));
of the timer list, SY.timer's own page, it says that it gives 100 timers without a `limit` (`limit`
up to 1000), `next_cursor` while timers follow and no `total`, and that a cursor that is not one
(`<fire_at_utc_ms>:<uuid>`) is refused with `TIMER_INVALID_PAYLOAD` (HTTP 400 on SY.admin's route;
the model reads the mesh answer's `error_code`). A well-formed cursor SY.timer never gave is no
error: it lists the timers after the place it names ([SY.timer §7.6](../sy-timer.md)).
`GET /hives/{hive}/commands` (FINDINGS A-201) reads SY.admin's command log
(`list_recent_commands`), newest first, with `?action=&status=&since_ms=`: each command with its
outcome, an executor plan's steps among them (origin `executor`). `GET /artifacts` (FINDINGS
A-212) lists the public pages that are up (`list_published_artifacts`), newest first, each by its
`publication_id` and never by its link's key; taking one down stays a plan the operator
confirms. Archi's own
reads of a list, the nodes of a hive a redeploy reads (`list_nodes_for_hive`) and the motherbee's
nodes its status shows, go page after page with a smaller page when SY.admin says how many rows fit
(`admin_listing_every_page`); an answer that is not ok is an error, never an empty list.

**A plan whose answer was too large** (FINDINGS A-198): SY.admin cuts an executor run's answer as
far as the frame needs, and past that answers `REPLY_TOO_LARGE` with `action_ran` and the run's
outcome. Archi takes it as a run that ran, never as a failed plan
(`executor_report_of_an_answer_too_large`): its message names the execution, its status and steps,
the command log where each step's outcome is and the run's full log on the motherbee; the report
carries the run's summary with no events and `answer_too_large`, so its verdict is never verified
(`verify_execution_result`) and the plan teaches no cookbook. A plan that stopped at a step fails
as before, naming that step. An `update_installation` step whose answer SY.admin cut to fit the
frame (`output_truncated`: its hives cut or dropped) is never judged on that answer: its update
is marked `answer_cut` (`executor_installation_updates`), counts as moving, and is read again with
`get_installation_update` before the verdict, as an update still moving is
(`follow_installation_updates_with`); a cut answer's `done` or `held` stands, its state kept
whole.

**What an agent can do**: the base tools every AI agent has ([agent-tools-v1](../agent-tools-v1.md)
T4). Their catalog is first `GET /hives/motherbee/runtimes/ai.generic` → `get_runtime`, whose
`tool_catalog` the installed version ships, with no AI node running; a running AI node answers the
same cards at `GET /hives/{hive}/nodes/{name}/help` → `node_help`. The prompt, the asset tools'
descriptions and the handbook send Archi there, in that order, before it designs a role or a
skill: a role names no channel and no tool, and no asset promises a tool the catalog does not
list.

**Mutations the intended way**: `fluxbee_plan_compiler` asks a plan-compiler agent for an
`executor_plan`, pre-validates it with SY.admin (`executor_validate_plan`) and keeps it until the
operator writes `CONFIRM`. The agent's prompt lists SY.admin's action catalog in its compact view,
each action's name, description and read_only, kept 5 minutes (`get_or_refresh_admin_actions`),
and it reads an action's schema with `get_admin_action_help` before each step. Archi's reads of
`/admin/actions` ask the same view (`admin_catalog_compact_params`): the full catalog is larger
than one mesh frame, and SY.admin answers it over HTTP alone (FINDINGS A-197). A catalog that
cannot be read is logged, and the agent runs on each action's help alone. Then
`execute_executor_plan_with_context` sends
`executor_validate_plan` and `executor_execute_plan` to SY.admin, which runs the steps (see
[SY.admin, the executor](sy-admin.md#the-executor)). Plans sent to `POST /api/executor/plan` or
pasted in the chat run without that confirmation, unless one is already pending in the session.

A confirmed run is verified (`verify_execution_result`): done, no step failed, each workflow it
deployed has its node running (`wf_deploys_left_not_running`), and each change of the
installation's `ai` applied on every hive. SY.admin's `set_installation_ai` writes the version and
answers `partial` while a hive has not applied it; the run's reason then names those hives, with
`GET /versions` and the update with category `config` that finishes one
(`installation_changes_not_applied`;
[SY.admin, the installation's ai](sy-admin.md#the-installations-ai)).
The installation's `ai` is changed only that way: `set_installation_ai` is closed to Archi's own
`ADMIN_COMMAND`, and its read, `get_installation`, is open to it. The installation's update
(`update_installation`, since 0.1.123) is the same: Archi stages it, and its read
`get_installation_update` (`GET /installation/update`, SCMD and the plan compiler's reads) is
Archi's. SY.admin answers the run once every core answered, and follows the rest: before its
verdict Archi follows each update the run left moving (`follow_installation_updates`), reading it
again every 10 s (`INSTALLATION_UPDATE_POLL_SECS`), up to 600 s
(`INSTALLATION_UPDATE_FOLLOW_SECS`, twice SY.admin's settle bound), while its hives only move: a
core in flight, its config after it, a hive not read yet. A run is verified once the update is
done, or held (every hive on the target but those the operator rolled back: the verdict and the
message name them); a hive in error, without an answer, `sync_pending` at its bound or left as it
is, a stale target or a superseded update keep it unverified, the reason naming each hive with its
`next`, as does an update still moving at the bound (`installation_updates_verdict`;
[SY.admin, the installation's update](sy-admin.md#the-installations-update)).

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
  declared rego; `wf_rules_compile_apply` exactly the declared hive, name and definition, with
  `auto_spawn: true` and the workflow's declared `tenant_id`, which SY.wf-rules reads only to spawn
  a node that does not exist (a deploy that must spawn one without a tenant is refused); an update
  is that one step, since SY.wf-rules restarts the node itself; `wf_rules_delete` exactly the workflow its
  operation names, with `force: true`, after the operator confirmed a plan that says how many of
  its instances run; `add_route` and `add_vpn` exactly the declared route or VPN rule, and the
  `delete_route` / `delete_vpn` of a replace exactly its prefix or pattern. A pinned step loses its
  `executor_fill`, so SY.admin's executor cannot add a field back.
- **The workflow check** (WF Case 5): once the designer's manifest passes validation, each
  declared workflow goes to `wf_rules_check` on its hive (`check_declared_workflows`). A refusal
  goes back to the designer as feedback, with the error's path; a check that cannot run blocks.
  A deploy's `COMPILE_ERROR` goes back to the designer too (`redesign_after_failed_deploy`), and
  `restart_from_design` gives the new run the blocked run's last error.
- **The verification** reads each deploy's node step too: SY.wf-rules answers a deploy ok when
  its node step fails or does not run, and says so in `wf_node`, so a run whose WF node does not
  run fails its verification, naming the node and why (`wf_deploys_left_not_running`). The next
  run deploys it again: a workflow whose definition is current and whose node is gone gets a
  deploy that spawns it (`reconcile_wf`).
- Failures are classified first by rules, then by an LLM if no rule matched
  (`failure_classifier.rs`), and either retried or escalated. At execution, only a design's
  failure goes back on its own (to the designer); any other is the operator's, with the options
  `retry`, `restart_from_design` and `discard`, since nothing retries a step there.

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
  nodes of that runtime on Archi's own hive with the new current version
  (`redeploy_runtime_nodes_with_current`): each node is killed with its instance purged and run
  again, with its stored config when `preserve_node_config` asks for it. Before the kill it reads
  the node's tenant from its stored config (`_system.tenant_id`, else `tenant_id`) and runs it
  again in it, a top-level `tenant_id`; a node whose tenant cannot be read is not touched and
  is named in the answer's `errors` (FINDINGS A-235).
- `POST /api/software/publish` publishes an uploaded `.zip`, or packages a single binary first.
- Pipeline artifacts are published inside the executor plan (`inline_package` or
  `bundle_upload`).
- SY.admin opens a `bundle_upload`'s zip under the blob root without following a symlink at any
  step, takes only a regular file of at most 512 MiB, extracts at most 10 000 entries and 1 GiB,
  only files and directories, each file once, with its own modes (0755 when the zip marks it
  executable, 0644 otherwise), and removes the zip through the directory it opened it in
  (`BundleBlob`, `materialize_bundle_runtime_package`; FINDINGS A-84).

## LLM, key and usage records

- **Provider**: `ai.default_provider` of the hive's `ai` settings (`architect_hive_ai`): the
  installation file once the hive holds one, hive.yaml's `ai` section while it holds none
  ([hive-config-update-v1](../hive-config-update-v1.md) §4); with neither, `openai`
  (`HiveAiConfig::fallback`). The start logs which, with the file's version and hash
  (`log_architect_ai_source`), and `CONFIG_GET`'s `ai_provider.settings` says it too.
- **Settings it cannot use** (an installation file no reader can use, never a fallback to
  hive.yaml; or an invalid hive.yaml `ai` while the hive holds no file): the AI stays off, the
  start logs the reason as an error, the chat answers with it
  (`architect_ai_unavailable_reason`), and `CONFIG_GET` and `CONFIG_SET` fail with it.
- **Model**: Archi's own, `config.ai.model`, set by `CONFIG_SET` and kept in its node directory's
  `config.json`; `null` goes back to the installation's, `ai.providers.<provider>.model`. The AI
  runtime is rebuilt at once, with no restart. `CONFIG_GET` shows the model and its source
  (`ai.model` or `hive`). Its provider is always the installation's default, so SY.admin's
  `set_installation_ai` refuses to switch the default provider while Archi names its own model
  (A-11, `NODES_LEFT_WITHOUT_ENGINE`): set it to `null` first, and again once the switch is done.
- **A new version of the installation file** is taken at the next chat turn, never by a restart:
  no config update restarts Archi, which may be the caller of the very change, and a restart would
  cut its reply and the chat ([hive-config-update-v1](../hive-config-update-v1.md) §4, "As built
  (stage 3c)"). Each verdict records the stamp of the file it read, the sha256 of its bytes
  (`ai_settings::InstallationBuiltFrom`), at the start and at every rebuild; each turn reads that
  one small file and hashes it (`architect_takes_new_installation_ai`). A changed file whose
  engine for Archi (the default provider, and its own `config.ai.model` or else the default's
  model, `architect_engine_of`) is the one the runtime runs keeps the runtime, with no key read;
  only another engine rebuilds it (`architect_keeps_its_runtime`, `refresh_architect_ai_runtime`,
  its key read once from the vault, `ai_settings::provider_key`), and the log then says once where
  its `ai` comes from. A vault that did not answer is no verdict: the runtime it had stays, no
  stamp is recorded and the next turn asks again. A key the vault does not hold for the new
  provider, or an `ai` no reader can use, takes the AI down with the reason, as at start (stage
  3c's review, F1). Its own `config.ai.model` stays its node config.
- **Limits**: every model call asks for up to 32,000 output tokens and may take 600 s
  (`ARCHITECT_MODEL_CALL_TIMEOUT`); a pipeline task's budget is 1,000,000 tokens
  (`TASK_AGENT_TOKEN_BUDGET`).
- **Key**: from SY.vault, read as Archi, in the root tenant: the key Archi owns, else one assigned
  to it, else the tenant's general one (8.x holds `anthropic_architect`, Archi's own).
  `VAULT_SECRET_CHANGED` for that type, or for `postgres`, rebuilds the AI runtime or the messages
  connection. `CONFIG_SET` refuses `ai_providers`, `api_key`, `api_key_ref`, the messages URL
  fields (`reject_architect_secret_fields`), and any key under `ai` but `model`.
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
  the next step; 90 s more for each workflow deploy or removal; 240 s more for each
  `set_installation_ai` step (`EXECUTOR_INSTALLATION_WAIT_SECS`), past SY.admin's 210 s ceiling for
  it; 580 s more for each `update_installation` step (`EXECUTOR_INSTALLATION_UPDATE_WAIT_SECS`),
  past SY.admin's 550 s ceiling for it, and then up to 600 s following the update to its end; 35 s
  for `get_installation`, past SY.admin's own 30 s read
  of `/versions`; 30 s default
  (`JSR_ADMIN_ORCH_TIMEOUT_SECS`) (`architect_admin_action_timeout`).
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
