# SY.deploy

SY.deploy is the CD node: it deploys user solutions from their sources into groupings, step by
step through SY.admin, and records what it did. In this stage it carries one source, the plain
Digital Executive (`plain-ed`), whose first deployment is an ED's birth. It keeps no ED data: what
an ED is lives in identity (each part's `works_for` and the person's document); SY.deploy's records
are its deployments.

Design: [digital-executive-v1-stage-2](../digital-executive-v1-stage-2.md) §2 to §5 and §9, and
[digital-executive-v1-design](../digital-executive-v1-design.md) §5. Source:
`src/bin/sy_deploy.rs` and `src/bin/sy_deploy/` (the module of each part is named below). The
agent assets' code it shares with Archi: `src/agent_assets.rs`.

| | |
| --- | --- |
| Binary | `/usr/bin/sy-deploy` (`packaging/build-deb.sh`, `RUST_BINS` entry `sy-deploy:sy_deploy`) |
| Node name | `SY.deploy@motherbee`, its system ILK seeded by SY.identity from the release's list |
| Role | motherbee only |
| systemd unit | `sy-deploy.service` (`Restart=always`, `RestartSec=5`, `ExecCondition` on `role: motherbee`) |
| Listens on | the local router socket only; no TCP port |
| State | `/var/lib/fluxbee/state/sy-deploy/`, lock `/var/run/fluxbee/sy-deploy.lock` |

## Where it runs, and how it starts

`SY.deploy` is in the motherbee's list of the release (`src/release_settings.rs`), after
SY.wf-rules, in no `wait_for`. Like SY.admin, it exits at once on any other role (and refuses a
motherbee whose `hive_id` is not `motherbee`); its unit's `ExecCondition` keeps it from starting
there at all.

At its start it takes its instance lock, connects to the router as `SY.deploy`, and opens its
state (`Deployer::open`, `service.rs`): it reads every record, refreshes its catalog (below), and
queues every deployment it left `deploying` or `removing`, which its runner resumes.

## The orders

SY.admin forwards each `cd_*` action to `SY.deploy@motherbee` (`send_l2_action_request`): a write
as a `command`, a read as a `query`, and waits 10 s. SY.deploy takes orders from
`SY.admin@motherbee` alone (the router stamps the sender); any other node is answered
`UNAUTHORIZED`. Every order is answered before any step: the writes one at a time, in the order
they come, and each read in a task of its own. The answers are SY.wf-rules' envelope:
`{"status":"ok", ...}` or `{"status":"error","error_code":"...","message":"..."}`, with
`retryable: true` on `BUSY` and `HIVE_UNAVAILABLE`.

| Action | Kind | Fields | What it answers |
| --- | --- | --- | --- |
| `cd_deploy` | command | `tenant_id`, `request_key`, `source` `{id, version?}`, `grouping` `{kind, id?}`, `blanks`, `hive?` | `{"accepted": true, "deployment_id", "grouping": {kind, id, short_id}, "source": {id, version, hash}, "state": "deploying"}`, which SY.admin answers 202 |
| `cd_remove` | command | exactly one of `deployment_id`, `grouping_id` (the ED's ILK or its short id); `tenant_id?` | `{"accepted": true, "deployment_id", "deployment_ids", "grouping", "state": "removing"}` |
| `cd_get` | query | `deployment_id`, `tenant_id?` | `{"deployment": <the record, each node with its status, and the last run's steps in progress>}` |
| `cd_list` | query | `tenant_id?`, `grouping_kind?`, `grouping_id?`, `limit?` (100, at most 200), `cursor?` | `{"deployments": [...], "count", "next_cursor"?}`, newest first, each node with its status |
| `cd_catalog` | query | `tenant_id?`, `kind?` | `{"sources": [{id, kind, title, description, founds, newest, versions: [{version, hash, blanks, parts, deployments}]}]}` |
| `cd_tree` | query | `tenant_id` | the tenant's tree (below) |

- **Who asked:** SY.admin adds `_caller` `{by, tenant_id}` to every order; no caller may send it.
  `by` goes into the record's history. With `_caller.tenant_id` set (Fluxbee Cloud's claim),
  nothing of another tenant is found: `cd_get`, `cd_remove`, `cd_tree` and `cd_deploy` answer
  `NOT_FOUND` for another tenant, also when the order's own `tenant_id` names another than the
  claimed one; `cd_list` lists only the claimed tenant, and nothing when the order names another.
  `cd_catalog` takes a `tenant_id` and lists the same for every tenant in this stage.
- **A null is an absent field,** at every depth (SY.admin drops the top-level ones).
- **A field an order does not take** is refused (`INVALID_REQUEST`), never dropped.
- **The codes:** `NOT_FOUND`, `REQUEST_KEY_REUSED`, `SOURCE_NOT_FOUND`, `INVALID_REQUEST`,
  `BLANK_INVALID`, `AI_KEY_MISSING`, `CREDENTIAL_MISSING`, `NUMBER_TAKEN`, `HIVE_UNAVAILABLE`
  (retryable), `BUSY` (retryable), `DEPLOYMENT_NOT_REMOVABLE`. A refusal names what is wrong,
  never a value.

## The model: one reader, one desired state, one step builder, one runner

SY.deploy is general from the first line (stage 2 §3): a new source is data; a new kind of part is
one more section in the desired state and in the step builder.

- **The source reader** (`source.rs`): the format every source compiles to, its checks
  (`parse_source`), and its blanks' checks (`check_blanks`). A source holds `blanks` (each with a
  name and a type that carries its check: `name`, `personality`, `whatsapp_channel`, `vault_key`,
  `tenant_ilk`, `value`), `founds` (the person it founds and its document, native sources only),
  `nodes` (each with its role, its kind `AI` or `IO`, its middle name part, its runtime, its
  `work_role`, its config, and for an agent its provider and its definition's assets) and `vault`
  (which blank's key goes to which node). Configs, documents and assets are templates:
  `{"$blank": "<blank>[.<field>]"}` (with `"optional": true`, a missing value drops its key) and
  `{"$ref": "<name>"}` for what SY.deploy makes concrete (`grouping.ilk_id`, `grouping.short_id`,
  `hive`, `tenant_id`, `nodes.<role>.node_name`, `nodes.<role>.ilk_id`,
  `assets.<role>.role|personality`). A source with a section SY.deploy cannot build yet (`workflows`,
  `routes`, `vpns`, `policy`, `hives`, `runtimes`) is refused before any step.
- **The desired state** (`desired.rs`): the source plus its blanks' values, with every name and id
  concrete. It is computed from the deployment's record (its ids, its hive, its pinned runtime
  versions) and the source's copy, so a restart computes the same. Each asset is made as Archi
  makes it (its type's builder, its checks, its canonical bytes and their SHA-256); the person's
  document's hash is the SHA-256 of its canonical bytes, as SY.identity takes it.
- **The step builder** (`steps.rs`, `build_steps`): from the desired state and what runs, the
  SY.admin actions still missing, in order, with their exact arguments; the record's step last.
- **The runner** (`runner.rs`): one run at a time, in the order they were taken, with a lock per
  grouping (a removal never crosses a deployment under way: `BUSY`).

## The plain ED's source

`src/bin/sy_deploy/sources/plain-ed-1.json`, built into the binary (`include_str!`), version `1`,
its hash `sha256:` of its canonical bytes. A new version is a Fluxbee release.

- **Its blanks:** `display_name` and `company_name` (names, at most 128 bytes); `personality`
  (`timezone`, `country_code`, `primary_language`, and optionally `region_code`,
  `additional_languages`, `description`, `biographical`, `narrative`); `whatsapp`
  (`phone_number_id`, digits; `waba_id`; `credential_key`, a key of `resource_type` `whatsapp` in
  the tenant).
- **What it founds:** a virtual person with the two names, and its document (`schema` 1,
  `display_name`, `company_name`, `short_id`, `personality` with its asset's `sha256:` hash and its
  system fields, `channels` `[{type: "whatsapp", address: <phone_number_id>, io_node}]`).
- **Its conscious** (`conscious`): `AI`, `ai.generic`, `work_role` `conscious`, provider
  `anthropic`, config `{"behavior": {"kind": "ai_chat", "model": "claude-sonnet-5-5"},
  "speaks_as": <the ED>}`; its definition: the native role ("Digital executive") and handbook
  ("Working with people on messaging channels"), the operator's texts, and the personality made
  from its blank under the ED's name.
- **Its channel** (`channel`): `IO`, middle part `wapp`, `io.wapp`, `work_role` `channel`, config
  `{"wapp": {"auth": {"type": "vault_ref", "resource_type": "whatsapp", "key": <credential_key>}},
  "io": {"phone_number_id", "waba_id", "dst_node": <the conscious>, "person_ilk": <the ED>}}`.
- **Its vault:** the credential key, to the channel only.

## A deployment: the checks, then the steps

`cd_deploy` checks, in this order, with no AI and before it records anything:

1. the tenant: a user tenant (never the root one), active in the identity SHM;
2. the request key: 1 to 64 of `[A-Za-z0-9._:-]`, new in the tenant, or the same order as before
   (its tenant, source, grouping, blanks and hive): the same deployment comes back; a `failed`
   one resumes where it stopped (a removal that failed answers `INVALID_REQUEST`: remove it again);
   another order under the key is `REQUEST_KEY_REUSED`;
3. the source: one in the catalog, at the version named or the newest (`SOURCE_NOT_FOUND`);
4. the grouping: of the source's kind; a new one only from a source that founds one, an existing
   one (of the tenant) only from a source that does not, and on its hive;
5. the blanks: exactly the source's, each by its type (`BLANK_INVALID`); a WhatsApp number no
   other deployment that is not removed holds (`NUMBER_TAKEN`);
6. the hive: the one named, or the live worker with the most room: connected in the hive registry,
   its router's link state fresh, and room for the nodes under the router's 1024 and the
   installation's 1024 remote nodes, counting the deployments under way (`HIVE_UNAVAILABLE`); each
   runtime's `current` version, pinned;
7. the keys, through SY.admin's `vault_list` (metadata only): for each agent, a general key (no
   owner, no assignment) of its provider in the tenant (`AI_KEY_MISSING`); each credential the
   blanks name, of its type, in the tenant (`CREDENTIAL_MISSING`);
8. the ids: the grouping's short id, the deployment's id, one ILK id for the person and one for
   each node, none held by an SHM entry or a record, nor the node names.

Then it writes the record (`deploying`) and the journal's first line (the ids), answers *accepted*,
and the runner takes the steps. For the plain ED, from nothing:

| # | Step | SY.admin action and arguments |
| --- | --- | --- |
| 1 | `create_virtual_person` | `create_virtual_person` (motherbee) `{tenant_id, display_name, company_name, ilk_id}` |
| 2 | `set_person_document` | `set_person_document` (motherbee) `{ilk_id, document}` |
| 3 | `write_assets:conscious` | none: the role, the handbook and the personality, written into `<blob>/agent-assets/<hash>.json` as Archi writes them |
| 4 | `run_node:conscious` | `run_node` (the hive) `{node_name: AI.<short_id>@<hive>, runtime: ai.generic, runtime_version, tenant_id, ilk_id, config}` |
| 5 | `set_ilk_works_for:conscious` | `set_ilk_works_for` (motherbee) `{ilk_id, works_for: <the ED>, work_role: conscious}` |
| 6 | `set_ilk_definition:conscious` | `set_ilk_definition` (motherbee) `{ilk_id, definition: {role_hash, skill_hashes: [], handbook_hashes, personality_hash}}` |
| 7 | `run_node:channel` | `run_node` (the hive) `{node_name: IO.wapp.<short_id>@<hive>, runtime: io.wapp, ...}` |
| 8 | `set_ilk_works_for:channel` | `set_ilk_works_for` (motherbee) `{ilk_id, works_for: <the ED>, work_role: channel}` |
| 9 | `vault_assign:channel` | `vault_assign` (motherbee) `{key, tenant_id, assigned_ilks: [<the IO's ILK>]}` |
| 10 | `record_active` | none: the record goes `active` |

Each call waits 40 s (`kill_node` 190 s), longer than SY.admin's own wait on the orchestrator and
on SY.identity, so SY.deploy never gives up while SY.admin's call still runs.

### A retry converges, it does not replay

Every try first reads what runs (`observe`): the person's ILK and its document's hash, each node's
ILK with its `works_for`, `work_role` and definition's hashes, in the identity SHM; the assets on
disk; each node through `get_node_status` (`NODE_NOT_FOUND`: absent; `STOPPED`: started with
`start_node`, never launched again); each key's assignment through `vault_list`. It builds the
steps from that and sends only what is missing; a `run_node` answered `NODE_ALREADY_EXISTS` is
done. So a restart of SY.deploy in the middle, a step with no answer, and a `cd_deploy` that
resumes a failed one, all converge on one ED and one ILK per node.

A step with no answer, or one its answer says may pass again (`payload.retryable`, or `BUSY`,
`NOT_TAKEN`, `TIMEOUT`, `UNREACHABLE`, `DB_NOT_READY`, `DOCUMENT_WRITE_FAILED`,
`IDENTITY_UNAVAILABLE`, `RUNTIME_NOT_PRESENT`, `RUNTIME_MANIFEST_BUSY`, `NODE_STATUS_GET_FAILED`
...), is tried again after 3, 10 and 30 s, each try reading the state first. A step that fails for
good, or still fails after the last try, leaves the deployment `failed` with
`failure: {operation, step, error_code, error_detail, at_ms}`. Nothing is undone by itself.

## The end: `cd_remove`

- **A grouping** (`grouping_id`): its deployments that are not removed, the newest first; each one's
  nodes in the source's reverse order (for the plain ED the IO first, so nothing more comes in,
  then the conscious), each by `kill_node` `{node_name, retire_instance: true}` on its hive; then
  `delete_ilk` `{ilk_id, tenant_id}` on the ED, which marks it; then the records go `removed`.
- **One deployment** (`deployment_id`): its nodes alone. The founding deployment alone is refused
  while the grouping has others (`DEPLOYMENT_NOT_REMOVABLE`); alone in its grouping, it ends the
  grouping as above.
- **A retire's mark:** the orchestrator removes a node's map entry before it marks its ILK, so a
  retire whose answer says the mark failed or is unknown (`ilk_retired` not `true`) is followed by
  SY.deploy's own `delete_ilk` on the ILK it recorded. A node already gone (`NODE_NOT_FOUND`) has
  its ILK marked the same way while the SHM still holds it; an ILK already marked or gone
  (`ILK_NOT_FOUND`) is done.
- A removal is `BUSY` while its grouping has a run under way. One that fails leaves its records
  `failed` (`operation: remove`), and `cd_remove` again resumes it.
- **What stays:** the ILKs, marked, with their relations; the ED's document; the credential, for
  Cloud to delete; pages until their expiry. Purging stays the operator's.

## Its state

Under `/var/lib/fluxbee/state/sy-deploy/` (`store.rs`), with no secret in any file, only key names:

- `deployments/<deployment_id>.json`: the record, written to a temporary file, synced and renamed:
  `deployment_id`, `grouping` `{kind, id, short_id}`, `tenant_id`, `request_key`, `source`
  `{id, version, hash}`, `founding`, `blanks`, `hive`, `nodes` (`role`, `node_name`, `ilk_id`,
  `runtime`, `runtime_version`, `work_role`), `assets` (each agent's hashes), `vault` (`key`,
  `resource_type`, `assigned_to`), `state`, `failure`, `removal`, `requested_by`, the times and the
  `history` (`at_ms`, `what`, `by`).
- `journal/<deployment_id>.jsonl`, appended and synced: `accepted` (with the ids, before step 1);
  each run (`run`, with its plan); each step's `start` (its action, target and arguments) and its
  end (`done`, `retry` or `failed`, with `duration_ms`); each state. `cd_get`'s `progress` is the
  last run's steps from it, the gate's timings.
- `sources/<source_id>/<version>.json`: the catalog's copies, immutable. At each start, and after
  each removal, SY.deploy keeps the last three versions of each source and every version a
  deployment that is not removed runs (§3.3), writes the native sources among them, and removes
  the copies beyond them. A copy that differs from the native source of its version is kept as it
  is, and named in the log.

The motherbee's snapshot is the backup. The factory reset removes `deployments/` and `journal/`
with SY.deploy held down, and keeps `sources/`.

## Ids and names

- A grouping's short id: its kind's two letters (`ed`; `in` and `ts` kept for infra and test), a
  digit from 2 to 7, and 7 of `[a-z2-7]`: `ed4qm7k2xa`. A deployment's id: `dp` and the same form.
- Node names: `<KIND>.<middle.>?<short_id>@<hive>`: `AI.ed4qm7k2xa@worker1`,
  `IO.wapp.ed4qm7k2xa@worker1` (io.wapp's fan-out pattern `IO.wapp.*@<hive>`).
- ILK ids: `ilk:<uuid>`, minted before the record, passed to `create_virtual_person` and
  `run_node`.

## The tree

`cd_tree` (`tree.rs`) reads, when asked: the tenant's deployments of kind `infra` (none yet); its
EDs, the tenant's virtual persons in the identity SHM, each with its document read from
`<blob>/people/person-<uuid>_<hash16>.json` (`document_state`: `ok`, `not_yet` while the file has
not arrived, `none`, `unreadable`); each ED's parts, the ILKs whose `works_for` names it, with their
role, node and status; and each ED's deployments, with their source, version, state, each agent's
assets and each IO's credential key names.

## What it reads on the motherbee

The identity SHM (the SDK's reader, `list_ilks_from_hive_id`, `tenant_status_in_hive_id`); the
hive registry (`/var/lib/fluxbee/hives`) and the router's link state (`/jsr-lsa-motherbee`) for the
workers' room; the runtimes manifest for each runtime's `current`; `<blob>/agent-assets/` and
`<blob>/people/`, `<blob>` being hive.yaml's `blob.path` (`/var/lib/fluxbee/blob` by default).
Everything else goes through SY.admin, whose row for `SY.deploy@motherbee` admits a node's whole
life (`run_node`, `start_node`, `restart_node`, `kill_node` plain or retiring, `get_node_status`,
`get_node_config`, `set_node_config`, `node_control_config_set`), a deployment's people
(`create_virtual_person`, `set_person_document`, `set_ilk_works_for`, `set_ilk_definition`,
`delete_ilk`, `restore_ilk`) and its tenant's keys (`vault_list`, `vault_assign`), and nothing
else.

## Operations

- Logs: `journalctl -u sy-deploy`; each order answered is one line (`order answered`, its action,
  status and code); each failure names its deployment, step and code.
- A package upgrade stops it with SY.admin (`deb-prerm`), so a deployment under way resumes when it
  starts again.
- Left for later (stage 2 §12): Archi's published sources (`cd_publish`), upgrades and waves,
  groupings of kind infra and test, the sections for WFs, routes, VPN rules and the user policy,
  and SY.deploy's bounds in SY.admin.
