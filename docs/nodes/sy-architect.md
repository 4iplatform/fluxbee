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
   compiler prepared, sent with labels `{"origin": "plan_compiler_confirmed"}`); once such a plan
   ran, the chat says what each step did ([What the chat says after a confirmed
   plan](#what-the-chat-says-after-a-confirmed-plan)). `CANCEL` or `ABORT` drops it; a dropped
   plan answers "Plan discarded." in mode `chat`, as a dropped SCMD does (since 0.1.137; before,
   "Programmer plan discarded." in mode `executor`, which the UI drew as a run marked "done").
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
(`handle_local_architect_scmd`). `SCMD: help` lists the forms; its `add_hive` example names the
fields SY.admin requires, `hive_id`, `address` and `ssh_user` (FINDINGS A-321, since 0.1.139: it
had no `ssh_user`, so a copied example was refused `INVALID_REQUEST`).

Each segment of the path is percent-decoded once, after the path is split, as SY.admin's HTTP
routes decode theirs (`scmd_path_parts`, FINDINGS A-287), and Archi's own two paths are matched
the same way (`local_architect_scmd`: `/architect/control/config%2Dget` is config-get): in
`/hives/worker1/nodes/AI.sales%40worker1/status` the node is `AI.sales@worker1`, a `%2F` stays
inside its segment (one route prefix, never two segments), and the vault key of
`/hives/{hive}/vault/secrets/{key}/metadata` is decoded once, never twice. A `%` without two hex
digits after it goes as it came, as in the query: no node name holds a `%`, so the node's hive
refuses it (`invalid node_name`). A `+` stays a `+`, as in the query, where SY.admin's routes read
one in a path segment as a space; no node, runtime, hive or vault key name holds either. Before
0.1.137 each segment went as it came, but the vault key's: a node name written `%40` matched no
node through Archi, and worked over SY.admin's route.

An SCMD typed by the operator runs immediately; a mutating action is recorded in the
`operations` table (`handle_scmd`, `execute_tracked_admin_translation`).
`DELETE /hives/{hive}/taps` names its tap (`match_src`, `match_dst`, `target`) in the body or, as
SY.admin's route and its `example_scmd` do, in the query, each value decoded once and read when
the body names none of the three (`scmd_tap_key_params`, the review of FINDINGS A-288). Before
0.1.133 the query went unread: an SCMD that named the tap there reached SY.config.routes as `{}`,
refused with `NOT_FOUND`, and the tap stayed.

A recorded mutation whose answer did not come within Archi's wait stays open (`timeout_unknown`),
as does an `add_hive` whose join runs in the background (`accepted`), and while it is, the same
action with the same parameters in the same chat is refused `OPERATION_ALREADY_TRACKED`. Such a
request first reads an open `add_hive` in the motherbee's hive registry, `get_hive` for its hive
(`reconcile_timeout_unknown_operation`): a hive `connected` with its join `done` closes the record
as `succeeded_after_timeout`, one `failed` or `interrupted` as `failed` with its join's error, and
`NOT_FOUND` as `failed`, its summary saying the registry does not have the hive (removed, or never
created; FINDINGS A-321, since 0.1.139, where it said the hive no longer exists, of one a timed-out
`add_hive` usually never created). `INVALID_HIVE_ID` closes it as `failed` too:
the hive id is not one, and `add_hive` refuses it, so the request goes on and gets `add_hive`'s
own refusal (FINDINGS A-311). That refusal is `INVALID_HIVE_ID` once the request's other fields
are valid, since `add_hive` checks its SSH fields and its role first: one without `ssh_user` is
refused `INVALID_REQUEST` for that. `get_hive` refuses such an id since 0.1.137, where it answered
`NOT_FOUND`; read as any other answer, the record would stay open for good. A join still running,
or any other answer, leaves the record open.

Any other recorded mutation that timed out is read in SY.admin's command log (FINDINGS A-320, since
0.1.139; `logged_outcome_of_operation`), which keeps each command's outcome once SY.admin ends it,
whether Archi still waited or not: `list_recent_commands` for the record's action on its hive since
5 s before its dispatch, 200 a page, up to 5 pages. The record's entry is the one this Archi sent
(`origin` `l2:SY.architect@<hive>`) for that hive, started no earlier than 5 s before the dispatch,
whose params hold each plain value the record's do (a value either side keeps redacted, and params
SY.admin cut, are not compared), the one that started nearest the dispatch when several could be
(`command_log_entry_is_records`). Its `ok` closes the record as `succeeded_after_timeout` and its
`error` as `failed`, the summary naming SY.admin's code and how long it took (the log keeps no
detail), and the request goes on. With no entry yet the record stays open, and the request is
refused `OPERATION_ALREADY_TRACKED` (*"timed out locally and has no outcome yet: it may still be
running"*); with none 10 minutes after its dispatch, the whole log read, it closes as `failed`, its
outcome unknown: SY.admin never ended it (a restart, or the request lost), or its entry rotated
out, so the state is read before asking again. The log keeps about the last 400 commands SY.admin
ran, reads included (its active file and 3 rotations of 100 entries; `list_recent_commands`'
help), so on a busy installation, or for a record left open before the upgrade, the entry may be
gone, and the summary says both. A log that cannot be read leaves it open. The log keeps every
recorded mutation SY.admin runs as a command (`action_in_command_log`): a hive's removal, a
node's run, start, restart, kill or removal, a runtime's publish or version removal, a route, a VPN
rule, an ILK's or a tenant's change, a node's config, a message to a node, the storage, a WF
instance's cancel. `update`, `sync_hint` and the OPA and wf-rules writes go another way in SY.admin
and are not in it, so a timed-out one stays open in that chat, as before: Archi waits for each as
long as SY.admin does, or longer. An identical request there is refused with a text that says so
(`operation_error_message`, `open_operation_is_read`): the record cannot be settled in that chat;
read the state, and repeat it in a new chat. Before 0.1.139 only an `add_hive` was read, and a
`remove_hive` that outlasted Archi's 30 s, or a node's purge SY.admin waits 180 s for, stayed open
in its chat for good.

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
own HTTP routes decode theirs (`parse_query`): a cursor written with `%3A`, or an owner with `%40`,
reaches SY.admin as it does written raw; an escape that is none goes as it came, and a `+` stays a
`+` (`percent_decode`, FINDINGS A-286), so `?from=2026-11-01T00:00:00+01:00` is that time. Before
0.1.137 a `+` read as a space, and SY.storage refused that `from` as no RFC 3339 time. Archi's own
HTTP API (`/api/...`) reads a `+` in its query as a space (`query_param`). The path's description
tells the model to follow `next_cursor`, and on `REPLY_TOO_LARGE` to ask again with
`error_detail.suggested_limit` ([SY.admin](sy-admin.md#pages));
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
confirms. `GET /deployments` and `GET /drift-alerts` (FINDINGS A-288) read `?limit=&category=`,
the drift alerts `&severity=&kind=` too, as SY.admin's routes do (`scmd_history_params`: `limit`
only when it is a number, so the orchestrator's 50 holds otherwise), and `?hive=` reads that
hive's history (`get_deployments`, `get_drift_alerts`); `GET /hives/{hive}/deployments` and
`/hives/{hive}/drift-alerts` read the same filters. Before 0.1.133 all four dropped their query.
An empty filter is none, an empty or blank `?hive=` too: every hive's history is read, where
SY.admin's route reads the local hive's (`scmd_query_hive`). A key that comes twice with an empty
last value keeps the value before it, where SY.admin's routes take the empty one, no filter; and
a key is read as it came, where SY.admin's routes decode it. The read tool's key paths and the
chat's line for deployments and drift name the query: newest first, 50 without a `limit` and at
most 500 (the orchestrator's), so the model puts what the operator asks for, a severity or a
kind, in the query, where it read the newest 50 of every kind and missed an older match.
Archi's own read of a list, the motherbee's nodes its status shows, goes page after page with a
smaller page when SY.admin says how many rows fit (`admin_listing_every_page`); an answer that is
not ok is an error, never an empty list.

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

**A workflow's removal and a node's unanswered state** (FINDINGS A-285, 0.1.133): the chat's
prompt and the plan compiler's facts say that `wf_rules_delete` without `force` is refused while
instances run (`INSTANCES_ACTIVE`) and while the WF node is not `RUNNING` or cannot count them
(`INSTANCES_UNKNOWN`, naming why; a node that is not `RUNNING` keeps its instances, their number
unknown, and `force` ends them only once the operator accepts it), and that a config read of the
node with no answer refuses it with `force` too (`ORCHESTRATOR_ERROR`, saying nothing was
changed: ask again). An `ORCHESTRATOR_ERROR` from the node's kill is another matter: SY.wf-rules
waits for `kill_node` a bounded time while the orchestrator goes on, so the node may have been
stopped or removed, and the workflow's status (`GET /hives/{hive}/wf-rules/status?workflow_name=`,
`wf_rules_get_status`) is read before the delete is asked again, as for any `ORCHESTRATOR_ERROR`
that does not say nothing was changed ([SY.wf-rules](sy-wf-rules.md#delete)). Both texts say that
a node whose `lifecycle_state` is `UNKNOWN` may run, its hive's systemd gave no full answer for
its unit, so it is read again and never taken for stopped, while a managed node with neither
config nor state reads `UNKNOWN` every time, and that `SERVICE_FAILED` with `retryable: true` from
`run_node`, `start_node`, `kill_node` or `remove_node_instance` did nothing and can be asked again
([SY.orchestrator](sy-orchestrator.md)).

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

### What the chat says after a confirmed plan

Since 0.1.137 (FINDINGS A-322), once a plan the plan compiler prepared ran on `CONFIRM`, and after
its verdict, Archi's model gets one turn with each step's answer and tells the operator what
happened (`explain_executed_plan`, `src/bin/sy_architect/execution_report.rs`). Before, the chat
showed only SY.admin's fixed line, "Execution completed successfully" or "Execution stopped at
step 's1'": a refusal's code and whether asking again may pass went unsaid, and a workflow's delete
that answered ok with versions left read as a full success.

- **The turn**: one call of Archi's model, with no tool, its prompt and handbook as in the chat,
  the session's summary and recent turns, and a message Archi writes: what to report (what each
  step did, what is done, what stayed and why, a refusal's code, its detail and whether asking
  again may pass, the pages left up and until when, the warning, the steps that did not run, and
  what each step's status and the followed update mean), in the operator's language and as the
  handbook says, then the run and each step as JSON. Its text becomes the run's `message`: the
  chat shows it under the plan's head, in place of the fixed line, and the next turns read it as
  Archi's own words. One model call per plan run, recorded with the stage `execution_report`,
  and bounded at 90 s (`EXPLANATION_TURN_TIMEOUT`).
- **Its cost**: Archi's prompt and handbook (about 40,000 tokens), the session's turns and the
  report, and a short answer. The turn sends no tools, and Anthropic caches a prompt as tools,
  then system, then messages, so it never reads what a chat turn cached: its prompt is billed in
  full, as a cache write, unless another such turn ran within the cache's lifetime.
- **What the turn is given**: an allowlist, never a step's answer copied (FINDINGS A-124). Of the
  run, its status, its steps done of all, the step it stopped at, its answer's code
  (`EXECUTION_FAILED`, `EXECUTOR_UNAVAILABLE`...), whether Archi verified it and its line
  (`run_view`). Of each step, in the plan's order (`step_views`): its `id`, `action`, `status`
  (`done`, `failed`, `not_run`, or `not_reported` when the run's answer says nothing of its
  steps: too large for one mesh frame, or an executor that failed before its report, its earlier
  steps possibly run, each one's outcome in SY.admin's command log), what it acted on (only the
  args `hive`, `hive_id`, `target`, `node_name`, `runtime`, `runtime_version`, `version` and
  `workflow_name`), its answer's `status`, and its `error_code` and `error_detail`, cut to 300
  characters, for a step that failed or was refused: its answer's, else those of the event that
  failed it (a step that could not be dispatched, an `add_hive` whose join failed). Of its
  answer's payload: `retryable`; `unanswered_hives`, each `{hive, reason, next}`, and `{more}`
  counting the hives SY.admin cut from it to fit the mesh frame (since 0.1.139, FINDINGS A-328),
  which the fallback says as "<n> more"; `next`;
  `warning`, cut to 400 characters; `packages_left`, each `{runtime, version, error_code, detail,
  retryable, unanswered_hives}`, a `PURGE_STOPPED`'s without its detail; `pages_left_up` as
  `count`, its `error` when SY.admin could not count them, `more` and each page's
  `publication_id` and expiry, nothing else of a page; `wf_node` as `{node_name, action,
  status}`; `pending` and `held` as hive names; `update` as `{id, state}`, or for an update Archi
  followed after the run (`follow_installation_updates`) as it found it: `{id, state,
  superseded_by, followed_s}`, with that view's `pending` and `held`; and its counts, the
  numbers, flags and list lengths of plain field names down to three levels, no text, no field
  whose name reads as a secret's and nothing under a field named `value` (a vault value is any
  JSON). The help the executor's model read on the way (`help_lookup`) is none of a step's, and
  an `add_hive`'s join, followed with `get_hive`, counts only for its status and failure. So
  `publish_cloud_endpoint`'s token, a page's link or key, a vault value, an API key, a password
  or a secret ref is in none of the fields the turn gets.
- **The texts it keeps** (a detail, a warning, a reason, `next`, a page ledger's error, the run's
  line) are cut and, as a backstop, scrubbed word by word (`scrub`): a link (`://`, `/public/`,
  `/e/`) becomes `[link removed]`; a channel id (`ich:<uuid>`), a credential written with its value
  (`token=abc`, `"api_key":"k"`) and a word holding 32 or more of a key's characters in a row
  (letters, digits, `-`, `_`, `+`, `/`, `=`, with a letter and a digit: a key, a JWT's part, a
  UUID) become `[removed]`; and the value after a credential's name and its joint (`password:
  x`, `"token": "x"`, `password = x`) or after `Bearer` (after `Authorization:`, the word after its
  scheme) becomes `[removed]`. A detail that is not a text is read as pretty JSON, so each of its
  values is a word. A short secret written any other way reads as a plain word: the allowlist is
  what keeps one out. A name keeps a long id, never a link, a channel id or a credential.
- **When there is no turn, or it fails** (no AI runtime, the session's turns cannot be read, the
  provider's error, no answer within 90 s, no text), the message is the fixed line as Archi had
  it (SY.admin's, or that the plan ran but is not done, with its reason) and one line per step
  under it, with its code, its detail and what it left: the versions with why, the pages left up,
  or that they were not counted and `/artifacts` lists them, the update's state (`fallback_text`),
  with no model.
- **The answer** carries `explanation`: its `source` (`model` or `fallback`), its `model_calls` (1,
  or 0 when no turn ran), the `steps` the turn was given, and its `tokens_used` or why it gave
  nothing (`error`). The turn's tokens are counted in the answer's `token_usage` too, as
  `execution_report` and in its `total`, which the executor card's chip shows.
- The pipeline's `CONFIRM 2` and a plan pasted in the chat or sent to `POST /api/executor/plan`
  keep their own answers, with no such turn.

### A secret an answer brings: shown once, kept masked

Since 0.1.139 (FINDINGS A-124, the operator's call of 2026-11-12: show it once, keep it masked).
`publish_cloud_endpoint` answers the Cloud's bearer once (its `token`), `externalize` an edge
channel's (`token`, or a fanout's `verify_token`) and `vault_get` a secret's `value`. Through Archi
such an answer comes back in a step's `result_preview` (`src/bin/sy_architect/shown_once.rs`):

- **The chat gets it once.** The answer the chat gets (`handle_chat_message`, `POST
  /api/executor/plan`) carries the step's answer as it came and, apart, `output.shown_once`:
  `{note, secrets}`, each secret as `{where, field, secret}` (`step s1 (publish_cloud_endpoint)`,
  `token`, its value), found outside the operator's own input (a step's args, a plan). The chat
  shows them in a "Shown once: copy it now" block under the answer, and keeps that answer on
  screen: the periodic reload of the chat's history does not replace it until the operator sends
  again or opens another chat (`shownOnceHold`). The reload starts nothing while a message is in
  flight, and one already under way when the answer came checks the hold after its last read,
  right before it redraws (`loadSession`'s `unlessHeld`).
- **What Archi keeps is masked** (`shown_once::kept`): the chat's `messages` row of that answer
  (`kept_response_row`: its `metadata_json` and its `content`, and so the session's preview), and
  a pipeline run's `execution_report` in `pipeline_runs` (each way a run's state is updated
  merges through `pipeline_run_with_state_update`, CONFIRM 2's `advance_pipeline_run` included),
  have every field whose name reads as a secret's (`key_looks_secret`) and a vault-shaped
  object's `value` replaced by `***REDACTED***` (one that holds nothing, a public channel's null
  token, stays), and each such value of 8 characters or more masked wherever a text repeats it.
  So the token is in no row, no snapshot of Archi's directory, and nothing a later turn reads (its
  memory is made from the rows); the history shows the block as "Shown once (kept masked)". A
  run's plan is kept as it came: `CONFIRM 2` runs it.
- **No model gets it.** SY.admin's executor runs a step with nothing to fill as the plan wrote it,
  with no model (since 0.1.95); a step with `executor_fill` goes through its model, whose prompt
  holds the step with its `$steps.<id>...` references resolved, and which gets the step's answer
  back. So a step of `publish_cloud_endpoint`, `externalize` or `vault_get` with `executor_fill`
  is refused, and so is a step with `executor_fill` whose args take such a step's answer (a
  `vault_put` of `$steps.s1.payload.token`, say): by the plan compiler's prevalidation, by the
  precheck of a plan pasted or sent to `POST /api/executor/plan`, and before any plan runs
  (`refusal_of_a_plan_whose_secret_would_reach_a_model`, "executor plan validation failed: step s1
  (publish_cloud_endpoint) answers a secret..." or "step s2 (vault_put) takes the answer of step s1
  (publish_cloud_endpoint)..."). The explanation turn (above) is given no secret.
- **What the operator writes into a plan reaches a model.** SY.admin's plan review
  (`executor_validate_plan`, which Archi sends every plan to before it runs) gives its model each
  step as written, args included, when SY.admin's executor has an AI: one call per step, stage
  `executor_review`. A secret the operator writes into a step's args (a `vault_put`'s value, an
  `add_hive`'s SSH password) reaches the hive's AI provider there (FINDINGS A-337, SY.admin's to
  fix). A secret an answer brings does not: the review runs before any step, on the references as
  written.
- **What the operator sent is kept masked too**: an executor plan pasted in the chat, or sent to
  `POST /api/executor/plan`, with its steps' secret args masked (a vault value, an SSH password or
  key), as an SCMD's body already was (`sanitize_user_message_for_persistence`); the command line
  of an SCMD mutation in the `operations` table (`preview_command`, and `confirmed_command` in its
  answer) too, where an `add_hive`'s `ssh_password` stayed in clear. Archi's secret names are
  SY.admin's run log's (`redaction_field_is_secret`): `authorization`, `ssh_key`, `private_key`
  and `*_private_key` too, so an `add_hive`'s `ssh_key` is no longer kept in clear in the
  operation's `params_json`.
- **Archi's log** (`journalctl -u sy-architect`): each admin action Archi sends, and its answer, is
  logged at info level with its secrets masked as Archi keeps an answer (`admin_params_for_log`,
  `admin_payload_for_log`, through `shown_once::kept`): a `vault_get`'s value whatever it holds, a
  `vault_put` step's. Before the review only fields named as secrets were, so a vault value that is
  a plain text, or whose fields are not named so (a postgres URL), went to the journal in clear.
  SY.admin's command log keeps no answer.

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
  with a `next_hop_hive`, or DROP, case-sensitive; one route per prefix on a hive). So is a hive id
  SY.admin would refuse (`refuse_manifest_hive_id`, FINDINGS A-321, since 0.1.139): each hive the
  manifest names, a hive its topology adds, a VPN rule's, a node's, a route's and its next hop's,
  and a workflow's, must be 1 to 64 letters, digits, `_` or `-` as written (no `.`, `/` or space).
  The error names the field and the id, and says no hive has such an id. Before, a hive `a.b` went
  on into the plan and failed only at execution, once the operator had confirmed it (`add_hive`
  refused it `INVALID_HIVE_ID`; a node, a route, a VPN rule or a workflow on it failed its own
  way).
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
  its instances run, or that they were not counted, as for a WF node that is not `RUNNING`, whose
  removal ends the instances it keeps all the same (FINDINGS A-285); `add_route` and `add_vpn`
  exactly the declared route or VPN rule, and the `delete_route` / `delete_vpn` of a replace
  exactly its prefix or pattern. A pinned step loses its `executor_fill`, so SY.admin's executor
  cannot add a field back.
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
copied it (`turns.message`): the sender, the size and the detail come from it. Fluxbee Cloud's
traffic is not there: the router copies no message IO.cloud sends nor one addressed to it by name
(FINDINGS A-349, [rt-gateway](rt-gateway.md)); rows stored before stay. A message copied
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
  (`handle_package_publish_request`); `set_current` is true unless the request says false. That
  is all it sends: Archi recreates no node (FINDINGS A-298). Its answer is SY.admin's
  (`status`, `payload`, `error`) with `next`, what follows a publish that installed its version,
  for the operator to read. It is there whenever SY.admin's `payload` names that version
  (`runtime_version`), a follow-up of `sync_to` or `update_to` that failed included: the status
  is then `error`, and the version is installed, current with `set_current`.
  - **With `set_current`:** each hive's orchestrator moves its running nodes of that runtime that
    follow `current` onto the new version within a minute of the version reaching that hive (its
    reconcile, FINDINGS A-109), in place: each keeps its ILK, its secrets, its config and its
    `launched_by`. A package built on that runtime (`config_only`, `workflow`) runs its `current`
    (`resolve_runtime_spawn_entrypoint`), so the reconcile relaunches its running nodes onto the
    new version too, pinned to the package's version or not, in place (`running_node_move`: their
    unit no longer runs the target's start script). A node of that runtime pinned to a version,
    and a stopped node, stay as they are; a stopped node that follows `current`, or of a package
    built on that runtime, runs the new version when it is started (`start_node` resolves what
    it runs again).
  - **Without it:** this publish moves no node, whether or not the version was current already (a
    version published again); a node launched with that `runtime_version` runs it.
  - **A node's config is its own,** written at its launch from the template of the version it was
    launched on (`config/default-config.json`, or the package's `config_template`). A package's new
    template reaches only the nodes launched after it: an `ai.generic` node reads its own
    `config.json` alone, so a config-only package's new template config never reaches a node
    launched before it. To give an existing node another config, change it with a config change of
    its own (`node_control_config_set`, the node's `CONFIG_SET`), which keeps its ILK and its
    secrets; Archi stages it as a plan the operator confirms. The new template's content is in the
    `.zip` that was uploaded (its `config/default-config.json`, or its `config_template`): no read
    shows a published version's template (`get_runtime` carries none, and SY.admin removes the
    uploaded bundle once it is installed).
  - **Up to 0.1.136** a publish with `set_current` then took every node of that runtime on the
    motherbee, by runtime name, running or stopped, pinned or not, and ran `kill_node` with `force`
    and `purge_instance` and then `run_node` on `current` (`redeploy_runtime_nodes_with_current`):
    each came back with a new ILK, without the secrets it owned, running even if it had been
    stopped, with the template config unless `preserve_node_config`, and with
    `launched_by: "archi"`, so a node the Cloud launched answered `NODE_NOT_FOUND` to the Cloud's
    start, stop and delete. The request's `preserve_node_config` and the answer's `redeploy` are
    gone with it.
- `POST /api/software/publish` publishes an uploaded `.zip`, or packages a single binary first,
  through the same publish (`handle_software_publish_request`): its answer carries that publish's
  answer, `next` with it, under `publish`. The **Publish Software** panel of the chat sidebar
  calls either; its **Promote To Current** box, unchecked when a file is chosen, is `set_current`,
  and the panel shows `next` under the published version.
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
  (`ARCHITECT_MODEL_CALL_TIMEOUT`), but the turn that explains a confirmed plan's run, which the
  chat waits for 90 s at most; a pipeline task's budget is 1,000,000 tokens
  (`TASK_AGENT_TOKEN_BUDGET`).
- **Key**: from SY.vault, read as Archi, in the root tenant: the key Archi owns, else one assigned
  to it, else the tenant's general one (8.x holds `anthropic_architect`, Archi's own).
  `VAULT_SECRET_CHANGED` for that type, or for `postgres`, rebuilds the AI runtime or the messages
  connection. `CONFIG_SET` refuses `ai_providers`, `api_key`, `api_key_ref`, the messages URL
  fields (`reject_architect_secret_fields`), and any key under `ai` but `model`.
- **Usage records**: every model call is charged to the root tenant (`AiAccount::root`, source
  `root`) with work `{session_id, pipeline_run_id, solution_id, stage}` (`usage_account`). The
  stage is `chat`, `designer`, `design_auditor`, `programmer`, `plan_compiler`,
  `failure_classifier` or `execution_report` (the turn that explains a confirmed plan's run, one
  per run, since 0.1.137). Records go to the router stream subject `storage.usage`.

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
  past SY.admin's 550 s ceiling for it, and then up to 600 s following the update to its end; 70 s
  more for each worker put behind the egress or taken back (`EXECUTOR_EGRESS_SWITCH_WAIT_SECS`);
  240 s more for each `publish_cloud_endpoint` step (`EXECUTOR_CLOUD_ENDPOINT_WAIT_SECS`, since
  0.1.139's integration review), past SY.admin's ceiling for a run, 225 s for a door that opens
  and 235 s for one that does not (its vault rollback, the operator's call of 2026-11-12), whose
  answer is the only place the operator gets the Cloud's token (FINDINGS A-124, A-317); 35 s for
  `get_installation`, past SY.admin's own 30 s read of `/versions`; 30 s default
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
