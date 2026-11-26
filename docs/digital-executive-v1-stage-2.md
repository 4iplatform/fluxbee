# The Digital Executive v1, stage 2: SY.deploy and a plain ED's birth

**Status:** the build design, draft of 2026-11-26. It waits for the operator's calls (§11). Nothing
is built.

**What it builds:** stage 2 of [digital-executive-v1-design](digital-executive-v1-design.md) §15:

- **A minimal SY.deploy,** the CD node (design §5).
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

| Action | HTTP (operator) | Cloud op | Archi | What it does |
| --- | --- | --- | --- | --- |
| `create_ed` | `POST /eds` | `create_ed` | SCMD | Births a plain ED. Answers *accepted* with its id. |
| `get_ed` | `GET /eds/{ed_id}` | `get_ed` | SCMD | One ED's record and state. |
| `list_eds` | `GET /eds` | `list_eds` | SCMD | The EDs, by tenant. |
| `retire_ed` | `POST /eds/{ed_id}/retire` | `retire_ed` | SCMD | Ends an ED (§10). Answers *accepted*. |

- **SY.admin forwards each one to `SY.deploy@motherbee`,** as it does a WF's to SY.wf-rules: a write
  as a `command`, a read as a `query`. It waits at most 10 s, since SY.deploy answers before it
  starts any step.
- **SY.deploy takes orders only from `SY.admin@motherbee`,** as SY.wf-rules does.
- **Cloud.** Four rows join `CLOUD_OP_ACTIONS` (`crates/fluxbee_sdk/src/cloud.rs`), the one table
  from which IO.cloud's translation and SY.admin's allowlist both come.
  - IO.cloud puts the tenant Cloud claims in each.
  - SY.deploy answers *not found* for an ED of another tenant.
  - The contract goes in [io-cloud-api](io-cloud-api.md), for Fluxbee Cloud's team.
- **Cloud's flow:**
  1. `put_token` with `resource_type` `whatsapp`, in the ED's tenant;
  2. `create_ed`, naming that key;
  3. `get_ed`, until the state is `active` or `failed`.

### 1.1 `create_ed`

```json
{
  "tenant_id": "tnt:…",
  "request_key": "cloud-ed-7f3a…",
  "display_name": "Laura Gómez",
  "company_name": "Acme",
  "personality": {
    "timezone": "America/Argentina/Buenos_Aires",
    "country_code": "AR",
    "primary_language": "es",
    "additional_languages": ["en"],
    "description": "…", "biographical": {…}, "narrative": {…}
  },
  "whatsapp": {"phone_number_id": "1234567890", "waba_id": "…", "credential_key": "acme-laura-wapp"},
  "hive": "worker1"
}
```

- **`request_key` is the idempotency key** (stage 1 §7): Cloud's own id for the ED, at most 64
  characters of `[A-Za-z0-9._:-]`.
  - The same key in the same tenant gives back the same ED.
  - The same key with other content answers `REQUEST_KEY_REUSED`.
- **`personality`** takes the fields of Archi's personality asset (`build_personality_asset_value`,
  `src/bin/sy_architect.rs`): its system fields, and its optional texts. Its name is the ED's.
- **`hive`** is optional. Without it, SY.deploy picks the worker with the most room.
- **No secret travels in the order:** the WhatsApp credential is a key name, put before by Cloud or
  the operator.
- **The answer:** HTTP 202, `{"ed_id": "edq4m7k2xa", "ed_ilk": "ilk:…", "state": "deploying"}`.

### 1.2 An ED's states

`deploying` → `active`; `deploying` → `failed`; `active` or `failed` → `retiring` → `retired`.

- **A failed birth stays `failed`,** with the step, its code and its detail, and nothing is undone
  by itself (*no magic automation*).
- **From `failed` there are two ways out:**
  - `create_ed` again with the same `request_key` resumes where it stopped (§4.2);
  - `retire_ed` ends what was made.

---

## 2. SY.deploy, the node

- **A Rust system node,** `src/bin/sy_deploy.rs`.
  - It reads the identity SHM with the core's reader, which it needs for every check (§4.1).
  - It shares Archi's agent-asset code, moved to the core library (§7.2).
  - Its admin calls use the SDK's admin RPC, as ai.generic's `publish_html_page` does.
- **On the motherbee only,** as SY.admin, behind the same role gate in its unit.
- **Its state is files,** as SY.wf-rules':
  - **Where:** `/var/lib/fluxbee/state/sy-deploy/`.
  - **The records:** `eds/<ed_id>.json`, written to a temporary file and renamed.
  - **The journals:** `journal/<ed_id>.jsonl`, appended.
  - **No secret** goes in either, only key names.
  - **The backup** is the motherbee's snapshot, which keeps SY.admin's state the same way.
- **One deployment at a time,** in order, as the orchestrator runs node actions one at a time
  anyway. A lock per ED keeps a `retire_ed` from crossing a birth still under way.
- **Its own system ILK,** seeded from the release's lists like every SY node's.

### 2.1 A deployment's record

The design's manifest, which is its record (design §5.3), for a plain ED:

```json
{
  "ed_id": "edq4m7k2xa",
  "grouping": {"kind": "ed", "ed_ilk": "ilk:…", "name": "Laura Gómez"},
  "tenant_id": "tnt:…",
  "request_key": "cloud-ed-7f3a…",
  "source": {"id": "plain-ed", "version": "1", "hash": "sha256:…"},
  "blanks": {"display_name": "…", "company_name": "…", "personality": {…},
             "whatsapp": {"phone_number_id": "…", "waba_id": "…", "credential_key": "…"}},
  "hive": "worker1",
  "nodes": [
    {"role": "conscious", "node_name": "AI.edq4m7k2xa@worker1", "ilk_id": "ilk:…",
     "runtime": "ai.generic", "runtime_version": "…"},
    {"role": "whatsapp", "node_name": "IO.wapp.edq4m7k2xa@worker1", "ilk_id": "ilk:…",
     "runtime": "io.wapp", "runtime_version": "…"}
  ],
  "assets": {"role": "…", "handbooks": ["…"], "personality": "…"},
  "state": "active",
  "history": [{"at_ms": 0, "what": "created", "by": "cloud"}]
}
```

- **The ED's id** is `ed` plus 8 characters of `[a-z2-7]`. Its node names come from it (design
  §5.5): `AI.<ed_id>@<hive>` for the conscious, and `IO.wapp.<ed_id>@<hive>` for its WhatsApp. The
  second matches io.wapp's fan-out pattern `IO.wapp.*@<hive>`.
- **Runtime versions are pinned:** the hive's `current` at the birth is written as the exact
  version, in the record and in each `run_node` (design §5.6).
- **`get_ed` adds each node's status,** read with `get_node_status`, as the first reading of drift.

---

## 3. The plain ED's source

**Proposed** (§11, call 1): the plain ED's source ships with Fluxbee, inside SY.deploy. It is not
one of Archi's.

- **Why:**
  - it is the native layer, Fluxbee's own, the same for every ED (design §6.2);
  - Archi's sources have no blanks, no tenant, no pinned versions and no way to be read (§0);
  - the operator: *"el nacimiento es con un CI fijo de ED plano, primera capa"*.
- **What it is:** a JSON file in the repository, built into SY.deploy, with its version and its
  content's hash. A new version of it is a Fluxbee release. Moving existing EDs to it is an upgrade,
  which comes later, in waves (design §6.1).
- **What it declares:**
  - **its blanks** (§1.1);
  - **the conscious:**
    - runtime `ai.generic`;
    - `behavior` `{kind: "ai_chat", model: "claude-sonnet-5-5"}`, the installation's model today;
    - the native role and handbook, as asset contents;
  - **the WhatsApp channel:** runtime `io.wapp`, and its config with the blanks in place.
- **Its format** is SY.deploy's own, and minimal. Archi's sources with blanks get theirs with the
  standard layer, which may grow from this one.

---

## 4. The birth, step by step

### 4.1 Checks first, before any step

All are fixed checks, with no AI (design §5.5):

- **The tenant:** a user tenant, active (not root, deleted, pending or suspended), in the identity
  SHM.
- **The names:** `display_name` and `company_name` at most 128 bytes, as stage 1's.
- **The request key:** new in this tenant, or the same content as before (§1.1).
- **WhatsApp:**
  - `phone_number_id` is digits, and no ED of the records holds it;
  - the credential key exists in the ED's tenant with `resource_type` `whatsapp` (`vault_list`,
    metadata only).
- **The AI key:** the tenant has a usable key of the conscious's provider, `anthropic` (its own
  general one; `vault_list`). Otherwise `AI_KEY_MISSING`, before anything is made.
- **The hive:** a worker that is alive, with room for the ED's two nodes and a margin under the
  router's 1024 (design §5.7).
- **Ids and names:** SY.deploy mints the ED's id and the three ILK ids (the ED's, the conscious's,
  the IO's), and checks that no SHM entry holds them or the node names.

Only then does it write the record (`deploying`) and the journal, and answer *accepted*.

### 4.2 The steps

Each step is written to the journal before it is sent and after its answer. All are SY.admin
actions, called one by one over the mesh.

| # | Step | Arguments |
| --- | --- | --- |
| 1 | `create_virtual_person` | `tenant_id`, `display_name`, `company_name`, **`ilk_id`** (the ED's, minted) |
| 2 | `add_virtual_person_channel` | the ED, `whatsapp`, the `phone_number_id` (§6.2) |
| 3 | `add_virtual_person_channel` | the ED, `hand`, `AI.<ed_id>@<hive>` (§8) |
| 4 | the assets | the native role and handbook, and the ED's personality, written by SY.deploy (§7.2) |
| 5 | `run_node`, the conscious | `AI.<ed_id>@<hive>`, `ai.generic`, the pinned version, `tenant_id`, **`ilk_id`**, `config`: `{behavior, speaks_as: <the ED>}` (§7.1) |
| 6 | `set_ilk_definition` | the conscious's ILK: `role_hash`, `handbook_hashes`, `personality_hash` |
| 7 | `run_node`, the IO | `IO.wapp.<ed_id>@<hive>`, `io.wapp`, the pinned version, `tenant_id`, **`ilk_id`**, `config`: `{wapp.auth: {type: vault_ref, resource_type: whatsapp, key}, io: {phone_number_id, waba_id, dst_node: AI.<ed_id>@<hive>, person_ilk: <the ED>}}` (§9) |
| 8 | `vault_assign` | the credential key, to the IO only |
| 9 | the record | `active` |

- **The order** follows the design (§4.1): the ED and its channels first, the conscious before its
  IO, so that no message reaches a node that does not exist yet. The hand is written before the
  conscious exists, since its name is known.
- **The credential is assigned to the IO alone** (step 8), after the IO's ILK exists. io.wapp
  reloads it on `VAULT_SECRET_CHANGED`.
- **Each step's wait** is longer than SY.admin's own wait on the orchestrator, 40 s for a
  `run_node`, so SY.deploy never gives up while SY.admin's call still runs (SY.wf-rules' gap, §15).
- **A retry converges; it does not replay** (design §5.3).
  - **When:** after a restart of SY.deploy or of the motherbee, after a timeout, or on a `create_ed`
    that resumes a failed birth.
  - **How:** SY.deploy reads what each step makes: the ILK and its channels in the SHM, the asset
    files, the node through `get_node_status`, the definition's hashes in the SHM, the assignment
    through `vault_list`. It sends only the steps whose result is missing.
  - **The ids:** they are in the journal before step 1, so a retry never mints a second ED or a
    second node ILK.
- **A step that fails for good** leaves the ED `failed` (§1.2), with the step and its error.

---

## 5. SY.deploy's admission in SY.admin

Decided (design §4.1): SY.deploy's row is bounded, or it would be a second Archi with no operator's
CONFIRM.

**Proposed bound: a namespace of names, the launcher's stamp, and the SHM.** SY.admin does not read
SY.deploy's records, which would make each depend on the other.

- **The namespace:** a node whose local name ends in `.<ed_id>` or is `<KIND>.<ed_id>`, with an
  `ed_id` of §2.1's form.
  - SY.deploy may launch only names in it.
  - Nobody else may launch one: Archi, Cloud, WF and the operator's HTTP alike get
    `NAME_RESERVED`. No one squats an ED's name, and Archi's solutions cannot take an ED's node as
    their own (design §5.6).
- **A clause of its own, for `SY.deploy@motherbee` exactly,** next to Archi's and IO.cloud's in
  `authorize_mesh_caller`:

  | Action | Admitted when |
  | --- | --- |
  | `create_virtual_person`, `add_virtual_person_channel` | a user tenant |
  | `run_node` | a name in the namespace, `AI.*` or `IO.*`, a user tenant, with `ilk_id` |
  | `kill_node` | `retire_instance` only (§10), in the namespace |
  | `get_node_status` | in the namespace |
  | `set_ilk_definition` | an agent ILK whose node is in the namespace (the SHM's `handler_node`) |
  | `delete_ilk` | a virtual ILK, or an agent ILK whose node is in the namespace |
  | `vault_list`, `vault_assign` | the deployment's tenant; nodes assigned only in the namespace |

  Nothing else: no config writes, no purge, no restore, no route or VPN rule, no policy. Those come
  with upgrades and responsibilities, each with its row.
- **The SHM checks** run after the admission, next to `refuse_cloud_action_on_a_virtual_ilk`, as
  reads of memory.
- **The stamp:** SY.admin stamps `launched_by` `SY.deploy@motherbee` on its launches, and adds
  `require_launched_by` to its `kill_node` and `get_node_status`, as it does for Cloud's. So
  SY.deploy reaches only the nodes it launched.

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

**Proposed** (§11, call 2):

- SY.deploy writes the conscious's assets on the motherbee, into `<blob>/agent-assets/`, where Archi
  writes them:
  - the native role and handbook, from the source;
  - the ED's personality, from the order.

  Syncthing carries them to the worker. ai.generic takes the definition once every asset is on its
  hive.
- Archi's asset checks and hashing move to the core library, so both write the same files. They are
  `validate_agent_asset_value` and its builders in `src/bin/sy_architect.rs`.
- **The native texts are the operator's,** to approve before the build. One line they carry: asked
  sincerely, the ED says it is a digital executive, an AI, and never claims to be human.
- **The other way:** the conscious is born with no definition, answering "unconfigured", and the
  operator gives it one with Archi (notes §6: *"dejando el esqueleto vacío para que archi le asigne
  las herramientas"*). An ED born from Cloud would then wait for the operator.

---

## 8. The router: the hand

**Proposed** (§11, call 3): the conscious is named as the ED's hand by a channel of type `hand` on
the ED's ILK, whose address is the conscious's node name (§6.2).

- **No layout change:**
  - the SHM keeps the channel like any other;
  - the router's watcher reads the channel table in the same seqlock loop as its ILK kinds;
  - the router's view keeps, for each virtual person, the names of its hands.
- **The rule:** a virtual person's `src_ilk` is accepted from a `SY.*` sender (stage 1) or from one
  of its hands. Everything else is reported, as in stage 1.
- **Why identity and not the node's config:** the router must know it without reading any node, and
  identity owns a person's data (*the datum's owner decides*). Identity's uniqueness of a channel's
  key makes a node the hand of one ED at most.
- **Stage 1's extension to the owners of its channels** (stage 1 §3.2) is not needed. An ED's IO
  stamps it as `dst_ilk`, never as `src_ilk` (§9).
- **The other way:** a hand field in the identity SHM's `IlkEntry`. That is a layout change, with
  `IDENTITY_VERSION` 7 and every reader in lockstep: the router, the SY nodes, the Rust and Go SDKs
  and their nodes.
- **Still reported:** a WF that forwards the conscious's reply keeps its `src_ilk`, and the rule
  does not take it. No WF stands in an ED's path in this stage; it comes with the responsibilities.
- **Release C:** proposed right after this stage's gate (§11, call 5). By then the conscious is a
  real sender of a virtual ILK, and the gate has read the report with it.

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
- **`retire_ed`'s steps:**
  1. `kill_node` with `retire_instance` on the IO, so nothing more comes in;
  2. the same on the conscious;
  3. `delete_ilk` on the ED, which marks it. Its channels leave the lookups, its hand with them;
  4. the record goes to `retired`, with its history.
- **What stays:**
  - the ILKs, marked, with their history;
  - the WhatsApp credential, assigned to a node that no longer runs, for Cloud to delete;
  - pages, until their fixed expiry;
  - cognition's rows.
- **Purging** stays the operator's, one ILK at a time (design §2.1).

---

## 11. The operator's calls (pending)

1. **The plain ED's source ships with Fluxbee, inside SY.deploy,** not in Archi (§3).
   - Recommended: yes. Archi's sources cannot carry it today.
2. **The conscious is born with its definition** (§7.2).
   - SY.deploy writes the native role and handbook and the ED's personality as agent assets, with
     Archi's checks shared. The texts are the operator's to approve, with the line on saying it is
     an AI when asked sincerely.
   - The other way: born empty, configured later with Archi.
   - Recommended: born with it, so that a birth from Cloud needs no one.
3. **The ED's channels in identity, its hand among them** (§6.2, §8).
   - A `hand` channel names the conscious, and the router reads it, with no layout change.
   - The other way: a new SHM field, with every binary in lockstep.
   - Recommended: the channel.
4. **Cloud's four operations come in this stage** (§1).
   - The contract goes to Cloud's team; their screens are theirs.
   - Recommended: yes; the gate drives them through Cloud's door.
5. **Release C (`enforce`) right after this stage's gate** (§8).
   - Recommended: yes, as one more release with its own gate.

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
  - **The orchestrator:** `retire_instance`, and `ilk_id` in `run_node`'s help.
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
  - `create_ed` answers 202 well under IO.cloud's wait;
  - `get_ed` reaches `active`;
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
  - `create_ed` again with the same key gives the same ED;
  - another key with the same number is refused;
  - an ED of another tenant reads as not found;
  - Cloud's `delete_ilk` on the ED still answers `ILK_VIRTUAL`;
  - a `run_node` from the operator into the namespace answers `NAME_RESERVED`.
- **A restart of SY.deploy in the middle of a second birth:** the birth converges, with one ED and
  one ILK per node.
- **The end:** `retire_ed`, and then:
  - the nodes are gone;
  - the three ILKs are marked;
  - the names are held;
  - the credential is kept;
  - the record is `retired`.
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
| Upgrades of EDs, waves, rollback | With the first new version of the plain ED (design §6.1) |
| Config writes, routes, VPN rules and policy in SY.deploy's row | With the responsibilities that need them (§5) |
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
