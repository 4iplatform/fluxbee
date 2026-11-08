# Hive config v1: a hive's shared config travels in the update

**Status:**

- The direction was agreed with the operator on 2026-11-08, in a design chat that started from
  FINDINGS A-181 (a blob folder turned on after the joins).
- The operator answered the first draft's open points the same day (§9), and added the rule of §7:
  every hive's config is at one version.
- The update path's current state (§10) and a staged plan (§11) were written, and a DTAP panel
  reviewed them the same day.
- **The panel found four blockers** in §1–§11. §12 has them, and a proposal for the second draft
  that the operator decides.
- Nothing is built.

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
- **A release's new SY node.** A spoke runs the SY nodes of its own `system_nodes`, copied from
  the motherbee's at its join, and its core update installs the components of that list
  (`core_component_names_for_role`).
  - A release that adds an SY node for workers puts its binary in `dist/core/worker`, since the
    motherbee's list decides that tree (`materialize_role_core_trees`).
  - No worker installs or runs it until its file is edited by hand.
  - The motherbee's own file needs the same edit: the .deb never touches an existing `hive.yaml`
    (`deb-postinst`). The motherbee on 8.x keeps eight `.bak` and `.pre` copies of such edits.
- **The blob folder (A-181).** Turned on after the joins, it is shared with no spoke. The spokes'
  files still say off, and only a join adds a spoke to a folder.

**The operator's direction:**

- On the channel: *"Todo lo define una versión de software, si querés cambiar el hive.yaml cuando
  el sistema está funcionando es como un update de software, no me parece mal que se vea así dado
  que ese archivo cambia el comportamiento del soft. Así que quedaría todo dentro del canal de
  update."*
- On what travels: *"iría todo lo que se necesita propagar del hive que es general para todos los
  hives o el JSON que diga el cambio según el perfil del hive."*

**So:** a change to a hive's config is an update. A hive's shared config travels in the update
channel, with the core, and each hive's orchestrator applies it to its own file.

## 2. What the file holds

| Kind | Keys (a worker's file) | Who decides | Travels |
| --- | --- | --- | --- |
| The hive's identity | `hive_id`, `role` | the join | never |
| The control path | `wan.*` (`uplinks`, `gateway_name`, `mtls`), `identity.sync.*` | the join | never |
| Local | `storage.path`, `blob.path`, `blob.sync.api_port`, `blob.sync.data_dir`, `dist.path`, a worker's `egress` section | the hive | never |
| Shared, by the release | `system_nodes.<role>`, `blob.enabled`, `blob.sync.enabled`, `blob.sync.public_enabled`, `blob.sync.tool`, `dist.sync.*`, `government.identity_frontdesk` | the software version | yes |
| Shared, by the operator | `ai`, `blob.gc.*` | the operator, through SY.admin | yes |

- **The control path never travels:** the update rides the mesh, so a wrong `wan.mtls` or uplink
  would cut every spoke at once, and the fix could no longer reach them.
- **The motherbee's own sections** (`admin`, `architect`, `acme`, `web`, `egress`) are its own.
- **The shared part has two halves,** as the operator put it: what is general to every hive, and
  what goes by the hive's role (its profile).

## 3. The channel: the update

- **A spoke's core update carries its shared config.** Today's path is `POST /hives/{hive}/update`,
  category `core`: SY.admin sends `SYSTEM_UPDATE` to the hive's orchestrator. The message gains
  the hive's shared config, one JSON, with its hash.
- **The policy needs nothing new:** rule (2) of `policy/system.rego` already admits
  `SYSTEM_UPDATE` from SY.admin@motherbee to any hive's orchestrator.
- **A release** sends the release's config in the same message as its core. They go together
  because the role's `system_nodes` decides which core components the hive installs, so the config
  must be in place before the core is.
- **An operator's change** (the AI model) is an update too:
  1. SY.admin has the motherbee's orchestrator write it into the motherbee's file.
  2. It sends each hive the update, with the core that hive already has and the new config.

  It is an SY.admin action, so Archi can make it.
- **On the spoke, in this order:**
  1. the config (§5);
  2. the core, by the new list;
  3. the restarts: the whole core when the core changed, else only the readers of the changed keys.

## 4. What travels: the state, not a diff

- **The JSON is the hive's whole shared part,** general and its role's, not the changes.
  - A hive that missed two updates is current after one.
  - The same message twice changes nothing.
  - The orchestrator works out what changed against its own file.
- **Its hash is the config's version.** The hive keeps the hash it applied and reports it in
  `/versions`.
- **One function renders it,** from the motherbee's file. `add_hive` writes it at a join and the
  update sends it, so a hive that joins later starts like the others.

What worker1 would get today (the values on 8.x, read 2026-11-08):

```json
{
  "config_hash": "sha256:…",
  "general": {
    "ai": {
      "default_provider": "anthropic",
      "providers": {"anthropic": {"model": "claude-sonnet-5-5"}, "openai": {"model": "gpt-5.5"}}
    },
    "government": {"identity_frontdesk": "SY.frontdesk.gov@motherbee"}
  },
  "role": {
    "system_nodes": {"worker": {
      "nodes": ["SY.config.routes", "SY.identity", "SY.opa.rules", "SY.cognition", "SY.timer", "SY.wf-rules"],
      "wait_for": ["SY.config.routes", "SY.identity", "SY.opa.rules", "SY.cognition", "SY.timer"]
    }},
    "blob": {"enabled": true, "sync": {"enabled": true, "tool": "syncthing"}},
    "dist": {"sync": {"enabled": true, "tool": "syncthing"}}
  }
}
```

## 5. On the hive: its orchestrator applies it

1. **It checks the JSON:** only the shared keys of §2; anything else refuses the update.
2. **It merges it into its `hive.yaml`:** the shared keys are replaced whole, the rest is kept.
3. **It parses the result** with the readers the nodes use, the orchestrator's and the router's,
   before it writes anything.
4. **It writes the file atomically** and keeps the previous one (`hive.yaml.prev`).
5. **It applies each changed key:**
   - **blob and dist:** its next reconcile, which reads the file again
     (`current_blob_runtime_config`, `current_dist_runtime_config`);
   - **`system_nodes`:** the core it installs and the nodes it runs. Its watchdog's list is read
     at its start, so the orchestrator restarts.
   - **`ai`:** the nodes that read it, SY.cognition on a worker;
   - **`government.identity_frontdesk`:** the router.
6. **It answers as an update does:** `updated`, `unchanged`, `restarted`, `errors`, and the
   config's hash.

## 6. Seen from the motherbee

- A hive that does not answer is `unreachable` in the update's result, as today.
- `/versions` shows each hive's config hash beside its core, and which hives differ from the
  motherbee's. Archi reads it too.
- Sending it again is the same update.

## 7. One version for every hive

The operator: *"Lo que sí no puede pasar es que los archivos de config entre hives sean de distinta
versión. Si cambia versión cambia todo, como un pedazo de software de update."*

- **One version for the installation.** The shared config has one version, and every hive's file
  carries it beside the shared part. Each release and each operator's change makes a new version,
  and the update takes it whole (§4) to every hive.
- **No hive gets a config of its own.** A change is made for the installation, and its update goes
  to every hive.
- **An update is done when every hive is on its version.** Until then, its result names the hives
  still on another one, and `/versions` shows them out of date, as it does with a core.
- **A hive that was down when the version changed** gets it from the motherbee when it is back,
  once, and the motherbee reports it in its log and in `/versions`. This is the design's one
  automatic step, and it is what makes the rule hold. It is proposed; the operator confirms it.
- **A hive that joins** gets the current version at its join.
- **The version is a number** the motherbee raises with each change. `/versions` shows it beside the
  release, and the hash of each role's part checks its content.
- **A rollback takes both back.** The core update's backup generation keeps the `hive.yaml` it
  replaced beside the binaries, and `SYSTEM_CORE_ROLLBACK` restores the two together. Today the
  generation holds only `/usr/bin`'s binaries (`prepare_core_update`).

## 8. A-181 in this design

- **The blob's topology goes by role, and the release decides it:** a worker shares `blob/active`,
  the ingress receives `blob/public`, and the egress gets neither. The operator has no switch for
  it.
- **The members:** each blob folder of the motherbee follows the members of the dist folder with
  its audience, as agent-assets does (A-72).
  - `fluxbee-blob` follows the workers' (`fluxbee-dist-runtimes`).
  - `fluxbee-blob-public` follows the ingress's core folder.
  - A worker that joined before the folder existed gets it at the next reconcile.
- **With both,** an installation whose spokes joined with the sync off gets it at its next update.

## 9. The operator's calls (2026-11-08)

1. **The motherbee's own file at a release:** yes, under the rule of §7.
   - At the install, the release's keys (§2) are taken from the release's `hive.yaml.example` and
     replace the motherbee's.
   - The operator's keys stay.
   - The previous file is kept, and the deploy's report lists what changed.
   - Until now, the .deb never touched an existing `hive.yaml`.
2. **The keys of §2:** as listed. `government.identity_frontdesk` goes with the release, and
   `blob.gc.*` with the operator.
3. **The blob's switch:** it goes, and the release decides it by role (§8).
4. **The worker that joined before the egress** (`egress.gateway_ip`): apart, to be talked over
   later (*"luego la hablamos"*).

## 10. The update path today (read 2026-11-08)

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
- **Who reads the shared keys, and when:**
  - blob and dist: the reconcile, at every pass (`current_blob_runtime_config`, `:6306`;
    `current_dist_runtime_config`, `:6320`);
  - the watchdog's list: read at the orchestrator's start (`:971`);
  - `ai`: SY.cognition, at its start;
  - `government.identity_frontdesk`: the router, at its start.

## 11. The plan, by stages

Each stage is a release with its gate on 8.x, and the next starts only after it. To be refined by
the DTAP panel.

1. **The shared config, rendered and shown; nothing applied yet.**
   - One function renders a role's shared part, general and its role's, from the motherbee's file,
     with the installation's version and the part's hash.
   - The join writes it, with the same content as today.
   - `/versions` shows each hive's config version and hash; a hive that never got one says so.
   - Gate: `/versions` on 8.x, where no hive has a version yet.
2. **The core update carries it, and the hive applies it.**
   - `SYSTEM_UPDATE` with category `core` gains the hive's shared config.
   - The orchestrator applies it before the core (§5) and keeps the replaced file in the backup
     generation, and the rollback restores both.
   - The deploy waits for every hive on the version.
   - Gate: a deploy brings every hive on 8.x to version 1, its content today's, with no change in
     behavior. A unit test covers a worker whose file lacks a key.
3. **The motherbee's file at a release, and the operator's change.**
   - At the install, the release's keys come from `hive.yaml.example`; the previous file is kept,
     and the report lists what changed.
   - An SY.admin action for the operator's keys raises the version and updates every hive.
     Archi's handbook says how to use it.
4. **A hive that comes back** on another version gets the current one from the motherbee, reported
   (§7).
5. **A-181's blob:** the switch goes, by role. The motherbee's blob folders follow the dist folders'
   members (§8).

## 12. The DTAP panel (2026-11-08), and the second draft's proposal

**The panel:** four reviewers, one per lens, each re-checking its findings in the code. They raised
70 findings, many the same seen from different lenses:

| Lens | Blockers | Major | Minor |
| --- | --- | --- | --- |
| D, development | 2 | 10 | 4 |
| T, test | 1 | 17 | 3 |
| A, acceptance | 2 | 9 | 5 |
| P, production | 3 | 10 | 4 |

**The blockers, the same four from every lens:**

1. **The receiving orchestrator is the previous release's** (D-1, P-3, A-2, T-1, T-13). The update
   that carries a release's config is handled by the orchestrator that update replaces.
   - It ignores fields it does not know, so stage 2's own deploy cannot reach version 1.
   - A release that adds a key is refused by every spoke, for good.
2. **The file can name binaries the hive lacks** (D-2, P-1, A-1, T-4). Syncthing may deliver the
   manifest before a binary, and the orchestrator's start stops on a missing binary before it
   starts Syncthing. The spoke is then off the mesh, and nothing can repair it but SSH. That boot
   check is also a hazard today: A-186.
3. **An update's automatic rollbacks restore the binaries and not the file** (P-2, A-1, T-7), so a
   failed release leaves the new config on the old core.
4. **An operator's change rides the core path** (D-3, P-4, P-5, A-3, T-3).
   - The core path restarts the whole core even when it swaps nothing.
   - It installs what dist holds, not what the hive runs, so a model change re-installs a release
     the operator rolled back from.

**Also found, and true today:**

- **A-186:** a spoke that restarts while a release syncs never starts again.
- **A-187:** CI skips the Rust tests that read `packaging/` files when only those files change.
- **A-188:** a worker's blob GC deletes the motherbee's published sources through the shared
  folder (T-16).

**The proposal: take the release's keys out of the file.** *"Todo lo define una versión de
software"* read literally: what the release defines lives in the release, not in a file that must
be kept in step with it.

1. **The release's keys go into the code, by role:**
   - `system_nodes` with its order and waits;
   - the blob's and dist's topology and tool;
   - the frontdesk's name, with each role's values of today.

   They come with the binaries that use them, so the four blockers no longer apply:
   - no orchestrator ever judges another release's keys;
   - a list and its binaries arrive together;
   - a rollback or a downgrade takes them back with the binaries;
   - nothing is merged into a file at the install.
2. **The operator's settings live in one installation file,** owned by the motherbee and
   distributed to every hive like the policy and the runtimes. Today that is `ai`.
   - The operator changes it through SY.admin (Archi).
   - It is applied with the update channel's own pattern: content by Syncthing, then
     `SYSTEM_UPDATE` with category `config` and the file's hash. The hive answers `sync_pending`
     until the file arrives, then restarts only what reads a changed setting.
   - One file means one version by construction, and a hive that was down gets it when it is back.
3. **`hive.yaml` becomes the hive's own:** its identity, the control path, its local paths and
   ports. The join writes it, and only that hive's actions change it.
4. **The start never stops on a dist mismatch** (A-186). The orchestrator starts on what is
   installed, starts Syncthing, and reports that it waits for its update.
5. **A node a release drops is stopped and removed** by the core update, and kept in the backup
   generation for a rollback (D-4, P-16, A-9, T-8).
6. **The installation's update is SY.admin's** (A-10).
   - One action updates every hive, core and config. It keeps its state, names the hives not on
     the version, and resumes.
   - `ops.py` and Archi call it.
   - The automatic step of §7 becomes finishing that open update for a hive that comes back. Never
     a hive the operator rolled back (A-4, P-12, T-17).
7. **`/versions`** shows each hive's config version and hash beside its core, compared with the
   motherbee's.
8. **`blob.gc` stays the motherbee's own** (A-188). **A-181's blob folders** follow the dist
   folders' members (§8).

**Stages, re-cut:**

1. A-186's start.
2. The release's keys into the code, with today's values. Golden tests against today's templates
   and a throwaway worker join.
3. The installation file, `ai` moved to it, the config update and the operator's action.
4. The installation's update in SY.admin.
5. A-181's blob.

The other findings (validation of `ai` with every reader's rules, the readers per role, one locked
writer of `hive.yaml`, the tests over temporary roots, the gates) go into the second draft's
sections.

**For the operator:** the proposal's points 1 to 3 (the shape), and point 6's automatic step.
