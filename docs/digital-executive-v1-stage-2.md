# The Digital Executive v1, stage 2: SY.deploy and a plain ED's birth

**Status:** the build design, draft of 2026-11-26, revised the same day with the operator's first
answers (§11): calls 1, 2 and 4 are decided; calls 3 and 5 are explained again and wait. Nothing is
built.

**What it builds:** stage 2 of [digital-executive-v1-design](digital-executive-v1-design.md) §15:

- **A minimal SY.deploy,** the CD node (design §5). It will deploy every user solution, not only
  EDs (the operator, §11 call 4). So its model is general from the first line: groupings and
  deployments, sources with blanks, one step builder. The plain ED is its first source, and an ED is
  one kind of grouping.
- **A plain ED's birth:**
  - its ILK with its company;
  - its conscious;
  - one channel, WhatsApp, with its IO assigned by configuration;
  - the deployment's manifest, which is its record.
- **Its end:** the retire mode (design §4.3).
- **Two items carried from stage 1** ([stage 1](digital-executive-v1-stage-1.md) §7):
  - an idempotency key on the ED's creation;
  - the IOs' lookup leaving an ED's own addresses out.

**What it leaves out:**

- an ED's responsibilities, standard or particular (Archi's sources with blanks);
- upgrades and waves;
- the profile;
- the email;
- ED–ED talk;
- the instances;
- an AI check of deployments.

§14 lists them with where each goes.

**What it leaves working:** every node and path of today. An ED of this stage answers people on
WhatsApp as itself, and nothing else speaks as it.

---

## 0. What the code says (read on 2026-11-26)

The facts this design stands on, read in the code for it:

- **Archi's sources cannot be the plain ED's.**
  - A solution's manifest (`SolutionManifestV2`, `src/bin/sy_architect/pipeline_types.rs`) has no
    blanks, no parameters, no released version, no tenant and no runtime version per node.
  - The plan compiler, an LLM, writes the arguments of `run_node` and of a config write. It never
    sees the manifest's node entries.
  - No node can read a source: there is no mesh message, SY.admin action or HTTP route for it, only
    the files on the motherbee's disk (`/var/lib/fluxbee/state/manifests/<solution>/v<ms>.json`).
- **The pieces a birth needs exist, and are run one by one through SY.admin:**
  - `create_virtual_person` (stage 1);
  - `run_node`, which already takes a caller's `ilk_id` and registers it (`resolve_node_ilk_id`,
    `src/bin/sy_orchestrator.rs`, undocumented in SY.admin's help);
  - `set_ilk_definition`;
  - `vault_list` and `vault_assign`.
- **The WF precedent** (`go/sy-wf-rules`):
  - SY.admin forwards each order to SY.wf-rules and waits up to 75 s;
  - SY.wf-rules runs one request at a time and calls SY.admin's node actions with its own budgets
    (15 s for a `run_node`);
  - its state is plain files;
  - its row in SY.admin's mesh list (`authorize_wf_rules_node`, `src/bin/sy_admin.rs`) admits only
    `WF.<x>@<its hive>`.
- **The answer to Cloud must come at once.**
  - The edge gives Cloud 30 s, and IO.cloud waits 20 to 25 s.
  - The motherbee's orchestrator runs node actions one at a time, each to its end, and holds its
    lane while a worker launches.
  - SY.admin has an "accepted, read it later" pattern: `add_hive`, `acme_renew` and
    `update_installation` answer 202.
  - SY.admin has no deduplication of requests.
- **Who launched a node is recorded,** in `_system.launched_by`.
  - SY.admin stamps it from the caller: `cloud`, `archi`, `operator`, or the caller's name.
  - It restricts IO.cloud's `start_node`, `kill_node` and `get_node_status` to Cloud's own nodes,
    with `require_launched_by`, which the orchestrator checks.
  - The orchestrator does not check it on `set_node_config`, `get_node_config`,
    `remove_node_instance` or `node_control_config_set`.
- **No removal keeps an ILK** (design §4.3):
  - `kill_node` with `purge_instance`, and `remove_node_instance`, end in SY.admin's
    `teardown_node`. It marks and purges the node's ILK, including one that is already marked.
  - A marked ILK keeps holding its node's name: `ILK_REGISTER` answers `ILK_DELETED`, and only
    SY.admin may send `ILK_RESTORE`.
- **io.wapp** (`nodes/io/io-wapp`):
  - **Its config:** a vault key of `resource_type` `whatsapp` holding `{access_token, app_secret,
    verify_token}`, plus `io.phone_number_id`, `io.waba_id` and `io.dst_node`.
  - **The inbound path:** one fan-out endpoint per hive, made by the operator's `externalize`. Each
    node picks its own messages by `phone_number_id`, and the HMAC of the body (`X-Hub-Signature-256`)
    is the only check.
  - **The reply path:** a reply goes out through `meta.context.io.reply_target`, always to the real
    `graph.facebook.com`.
  - **What it has not got:** a config key naming a person (no IO has one).
  - **Where `dst_ilk` would go:** it is passed as `None` in one place, `InboundProcessor::process_message_parts`
    (`nodes/io/common/src/inbound.rs`).
- **The reply's ILK** (`crates/fluxbee_ai_sdk/src/runtime.rs`):
  - `stamp_reply` stamps every reply with `RuntimeConfig.self_ilk_id`, which is fixed when the
    runtime starts.
  - The writer is set on solution kinds only.
  - Vault calls carry the node's own ILK, and SY.vault binds it to the caller's name. They must keep
    it.
- **The router's view** (`PeopleView`, `src/router/mod.rs`):
  - It holds one bit per person: virtual or not.
  - The identity SHM keeps each channel's ILK, type, address and owner IO (`IchEntry`,
    `src/shm/mod.rs`).
  - Nothing names a hand. The 8 reserved bytes of an `IlkEntry` are spoken for (the profile's hash,
    design §10.4).
  - Growing the entry is a layout change, with every reader in lockstep.
- **The conscious's definition** is a set of agent assets:
  - files named by their hash in `<blob>/agent-assets/`, written today only by Archi's tools, and
    carried to the workers by Syncthing;
  - put on the agent's ILK by `set_ilk_definition`;
  - `ILK_REGISTER` takes no definition, so a node is born without one and answers with the fixed
    "unconfigured" prompt until it gets one.

---

## 1. The orders: four actions in SY.admin

SY.deploy deploys every user solution (design §5), so its orders are about deployments, not about
EDs. An ED's birth is the first deployment of a new ED grouping, from the source `plain-ed`.

| Action | HTTP (operator) | Cloud op | Archi | What it does |
| --- | --- | --- | --- | --- |
| `cd_deploy` | `POST /cd/deployments` | `cd_deploy` | SCMD | Deploys a source, with its blanks, into a grouping. A grouping of kind `ed` with no id is a new ED. Answers *accepted*. |
| `cd_get` | `GET /cd/deployments/{id}`, `GET /cd/groupings/{id}` | `cd_get` | SCMD | One deployment, or one grouping with its deployments, and its state. |
| `cd_list` | `GET /cd/deployments` | `cd_list` | SCMD | The deployments, by tenant, grouping kind or grouping. |
| `cd_remove` | `POST /cd/deployments/{id}/remove`, `POST /cd/groupings/{id}/remove` | `cd_remove` | SCMD | Removes a deployment, or a whole grouping (§10). Answers *accepted*. |

- **The names:** `list_deployments` and `get_deployments`, and the HTTP path `/deployments`, are
  taken by the orchestrator's software history (design §5.1). The prefix `cd_` follows SY.wf-rules'
  `wf_rules_*`.
- **SY.admin forwards each one to `SY.deploy@motherbee`,** as it does a WF's to SY.wf-rules: a write
  as a `command`, a read as a `query`. It waits at most 10 s, since SY.deploy answers before it
  starts any step.
- **SY.deploy takes orders only from `SY.admin@motherbee`,** as SY.wf-rules does.
- **Cloud.** Four rows join `CLOUD_OP_ACTIONS` (`crates/fluxbee_sdk/src/cloud.rs`), the one table
  from which IO.cloud's translation and SY.admin's allowlist both come.
  - IO.cloud puts the tenant Cloud claims in each.
  - SY.deploy answers *not found* for a deployment or grouping of another tenant.
  - The contract goes in [io-cloud-api](io-cloud-api.md), for Fluxbee Cloud's team.
- **Cloud's flow for a new ED:**
  1. `put_token` with `resource_type` `whatsapp`, in the ED's tenant;
  2. `cd_deploy` of `plain-ed` into a new ED grouping, naming that key;
  3. `cd_get` of the deployment, until its state is `active` or `failed`.

  A responsibility, later, is the same order: another source, into the ED's grouping by its id.

### 1.1 `cd_deploy`

A new ED:

```json
{
  "tenant_id": "tnt:…",
  "request_key": "cloud-ed-7f3a…",
  "source": {"id": "plain-ed", "version": "1"},
  "grouping": {"kind": "ed"},
  "blanks": {
    "display_name": "Laura Gómez",
    "company_name": "Acme",
    "personality": {
      "timezone": "America/Argentina/Buenos_Aires",
      "country_code": "AR",
      "primary_language": "es",
      "additional_languages": ["en"],
      "description": "…", "biographical": {…}, "narrative": {…}
    },
    "whatsapp": {"phone_number_id": "1234567890", "waba_id": "…", "credential_key": "acme-laura-wapp"}
  },
  "hive": "worker1"
}
```

A responsibility, later: `"source": {"id": "collections", "version": "3"}`, `"grouping": {"kind":
"ed", "id": "ed4qm7k2xa"}`, and that source's blanks.

- **`request_key` is the idempotency key** (stage 1 §7): the caller's own id for the order, at most
  64 characters of `[A-Za-z0-9._:-]`.
  - The same key in the same tenant gives back the same deployment.
  - The same key with other content answers `REQUEST_KEY_REUSED`.
- **`source`** names a released version (design §6.1). In this stage the only source is `plain-ed`
  (§3), and the order may leave its version out for the newest one SY.deploy carries. The answer and
  the record always name the exact version and hash.
- **`grouping`:** its kind and, for an existing one, its id. In this stage the only kind is `ed`, and
  a source of the native layer is the only one that founds it (§3).
- **`blanks`:** exactly the ones the source declares, each checked by its type (§4.1). For
  `plain-ed`:
  - `display_name` and `company_name`, the ED's names;
  - `personality`, the fields of Archi's personality asset (`build_personality_asset_value`,
    `src/bin/sy_architect.rs`): its system fields, and its optional texts. Its name is the ED's;
  - `whatsapp`, its number and the name of its credential's key.
- **`hive`** is optional for a new grouping. Without it, SY.deploy picks the worker with the most
  room. A grouping's later deployments go to its hive (design §5.7).
- **No secret travels in the order:** a credential is a key name, put before by Cloud or the
  operator.
- **The answer:** HTTP 202, `{"deployment_id": "dp7xk3m2qa", "grouping": {"kind": "ed", "id":
  "ed4qm7k2xa", "ed_ilk": "ilk:…"}, "state": "deploying"}`.

### 1.2 States

A deployment: `deploying` → `active`; `deploying` → `failed`; `active` or `failed` → `removing` →
`removed`. A grouping is `active` while it has deployments, and `removed` after the last one goes.
An ED grouping's state is its ED's.

- **A failed deployment stays `failed`,** with the step, its code and its detail, and nothing is
  undone by itself (*no magic automation*).
- **From `failed` there are two ways out:**
  - `cd_deploy` again with the same `request_key` resumes where it stopped (§4.2);
  - `cd_remove` ends what was made.

---

## 2. SY.deploy, the node

- **A Rust system node,** `src/bin/sy_deploy.rs`.
  - It reads the identity SHM with the core's reader, which it needs for every check (§4.1).
  - It shares Archi's agent-asset code, moved to the core library (§7.2).
  - Its admin calls use the SDK's admin RPC, as ai.generic's `publish_html_page` does.
- **On the motherbee only,** as SY.admin, behind the same role gate in its unit.
- **General, not the ED's** (§11 call 4). Its parts know nothing of an ED, except what an ED
  grouping founds:
  - **a source reader:** the native sources it carries now (§3), Archi's sources with blanks later.
    Both give the same desired state;
  - **the desired state:** the source plus its blanks' values, with every name and id made concrete;
  - **one step builder:** from the desired state and what runs, the SY.admin actions still missing,
    in order, with their exact arguments (design §5.1);
  - **one runner:** the steps one by one, journaled (§4.2).

  A new source is data. A new kind of part in a source (a WF, a route, a policy) is one more section
  in the desired state and in the step builder, and one more row in its admission (§5). Nothing else
  changes.
- **Its state is files,** as SY.wf-rules':
  - **Where:** `/var/lib/fluxbee/state/sy-deploy/`.
  - **The records:** `groupings/<grouping_id>.json` and `deployments/<deployment_id>.json`, each
    written to a temporary file and renamed.
  - **The journals:** `journal/<deployment_id>.jsonl`, appended.
  - **No secret** goes in any, only key names.
  - **The backup** is the motherbee's snapshot, which keeps SY.admin's state the same way.
- **One deployment at a time,** in order, as the orchestrator runs node actions one at a time
  anyway. A lock per grouping (design §5.3) keeps a removal from crossing a deployment still under
  way.
- **Its own system ILK,** seeded from the release's lists like every SY node's.

### 2.1 Its records

The design's manifest, which is its record (design §5.3), split in two: the grouping, which is the
why, and each deployment in it.

A grouping, for an ED:

```json
{
  "grouping_id": "ed4qm7k2xa",
  "kind": "ed",
  "tenant_id": "tnt:…",
  "hive": "worker1",
  "ed": {"ilk_id": "ilk:…", "display_name": "Laura Gómez", "company_name": "Acme"},
  "deployments": ["dp7xk3m2qa"],
  "state": "active",
  "history": [{"at_ms": 0, "what": "founded", "by": "cloud", "deployment_id": "dp7xk3m2qa"}]
}
```

A deployment, the plain ED:

```json
{
  "deployment_id": "dp7xk3m2qa",
  "grouping_id": "ed4qm7k2xa",
  "tenant_id": "tnt:…",
  "request_key": "cloud-ed-7f3a…",
  "source": {"id": "plain-ed", "version": "1", "hash": "sha256:…"},
  "blanks": {"display_name": "…", "company_name": "…", "personality": {…},
             "whatsapp": {"phone_number_id": "…", "waba_id": "…", "credential_key": "…"}},
  "nodes": [
    {"role": "conscious", "node_name": "AI.ed4qm7k2xa@worker1", "ilk_id": "ilk:…",
     "runtime": "ai.generic", "runtime_version": "…"},
    {"role": "whatsapp", "node_name": "IO.wapp.ed4qm7k2xa@worker1", "ilk_id": "ilk:…",
     "runtime": "io.wapp", "runtime_version": "…"}
  ],
  "assets": {"conscious": {"role": "…", "handbooks": ["…"], "personality": "…"}},
  "vault": [{"key": "…", "assigned_to": "IO.wapp.ed4qm7k2xa@worker1"}],
  "state": "active",
  "history": [{"at_ms": 0, "what": "deployed", "by": "cloud"}]
}
```

- **The ids:**
  - a grouping's: its kind's two letters (`ed` for an ED; `in` and `ts` kept for infra and test,
    design §5.4), a digit from 2 to 7, and 7 characters of `[a-z2-7]`. The digit keeps an ordinary
    word, such as `inventory2`, from reading as one;
  - a deployment's: `dp` and the same 8 characters' form.
- **Node names come from the grouping's id** (design §5.5): the source gives each node its kind and
  an optional middle part, and SY.deploy writes `<KIND>.<middle.>?<grouping_id>@<hive>`. For the
  plain ED: `AI.ed4qm7k2xa@worker1` for the conscious, and `IO.wapp.ed4qm7k2xa@worker1` for its
  WhatsApp, which matches io.wapp's fan-out pattern `IO.wapp.*@<hive>`. A responsibility's, later:
  `AI.collections.ed4qm7k2xa@worker1`.
- **Runtime versions are pinned:** the hive's `current` at the deployment is written as the exact
  version, in the record and in each `run_node` (design §5.6).
- **`cd_get` adds each node's status,** read with `get_node_status`, as the first reading of drift.

---

## 3. The plain ED's source

**Decided** (§11, call 1): the plain ED's source ships with Fluxbee, inside SY.deploy. It is not
one of Archi's.

- **Why:**
  - it is the native layer, Fluxbee's own, the same for every ED (design §6.2);
  - Archi's sources have no blanks, no tenant, no pinned versions and no way to be read (§0);
  - the operator: *"el nacimiento es con un CI fijo de ED plano, primera capa"*.
- **What it is:** a JSON file in the repository, built into SY.deploy, with its version and its
  content's hash. A new version of it is a Fluxbee release. Moving existing EDs to it is an upgrade,
  which comes later, in waves (design §6.1).
- **Its format is the one every source will compile to,** not one made for the ED (§2). A source
  declares:
  - **`blanks`:** each with a name and a type that carries its check: a name, a personality, a key
    in SY.vault of a given `resource_type` in the deployment's tenant, a WhatsApp number, an ILK of
    the tenant, a business value;
  - **`founds`** (the native layer only): the grouping it founds. For `plain-ed`: an ED, its virtual
    person with its names from the blanks, and its channels: `whatsapp` from its blank, and the
    hand, the node of role `conscious` (§8);
  - **`nodes`:** each with its role, its kind (`AI` or `IO` in this stage), its middle name part,
    its runtime, its config with the blanks in place, and, for an agent, its definition's assets.
    Two keys name the grouping's person: `speaks_as` for its conscious (§7.1) and `io.person_ilk`
    for an IO assigned to it (§9);
  - **`vault`:** which blank's key goes to which node.
- **For `plain-ed`:**
  - **the conscious:**
    - runtime `ai.generic`;
    - `behavior` `{kind: "ai_chat", model: "claude-sonnet-5-5"}`, the installation's model today;
    - the native role and handbook, as asset contents, with the personality from its blank;
  - **the WhatsApp channel:** runtime `io.wapp`, middle part `wapp`, and its config with the blanks
    in place.
- **A source with a section SY.deploy cannot build yet** (a WF, a route, a VPN rule, the user
  policy) is refused before any step, with its name. Each comes with the first source that needs it.
- **Archi's sources with blanks** come with the standard layer, read into the same desired state.

---

## 4. A deployment, step by step: the plain ED's

### 4.1 Checks first, before any step

All are fixed checks, with no AI (design §5.5). The general ones run for every deployment; the
blanks' ones come from each blank's type (§3).

- **General:**
  - **the tenant:** a user tenant, active (not root, deleted, pending or suspended), in the identity
    SHM;
  - **the request key:** new in this tenant, or the same content as before (§1.1);
  - **the source:** one SY.deploy carries, at a released version, with no section it cannot build
    (§3);
  - **the grouping:** of the tenant, and of a kind the source fits. A new one only from a source
    that founds it;
  - **every blank** the source declares is filled, and nothing more;
  - **the AI key:** for each agent the source declares, the tenant has a usable key of its provider,
    `anthropic` for `plain-ed` (its own general one; `vault_list`). Otherwise `AI_KEY_MISSING`,
    before anything is made;
  - **the hive:** a worker that is alive, with room for the deployment's nodes and a margin under
    the router's 1024 (design §5.7);
  - **ids and names:** SY.deploy mints the ids it needs (a new grouping's, the deployment's, and
    one ILK id for each node and for a person it founds), and checks that no SHM entry holds them or
    the node names.
- **The blanks of `plain-ed`:**
  - `display_name` and `company_name` at most 128 bytes, as stage 1's;
  - `whatsapp`: `phone_number_id` is digits, and no other grouping's record holds it; the credential
    key exists in the tenant with `resource_type` `whatsapp` (`vault_list`, metadata only).

Only then does it write the records (`deploying`) and the journal, and answer *accepted*.

### 4.2 The steps

The step builder makes them from the desired state (§2). For `plain-ed`, these. Each step is
written to the journal before it is sent and after its answer. All are SY.admin actions, called one
by one over the mesh.

| # | Step | Arguments |
| --- | --- | --- |
| 1 | `create_virtual_person` | `tenant_id`, `display_name`, `company_name`, **`ilk_id`** (the ED's, minted) |
| 2 | `add_virtual_person_channel` | the ED, `whatsapp`, the `phone_number_id` (§6.2) |
| 3 | `add_virtual_person_channel` | the ED, `hand`, `AI.<grouping_id>@<hive>` (§8) |
| 4 | the assets | the native role and handbook, and the ED's personality, written by SY.deploy (§7.2) |
| 5 | `run_node`, the conscious | `AI.<grouping_id>@<hive>`, `ai.generic`, the pinned version, `tenant_id`, **`ilk_id`**, `config`: `{behavior, speaks_as: <the ED>}` (§7.1) |
| 6 | `set_ilk_definition` | the conscious's ILK: `role_hash`, `handbook_hashes`, `personality_hash` |
| 7 | `run_node`, the IO | `IO.wapp.<grouping_id>@<hive>`, `io.wapp`, the pinned version, `tenant_id`, **`ilk_id`**, `config`: `{wapp.auth: {type: vault_ref, resource_type: whatsapp, key}, io: {phone_number_id, waba_id, dst_node: AI.<grouping_id>@<hive>, person_ilk: <the ED>}}` (§9) |
| 8 | `vault_assign` | the credential key, to the IO only |
| 9 | the records | the deployment and the grouping `active` |

- **The order** follows the design (§4.1): the ED and its channels first, the conscious before its
  IO, so that no message reaches a node that does not exist yet. The hand is written before the
  conscious exists, since its name is known.
- **The credential is assigned to the IO alone** (step 8), after the IO's ILK exists. io.wapp
  reloads it on `VAULT_SECRET_CHANGED`.
- **Each step's wait** is longer than SY.admin's own wait on the orchestrator, 40 s for a
  `run_node`, so SY.deploy never gives up while SY.admin's call still runs (SY.wf-rules' gap, §15).
- **A retry converges; it does not replay** (design §5.3).
  - **When:** after a restart of SY.deploy or of the motherbee, after a timeout, or on a `cd_deploy`
    that resumes a failed deployment.
  - **How:** SY.deploy reads what each step makes: the ILK and its channels in the SHM, the asset
    files, the node through `get_node_status`, the definition's hashes in the SHM, the assignment
    through `vault_list`. It sends only the steps whose result is missing.
  - **The ids:** they are in the journal before step 1, so a retry never mints a second ED or a
    second node ILK.
- **A step that fails for good** leaves the deployment `failed` (§1.2), with the step and its error.

---

## 5. SY.deploy's admission in SY.admin

Decided (design §4.1): SY.deploy's row is bounded, or it would be a second Archi with no operator's
CONFIRM. Decided too (§11 call 4): it is not bounded to what an ED needs, since SY.deploy will
deploy every user solution. The bound is **where** it acts, not **which** solution it deploys.

**The bound: a namespace of names, the launcher's stamp, and the SHM.** SY.admin does not read
SY.deploy's records, which would make each depend on the other.

- **The namespace:** a node whose local name is `<KIND>.<grouping_id>` or ends in
  `.<grouping_id>`, with a grouping id of §2.1's form, of any kind. SY.admin checks the form only,
  so a new kind of grouping needs no change in it.
  - SY.deploy may launch only names in it.
  - Nobody else may launch one: Archi, Cloud, WF and the operator's HTTP alike get
    `NAME_RESERVED`. No one squats a deployment's name, and Archi's solutions cannot take a
    deployment's node as their own (design §5.6).
- **A clause of its own, for `SY.deploy@motherbee` exactly,** next to Archi's and IO.cloud's in
  `authorize_mesh_caller`. It admits whole classes of actions, so that the next solutions need no
  new row:

  | Class | Actions | Admitted when |
  | --- | --- | --- |
  | A node's whole life | `run_node`, `start_node`, `restart_node`, `kill_node` (plain or with `retire_instance`, §10), `get_node_status`, `get_node_config`, `set_node_config`, `node_control_config_set` | a name in the namespace, of kind `AI`, `IO` or `WF`, in a user tenant |
  | A deployment's people | `create_virtual_person`, `add_virtual_person_channel`, `set_ilk_definition`, `delete_ilk`, `restore_ilk` | a user tenant; a virtual person, or an agent ILK whose node is in the namespace (the SHM's `handler_node`) |
  | Its tenant's keys | `vault_list`, `vault_assign` | the deployment's tenant; nodes assigned only in the namespace |

- **What stays out:**
  - **purges,** of a node or of an ILK: the operator's, one at a time (design §2.1);
  - **what is shared by the whole installation** (design §5.6): routes, VPN rules, the user OPA
    policy, hives, runtimes and their versions. Two deployments write over each other there. Each
    gets its row, and its section in the step builder (§2), with the first source that needs it.
- **The SHM checks** run after the admission, next to `refuse_cloud_action_on_a_virtual_ilk`, as
  reads of memory.
- **The stamp:** SY.admin stamps `launched_by` `SY.deploy@motherbee` on its launches, and adds
  `require_launched_by` to every node action of its clause but `run_node`, as it does for Cloud's.
  So SY.deploy reaches only the nodes it launched. The orchestrator checks it today on `kill_node`,
  `start_node`, `restart_node` and `get_node_status` only (§0), and gets it on the config actions
  too.

---

## 6. Identity

### 6.1 A virtual person born with its id

- `ILK_CREATE_VIRTUAL` takes an optional `ilk_id`. With one, the call is idempotent:
  - an active virtual ILK with that id, the same tenant and the same names answers ok with it;
  - any other holder of the id answers `ILK_ID_TAKEN`.
- SY.admin's `create_virtual_person` passes it through. Its help names it.

### 6.2 A virtual person's channels

Stage 1 closed every way to add a channel to a virtual ILK. Its channels were to come in stage 2,
through SY.deploy and SY.admin (stage 1 §2.1).

- **A new verb, `ILK_ADD_VIRTUAL_CHANNEL`,** taken from exactly `SY.admin@motherbee`, primary only,
  as `ILK_CREATE_VIRTUAL`. Its SY.admin action is `add_virtual_person_channel`, for SY.deploy, the
  operator and Archi.
- **On a virtual ILK only,** and of two types in this stage:
  - **`whatsapp`:** the ED's number, as its `phone_number_id`. A sender's key is
    `<phone_number_id>:<wa_id>`, so the two never collide. Identity's uniqueness of a channel's key
    keeps two EDs off the same number (design §3).
  - **`hand`:** the conscious's node name (§8).
- **The other refusals of stage 1 stay:** no merge into a virtual ILK, no channel from an IO, no
  `cloud` channel.

---

## 7. The conscious

### 7.1 It speaks as the ED

Decided (design §7.2): the ED comes from the conscious's own config, written by SY.deploy, never
from the incoming message.

- **ai.generic's config takes `speaks_as`,** the ED's ILK, at the top level.
  - It is read at boot into the runtime's config.
  - A `CONFIG_SET` that changes it is refused: it is set at launch.
- **The AI SDK's `stamp_reply`:** on a solution-kind reply whose `src_ilk` is empty, it stamps
  `speaks_as` when set, and the node's own ILK otherwise.
  - Control replies keep the node's own ILK.
  - The writer stays the conscious (decided).
  - Vault calls keep the node's own ILK.
- **ai.generic checks it in the SHM** before it speaks as the ED, and again at each change of the
  region, which it already polls for its definition:
  - the ILK is a person, virtual, in the node's tenant;
  - its `hand` channel names this node.

  Until all of that holds, as during replication lag, its replies carry its own ILK and its status
  says why. It never fails hard on a lag.
- **The contract test changes on purpose** (design §2.2, 4). A reply carries its agent's own ILK, or
  the person its config says it speaks as, never one copied from the message. The router's test of
  stage 1 changes with §8.

### 7.2 It is born with its definition

**Decided** (§11, call 2):

- SY.deploy writes the conscious's assets on the motherbee, into `<blob>/agent-assets/`, where Archi
  writes them:
  - the native role and handbook, from the source;
  - the ED's personality, from its blank.

  Syncthing carries them to the worker. ai.generic takes the definition once every asset is on its
  hive.
- Archi's asset checks and hashing move to the core library, so both write the same files. They are
  `validate_agent_asset_value` and its builders in `src/bin/sy_architect.rs`. Any source's agent
  uses the same code (§3).
- **The native texts are the operator's,** to approve before the build. They say nothing on whether
  the ED is an AI: the operator left that line out.
- **Born with it,** a deployment from Cloud needs no one.

---

## 8. The router: the hand

**Proposed** (§11, call 3, explained again to the operator on 2026-11-26).

**The problem.** Since stage 1, the router reads each message's `src_ilk`, the person it says it
speaks for. A virtual person's is taken only from a `SY.*` sender; anything else is reported now and
refused after release C. In this stage the conscious speaks as the ED (§7.1):

| Sender | `src_ilk` | What the router does today |
| --- | --- | --- |
| `AI.ed4qm7k2xa@worker1`, Laura's conscious | Laura, the ED | reports it: not a `SY.*` |
| `AI.support@worker1`, any other agent | Laura | reports it: not a `SY.*` |

The router must take the first and keep reporting the second. So it has to know, by itself and on
every message, which node may speak as Laura.

**The proposal: Laura's ILK names her hand.** At the birth, SY.deploy adds to Laura's ILK a channel of
type `hand` whose address is her conscious's name (§4.2, step 3):

| ILK | Channel type | Address |
| --- | --- | --- |
| Laura | `whatsapp` | `1234567890`, her number |
| Laura | `hand` | `AI.ed4qm7k2xa@worker1` |

- **Where it lives:** SY.identity writes it into the identity SHM's channel table, like any channel.
  Every node already reads that memory, the router among them.
- **What the router does with it:** its watcher, which reads the SHM every 200 ms for who is virtual,
  also reads the channel table, and keeps "Laura: hands `AI.ed4qm7k2xa@worker1`". On each message,
  one more lookup:
  - a `SY.*` sender, or a hand of that person: taken;
  - any other sender: reported, as in stage 1.
- **Who writes a hand:** only SY.identity, on SY.admin's `add_virtual_person_channel`, which only
  SY.deploy, the operator and Archi may call. No node can name itself a hand.
- **Who else reads it:** the conscious, before it speaks as Laura (§7.1). One datum, two readers.
- **What it is not:** a way to reach Laura. An IO looks a sender up by its own channel type, such as
  `whatsapp` with the sender's id, so no IO ever finds a `hand`.

**Why not the conscious's config, `speaks_as`:** a node's config is written by whoever launches it.
If the router believed it, any node launched with `speaks_as: Laura` would speak as her. The
permission belongs to Laura's record, which identity owns (*the datum's owner decides*). The router
never reads a node's config anyway.

**Why a channel, not a new field:** a field in the ILK's entry changes the SHM's layout
(`IDENTITY_VERSION` 7). Every binary that reads it must then change at once: the router, the SY
nodes, the Rust and Go SDKs and their nodes. The channel table already exists, with room for a node's
name, and identity already keeps one channel key per tenant, so a node is the hand of one ED of its
tenant at most.

- **Stage 1's extension to the owners of its channels** (stage 1 §3.2) is not needed. An ED's IO
  stamps it as `dst_ilk`, never as `src_ilk` (§9).
- **Still reported:** a WF that forwards the conscious's reply keeps its `src_ilk`, and the rule
  does not take it. No WF stands in an ED's path in this stage; it comes with the responsibilities.
  A responsibility's agent that speaks as the ED is one more hand, added by its deployment.
- **Release C:** proposed right after this stage's gate (§11, call 5).

---

## 9. The IO: assigned to a person by configuration

Decided (design §7.1): an IO is assigned to a person, any person, by its config.

- **The key:** `io.person_ilk`, next to `io.dst_node`.
  - The IO family's common code stamps it as `dst_ilk` on each inbound message
    (`process_message_parts`).
  - It checks it against the SHM on each message, with the lookup's cache: a person of the IO's
    own tenant. Otherwise it is not stamped, and a WARN once a minute names it.
  - io.wapp reads it in this stage, and its contract lists it.
  - The other IOs get it when one needs it (io.slack is the reference IO, and waits for its
    revamp).
- **An ED's own address is left out of the lookup** (stage 1 §7).
  - The common lookup (`ShmIdentityResolver::lookup`) reads the ILK's kind.
  - A hit on a virtual ILK means a message from an ED's own address. It is dropped with an INFO,
    never delivered under the ED's ILK, and never provisioned. SY.identity's refusal to provision
    onto a virtual ILK stays as the fallback.
  - For WhatsApp this cannot happen: an ED's number is not a sender's key (§6.2). It is in place
    for the next channels.
- **The frontdesk gate stays off** on an ED's IO: the ED does the human part (design §9).
- **Infra, once per hive:**
  - **The fan-out endpoint:** the operator's `externalize` with `fanout_family` `IO.wapp.*@<hive>`,
    and `inbound_family` `io.wapp.inbound.v1`. Without the second, every event is dropped (§15).
  - **The company's Meta app** points its webhook at that endpoint.

  Neither is SY.deploy's (design §6.4).

**What a message carries, then:**

- **Inbound:**
  - `src_ilk`: the writer, a temporary ILK in the ED's tenant for a stranger;
  - `dst_ilk`: the ED;
  - to the conscious.
- **The reply:**
  - `src_ilk`: the ED;
  - `dst_ilk`: the writer;
  - the writer field: the conscious;
  - to the IO, which sends it through its reply target.
- **Cognition:**
  - **Unchanged:** the memory's key (the thread and the conscious's ILK), and the episodes, which
    still name the conscious (design §8.2, later).
  - **Changed:**
    - its out-turns' `src_ilk` becomes the ED;
    - SY.storage's `from_ilk` becomes the ED, and `writer_ilk` stays the conscious;
    - the ILK weights move, as the design says (§8.3).

---

## 10. The end: the retire mode

Decided (design §4.3).

- **In the orchestrator:** `kill_node` takes `retire_instance`, exclusive with `purge_instance`. It
  does what a purge does on the hive:
  - stop the node;
  - purge its timers;
  - remove its instance's directory, its uuid file and its map entry.

  Instead of SY.admin's `teardown_node`, it sends `ILK_DELETE` for the node's ILK, which marks it
  with a reason of its own, `node_retired`. The orchestrator may already send that for its own
  hive's nodes.
  - Its vault secrets are kept (*ILKs, secrets and history go only with a whole tenant*).
  - The marked ILK keeps the name, so no other node takes it.
- **A recreate that keeps the ILK:** SY.admin's `restore_ilk`, then `run_node` with that `ilk_id`.
  Both exist; `run_node`'s help names `ilk_id`. In this stage only a converge uses it, for a node
  whose files are gone while its ILK is active.
- **Removing an ED: `cd_remove` of its grouping.** It removes the grouping's deployments, the
  newest first, each node by `kill_node` with `retire_instance`, and then the person it founded:
  1. for `plain-ed`, the IO first, so nothing more comes in;
  2. then the conscious;
  3. `delete_ilk` on the ED, which marks it. Its channels leave the lookups, its hand with them;
  4. the records go to `removed`, with their history.

  A `cd_remove` of the deployment that founded a grouping, alone, is refused while the grouping has
  others: the grouping is removed as a whole (design §5.4).
- **What stays:**
  - the ILKs, marked, with their history;
  - the WhatsApp credential, assigned to a node that no longer runs, for Cloud to delete;
  - pages, until their fixed expiry;
  - cognition's rows.
- **Purging** stays the operator's, one ILK at a time (design §2.1).

---

## 11. The operator's calls

The operator's answers of 2026-11-26:

1. **Decided: the plain ED's source ships with Fluxbee, inside SY.deploy,** not in Archi (§3).
2. **Decided: the conscious is born with its definition** (§7.2): the native role and handbook and
   the ED's personality, written by SY.deploy as agent assets, with Archi's checks shared. The texts
   are the operator's to approve, with no line on whether the ED is an AI.
3. **Pending: the hand, a `hand` channel on the ED's ILK** that the router reads (§8, explained again
   with an example). The other way: a new SHM field, with every binary in lockstep.
4. **Decided: Cloud's four operations come in this stage** (§1). The operator: SY.deploy will deploy
   every solution, not only EDs, so it must not be made so narrow that the next solutions mean a lot
   of new code. Applied:
   - the orders are about deployments (`cd_*`), and an ED's birth is a deployment of `plain-ed`
     into a new ED grouping (§1);
   - the records are groupings and deployments (§2.1);
   - the source format is the one every source compiles to, and one step builder serves them all
     (§2, §3);
   - the admission's namespace is any grouping's, and its clause admits whole classes of actions:
     a node's whole life, a deployment's people, its tenant's keys. Only purges and what the whole
     installation shares stay out (§5).
5. **Pending: release C, `enforce`, right after this stage's gate** (§8). Explained again:
   - **What changes:** the router's binding of `src_ilk` (stage 1 §3) runs today in report mode. It
     reads every message, and when the sender may not speak for that person it writes a
     `would_refuse` line and delivers anyway. In enforce it does not deliver: the sender gets
     `UNREACHABLE` with `SRC_ILK_NOT_BOUND`. Enforce is built and tested; the mode is a constant in
     the code, so turning it on is a release of the router alone.
   - **What it would refuse:** an agent sending with a real person's ILK; an agent other than her
     hand sending as an ED; an IO sending as an ED. **What it takes:** an IO with the person it
     identified; a `SY.*`; an ED's hand as its ED.
   - **Why not before this stage:** until now nothing legitimate sends as an ED. The first is its
     conscious, in this stage, and it is taken only with the hand (call 3). Enforcing before 2C
     would refuse every ED reply.
   - **Why right after it:** from 2C on, a company's people write to an ED, and the ED acts on who
     wrote. That was the panel's high hole (design §17.3): in report mode, a wrong or broken node
     speaking as a person or as an ED is only a log line. 2C's gate reads the report with a real
     ED's traffic, and the report since 0.1.154 holds no line outside the gate's probe.
   - **The other way:** wait longer, for more of the installation's own traffic, or for the first
     responsibility with a WF in an ED's path (§8, still reported). The cost: EDs live with a hole
     the router already knows how to close.
   - Recommended: right after 2C's gate, as one more release with its own gate.

Decided by the earlier talk, and applied here without a new call:

- the steps one by one, with no AI;
- the bounded admission;
- the retire mode, and a recreate that keeps the ILK;
- released, pinned versions;
- the IO assigned to a person by its config;
- the ED's ILK from the conscious's config;
- the writer stays the conscious.

---

## 12. How it ships (METHOD §9)

Shared code goes alone, then items on different binaries, a gate each.

- **Release 2A, the shared code:**
  - **`src/shm`:** the channel table read with the ILK kinds (§8).
  - **The Rust SDK:**
    - the four Cloud rows;
    - the lookup that reads an ILK's kind.
  - **The AI SDK:** `speaks_as` in the runtime's config and in `stamp_reply`.
  - **The IO family's common code:**
    - `io.person_ilk` stamped as `dst_ilk`;
    - a virtual hit dropped.
  - **Behaviour:** none changes with the keys unset. Its gate: the smoke, an agent's reply through
    io.api still carrying the agent's own ILK, and the router's report read.
- **Release 2B, identity, the orchestrator and the router:**
  - **Identity:** `ILK_CREATE_VIRTUAL` with `ilk_id`, and `ILK_ADD_VIRTUAL_CHANNEL`.
  - **SY.admin:** `add_virtual_person_channel`.
  - **The orchestrator:** `retire_instance`; `require_launched_by` checked on the config actions
    too (§5); `ilk_id` in `run_node`'s help.
  - **The router:** the hands in its view and in its rule (report).
  - **Its gate:**
    - a virtual person created twice with one id is one;
    - its two channels are added;
    - a probe under the hand's name is not reported, and one under another `AI.*` name is;
    - a throwaway node retired: its files gone, its ILK marked, its name held;
    - then restored with its ILK.
- **Release 2C, the new node and its users:**
  - **SY.deploy;**
  - **SY.admin:** the four actions, SY.deploy's clause, the namespace;
  - **IO.cloud's translation;**
  - **ai.generic:** `speaks_as` and its check;
  - **io.wapp:** `io.person_ilk`;
  - **Archi:** the SCMD routes and the handbook;
  - **the packaging** (§12.1).

  Its gate is §13.
- **Release C:** `enforce`, after 2C's gate, with its own gate.

### 12.1 Where a new SY node is registered

Read for SY.wf-rules and SY.admin on 2026-11-26. SY.deploy touches each:

- **The release's role lists** (`src/release_settings.rs`, the motherbee's), which feed:
  - the orchestrator's start and watchdog;
  - SY.identity's system ILKs;
  - SY.vault's well-known ILKs.

  Their tests pin them.
- **Packaging:**
  - `packaging/build-deb.sh`: the Rust binary, its unit, its role gate (as `sy-admin|sy-storage`),
    and the package's description;
  - `deb-postinst`: its state directory;
  - `deb-prerm`: the removal stop list;
  - `fluxbee-factory-reset`: its state's reset step.
- **The orchestrator:** `BOX_INSTALL_UNITS`.
- **SY.admin:**
  - the internal action registry and its version;
  - the route to SY.deploy;
  - the mesh clause;
  - the HTTP routes;
  - the help and catalog functions;
  - `docs/nodes/sy-admin.md`.
- **Archi:** `is_mutating_admin_action`, and the command-log exclusions if the actions do not enter
  SY.admin's command log, as `wf_rules_*` do not.
- **The lab:**
  - `scripts/fluxbee_stop.sh`;
  - `lab/ops.py`'s `TIMELINE_UNITS`;
  - `docs/packaging-and-build.md`;
  - a node doc, `docs/nodes/sy-deploy.md`.
- **The system policy** needs no change: admin messages are outside it, and SY.deploy gets no SYSTEM
  authority.

---

## 13. The gate of release 2C

It runs in the tenant `pruebas`, with no real Meta traffic.

- **Before, once:**
  - the WhatsApp fan-out on worker1, with `inbound_family` `io.wapp.inbound.v1` (§9);
  - a test credential through Cloud's `put_token`, `resource_type` `whatsapp`, with a known test
    `app_secret` and a fake `access_token`.
- **The smoke:** a new throwaway node, and a stopped one read.
- **The birth, through Cloud's door:**
  - `cd_deploy` of `plain-ed` into a new ED grouping answers 202 well under IO.cloud's wait;
  - `cd_get` reaches `active`;
  - each step is timed from the journal.
- **Read back:**
  - the ED's ILK is virtual, in `pruebas`, with its `whatsapp` and `hand` channels;
  - the conscious runs with its definition's hashes in the SHM;
  - the IO is `CONFIGURED`;
  - both carry `launched_by` `SY.deploy@motherbee`.
- **A message, through the edge:**
  1. a webhook signed with the test secret reaches the IO;
  2. it goes to the conscious with `dst_ilk` the ED;
  3. the conscious answers with one model call;
  4. the reply carries `src_ilk` the ED and the conscious as its writer;
  5. io.wapp logs its failed Graph send with the trace, since the token is fake. That proves the
     reply reached it with its target.
- **The router:**
  - no `would_refuse` for the conscious;
  - a probe under another `AI.*` name, sending as the ED, is reported.
- **The edges:**
  - `cd_deploy` again with the same key gives the same deployment and ED;
  - another key with the same number is refused;
  - a deployment or grouping of another tenant reads as not found;
  - Cloud's `delete_ilk` on the ED still answers `ILK_VIRTUAL`;
  - a `run_node` from the operator into the namespace answers `NAME_RESERVED`.
- **A restart of SY.deploy in the middle of a second birth:** the birth converges, with one ED and
  one ILK per node.
- **The end:** `cd_remove` of the ED's grouping, and then:
  - the nodes are gone;
  - the three ILKs are marked;
  - the names are held;
  - the credential is kept;
  - the records are `removed`.
- **Cleanup:** the operator purges the gate's ILKs and its credential.
- **The timings:** the answer, each step, the time to `active`, and from the webhook to the Graph
  call.

A real WhatsApp test needs a Meta app with a test number pointed at the installation. That is the
operator's call, and the gate does not need it.

---

## 14. Left for later

| What | Where it goes |
| --- | --- |
| Responsibilities: Archi's sources with blanks, standard and particular | A stage of its own, with Cloud's hiring flow (design §13, 18) |
| Upgrades of deployments, waves, rollback | With the first new version of a source, the plain ED's or another (design §6.1) |
| WFs, routes, VPN rules, the user policy, hives and runtimes: their sections in a source and their rows | With the first source that needs each (§3, §5) |
| Groupings of kind infra and test | When the first is deployed (design §5.4) |
| The profile | Stage 4 (design §10) |
| The email IO | To see (design §7.3) |
| ED–ED talk, a WF in an ED's path | Design §8.5 |
| The instances | Design §13.1 |
| Restoring a retired ED | When asked |
| An AI check of deployments | Design §5.5 |

---

## 15. Seen in the code on the way (recorded in FINDINGS)

- **A-364:** `externalize` with `fanout_family` defaults `inbound_family` to `user`. io.wapp takes
  its events only as `io.wapp.inbound.v1`, and drops a `user` one as a reply with no target. A
  WhatsApp fan-out made without `inbound_family` silently drops every event.
- **A-365:** io.wapp's contract lists `identity.*`, `io.blob.*`, `node.*` and `runtime.*` as optional
  fields it never reads.
- **A-366:** Archi's designer is told that a resource without `ownership` defaults to `external`,
  untouched. The code defaults a node to `solution`, and the reconciler does not read a node's
  ownership.
- **SY.wf-rules gives up on a step before SY.admin's own wait on the orchestrator ends,** with no
  cancel (15 s against 30 s). SY.deploy waits longer than SY.admin (§4.2). Not recorded on its own:
  a WF's apply converges on its next try.
