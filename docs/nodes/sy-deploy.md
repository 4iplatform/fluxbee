# SY.deploy

SY.deploy is the CD node: it deploys solutions from their packages into groupings, step by step
through SY.admin, and records what it did. It only deploys solutions: its catalog is what Archi
publishes to it (`cd_publish`), the packages Fluxbee ships (the plain Digital Executive, `plain-ed`,
among them) and the ones Archi makes. A package is of one of three kinds: an `executive`, whose
deployment is an ED's birth; a `capability`, agents behind an ED with a WF at its door; an
`infrastructure`, general for a tenant's EDs. It keeps no ED data: what an ED is lives in identity
(each part's `works_for` and the person's document); SY.deploy's records are its deployments.

Design: [digital-executive-v1-stage-2](../digital-executive-v1-stage-2.md) §2 to §5 and §9,
[digital-executive-v1-stage-3](../digital-executive-v1-stage-3.md) §1 to §4, and
[digital-executive-v1-design](../digital-executive-v1-design.md) §5. Source:
`src/bin/sy_deploy.rs` and `src/bin/sy_deploy/` (the module of each part is named below). The
package format, its checks and its templates are the core library's: `src/solution_package.rs`.
The agent assets' code it shares with Archi: `src/agent_assets.rs`.

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
state (`Deployer::open`, `service.rs`): it reads every record, refreshes its catalog from its own
copies and their publications (below), and queues every deployment it left `deploying` or
`removing`, which its runner resumes. It reads no package from the .deb itself: Archi publishes
them at its start. On a new installation it holds none until then, and answers `cd_deploy` with
`CATALOG_NOT_READY`; after a restart its copies are its catalog.

## The orders

SY.admin forwards each `cd_*` action to `SY.deploy@motherbee` (`send_l2_action_request`): a write
as a `command`, a read as a `query`, and waits 10 s. SY.deploy takes orders from
`SY.admin@motherbee` alone (the router stamps the sender); any other node is answered
`UNAUTHORIZED`. Every order is answered before any step: the writes one at a time, in the order
they come, and each read in a task of its own. The answers are SY.wf-rules' envelope:
`{"status":"ok", ...}` or `{"status":"error","error_code":"...","message":"..."}`, with
`retryable: true` on `BUSY`, `HIVE_UNAVAILABLE` and `CATALOG_NOT_READY`.

| Action | Kind | Fields | What it answers |
| --- | --- | --- | --- |
| `cd_deploy` | command | `tenant_id`, `request_key`, `source` `{id, version?}`, `grouping` `{kind, id?}`, `blanks`, `hive?` | `{"accepted": true, "deployment_id", "grouping": {kind, id, short_id, conscious?}, "source": {id, version, hash}, "state": "deploying"}`, which SY.admin answers 202 |
| `cd_remove` | command | exactly one of `deployment_id`, `grouping_id` (an ED's ILK, a tenant's id, or a grouping's short id); `tenant_id?` | `{"accepted": true, "deployment_id", "deployment_ids", "grouping", "state": "removing"}` |
| `cd_publish` | command | `package` (the package's JSON object) | `{"kept": "new"\|"already", "id", "version", "hash"}` |
| `cd_unpublish` | command | `id`, `version` | `{"id", "version", "published": false, "deployments"}` (how many deployments still run it) |
| `cd_get` | query | `deployment_id`, `tenant_id?` | `{"deployment": <the record, each node with its status, and the last try's steps in progress>}` |
| `cd_list` | query | `tenant_id?`, `grouping_kind?`, `grouping_id?`, `limit?` (100, at most 200), `cursor?` | `{"deployments": [...], "count", "next_cursor"?}`, newest first, each node with its status |
| `cd_catalog` | query | `tenant_id?`, `kind?`, `grouping_id?` | `{"sources": [{id, kind, origin, offered_to, deploys_into, title, description, founds, newest, blanks, versions: [{version, hash, blanks, parts, deployments}]}]}` |
| `cd_tree` | query | `tenant_id` | the tenant's tree (below) |

- **Who asked:** SY.admin adds `_caller` `{by, tenant_id}` to every order; no caller may send it.
  `by` goes into the record's history. With `_caller.tenant_id` set (Fluxbee Cloud's claim),
  nothing of another tenant is found: `cd_get`, `cd_remove`, `cd_tree` and `cd_deploy` answer
  `NOT_FOUND` for another tenant, also when the order's own `tenant_id` names another than the
  claimed one; `cd_list` and `cd_catalog` list only for the claimed tenant, and nothing when the
  order names another. A publish and a withdraw are Archi's and the operator's: with a claimed
  tenant they are refused (`INVALID_REQUEST`); SY.admin never forwards them from Cloud.
- **A null is an absent field,** at every depth (SY.admin drops the top-level ones).
- **A field an order does not take** is refused (`INVALID_REQUEST`), never dropped.
- **The codes:** `NOT_FOUND`, `REQUEST_KEY_REUSED`, `SOURCE_NOT_FOUND`, `INVALID_REQUEST`,
  `BLANK_INVALID`, `AI_KEY_MISSING`, `CREDENTIAL_MISSING`, `NUMBER_TAKEN`, `HIVE_UNAVAILABLE`
  (retryable), `BUSY` (retryable), `DEPLOYMENT_NOT_REMOVABLE`, `CATALOG_NOT_READY` (retryable),
  and a publish's `PACKAGE_INVALID`, `PACKAGE_CHANGED`, `PACKAGE_TOO_LARGE`. A refusal names what
  is wrong, never a value.

## The package

One format for every solution (stage 3 §2), read and checked by the core library
(`src/solution_package.rs`, `parse_package`), which SY.deploy reads every copy and every publish
through. Its sections:

- `schema` (1), `id` and `version` (each 1 to 64 of `[a-z0-9._-]`), `title`, `description`;
- `kind`: `executive`, `capability` or `infrastructure` (`ed`, stage 2's name of an executive, is
  read only in a copy SY.deploy kept before stage 3, below);
- `origin`: `system` (ships with Fluxbee), `catalog` (Archi's, for every tenant) or `custom`
  (Archi's, for one tenant); `offered_to`: `{"tenants": "all"}` or `{"tenants": ["tnt:..."]}` (a
  custom package names exactly one tenant), and for a capability `"eds": "all"` (the default) or
  `"eds": ["<short_id>", ...]`;
- `blanks`, each with a name and a type that carries its check: `name`, `personality`,
  `whatsapp_channel`, `vault_key`, `tenant_ilk`, `value`;
- `founds` (an executive only, and every executive): the person it founds and its document;
- `nodes`, each with its `role` (its name in the package), its `kind` (`AI`, `IO` or `WF`), its
  `middle` name part, its `work_role`, and:
  - an `AI` or an `IO`: its `runtime` and its `config`; an agent (`AI`) also its `provider` and,
    optionally, its `definition`'s assets (`role`, `skills`, `handbooks`, `personality`);
  - a `WF`: its `definition` alone, the WF definition; a `runtime` or a `config` refuses the
    package (SY.deploy deploys a WF with `wf_rules_compile_apply`, and SY.wf-rules runs it on
    `wf.engine`);
- `vault`: which blank's key goes to which node (an `AI` or an `IO`).

What its kind allows:

| Kind | Its nodes' work roles | Deployed into |
| --- | --- | --- |
| `executive` | exactly one `conscious` (an `AI`), and `channel`s (`IO`) | a new ED grouping |
| `capability` | `agent`s (`AI`), `connector`s (`IO`), and exactly one `capability`, its door, a `WF` | the ED's grouping, or for every ED its tenant's |
| `infrastructure` | `agent`s (`AI`), `connector`s (`IO`) | the tenant's grouping |

A capability's and an infrastructure's nodes each have a `middle` part, since they share their
grouping with other packages' nodes; a WF's starts with a letter. Two nodes of one kind never get
one name. A section SY.deploy cannot build yet (`workflows`, `routes`, `vpns`, `policy`, `hives`,
`runtimes`) refuses the package.

**The door** of a capability starts from its ED's conscious or from every conscious of its tenant,
nothing else (docs/wf-orchestration-v1.md §5.6): its definition's `start_from` is exactly
`[{"$ref": "grouping.conscious"}]`, a capability of one ED, deployed into that ED's grouping; or
`[{"consciouses": "tenant"}]`, written as it is, a capability of every ED of its tenant, deployed
into the tenant's grouping, which names no ED (no `grouping.ilk_id`, no `grouping.conscious`) and
is offered to all of them (no `eds` list).

**The templates.** Configs, the person and its document, assets, vault keys and WF definitions are
templates: `{"$blank": "<blank>[.<field>]"}` (with `"optional": true`, a missing value drops its
key) and `{"$ref": "<name>"}` for what SY.deploy makes concrete:

- the grouping's: `grouping.ilk_id` (the ED's ILK, in an ED's grouping), `grouping.short_id`,
  `grouping.tenant_id`, `grouping.hive`, and `grouping.conscious` (a capability of one ED: the node
  name of the ED's conscious, from the ED's founding deployment);
- each node's: `nodes.<role>.node_name`, and `nodes.<role>.ilk_id` of an `AI` or an `IO` (a WF's
  ILK is the orchestrator's, known only once it runs);
- each agent's assets: `assets.<role>.role|personality`.

A `$ref` the package does not have refuses it. **In a WF definition,** which uses `$ref` itself
(`{"$ref": "input.number"}`, `event.*`, `state.*`, `vars.*`), SY.deploy fills the `$blank`s and
only the `$ref`s whose path starts with `grouping.`, `nodes.` or `assets.`; every other `$ref`
stays exactly as written, for the WF engine, and so do a JSON Schema's `$` keys (`$schema`,
`$defs`).

## The catalog: Archi's publishes

`cd_publish` takes a released package (stage 3 §3): Archi publishes every package it has, the
shipped ones and its own, one by one at its start, through SY.admin (Archi alone on the mesh, and
the operator's HTTP, never Cloud).

- **The checks:** a package is at most 128 KiB (`PACKAGE_TOO_LARGE`); it reads and passes every
  check above (`PACKAGE_INVALID`, its message the reason).
- **Its copy:** a version new to SY.deploy is kept, written once (`kept: "new"`); the same content
  again is `kept: "already"`; the same id and version with another content is refused
  (`PACKAGE_CHANGED`): a change is a new version. The answer's `hash` is the copy's, `sha256:` of
  its canonical bytes, the one a deployment pins.
- **Its publication:** each publish writes the version's offer (`origin`, `offered_to`) and marks
  it published; `cd_unpublish` marks it not published. A version not published is not in the
  catalog and is deployed no more (`SOURCE_NOT_FOUND`); the deployments that run it keep it, and
  converge and are removed with it as before. A publish of it again offers it again.
- **Ready:** SY.deploy takes deployments while it holds a published version: until then
  `cd_deploy` answers `CATALOG_NOT_READY`, retryable.
- **A copy kept before stage 3** (8.x's `sources/plain-ed/1.json`, kind `ed`, no offer): it reads,
  serves the deployments that run it, and is offered by no publication until Archi publishes the
  shipped `plain-ed` 1, whose content is the same (kind `executive`, and its offer added). That
  publish is `kept: "already"`, with the copy's hash: the copy's bytes and the hash its deployments
  pin stay as they are, and the publish's offer becomes the version's.
- **What `cd_catalog` lists:** the published versions the caller's tenant is offered (`system` and
  `catalog` packages to the tenants their `offered_to` names, every one for `all`; a `custom` one to
  its tenant), by `kind` (`ed` read as `executive`); with a `grouping_id` (an ED's ILK or short id),
  the capabilities of one ED whose `eds` does not offer that ED are left out. Each package with its
  newest version's `kind`, `origin`, `offered_to`, `deploys_into` (`ed` or `tenant`), `blanks`, and
  each version with its hash, blanks, parts and how many deployments run it.

## The groupings

- **An ED's** (`ed`): its id is the ED's ILK (its person), founded by an executive's deployment;
  every node deployed into it works for the ED, in its role, and never `speaks_as` it.
- **A tenant's** (`tenant`, stage 3 §4): one per tenant, its id the tenant's id, its short id `tn`
  and the same form. The first deployment that needs it makes it: an infrastructure, or a
  capability of every ED. The next ones go into it while it has a deployment that is not removed;
  once none is left, the next one makes a new one. Only a capability's door carries a relation
  there: `works_for` nobody, `work_role` `capability` (identity takes that from a capability alone,
  0.1.162); its agents and connectors, and an infrastructure's nodes, carry none in this release.

## The model: one reader, one desired state, one step builder, one runner

SY.deploy is general from the first line (stage 2 §3): a new package is data; a new kind of part
is one more section in the desired state and in the step builder.

- **The package reader** is the core library's (above), with the blanks' checks (`check_blanks`).
- **The desired state** (`desired.rs`): the package plus its blanks' values, with every name and
  id concrete. It is computed from the deployment's record (its ids, its hive, its grouping and its
  conscious, its pinned runtime versions, a WF's ILK once learned) and the package's copy, so a
  restart computes the same. Each asset is made as Archi makes it (its type's builder, its checks,
  its canonical bytes and their SHA-256); the person's document's hash is the SHA-256 of its
  canonical bytes, as SY.identity takes it; each node has its relation (whom it works for, and as
  what) or none.
- **The step builder** (`steps.rs`, `build_steps`): from the desired state and what runs, the
  SY.admin actions still missing, in order, with their exact arguments; the record's step last.
- **The runner** (`runner.rs`): one run at a time, in the order they were taken, with a lock per
  grouping (a removal never crosses a deployment under way: `BUSY`).

## The plain ED's package

`packaging/solutions/plain-ed/1.json`, a `system` executive offered to every tenant, shipped by the
.deb at `/usr/share/fluxbee/solutions/plain-ed/1.json` and published by Archi at its start. A new
version is a Fluxbee release.

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
   (its tenant, package, grouping, blanks and hive): the same deployment comes back; a `failed`
   one resumes where it stopped (a removal that failed answers `INVALID_REQUEST`: remove it again);
   another order under the key is `REQUEST_KEY_REUSED`;
3. the catalog: a published version at all (`CATALOG_NOT_READY`);
4. the package: published, offered to the tenant, at the version named or the newest
   (`SOURCE_NOT_FOUND`);
5. the grouping, of the kind the package deploys into (`grouping.kind`, else `INVALID_REQUEST`):
   - an executive: a new ED grouping, named by no id;
   - a capability of one ED: the ED's (`grouping.id`, its ILK or its short id), founded by an
     active deployment of the tenant (`NOT_FOUND`; `INVALID_REQUEST` while it is not active),
     offered to that ED by its `eds` (`SOURCE_NOT_FOUND`), on the ED's hive;
   - an infrastructure or a capability of every ED: the tenant's (an id, when given, its tenant's
     id or its short id), or a new one;
   - a grouping with a run under way is `BUSY`; one that runs the package already is
     `INVALID_REQUEST`;
6. the blanks: exactly the package's, each by its type (`BLANK_INVALID`); a WhatsApp number no
   other deployment that is not removed holds (`NUMBER_TAKEN`);
7. the hive: the grouping's, or for a new one the one named or the live worker with the most room:
   connected in the hive registry, its router's link state fresh, and room for the nodes under the
   router's 1024 and the installation's 1024 remote nodes, counting the deployments under way
   (`HIVE_UNAVAILABLE`); each `AI`'s and `IO`'s runtime's `current` version, pinned;
8. the keys, through SY.admin's `vault_list` (metadata only): for each agent, a general key (no
   owner, no assignment) of its provider in the tenant (`AI_KEY_MISSING`); each credential the
   blanks name, of its type, in the tenant (`CREDENTIAL_MISSING`);
9. the ids: a new grouping's short id (and an ED's ILK), the deployment's id, one ILK id for each
   `AI` and `IO`, none held by an SHM entry or a record, nor the node names; in a grouping that is
   there already the names are fixed, and one that is taken (by a node of another deployment, or
   by one a removed deployment left) refuses the order (`INVALID_REQUEST`).

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

Each call waits 40 s (`kill_node` 190 s, `wf_rules_compile_apply` and `wf_rules_delete` 90 s),
longer than SY.admin's own wait on the orchestrator, on SY.identity and on SY.wf-rules (75 s), so
SY.deploy never gives up while SY.admin's call still runs.

### A capability's steps

Its nodes in the package's order, each as the plain ED's are (assets, launch, relation,
definition), and a `WF` by its own steps. A capability with its agent first, its connector, and its
door last (so the conscious finds the door only once the rest runs), into the ED `ed4qm7k2xa` on
`worker1`:

| # | Step | SY.admin action and arguments |
| --- | --- | --- |
| 1 | `write_assets:billing` | none: the agent's assets |
| 2 | `run_node:billing` | `run_node` `{node_name: AI.billing.ed4qm7k2xa@worker1, ...}` |
| 3 | `set_ilk_works_for:billing` | `{ilk_id, works_for: <the ED>, work_role: agent}` |
| 4 | `set_ilk_definition:billing` | its definition's hashes |
| 5 | `run_node:crm` | `run_node` `{node_name: IO.crm.ed4qm7k2xa@worker1, ...}` |
| 6 | `set_ilk_works_for:crm` | `{ilk_id, works_for: <the ED>, work_role: connector}` |
| 7 | `wf_rules_compile_apply:door` | `wf_rules_compile_apply` (the hive) `{workflow_name: billing.ed4qm7k2xa, definition, auto_spawn: true, tenant_id}`: SY.wf-rules makes `WF.billing.ed4qm7k2xa@worker1`, and the orchestrator its ILK |
| 8 | `read_node:door` | none: the nodes are read again (`get_node_status`), and the WF's ILK, from its status's `identity.ilk_id`, goes into the record (the journal's `learned`) |
| 9 | `set_ilk_works_for:door` | `{ilk_id: <the WF's>, works_for: <the ED>, work_role: capability}` |
| 10 | `vault_assign:crm` | the connector's key |
| 11 | `record_active` | none |

Its WF definition goes with its blanks and SY.deploy's refs in place: `start_from` the ED's
conscious (`AI.ed4qm7k2xa@worker1`), the agent's and the connector's names where it sends. The read
again (step 8) is part of the same try, and `cd_get`'s progress shows it with the rest. A WF that
runs and whose status names no ILK yet is tried again (`NODE_ILK_UNKNOWN`), and SY.wf-rules'
`WF_NODE_NOT_DEPLOYED` (its definition current, its node not running it) is retryable. In the
tenant's grouping the same capability's door goes with `works_for` null, and its agent and
connector with no relation step.

### A retry converges, it does not replay

Every try first reads what runs (`observe`): the person's ILK and its document's hash, each node's
ILK with its `works_for`, `work_role` and definition's hashes, in the identity SHM; the assets on
disk; each node through `get_node_status` (`NODE_NOT_FOUND`: absent; `STOPPED`: started with
`start_node`, never launched again; a WF's ILK from its status); each key's assignment through
`vault_list`. It builds the steps from that and sends only what is missing; a `run_node` answered
`NODE_ALREADY_EXISTS` is done, and a WF that runs is not compiled again. So a restart of SY.deploy
in the middle, a step with no answer, and a `cd_deploy` that resumes a failed one, all converge on
one ED and one ILK per node.

A step with no answer, or one its answer says may pass again (`payload.retryable`, or `BUSY`,
`NOT_TAKEN`, `TIMEOUT`, `UNREACHABLE`, `DB_NOT_READY`, `DOCUMENT_WRITE_FAILED`,
`IDENTITY_UNAVAILABLE`, `RUNTIME_NOT_PRESENT`, `RUNTIME_MANIFEST_BUSY`, `NODE_STATUS_GET_FAILED`,
`WF_NODE_NOT_DEPLOYED` ...), is tried again after 3, 10 and 30 s, each try reading the state first.
A step that fails for good (a WF definition SY.wf-rules refuses, `COMPILE_ERROR`, among them), or
still fails after the last try, leaves the deployment `failed` with
`failure: {operation, step, error_code, error_detail, at_ms}`. Nothing is undone by itself.

## The end: `cd_remove`

- **A grouping** (`grouping_id`): its deployments that are not removed, the newest first (an ED's
  capabilities before its executive); each one's nodes in the package's reverse order (a
  capability's door first, so nothing more comes in; the plain ED's IO before its conscious); a
  `WF` by `wf_rules_delete` `{workflow_name, force: true, retire: true}` on its hive (its running
  instances go with it; SY.wf-rules removes its node with the orchestrator's retire, its ILK marked
  `node_retired`, never purged; `WORKFLOW_NOT_FOUND` is done), any other node by `kill_node`
  `{node_name, retire_instance: true}`; then, for an ED, `delete_ilk` `{ilk_id, tenant_id}` on the
  ED, which marks it; then the records go `removed`. A tenant's grouping is named by the tenant's
  id or its short id, and has no person to mark.
- **One deployment** (`deployment_id`): its nodes alone. The founding deployment alone is refused
  while the grouping has others (`DEPLOYMENT_NOT_REMOVABLE`); alone in its grouping, it ends the
  grouping as above.
- **A node's mark:** the orchestrator removes a node's map entry before it marks its ILK, so a
  retire whose answer says the mark failed or is unknown (`ilk_retired` not `true`) is followed by
  SY.deploy's own `delete_ilk` on the ILK it recorded. A node already gone (`NODE_NOT_FOUND`), and a
  WF after its workflow's delete, have their ILK marked the same way while the SHM still holds it
  as active; an ILK already marked or gone (`ILK_NOT_FOUND`) is done.
- A removal is `BUSY` while its grouping has a run under way. One that fails leaves its records
  `failed` (`operation: remove`), and `cd_remove` again resumes it.
- **What stays:** the ILKs, marked, with their relations; the ED's document; the credentials, for
  Cloud to delete; pages until their expiry; the removed deployments' node names, so the same
  capability is not deployed again into the same ED in this release. Purging stays the operator's.

## Its state

Under `/var/lib/fluxbee/state/sy-deploy/` (`store.rs`), with no secret in any file, only key names:

- `deployments/<deployment_id>.json`: the record, written to a temporary file, synced and renamed:
  `deployment_id`, `grouping` `{kind, id, short_id, conscious?}`, `tenant_id`, `request_key`,
  `source` `{id, version, hash}`, `founding`, `blanks`, `hive`, `nodes` (`role`, `node_name`,
  `ilk_id`, `runtime`, `runtime_version`, `work_role`; a WF's ILK empty until learned, its runtime
  `wf.<workflow>`), `assets` (each agent's hashes), `vault` (`key`, `resource_type`,
  `assigned_to`), `state`, `failure`, `removal`, `requested_by`, the times and the `history`
  (`at_ms`, `what`, `by`).
- `journal/<deployment_id>.jsonl`, appended and synced: `accepted` (with the ids, before step 1);
  each run (`run`, with its plan; `continues: true` on a read again within a try); each step's
  `start` (its action, target and arguments) and its end (`done`, `retry` or `failed`, with
  `duration_ms`); a WF's ILK `learned`; each state. `cd_get`'s `progress` is the last try's steps
  from it, the gate's timings.
- `sources/<id>/<version>.json`: each package's copy, its canonical bytes, written once.
- `published/<id>/<version>.json`: each kept version's publication: `published`, `origin`,
  `offered_to`, `by`, `updated_at_ms`.
- At each start, after each publish and after each removal, SY.deploy keeps the last three
  versions of each package and every version a deployment that is not removed runs (§3.3), and
  removes the copies beyond them with their publications.

The motherbee's snapshot is the backup. The factory reset removes `deployments/` and `journal/`
with SY.deploy held down, and keeps `sources/` and `published/`, the installation's catalog, as it
keeps the system packages the .deb installs.

## Ids and names

- A grouping's short id: its kind's two letters (`ed` for an ED's, `tn` for a tenant's; `ts` kept
  for test), a digit from 2 to 7, and 7 of `[a-z2-7]`: `ed4qm7k2xa`, `tn5aaaaaaa`. A deployment's
  id: `dp` and the same form.
- Node names: `<KIND>.<middle.>?<short_id>@<hive>`: `AI.ed4qm7k2xa@worker1`,
  `IO.wapp.ed4qm7k2xa@worker1` (io.wapp's fan-out pattern `IO.wapp.*@<hive>`),
  `AI.billing.ed4qm7k2xa@worker1`. A WF's workflow is `<middle>.<short_id>`, unique per grouping
  (`billing.ed4qm7k2xa`, or `billing.tn5aaaaaaa` for every ED of a tenant), its node
  `WF.<workflow>@<hive>`.
- ILK ids: `ilk:<uuid>`, minted before the record for the person and each `AI` and `IO`, passed to
  `create_virtual_person` and `run_node`; a WF's is the orchestrator's.

## The tree

`cd_tree` (`tree.rs`) reads, when asked: the tenant's general infra, its tenant grouping's
deployments (its infrastructure and its capabilities of every ED), under `infra`; its EDs, the
tenant's virtual persons in the identity SHM, each with its document read from
`<blob>/people/person-<uuid>_<hash16>.json` (`document_state`: `ok`, `not_yet` while the file has
not arrived, `none`, `unreadable`); each ED's parts, the ILKs whose `works_for` names it (its
conscious, its channels, its capabilities' doors, agents and connectors), with their role, node and
status; and each ED's deployments (its executive's and each capability's), with their package,
version, state, each node's role and work role, each agent's assets and each IO's credential key
names.

## What it reads on the motherbee

The identity SHM (the SDK's reader, `list_ilks_from_hive_id`, `tenant_status_in_hive_id`); the
hive registry (`/var/lib/fluxbee/hives`) and the router's link state (`/jsr-lsa-motherbee`) for the
workers' room; the runtimes manifest for each runtime's `current`; `<blob>/agent-assets/` and
`<blob>/people/`, `<blob>` being hive.yaml's `blob.path` (`/var/lib/fluxbee/blob` by default).
Everything else goes through SY.admin, whose row for `SY.deploy@motherbee` admits a node's whole
life (`run_node`, `start_node`, `restart_node`, `kill_node` plain or retiring, `get_node_status`,
`get_node_config`, `set_node_config`, `node_control_config_set`), a WF's
(`wf_rules_compile_apply`, `wf_rules_delete`, `wf_rules_get_workflow`, `wf_rules_get_status`), a
deployment's people (`create_virtual_person`, `set_person_document`, `set_ilk_works_for`,
`set_ilk_definition`, `delete_ilk`, `restore_ilk`) and its tenant's keys (`vault_list`,
`vault_assign`), and nothing else.

## Operations

- Logs: `journalctl -u sy-deploy`; each order answered is one line (`order answered`, its action,
  status and code); each publish and withdraw one more (`a package was published`, its id, version,
  `kept` and who); each failure names its deployment, step and code.
- A package upgrade stops it with SY.admin (`deb-prerm`), so a deployment under way resumes when it
  starts again.
- Left for later (stage 3 §10): upgrades of packages and waves, the same capability deployed again
  into an ED after its removal (its node names stay held), relations for the agents and connectors
  of a capability of every ED, a WF's definition checked before the order is taken (it is checked
  by SY.wf-rules at its step), the sections for routes, VPN rules and the user policy, and
  SY.deploy's bounds in SY.admin.
