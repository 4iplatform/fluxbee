# Hive config v1: the release's settings live in the release, the operator's travel in the update

**Status:**

- The direction was agreed with the operator on 2026-11-08, in a design chat that started from
  FINDINGS A-181 (a blob folder turned on after the joins).
- A first draft carried the release's keys inside the update message, applied to each hive's
  `hive.yaml`. A DTAP panel reviewed it the same day: 70 findings, four blockers (§11).
- The operator approved this second draft's shape the same day: *"Sí, vamos como describiste"*.
- **Stage 1 is live and checked on 8.x** (0.1.116): A-186, with A-189's vendored Syncthing.
- **Stage 2 is built** (branch `coa/hive-config-s2`, CI only): the release's settings in the code,
  `src/release_settings.rs` (§3). Its gate on 8.x is next (§10).

This is the single place for the topic.

---

## 1. What it is, from outside

A spoke's `hive.yaml` is written once, at its join: `add_hive` writes it over SSH from the
motherbee's settings of that moment. Nothing writes it again but `set_storage`, which changes
`storage.path` only. So whatever the installation decides after a join never reaches that hive.
Three cases bite today:

- **The AI model.** SY.cognition on worker1 reads its model from worker1's `hive.yaml`, written
  at its join. When the installation moved to Claude (0.1.72), worker1's file was edited by hand;
  its `hive.yaml.pre-claude-0-1-72` is still there.
- **A release's new SY node.** A spoke runs and installs the SY nodes of its own `system_nodes`,
  copied from the motherbee's at its join (`core_component_names_for_role`).
  - A release that adds an SY node for workers puts its binary in `dist/core/worker`, since the
    motherbee's list decides that tree (`materialize_role_core_trees`).
  - No worker installs or runs it until its file is edited by hand.
  - The motherbee's own file needs the same edit: the .deb never touches an existing `hive.yaml`
    (`deb-postinst`). The motherbee on 8.x keeps eight `.bak` and `.pre` copies of such edits.
- **The blob folder (A-181).** Turned on after the joins, it is shared with no spoke. The spokes'
  files still say off, and only a join adds a spoke to a folder.

**The operator's direction:**

- *"Todo lo define una versión de software, si querés cambiar el hive.yaml cuando el sistema está
  funcionando es como un update de software, no me parece mal que se vea así dado que ese archivo
  cambia el comportamiento del soft. Así que quedaría todo dentro del canal de update."*
- *"Lo que sí no puede pasar es que los archivos de config entre hives sean de distinta versión.
  Si cambia versión cambia todo, como un pedazo de software de update."*

**So:**

- What the release defines lives in the release: it comes and goes with the binaries that use it.
- What the operator chooses lives in one installation file, and the update channel takes that file
  to every hive.
- Each hive's `hive.yaml` is its own.

## 2. Where each setting lives

| Setting | Lives in | Who changes it | How it reaches a hive |
| --- | --- | --- | --- |
| `hive_id`, `role` | the hive's `hive.yaml` | the join | never travels |
| The control path: `wan.*`, `identity.sync.*` | the hive's `hive.yaml` | the join | never travels: a wrong value cuts the mesh that would carry the fix |
| Local: `storage.path`, `blob.path`, `blob.sync.api_port`, `blob.sync.data_dir`, `dist.path`, a worker's `egress`, the ingress's `edge` and SSH table, the motherbee's `admin`, `architect`, `acme`, `web`, `egress` | the hive's `hive.yaml` | the join; the hive's own actions (`set_storage`) | never travels |
| `system_nodes` by role, with its order and waits | the release: the code, by role | a release | with the core's binaries |
| The blob's and dist's topology by role (`enabled`, `sync.enabled`, `public_enabled`, the tool) | the release: the code, by role | a release | with the core's binaries |
| The frontdesk's name (`government.identity_frontdesk`) | the release: the code, by role | a release | with the core's binaries |
| `ai`: the default provider and each provider's model | the installation file | the operator, through SY.admin (Archi) | Syncthing, then `SYSTEM_UPDATE` category `config` |
| `blob.gc.*` | the motherbee's `hive.yaml` | the operator, on the motherbee | never travels: a worker's GC cannot pin publications (A-188) |

## 3. The release's settings, in the code

- **One table in the code, by role:** the motherbee, the worker, the ingress and the egress, with
  each role's values of today (§12's templates).
  - The orchestrator runs, installs and watches its role's nodes from it.
  - The motherbee materializes each role's core tree from it (`materialize_role_core_trees`).
  - The blob and dist reconcile reads its topology from it.
  - Golden tests hold each role's values to what today's join templates write.
- **A list and its binaries arrive together:** the new list is in the new orchestrator, and its
  binaries are in the same core.
  - A rollback (`SYSTEM_CORE_ROLLBACK`) or a downgrade takes the list back with the binaries.
  - No orchestrator ever judges another release's settings.
  - Nothing is merged into a file at the install.
- **A node a release drops** is stopped, disabled and removed by the core update, unit and
  binary. The backup generation keeps both, so a rollback brings it back. A rollback stops a node
  it removes before deleting its binary (D-4, P-16, A-9, T-8).
  - **As built (stage 2):** a spoke's core update runs in the previous release's orchestrator,
    which knows only its own list. It installs what dist holds of that list, and leaves out a
    component dist no longer holds, only when the update replaces sy-orchestrator
    (`core_update_sets`). The new orchestrator finishes the update at its start, before its nodes
    start (`settle_core_generation`): it drops each core unit its list no longer names, and installs
    each node of its list `/usr/bin` lacks, a node the release added, recorded as created by the
    update. Both go into the update's generation, so the same rollback undoes them.
- **The frontdesk's name stays as each role has it today:**
  - a worker names `SY.frontdesk.gov@motherbee`;
  - the ingress and the egress have none, so their router defaults to a frontdesk on their own
    hive, where none runs.

  Changing that is a call of its own (A-15, T-11).
- **The old keys in a `hive.yaml`** are ignored, and the orchestrator reports them once. The join
  and `hive.yaml.example` stop writing them.

## 4. The operator's settings, in one installation file

- **One file, owned by the motherbee:** today it holds `ai`.
  - It carries its version, a number raised at each change, and every hive's copy has the same
    hash.
  - It is distributed like the policy: a dist folder that the motherbee sends and every hive
    receives, with members that follow the vendor folder's (A-72's pattern).
- **Its readers read it there,** never from `hive.yaml`:
  - on the motherbee: SY.admin (its executor), SY.architect, SY.cognition, SY.frontdesk.gov, and
    ai.generic at each request;
  - on a worker: SY.cognition, and ai.generic at each request;
  - rt-gateway parses `ai` today only to check it, and stops.
- **The operator's change**, an SY.admin action that Archi can call:
  1. **SY.admin checks the new settings** with every reader's rules: `HiveAiConfig::effective` and
     each provider's model.
     - It refuses to switch the default provider while a node names a model of the old one, and
       names those nodes (A-11).
     - It refuses while a join is running (A-12).
  2. **The motherbee's orchestrator writes the file,** the only writer, atomically, with the next
     version.
  3. **SY.admin runs the installation's update for it** (§6). Each hive answers `sync_pending` until
     its copy's hash is the new one. Then it restarts only the readers of the settings that
     changed, by role.
  4. **On the motherbee, SY.admin restarts last,** after the update has reported.
- **An operator's version is undone by another one:** SY.admin keeps each version's values, so
  Archi can put the previous ones back. A core rollback never touches the file (D-8, A-6, P-10).
- **A hand edit of the file is caught:** the motherbee records each version's hash, and a file
  that no longer matches is reported in `/versions`. Changes are refused until the operator makes
  the edit a version or restores the file (A-7).
- **The move from today:**
  - The first release of this stage writes the motherbee's current `ai` as version 1. On 8.x it
    lists both providers, as worker1's hand-edited copy does.
  - The `ai` copies in each `hive.yaml` are then ignored and reported.

## 5. `hive.yaml`, the hive's own

- **It holds** the hive's identity, its control path, and its local paths, ports and sections.
- **One writer**, under one lock, atomically: the join and the hive's own actions. `set_storage`
  wrote it with a plain `fs::write` (D-14, P-17, A-13).
- **The join writes it** without the release's keys and without `ai`.

## 6. The installation's update, SY.admin's

- **One SY.admin action updates every hive:** the core (a release), the installation file (an
  operator's change), or both (A-10).
  - It keeps its state on disk: open until every hive is on the version, the hives not on it
    named, and the update resumed after SY.admin restarts.
  - `ops.py deploy` and Archi call it.
- **Per hive:**
  - **Core:** today's `SYSTEM_UPDATE` with category `core`. A core update that replaces nothing
    restarts nothing (D-3, P-4, T-3).
  - **Config:** `SYSTEM_UPDATE` with category `config` and the file's hash. It has no binary step,
    and it restarts only the readers of what changed.
- **The automatic step:** a hive that comes back while an update is open gets that update
  finished, once per hive and version. The rules (A-4, P-12, T-17):
  - SY.admin sends it, under policy rule (2).
  - It waits through `sync_pending`, with backoff, up to a limit.
  - It never acts on a hive the operator rolled back (`core.installed.source` is `core_rollback`),
    a hive ahead of the motherbee, or a hand-edited one. Those are reported.
  - Each outcome goes to SY.admin's log and to `/versions`.
- **The one allowed divergence:** a `core_rollback` on one hive. `/versions` shows it as rolled back
  by the operator, and the automatic step leaves it alone (A-5).
- **The installation's rollback:** install the previous release on the motherbee, then run the
  installation's update.

## 7. `/versions`

- **Per hive:** the core, as today, plus `config` with:
  - the version;
  - the hash of the hive's copy;
  - its state: current, behind, pending or edited by hand.
- **Each hive is compared** with the motherbee's file.
- **An update's result says `not_answered`** for a hive that did not answer, never "not applied",
  and points to `/versions` (P-13).

## 8. A-186, A-181 and A-188 in this design

- **A-186: the start never stops on a dist mismatch.**
  - The orchestrator starts Syncthing before it checks dist, then starts on what is installed in
    `/usr/bin`.
  - It reports that it is waiting for its update.
  - This also covers a list that names a binary dist no longer has (A-8, T-4).
- **A-181's blob:** the topology goes by role in the code (§3), and the motherbee's blob folders
  follow the dist folders' members, as agent-assets does (A-72).
  - `fluxbee-blob` follows the workers' (`fluxbee-dist-runtimes`).
  - `fluxbee-blob-public` follows the ingress's core folder.
  - Removing a hive takes it out of both folders, flags or not (T-21).
- **A-188:** the blob GC runs only where it can pin publications, on the motherbee.

## 9. The operator's calls (2026-11-08)

- **On the first draft:**
  - The motherbee's file takes the release's keys at the install: yes, under the version rule.
    This draft replaces it: the release's keys no longer live in the file.
  - The keys stay as listed.
  - The blob's switch goes.
  - The worker that joined before the egress (`egress.gateway_ip`) is apart, to be talked over
    later.
- **The version rule:** *"no puede pasar que los archivos de config entre hives sean de distinta
  versión"*. This draft keeps it by construction: one file, one version, and SY.admin's update
  stays open until every hive has it.
- **This draft's shape:** *"Sí, vamos como describiste"*. That covers the release's settings in
  the code, the operator's in one installation file, `hive.yaml` as the hive's own, and the
  bounded automatic step.

## 10. The plan, by stages

Each stage is a release with its gate on 8.x, and the next starts only after it. Each stage's tests
run in CI over temporary roots, so no sequence runs first on PROD (T-2).

1. **A-186: the start never stops on a dist mismatch** (the orchestrator).
   - Unit tests: Syncthing starts before the check; a missing or mismatched dist binary is
     reported, not fatal.
   - Gate: on egress1, the orchestrator restarted with one dist binary set aside. It must start,
     report it, and run Syncthing.
2. **The release's settings into the code** (the orchestrator, the join, the reconcile), with
   today's values per role.
   - Golden tests against the three join templates and the example.
   - Tests that a node a release drops is stopped and removed, and that a rollback brings it back.
   - Gate: a deploy changes nothing in `/versions` or `ops.py health`. A throwaway worker joins
     8.x (a VM, removed after) and gets a `hive.yaml` without the release's keys.
3. **The installation file and the operator's action** (the readers, SY.admin, the folder,
   `SYSTEM_UPDATE` category `config`, `/versions`).
   - Gate: a reversible change on 8.x, `ai.providers.openai.model` (not the default provider's)
     set and back. Every hive must be current, the readers restarted only where they read it,
     AI.pruebas still answering, and the action's reply arriving.
4. **The installation's update in SY.admin,** with the automatic step. `ops.py deploy` calls it.
   - Gate: a deploy through it.
   - Gate: egress1 down across a config change, then back: finished once and reported.
   - Gate: a rolled-back egress1 is left alone.
5. **A-181's blob members, and A-188.**
   - A-72's tests for both blob folders, and a removal without flags.
   - Gate: a throwaway worker. Its device is taken out of `fluxbee-blob` through Syncthing's REST;
     the reconcile must restore it, and `remove_hive` must leave both folders.

## 11. The DTAP panel (2026-11-08)

Four reviewers, one per lens, each re-checking its findings in the code, reviewed the first draft.
They raised 70 findings, many of them the same seen from several lenses:

| Lens | Blockers | Major | Minor |
| --- | --- | --- | --- |
| D, development | 2 | 10 | 4 |
| T, test | 1 | 17 | 3 |
| A, acceptance | 2 | 9 | 5 |
| P, production | 3 | 10 | 4 |

**The four blockers, and what this draft does with them:**

1. **The receiving orchestrator was the previous release's** (D-1, P-3, A-2, T-1, T-13). No
   message carries the release's settings any more: they are in the release (§3).
2. **The file could name binaries the hive lacks,** and the start then stopped for good (D-2,
   P-1, A-1, T-4, A-8). The list and its binaries arrive together (§3), and the start no longer
   stops on a dist mismatch (A-186, §8).
3. **An update's rollbacks restored the binaries and not the file** (P-2, A-1, T-7). The release's
   settings roll back with the binaries (§3). A core rollback never touches the operator's file
   (§4).
4. **An operator's change rode the core path** (D-3, P-4, P-5, A-3, T-3). It travels as category
   `config`, with no binary step and no core restart (§6).

**Found on the way, true today:** A-186, A-187 (fixed on daily, CI only) and A-188.

**The other findings** went into §3 to §10. Each section names the findings it answers.

**Dropped:** D-7's worker1 case. It read §1's example as 8.x, and on 8.x worker1's blob sync is on.

## 12. The update path today (read 2026-11-08)

- **The deploy** (`lab/ops.py deploy`):
  1. It installs the .deb on the motherbee.
  2. For each spoke, it sends `POST /hives/{hive}/update` with category `core` and the new core
     manifest's hash.
  3. It waits until each spoke reports the version in `/versions`.
- **SY.admin** turns that call into `SYSTEM_UPDATE` `{category, manifest_version,
  manifest_hash, runtime?}` to `SY.orchestrator@<hive>` (`src/bin/sy_admin.rs:18763`).
- **The spoke's orchestrator** (`handle_system_update_message`, `src/bin/sy_orchestrator.rs:5417`):
  - **The check:** it compares the expected manifest with the one Syncthing delivered, and
    answers `sync_pending` while they differ.
  - **The core:** `apply_core_update_replying_first` (`:5273`) runs `prepare_core_update`
    (`:5049`): a backup generation with its stamp, then the swap into `/usr/bin`. It answers
    `restarting` and restarts in the background (`restart_local_core_services_with_health_gate`,
    `:4337`). The outcome lands in `/versions` → `core.last_update`.
  - **The components** it installs come from its own file's `system_nodes`
    (`core_component_names_for_role`, `:23250`).
  - **The motherbee's lists** decide each role's `dist/core/<role>` tree
    (`materialize_role_core_trees`, `:23366`).
- **`/versions`** (`local_versions_snapshot`, `:13359`): the core's manifest hash, its components,
  what is installed, the last update and the rollback generations. It shows nothing of the config.
- **The join** writes a spoke's whole `hive.yaml` over SSH, from three templates:
  - the worker's: `:25968`, written at `:25992`;
  - the egress's: `:26785`;
  - the ingress's: `:27542`.

  Then `ADD_HIVE_FINALIZE` runs on the spoke (`:24236`).
- **After the join,** only `set_storage` writes the file, and only `storage.path`. It round-trips
  the YAML through serde, so the comments go.
- **The motherbee's file at an install:** `deb-postinst` copies the example only when there is no
  `hive.yaml`.
- **The start** (`bootstrap_local`, `:1658`) checked its role's binaries in `dist/core/bin` before
  it started Syncthing, and `main` exited on a mismatch (A-186). Since 0.1.116 it checks after
  Syncthing starts, and only reports: `core.awaiting_update`.
- **Who reads the shared keys, and when:**
  - blob and dist: the reconcile, at every pass (`current_blob_runtime_config`, `:6306`;
    `current_dist_runtime_config`, `:6320`);
  - the watchdog's list: read at the orchestrator's start (`:971`);
  - `ai`: at the start, SY.cognition, SY.admin, SY.architect, SY.frontdesk.gov, and rt-gateway
    for its check only; ai.generic at each request;
  - `government.identity_frontdesk`: the router and SY.identity, at their start.
