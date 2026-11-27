# The Digital Executive v1, stage 2: the ED's identity, SY.deploy and a plain ED's birth

**Status:** the build design. Drafted on 2026-11-26, and rewritten on 2026-11-27 on the new path:
after the cleanup of stage 1's security path (0.1.155, 0.1.156) and the operator's calls (§13).
Every call is made. Nothing is built.

**What it builds:** stage 2 of [digital-executive-v1-design](digital-executive-v1-design.md) §15,
with the ED's identity where the operator put it (design §3.0, notes §32):

- **The ED's identity, in identity:**
  - every ILK names the person it works for, and its role, in the SHM;
  - a person's document, layer 2 now, with room for layer 1.
- **A minimal SY.deploy,** the CD node (design §5). It will deploy every user solution, so its model
  is general from the first line. It executes and records what it did; it keeps no ED data.
- **A plain ED's birth:**
  - its ILK and its document;
  - its conscious;
  - one channel, WhatsApp, with its IO assigned by configuration and confirmed by identity.
- **Its end:** the retire mode (design §4.3).
- **For Cloud:** the orders, a catalog of what can be deployed, and a tree of what is.
- **Versions kept:** three, and never one in use.

**What it leaves out** (§12): responsibilities and Archi's published packages; upgrades and waves;
the profile's fields and the people tool; layer 1's contents; the email; ED–ED talk; the instances;
an AI check of deployments; a reserved namespace and the launcher's checks on SY.deploy's row.

**What it leaves working:** every node and path of today. Nothing changes for a node whose ILK works
for nobody, and for an IO or an agent with no person in its config. The router is not touched.

---

## 0. What the code says (read on 2026-11-26 and 2026-11-27)

- **Archi's sources cannot be the plain ED's.**
  - A solution's manifest (`SolutionManifestV2`, `src/bin/sy_architect/pipeline_types.rs`) has no
    blanks, no parameters, no released version, no tenant and no runtime version per node.
  - The plan compiler, an LLM, writes the arguments of `run_node` and of a config write.
  - No node can read a source: there are only the files on the motherbee's disk
    (`/var/lib/fluxbee/state/manifests/<solution>/v<ms>.json`).
  - Archi keeps every save as a version, and reads the versions whose plan ran (`executed_manifests`)
    to know which solution declared a running WF or the user policy.
- **The pieces a birth needs exist, and are run one by one through SY.admin:**
  `create_virtual_person`; `run_node`, which already takes a caller's `ilk_id` and registers it
  (`resolve_node_ilk_id`, `src/bin/sy_orchestrator.rs`, undocumented in SY.admin's help);
  `set_ilk_definition`; `vault_list` and `vault_assign`.
- **The orchestrator registers a node's ILK in identity at spawn** (`ensure_node_identity_registered`)
  and keeps only the node's deployment history. The precedent for SY.deploy (notes §32).
- **The WF precedent** (`go/sy-wf-rules`): SY.admin forwards each order to SY.wf-rules and waits;
  SY.wf-rules runs one request at a time, calls SY.admin's node actions with its own budgets, keeps
  its state in plain files, and has its own row in SY.admin's mesh list (`authorize_wf_rules_node`).
- **The answer to Cloud must come at once.** The edge gives Cloud 30 s and IO.cloud waits 20 to 25 s,
  while the motherbee's orchestrator runs node actions one at a time. SY.admin has an "accepted,
  read it later" pattern (`add_hive`, `acme_renew`, `update_installation` answer 202), and no
  deduplication of requests.
- **Who launched a node is recorded,** in `_system.launched_by`: SY.admin stamps it from the caller.
- **No removal keeps an ILK** (design §4.3): `kill_node` with `purge_instance`, and
  `remove_node_instance`, end in SY.admin's `teardown_node`, which marks and purges the node's ILK. A
  marked ILK keeps holding its node's name.
- **The identity SHM** (`src/shm/mod.rs`): `IDENTITY_VERSION` 6, a fixed 1160-byte `IlkEntry`
  (id, type, registration, flags, tenant, display name, `handler_node`, channels, the definition's
  hashes, times) ending in 8 reserved bytes, which design §10.4 gave to the person's document. A
  reader refuses another version; its three copies are `src/shm`, the Rust SDK's `identity.rs` and
  the Go SDK's `identity_shm.go`.
- **The agent-assets folder** (`fluxbee-dist-agent-assets`, `/var/lib/fluxbee/blob/agent-assets`):
  a dist folder the motherbee sends and the workers receive, outside `read_blob` and the blob GC. The
  exact precedent for the people folder (design §10.5).
- **io.wapp** (`nodes/io/io-wapp`):
  - its config: a vault key of `resource_type` `whatsapp` (`{access_token, app_secret,
    verify_token}`), `io.phone_number_id`, `io.waba_id`, `io.dst_node`;
  - its inbound: one fan-out endpoint per hive, made by the operator's `externalize`; each node picks
    its messages by `phone_number_id`, and the HMAC of the body is the only check;
  - its reply: through `meta.context.io.reply_target`, always to the real `graph.facebook.com`;
  - `dst_ilk` is passed as `None` in one place, `InboundProcessor::process_message_parts`
    (`nodes/io/common/src/inbound.rs`).
- **The reply's ILK** (`crates/fluxbee_ai_sdk/src/runtime.rs`): `stamp_reply` stamps every reply
  with `RuntimeConfig.self_ilk_id`, fixed when the runtime starts. Vault calls carry the node's own
  ILK, which SY.vault binds to the caller's name.
- **ai.generic polls its definition in the SHM** (`cognitive_definition_poll_loop`,
  `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`), and reads its assets from its own disk.

---

## 1. The ED's identity, in identity

Decided (design §3.0, the operator, 2026-11-27).

### 1.1 Whom an ILK works for

- **On every ILK record:** `works_for`, a person's ILK, and `work_role`: `conscious`, `channel` or
  `agent`. Empty for every ILK of today. SY.identity's migration v7 adds the two columns, and the
  replication carries them.
- **In the SHM, version 7:** the `IlkEntry` gains `works_for` (16 bytes) and `work_role` (1 byte,
  0 for none), with 7 bytes of padding: 1184 bytes. The three copies change together, and every
  reader with them: the router, the SY nodes, the Rust and Go SDKs and their nodes. One release,
  with a reinstall of 8.x if the update needs it.
- **Set by `ILK_SET_WORKS_FOR`,** taken from exactly `SY.admin@motherbee`, on the primary. Its
  SY.admin action is `set_ilk_works_for`, for SY.deploy, the operator and Archi.
  - The ILK is an agent's (a node's).
  - The person is a person (`human`, real or virtual), active, in the ILK's own tenant.
  - An empty `works_for` clears it.
- **Read** in memory, with no request to anyone:
  - by the conscious and by the IO, to confirm their config (§6, §7);
  - by SY.deploy, to converge and to answer the tree (§3.4);
  - by any node that asks whether an ED stands behind a node, and in what role.
- **A person's parts** are the ILKs whose `works_for` names it.

### 1.2 The person's document

The profile's JSON (design §10.4) becomes the person's document. Identity keeps it.

- **Where:** one file per version in the people folder, `fluxbee-dist-people` at
  `/var/lib/fluxbee/blob/people`, the agent-assets folder's sibling: the motherbee sends it, the
  workers receive it, ingress and egress never get it (design §10.5). The file is
  `person-<uuid>_<hash16>.json`.
- **Its name in the SHM:** the 16-hex hash prefix in the `IlkEntry`'s 8 reserved bytes. The record
  keeps the full hash and the document, and the SHM is rebuilt from it.
- **Layer 2, inside Fluxbee, in this stage:**

  ```json
  {
    "schema": 1,
    "display_name": "Laura Gómez",
    "company_name": "Acme",
    "short_id": "ed4qm7k2xa",
    "personality": {"asset": "sha256:…", "timezone": "America/Argentina/Buenos_Aires",
                    "country_code": "AR", "primary_language": "es", "additional_languages": ["en"]},
    "channels": [{"type": "whatsapp", "address": "1234567890", "io_node": "IO.wapp.ed4qm7k2xa@worker1"}]
  }
  ```

  Room is kept for the profile (`position`, `reports_to`, `responsibilities`, stage 4) and for
  `public`, layer 1 (the internet identity's stage). Neither is written in this stage.
- **Set by `ILK_SET_DOCUMENT`,** taken from exactly `SY.admin@motherbee`, for a person only. Its
  SY.admin action is `set_person_document`, for SY.deploy, the operator and Archi. A change is a new
  version.
- **Not through `read_blob`** (design §10.4). In this stage it is read on the motherbee by SY.admin
  and SY.deploy, for Cloud's tree. The people tool for agents comes with the profile.
- **The versions:** the last three files of each person are kept. On another hive the SHM may name
  a file that has not arrived yet (A-338); a reader then answers "not yet".

### 1.3 A virtual person born with its id

- `ILK_CREATE_VIRTUAL` takes an optional `ilk_id`. With one, the call is idempotent: an active
  virtual ILK with that id, the same tenant and the same names answers ok with it; any other holder
  of the id answers `ILK_ID_TAKEN`.
- SY.admin's `create_virtual_person` passes it through, and its help names it.
- **No channel in identity's channel table** for an ED in this stage (the operator, 2026-11-26). Its
  addresses are in its document, and SY.deploy keeps two EDs off one number (§5.1).

---

## 2. The orders: SY.admin's `cd_*` actions

An ED's birth is the first deployment of a new ED grouping, from the source `plain-ed`.

| Action | HTTP (operator) | Cloud op | Archi | What it does |
| --- | --- | --- | --- | --- |
| `cd_deploy` | `POST /cd/deployments` | `cd_deploy` | SCMD | Deploys a source, with its blanks, into a grouping; a grouping of kind `ed` with no id is a new ED. Answers *accepted*. |
| `cd_get` | `GET /cd/deployments/{id}` | `cd_get` | SCMD | One deployment, with its state and its nodes' status. |
| `cd_list` | `GET /cd/deployments` | `cd_list` | SCMD | The deployments, by tenant, grouping kind or grouping. |
| `cd_remove` | `POST /cd/deployments/{id}/remove`, `POST /cd/groupings/{id}/remove` | `cd_remove` | SCMD | Removes a deployment, or a whole grouping (§9). Answers *accepted*. |
| `cd_catalog` | `GET /cd/catalog` | `cd_catalog` | SCMD | The sources ready to deploy, by kind (§3.3). |
| `cd_tree` | `GET /cd/tree?tenant_id=` | `cd_tree` | SCMD | A tenant's tree: its infra, its EDs, their parts and deployments (§3.4). |

- **The names:** `list_deployments`, `get_deployments` and `/deployments` are the orchestrator's
  software history. The prefix `cd_` follows SY.wf-rules' `wf_rules_*`.
- **SY.admin forwards each one to `SY.deploy@motherbee`,** a write as a `command`, a read as a
  `query`, and waits at most 10 s: SY.deploy answers before it starts any step.
- **SY.deploy takes orders only from `SY.admin@motherbee`.**
- **Cloud:** six rows join `CLOUD_OP_ACTIONS` (`crates/fluxbee_sdk/src/cloud.rs`). IO.cloud puts the
  tenant Cloud claims in each, and SY.deploy answers *not found* for another tenant's. The contract
  goes in [io-cloud-api](io-cloud-api.md), for Fluxbee Cloud's team.
- **Cloud's flow for a new ED:** `put_token` with `resource_type` `whatsapp` in the ED's tenant;
  `cd_deploy` of `plain-ed`, naming that key; `cd_get` until `active` or `failed`.

### 2.1 `cd_deploy`

```json
{
  "tenant_id": "tnt:…",
  "request_key": "cloud-ed-7f3a…",
  "source": {"id": "plain-ed", "version": "1"},
  "grouping": {"kind": "ed"},
  "blanks": {
    "display_name": "Laura Gómez",
    "company_name": "Acme",
    "personality": {"timezone": "America/Argentina/Buenos_Aires", "country_code": "AR",
                    "primary_language": "es", "additional_languages": ["en"],
                    "description": "…", "biographical": {…}, "narrative": {…}},
    "whatsapp": {"phone_number_id": "1234567890", "waba_id": "…", "credential_key": "acme-laura-wapp"}
  },
  "hive": "worker1"
}
```

A responsibility, later: another source, into `{"kind": "ed", "id": "<the ED's ILK>"}`.

- **`request_key`,** the idempotency key: the caller's own id, at most 64 characters of
  `[A-Za-z0-9._:-]`. The same key in the same tenant gives back the same deployment; with other
  content it answers `REQUEST_KEY_REUSED`.
- **`source`** names a released version; without one, the newest SY.deploy has. The answer and the
  record name the exact version and hash.
- **`grouping`:** its kind and, for an existing one, its id. In this stage the only kind is `ed`,
  and only a native source founds one.
- **`blanks`:** exactly the source's, each checked by its type (§5.1).
- **`hive`:** optional for a new grouping (the worker with the most room); a grouping's later
  deployments go to its hive (design §5.7).
- **No secret travels in the order:** a credential is a key name.
- **The answer:** 202, `{"deployment_id": "dp7xk3m2qa", "grouping": {"kind": "ed", "id": "ilk:…",
  "short_id": "ed4qm7k2xa"}, "state": "deploying"}`.

### 2.2 States

A deployment: `deploying` → `active` or `failed`; `active` or `failed` → `removing` → `removed`.

- **A failed deployment stays `failed`,** with the step, its code and its detail. Nothing is undone
  by itself (*no magic automation*).
- **Two ways out of `failed`:** `cd_deploy` again with the same `request_key` resumes where it
  stopped (§5.2); `cd_remove` ends what was made.

---

## 3. SY.deploy, the node

- **A Rust system node,** `src/bin/sy_deploy.rs`, on the motherbee only, behind the same role gate
  as SY.admin.
  - It reads the identity SHM with the core's reader.
  - It shares Archi's agent-asset code, moved to the core library (§6.2).
  - Its admin calls use the SDK's admin RPC, as ai.generic's `publish_html_page` does.
- **General, not the ED's** (the operator, 2026-11-26):
  - **a source reader:** the native sources it carries now, Archi's published ones later; both give
    the same desired state;
  - **the desired state:** the source plus its blanks' values, with every name and id made concrete;
  - **one step builder:** from the desired state and what runs, the SY.admin actions still missing,
    in order, with their exact arguments;
  - **one runner:** the steps one by one, journaled (§5.2).

  A new source is data. A new kind of part (a WF, a route, a policy) is one more section in the
  desired state and in the step builder, and one more class in its row (§8).
- **It executes and records; it keeps no ED data** (the operator, 2026-11-27). What an ED is lives
  in identity (§1). SY.deploy's records are its deployments.
- **Its state is files:**
  - `/var/lib/fluxbee/state/sy-deploy/deployments/<deployment_id>.json`, written to a temporary file
    and renamed;
  - `journal/<deployment_id>.jsonl`, appended;
  - `sources/<source_id>/<version>.json`, the catalog's immutable copies (§3.3);
  - no secret in any, only key names. The backup is the motherbee's snapshot.
- **One deployment at a time,** with a lock per grouping, so a removal never crosses a deployment
  under way.
- **Its own system ILK,** seeded from the release's lists like every SY node's.

### 3.1 A deployment's record

```json
{
  "deployment_id": "dp7xk3m2qa",
  "grouping": {"kind": "ed", "id": "ilk:…", "short_id": "ed4qm7k2xa"},
  "tenant_id": "tnt:…",
  "request_key": "cloud-ed-7f3a…",
  "source": {"id": "plain-ed", "version": "1", "hash": "sha256:…"},
  "blanks": {"display_name": "…", "company_name": "…", "personality": {…},
             "whatsapp": {"phone_number_id": "…", "waba_id": "…", "credential_key": "…"}},
  "hive": "worker1",
  "nodes": [
    {"role": "conscious", "node_name": "AI.ed4qm7k2xa@worker1", "ilk_id": "ilk:…",
     "runtime": "ai.generic", "runtime_version": "…"},
    {"role": "channel", "node_name": "IO.wapp.ed4qm7k2xa@worker1", "ilk_id": "ilk:…",
     "runtime": "io.wapp", "runtime_version": "…"}
  ],
  "assets": {"conscious": {"role": "…", "handbooks": ["…"], "personality": "…"}},
  "vault": [{"key": "…", "assigned_to": "IO.wapp.ed4qm7k2xa@worker1"}],
  "state": "active",
  "history": [{"at_ms": 0, "what": "deployed", "by": "cloud"}]
}
```

- **What it is:** what SY.deploy did, and with what. The ED's names and channels are in the ED's
  document; its parts, in their `works_for`.

### 3.2 Ids and names

- **A grouping's short id:** its kind's two letters (`ed` for an ED; `in` and `ts` kept for infra and
  test), a digit from 2 to 7, and 7 characters of `[a-z2-7]`. The digit keeps an ordinary word, such
  as `inventory2`, from reading as one. An ED's goes in its document (`short_id`).
- **A deployment's id:** `dp` and the same 8 characters' form.
- **Node names come from the short id** (design §5.5): `<KIND>.<middle.>?<short_id>@<hive>`. For the
  plain ED, `AI.ed4qm7k2xa@worker1` and `IO.wapp.ed4qm7k2xa@worker1`, which matches io.wapp's
  fan-out pattern `IO.wapp.*@<hive>`. A responsibility's, later: `AI.collections.ed4qm7k2xa@worker1`.
- **Runtime versions are pinned:** the hive's `current` at the deployment, written as the exact
  version in the record and in each `run_node` (design §5.6).

### 3.3 The catalog and its versions

- **Agreed** (the operator, 2026-11-26): Archi creates and publishes packages; SY.deploy copies a
  published source and keeps the copy, all on the motherbee. No polling: a publish is one event
  through SY.admin, and SY.deploy answers it at once, accepted or refused with the reason.
- **In this stage** the catalog holds the native sources SY.deploy carries (`plain-ed`). Archi's
  publish (`cd_publish`) comes with the responsibilities.
- **`cd_catalog`** lists each source with its kind (`ed`, `infra`, `custom`), its versions and its
  blanks. A custom source made for one ED is listed for that ED only.
- **Versions kept** (the operator: three, not more than five):
  - **SY.deploy:** the last three versions of each source, and every version a deployment still
    runs, which a converge and a rollback need.
  - **Archi:** the last three saves of each solution, and its last executed version, which it reads
    to know who declared a running WF or the user policy. Built with this stage's Archi release.

### 3.4 Cloud's tree

`cd_tree` answers one tenant's tree, read when it is asked:

- **its general infra:** the deployments of kind `infra` (none yet);
- **its EDs:** the tenant's virtual persons in the SHM, each with its document;
- **each ED's parts:** the ILKs whose `works_for` names it, with their role, node and status;
- **each ED's deployments:** SY.deploy's records of its grouping, with their source, version and
  state, the assets of each agent, and the key names of each IO's credentials. Never a secret.

---

## 4. The plain ED's source

**Decided** (the operator, 2026-11-26): it ships with Fluxbee, inside SY.deploy, as a JSON file in
the repository with its version and its content's hash. A new version is a Fluxbee release; moving
existing EDs to it is an upgrade, later, in waves.

- **Its format is the one every source compiles to:**
  - **`blanks`:** each with a name and a type that carries its check (a name, a personality, a key
    of a given `resource_type` in the tenant, a WhatsApp number, an ILK of the tenant, a value);
  - **`founds`** (the native layer only): the person it founds, its names from the blanks, and its
    document;
  - **`nodes`:** each with its role, its kind (`AI` or `IO` in this stage), its middle name part, its
    runtime, its config with the blanks in place, its `work_role`, and, for an agent, its
    definition's assets;
  - **`vault`:** which blank's key goes to which node.
- **For `plain-ed`:**
  - **the conscious:** `ai.generic`, `behavior` `{kind: "ai_chat", model: "claude-sonnet-5-5"}`, the
    native role and handbook, the personality from its blank, `work_role` `conscious`;
  - **the WhatsApp channel:** `io.wapp`, middle part `wapp`, its config with the blanks in place,
    `work_role` `channel`.
- **A section SY.deploy cannot build yet** (a WF, a route, a VPN rule, the user policy) makes the
  source refused before any step.

---

## 5. A deployment, step by step: the plain ED's

### 5.1 Checks first, before any step

All are fixed checks, with no AI (design §5.5).

- **General:**
  - the tenant: a user tenant, active, in the identity SHM;
  - the request key: new in this tenant, or the same content as before;
  - the source: one SY.deploy carries, at a released version, with nothing it cannot build;
  - the grouping: of the tenant and of a kind the source fits; a new one only from a source that
    founds it;
  - every blank the source declares is filled, and nothing more;
  - the AI key: for each agent, a usable key of its provider in the tenant (`anthropic` for
    `plain-ed`; `vault_list`), or `AI_KEY_MISSING`;
  - the hive: a worker that is alive, with room for the nodes under the router's 1024;
  - ids and names: SY.deploy mints the short id, the deployment's id and one ILK id for each node
    and for the person it founds, and checks that no SHM entry holds them or the node names.
- **The blanks of `plain-ed`:**
  - `display_name` and `company_name` at most 128 bytes;
  - `whatsapp`: `phone_number_id` is digits, and no other deployment of SY.deploy's holds it; the
    credential key exists in the tenant with `resource_type` `whatsapp` (`vault_list`, metadata
    only).

Only then does it write the record (`deploying`) and the journal, and answer *accepted*.

### 5.2 The steps

Each step is journaled before it is sent and after its answer; all are SY.admin actions, one by
one.

| # | Step | Arguments |
| --- | --- | --- |
| 1 | `create_virtual_person` | `tenant_id`, `display_name`, `company_name`, **`ilk_id`** (minted) |
| 2 | `set_person_document` | the ED: its layer 2 (§1.2) |
| 3 | the assets | the native role and handbook, and the ED's personality, written by SY.deploy (§6.2) |
| 4 | `run_node`, the conscious | `AI.<short_id>@<hive>`, `ai.generic`, the pinned version, `tenant_id`, **`ilk_id`**, `config`: `{behavior, speaks_as: <the ED>}` |
| 5 | `set_ilk_works_for` | the conscious's ILK: the ED, `conscious` |
| 6 | `set_ilk_definition` | the conscious's ILK: `role_hash`, `handbook_hashes`, `personality_hash` |
| 7 | `run_node`, the IO | `IO.wapp.<short_id>@<hive>`, `io.wapp`, the pinned version, `tenant_id`, **`ilk_id`**, `config`: `{wapp.auth: {type: vault_ref, resource_type: whatsapp, key}, io: {phone_number_id, waba_id, dst_node: AI.<short_id>@<hive>, person_ilk: <the ED>}}` |
| 8 | `set_ilk_works_for` | the IO's ILK: the ED, `channel` |
| 9 | `vault_assign` | the credential key, to the IO only |
| 10 | the record | `active` |

- **The order:** the ED and its document first; each node before its relation, since `run_node`
  makes the node's ILK; the conscious before its IO.
- **Between a node's start and its relation** the node reads itself as not confirmed (§6.1, §7), and
  confirms itself at the next move of the region, seconds later. The IO's webhook is not pointed at
  the installation until Cloud is told the ED is `active`.
- **Each step's wait** is longer than SY.admin's own wait on the orchestrator (40 s for a
  `run_node`), so SY.deploy never gives up while SY.admin's call still runs.
- **A retry converges; it does not replay** (design §5.3): after a restart, a timeout, or a
  `cd_deploy` that resumes, SY.deploy reads what each step makes (the ILK, the document's hash, the
  relations and the definition's hashes in the SHM, the assets on disk, the nodes through
  `get_node_status`, the assignment through `vault_list`) and sends only what is missing. The ids
  are in the journal before step 1.
- **A step that fails for good** leaves the deployment `failed`, with the step and its error.

---

## 6. The conscious

### 6.1 It speaks as the ED: from its config, confirmed by identity

- **ai.generic's config takes `speaks_as`,** the ED's ILK, at the top level, written by SY.deploy and
  read at boot. A change takes effect at the next start.
- **Confirmed by identity:** the node reads its own ILK's entry in the SHM at boot, and again at
  each move of the region, which it already polls for its definition. It is confirmed while its
  `works_for` names `speaks_as` with the role `conscious`.
- **The AI SDK's `stamp_reply`:** on a solution-kind reply whose `src_ilk` is empty, it stamps
  `speaks_as` while confirmed, and the node's own ILK otherwise; its status says why.
  - Control replies keep the node's own ILK.
  - The writer stays the conscious.
  - Vault calls keep the node's own ILK.
- **The contract test changes on purpose** (design §2.2, 4): a reply carries its agent's own ILK, or
  the person its config names and identity confirms, never one copied from the message.

### 6.2 It is born with its definition

**Decided** (the operator, 2026-11-26):

- SY.deploy writes the conscious's assets into `<blob>/agent-assets/`, where Archi writes them: the
  native role and handbook from the source, the ED's personality from its blank. Syncthing carries
  them to the worker, and ai.generic takes the definition once every asset is on its hive.
- Archi's asset checks and hashing (`validate_agent_asset_value` and its builders,
  `src/bin/sy_architect.rs`) move to the core library, so both write the same files.
- **The native texts are the operator's,** to approve before the build. They say nothing on whether
  the ED is an AI.

---

## 7. The IO: assigned by its config, confirmed by identity

**Decided** (design §7.1; the operator, 2026-11-27): the assignment is the IO's config, written by
SY.deploy at launch, and identity confirms it. Neither alone makes an IO an ED's.

- **The key:** `io.person_ilk`, next to `io.dst_node`, in the IO family's common inbound config.
- **Confirmed** when the config is read and at each move of the region: the IO's own ILK's
  `works_for` names `io.person_ilk`, with the role `channel`, and the person is of the IO's tenant.
- **While confirmed,** the common code stamps the person as `dst_ilk` on each inbound message
  (`process_message_parts`).
- **While not,** it stamps nothing, warns once a minute, and its status says why. Messages still go
  to `io.dst_node`.
- **What it gains** (the operator): a config written by hand that identity does not confirm, or a
  relation in identity that no config names, stamps nothing; and the mismatch is visible.
- **io.wapp** reads it in this stage; the other IOs when one needs it (io.slack waits for its
  revamp).
- **The frontdesk gate stays off** on an ED's IO: the ED does the human part (design §9).
- **Infra, once per hive, not SY.deploy's** (design §6.4):
  - the fan-out endpoint: the operator's `externalize` with `fanout_family` `IO.wapp.*@<hive>` and
    `inbound_family` `io.wapp.inbound.v1`; without the second every event is dropped (A-364);
  - the company's Meta app, pointing its webhook at that endpoint.

**What a message carries, then:**

- **Inbound:** `src_ilk` the writer (a temporary ILK in the ED's tenant for a stranger); `dst_ilk`
  the ED; to the conscious.
- **The reply:** `src_ilk` the ED; `dst_ilk` the writer; the writer field, the conscious; to the IO,
  which sends it through its reply target.
- **Cognition:** its memory's key (the thread and the conscious's ILK) and the episodes are
  unchanged. Its out-turns' `src_ilk` becomes the ED; SY.storage's `from_ilk` becomes the ED, its
  `writer_ilk` stays the conscious; the ILK weights move (design §8.3).

---

## 8. SY.deploy's row in SY.admin

**Agreed** (the operator, 2026-11-26): the row now, by classes of actions, so that the next
solutions need no new one. Its bounds come later (§12).

- **A clause of its own, for `SY.deploy@motherbee` exactly,** next to Archi's and IO.cloud's in
  `authorize_mesh_caller`:

  | Class | Actions |
  | --- | --- |
  | A node's whole life | `run_node`, `start_node`, `restart_node`, `kill_node` (plain or with `retire_instance`), `get_node_status`, `get_node_config`, `set_node_config`, `node_control_config_set` |
  | A deployment's people | `create_virtual_person`, `set_person_document`, `set_ilk_works_for`, `set_ilk_definition`, `delete_ilk`, `restore_ilk` |
  | Its tenant's keys | `vault_list`, `vault_assign` |

- **Out:** purges, of a node or of an ILK (the operator's, design §2.1), and what the whole
  installation shares (routes, VPN rules, the user OPA policy, hives, runtimes), each with the first
  source that needs it.
- **The launcher's stamp:** SY.admin stamps `launched_by` `SY.deploy@motherbee` on its launches, as
  for any caller.

---

## 9. The end: the retire mode

Decided (design §4.3).

- **In the orchestrator:** `kill_node` takes `retire_instance`, exclusive with `purge_instance`. It
  stops the node, purges its timers, and removes its instance's directory, its uuid file and its map
  entry. Instead of SY.admin's `teardown_node`, it sends `ILK_DELETE` for the node's ILK, which
  marks it with the reason `node_retired`. Its vault secrets are kept, and the marked ILK keeps the
  name.
- **A recreate that keeps the ILK:** `restore_ilk`, then `run_node` with that `ilk_id`.
- **`cd_remove` of an ED's grouping:** its deployments, the newest first, each node by `kill_node`
  with `retire_instance` (for `plain-ed` the IO first, so nothing more comes in, then the conscious);
  then `delete_ilk` on the ED, which marks it; then the records go to `removed`. A `cd_remove` of
  the founding deployment alone is refused while the grouping has others.
- **What stays:** the ILKs, marked, with their relations and history; the ED's document; the
  credential, for Cloud to delete; pages until their expiry; cognition's rows.
- **Purging** stays the operator's, one ILK at a time.

---

## 10. How it ships (METHOD §9)

Shared code goes alone, then items on different binaries, a gate each.

- **2A, the identity SHM's version 7, alone:** the layout in its three copies (`src/shm`, the Rust
  SDK, the Go SDK), SY.identity's migration v7 and its projection of `works_for` and `work_role`.
  Writer and readers change together. Nothing sets the fields yet.
  - **Its gate:** the smoke; every hive on v7; an AI, an IO, a WF and the Go SY nodes working; a
    probe reading the new fields as empty.
- **2B, the SDKs, alone:**
  - the AI SDK: `speaks_as` and its confirmation (§6.1);
  - the IO family's common code: `io.person_ilk` and its confirmation (§7);
  - the Rust SDK: Cloud's six rows.
  - **Its gate:** no behaviour changes with the keys unset; an agent's reply through io.api still
    carries the agent's own ILK.
- **2C, identity, the orchestrator and SY.admin's identity actions:**
  - SY.identity: `ILK_CREATE_VIRTUAL` with `ilk_id`, `ILK_SET_WORKS_FOR`, `ILK_SET_DOCUMENT` and the
    people folder's writer;
  - the orchestrator: `retire_instance`; the people folder (its constant, owner per role, the
    spoke's list, `folder_forbidden_for_role`, its members); `ilk_id` in `run_node`'s help;
  - SY.admin: `create_virtual_person` with `ilk_id`, `set_ilk_works_for`, `set_person_document`.
  - **Its gate:** a virtual person created twice with one id is one; its document written, read
    back, and present on worker1; a throwaway node's relation set, read in the SHM, cleared; the
    refusals (another tenant's person, a node's ILK as the person); a throwaway node retired (files
    gone, ILK marked, name held) and restored with its ILK.
- **2D, SY.deploy and its users:** SY.deploy; SY.admin's `cd_*` actions and SY.deploy's row;
  IO.cloud's translation; ai.generic's `speaks_as`; io.wapp's `io.person_ilk`; Archi's SCMD routes,
  handbook and version retention; the packaging (§10.1).
  - **Its gate** is §11.

### 10.1 Where a new SY node is registered

Read for SY.wf-rules and SY.admin on 2026-11-26. SY.deploy touches each:

- **The release's role lists** (`src/release_settings.rs`), which feed the orchestrator's start and
  watchdog, SY.identity's system ILKs and SY.vault's well-known ILKs. Their tests pin them.
- **Packaging:** `packaging/build-deb.sh` (the binary, its unit, its role gate as
  `sy-admin|sy-storage`, the description); `deb-postinst` (its state directory); `deb-prerm` (the stop
  list); `fluxbee-factory-reset` (its state's reset).
- **The orchestrator:** `BOX_INSTALL_UNITS`.
- **SY.admin:** the internal action registry and its version; the route to SY.deploy; the mesh
  clause; the HTTP routes; the help and catalog functions; `docs/nodes/sy-admin.md`.
- **Archi:** `is_mutating_admin_action`, and the command-log exclusions if the actions stay out of
  SY.admin's command log, as `wf_rules_*` do.
- **The lab:** `scripts/fluxbee_stop.sh`; `lab/ops.py`'s `TIMELINE_UNITS`;
  `docs/packaging-and-build.md`; `docs/nodes/sy-deploy.md`.
- **The system policy** needs no change: SY.deploy gets no SYSTEM authority.

---

## 11. The gate of 2D

In the tenant `pruebas`, with no real Meta traffic.

- **Before, once:** the WhatsApp fan-out on worker1 with `inbound_family` `io.wapp.inbound.v1`; a
  test credential through Cloud's `put_token`, with a known test `app_secret` and a fake
  `access_token`.
- **The smoke:** a new throwaway node, and a stopped one read.
- **The catalog:** `cd_catalog` lists `plain-ed` with its blanks.
- **The birth, through Cloud's door:** `cd_deploy` answers 202 well under IO.cloud's wait; `cd_get`
  reaches `active`; each step timed from the journal.
- **Read back:**
  - the ED's ILK is virtual, in `pruebas`, with its document (its names, short id, personality and
    WhatsApp channel);
  - the conscious's and the IO's `works_for` name the ED, with `conscious` and `channel`;
  - the conscious runs with its definition's hashes; the IO is `CONFIGURED`; both confirmed;
  - `cd_tree` shows the ED, its two parts and its deployment, with no secret.
- **A message, through the edge:** a webhook signed with the test secret reaches the IO; it goes to
  the conscious with `dst_ilk` the ED; the conscious answers with one model call, `src_ilk` the ED
  and itself as the writer; io.wapp logs its failed Graph send with the trace (the token is fake),
  which proves the reply reached it with its target.
- **The confirmation:**
  - the operator writes another person into the IO's `io.person_ilk`: the IO stops stamping and
    warns; written back, it stamps again;
  - the operator clears the conscious's `works_for`: its replies carry its own ILK and its status
    says why; set again, it speaks as the ED.
- **The edges:** `cd_deploy` again with the same key gives the same deployment; another key with the
  same number is refused; another tenant's deployment reads as not found; Cloud's `delete_ilk` on
  the ED still answers `ILK_VIRTUAL`.
- **A restart of SY.deploy in the middle of a second birth:** it converges, with one ED and one ILK
  per node.
- **The end:** `cd_remove` of the ED's grouping: the nodes gone, the three ILKs marked, the names
  held, the document and the credential kept, the records `removed`.
- **Cleanup:** the operator purges the gate's ILKs and its credential.
- **The timings:** the answer, each step, the time to `active`, and from the webhook to the Graph
  call.

A real WhatsApp test needs a Meta app with a test number pointed at the installation: the operator's
call, not needed for the gate.

---

## 12. Left for later

| What | Where it goes |
| --- | --- |
| Responsibilities: Archi's sources with blanks, and Archi's `cd_publish` | A stage of its own, with Cloud's hiring flow (design §13, 18) |
| Upgrades of deployments, waves, rollback | With the first new version of a source (design §6.1) |
| WFs, routes, VPN rules, the user policy, hives and runtimes: their sections and their classes | With the first source that needs each (§4, §8) |
| Groupings of kind infra and test | When the first is deployed (design §5.4) |
| SY.deploy's bounds: a reserved namespace, and `require_launched_by` on its node actions | Later, after the flow (the operator, 2026-11-26) |
| The profile's fields and the people tool | Stage 4 (design §10) |
| Layer 1's contents: the DID document and the card | The internet identity's stage (design §12) |
| The ED's channels in identity's channel table | The identity talk, once the path is built |
| The email IO | To see (design §7.3) |
| ED–ED talk, a WF in an ED's path | Design §8.5 |
| The instances | Design §13.1 |
| Restoring a retired ED | When asked |
| An AI check of deployments | Design §5.5 |

---

## 13. The operator's calls

- **2026-11-26:**
  1. the plain ED's source ships inside SY.deploy;
  2. the conscious is born with its definition, with no line on whether the ED is an AI;
  3. the hand as a channel: rejected, and stage 1's `src_ilk` path taken out of the code (0.1.155,
     0.1.156);
  4. Cloud's operations in this stage, with SY.deploy general for every solution, and a catalog and
     a tree for Cloud; Archi publishes as an event, SY.deploy copies and keeps; three versions kept;
  5. no `enforce`: no security mechanism before the flow it protects is built, and a panel's
     requirements talked one by one.
- **2026-11-26, after the cleanup:** the conscious's person from its config; SY.deploy's row by
  classes now, its bounds later; no WhatsApp channel on the ED in identity; the IO's person checked
  at its config.
- **2026-11-27** (notes §32): the ED's identity lives in identity, as each part's `works_for` and the
  person's document, layer 2 inside Fluxbee and layer 1 on the internet; SY.deploy creates and keeps
  no ED data. The IO's assignment stays its config, written by SY.deploy, and identity confirms it;
  the conscious follows the same rule.

---

## 14. Seen in the code on the way (recorded in FINDINGS)

- **A-364:** `externalize` with `fanout_family` defaults `inbound_family` to `user`; a WhatsApp
  fan-out made without it silently drops every event.
- **A-365:** io.wapp's contract lists optional fields it never reads.
- **A-366:** Archi's designer is told a resource's `ownership` defaults to `external`; the code
  defaults a node to `solution`, and the reconciler does not read it.
- **SY.wf-rules gives up on a step before SY.admin's own wait ends,** with no cancel (15 s against
  30 s). SY.deploy waits longer (§5.2). Not recorded on its own: a WF's apply converges on its next
  try.
