# Hive config v1: the release's settings live in the release, the operator's travel in the update

**Status:**

- The direction was agreed with the operator on 2026-11-08, in a design chat that started from
  FINDINGS A-181 (a blob folder turned on after the joins).
- A first draft carried the release's keys inside the update message, applied to each hive's
  `hive.yaml`. A DTAP panel reviewed it the same day: 70 findings, four blockers (§11).
- The operator approved this second draft's shape the same day: *"Sí, vamos como describiste"*.
- **Stage 1 is live and checked on 8.x** (0.1.116): A-186, with A-189's vendored Syncthing.
- **Stage 2 is live and checked on 8.x** (0.1.117): the release's settings in the code,
  `src/release_settings.rs` (§3). Its gate's throwaway worker got a `hive.yaml` without the
  release's keys and ran its role from the table. Its join failed at the finalize, for a reason
  stage 2 does not touch (FINDINGS A-190).
- **Stage 3, in three releases** (§10):
  - 3a, live on 8.x (0.1.120): the AI SDK alone, the installation file's type and reader
    (`fluxbee_ai_sdk::installation`).
  - 3b, live and checked on 8.x (0.1.121): the motherbee writes the file's first version and
    distributes it, the readers switch to it, and `/versions` shows it, with nothing changed in
    behavior (§4, "As built (stage 3b)").
  - 3c, live and checked on 8.x (0.1.122): the operator's action, SY.admin's `set_installation_ai`,
    and the update with category `config` (§4, "As built (stage 3c)"), with its review round's
    fixes (F1 to F7, named there).
- **Stage 4 is live and checked on 8.x** (0.1.123): the installation's update in SY.admin, kept
  on disk, with its automatic step (§6, "As built (stage 4)"). `ops.py deploy` calls it from
  0.1.123 on. Its gates passed: the deploy through it, egress1 away across a config change and
  finished once by the automatic step, and a rolled-back egress1 held and left alone by it.
- **Next:** stage 5 (§10).

This is the single place for the topic.

---

## 1. What it is, from outside

A spoke's `hive.yaml` is written once, at its join: `add_hive` writes it over SSH from the
motherbee's settings of that moment, and nothing writes it again. `set_storage`, which changes
`storage.path` only, writes the motherbee's own file and no spoke's: SY.admin sends it to its own
hive's orchestrator (`action_routes_via_local_orchestrator`), which never forwards it. So whatever
the installation decides after a join never reaches that hive. The one exception since 0.1.125 is a
worker's `egress`, which the operator's `set_hive_egress` and `clear_hive_egress` write on that
worker (A-219, [edge-egress-nat-spec](edge-egress-nat-spec.md) §15). Three cases bite today:

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
| Local: `storage.path`, `blob.path`, `blob.sync.api_port`, `blob.sync.data_dir`, `dist.path`, a worker's `egress`, the ingress's `edge` and SSH table, the motherbee's `admin`, `architect`, `acme`, `web`, `egress` | the hive's `hive.yaml` | the join; on the motherbee, `set_storage` (its own `storage.path`, §1); on a worker, `set_hive_egress` and `clear_hive_egress` (its `egress`, A-219) | never travels with an update; a worker's `egress` goes to it alone, in `SYSTEM_EGRESS_ROUTE`, on the operator's word: the one exception ([edge-egress-nat-spec](edge-egress-nat-spec.md) §15) |
| `system_nodes` by role, with its order and waits | the release: the code, by role | a release | with the core's binaries |
| The blob's and dist's topology by role (`enabled`, `sync.enabled`, `public_enabled`, the tool) | the release: the code, by role | a release | with the core's binaries |
| The frontdesk's name (`government.identity_frontdesk`) | the hive's `hive.yaml` (since 2026-11-10; first planned in the code) | the join | never travels: on a worker it is the only key that names the motherbee |
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
  - **One way across stage 2 (0.1.117):** an orchestrator of 0.1.116 or earlier reads the list
    and the topology from `hive.yaml`, and does not start without `system_nodes`. The hives
    joined before keep those keys (ignored, not removed), so they go back as before. A hive
    joined by 0.1.117 or later has none: going back below 0.1.117 needs them put back by hand
    first (HANDBOOK, "The rollback").
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
    - The update leaves a dropped node out once Syncthing has deleted its binary from the spoke's
      dist. Until then dist holds the previous build, and the update refuses it as it refuses any
      binary still syncing; it goes through when tried again.
    - A component the previous list names that dist lacks for another reason (a binary set aside
      by hand) is left out the same way. The new orchestrator, whose list names it, keeps the
      generation open: `/versions` shows it in `core.awaiting_update` and in
      `core.last_update.settled.pending` until a core update installs it.
    - A node still on its way at the start is installed by the dist recheck once dist holds it,
      for that update only: an update installed while the previous orchestrator still runs is
      settled by the orchestrator it installs, with its own list.
    - Not built, since no release needs it yet: a node dropped from the **motherbee's** list. dpkg
      owns the motherbee's core, so its package has to stop and remove it. A dropped node's
      system ILK stays registered too: SY.identity's seed only adds.
- **The frontdesk's name stays in each hive's `hive.yaml`, as the join writes it** (decided
  2026-11-10, when the router's release came to move it):
  - the router and SY.identity read it at their start, and the join writes it, so moving it into
    the code would touch three binaries;
  - on a worker it is the only key that names the motherbee (its uplinks are addresses), so the
    code would have to learn the motherbee's hive at run time, for a value that never changes in
    an installation;
  - a worker names `SY.frontdesk.gov@motherbee`; the ingress and the egress have none, so their
    router defaults to a frontdesk on their own hive, where none runs. No IO node runs on either
    on 8.x, so nothing reaches that default; changing it is a call of its own (A-15, T-11), for
    when one does.
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
  - rt-gateway parsed `ai` only to check it, and stopped in 0.1.124, the router's own release:
    until then an invalid hive.yaml `ai` stopped a router at its start. Since 0.1.124 the router
    parses neither `ai` nor `blob`, so hive.yaml's `ai` no longer needs to stay valid for it. A
    rollback below 0.1.124 brings the check back (HANDBOOK, "The rollback").
- **The operator's change**, an SY.admin action that Archi can call:
  1. **SY.admin checks the new settings** with every reader's rules: `HiveAiConfig::effective` and
     each provider's model.
     - It refuses to switch the default provider while a node names a model of the old one, or to
       drop a provider a node names without a model of its own, and names those nodes (A-11).
     - It refuses while a join is running (A-12). As built, the motherbee's orchestrator refuses it
       at the write.
  2. **The motherbee's orchestrator writes the file,** the only writer, atomically, with the next
     version.
  3. **SY.admin runs the installation's update for it** (§6). Each hive answers `sync_pending` until
     its copy's hash is the new one. Then it restarts only the readers of the settings that
     changed, by role.
  4. **No config update restarts SY.admin or SY.architect.** Each takes a new version at its next
     use: SY.admin's executor before it runs or reviews a plan, Archi at each chat turn. Archi may
     be the caller of the very change, and a restart of either would cut the action's reply and
     the operator's chat. This replaces the first draft's "SY.admin restarts last".
- **An operator's version is undone by another one:** each version's values are kept, so Archi can
  put the previous ones back (as built, the motherbee's record of its versions, which
  `get_installation` shows). A core rollback never touches the file (D-8, A-6, P-10).
- **A hand edit of the file is caught:** the motherbee records each version's hash, and a file
  that no longer matches is reported in `/versions`. Changes are refused until the operator makes
  the edit a version or restores the file (A-7).
- **The move from today:**
  - The first release of this stage writes the motherbee's current `ai` as version 1. On 8.x it
    lists both providers, as worker1's hand-edited copy does.
  - The `ai` copies in each `hive.yaml` are then ignored and reported.

### As built (stage 3b)

Nothing changes in behavior: version 1 holds the values every reader ran on.

- **The file:** `<dist.path>/installation/installation.json`, the SDK's type and reader
  (`fluxbee_ai_sdk::installation`, stage 3a): `{schema_version: 1, version, ai}`, valid when every
  provider it names has its model.
- **The writer** is the motherbee's orchestrator, alone (`write_installation_file_at_start`).
  - At its start, when its dist holds no file, it writes version 1 from its own hive.yaml `ai`
    (`InstallationFile::render`, which validates). It writes atomically (temp and rename), mode
    0644, in `dist/installation`, which its start makes root's and readable by the Syncthing
    user's group, as the rest of dist. A hive.yaml without `ai` gives a file without one: the
    fallback engine, as before.
  - The version is recorded before the file is written, in
    `/var/lib/fluxbee/orchestrator/installation-versions.json`: its sha256, `written_at_ms` and
    `source: migration`. A file is never there without its record, which is how a hand edit is
    caught.
  - An `ai` no reader can use writes nothing. The start warns, `/versions` says why
    (`config.not_written`), and every reader stays on its hive.yaml. A file missing after a later
    version was recorded is not written again as version 1.
  - A file that is there is never rewritten at a start. Nothing here stops the start.
- **Distribution:** the Syncthing folder `fluxbee-dist-installation`, send-only on the motherbee
  and receive-only on every spoke of every role. Its members follow the vendor folder's, as
  `fluxbee-dist-policy`'s do (A-72):
  - a join links it with the other dist folders;
  - a hive joined before gets it at the next start of the orchestrator on each side;
  - `remove_hive` unlinks it with every `fluxbee-dist*` folder.
- **The join** copies the motherbee's file into the new spoke's dist with its core and vendor
  (`sync_installation_to_worker`): the same bytes and mode 0644, failing the join with
  `INSTALLATION_SYNC_FAILED`. A motherbee that holds no file has nothing to copy. The join writes no
  `ai` into a spoke's `hive.yaml`.
- **The readers** read `fluxbee_ai_sdk::ai_settings_at(installation_file_path(<the dist its
  hive.yaml names>), <its hive.yaml ai>)`. The hive.yaml read is lazy, and its `ai` is typed only
  there, so an old or invalid `ai` never breaks a reader that has the file.
  - On the motherbee: SY.admin (its executor), SY.architect (Archi's own `config.ai.model` stays
    its node config), SY.cognition and SY.frontdesk.gov, at their start; ai.generic at each request
    that leaves its provider or model to the hive.
  - On a worker: SY.cognition at its start, ai.generic at each request.
  - Each logs once at its start where its `ai` came from: the installation file's version and
    hash, or hive.yaml. ai.generic logs it when it changes.
- **A file no reader can use** is never a fallback to hive.yaml. The same holds for an invalid
  hive.yaml `ai` while the hive holds no file, which used to stop SY.admin's, SY.cognition's and
  the orchestrator's start:
  - SY.cognition logs the reason and runs on; every turn's semantic work is skipped with it, as
    without a key (`degraded_ai_settings_invalid`).
  - SY.frontdesk.gov boots `FAILED_CONFIG` with it; handoffs still work.
  - Archi's AI stays off, and its chat answers with the reason.
  - ai.generic fails such a request with `installation_ai_settings_invalid` and the reason.
  - SY.admin and the orchestrator never fail their start on it. SY.admin warns, and its executor
    answers with the reason; the orchestrator reports the file in `/versions`.
- **hive.yaml's `ai`, once a hive holds a file,** is ignored. Each orchestrator's start names it
  once, as stage 2 names the superseded keys. Keep it while the hive may go back to a release
  whose readers read it.
- **`/versions`** (§7): each hive's `config` is `{file, version, sha256, state}`, with `error` for
  an invalid file.
  - Each hive's orchestrator reports its own copy (`installation_file_state`).
  - The motherbee's orchestrator, which builds `/versions` for SY.admin, compares each copy with
    its own file and records: `current` (its hash), `behind` (another version it recorded),
    `pending` (absent) or `edited_by_hand` (an invalid file, or a hash no version was recorded
    with).
  - Its own file is `current` while its hash is its version's recorded one, else
    `edited_by_hand`, and `pending` with `not_written` when its start wrote none. `unknown` means
    the records cannot be read.
  - A hive on an older orchestrator shows no `config`, and the fleet's `comparison` does not
    include it.
- **The transition:** a reader falls back to hive.yaml only while its hive holds no file, and
  its log says which it read.
  - **On the motherbee it ends at the deploy:** its orchestrator writes the file at its first start
    of the release, before its boot restarts the SY nodes onto their new binaries, so they read the
    file from that start.
  - **On a spoke, the deploy's core update restarts SY.cognition before the file arrives:** the
    new orchestrator, which the same update installs, declares the folder, and Syncthing then
    brings the file. ai.generic takes it at its next request, and SY.cognition at its next start.
    Until then SY.cognition runs on its hive.yaml's `ai`, which holds the same values on 8.x.
  - **On a new spoke it ends at the join:** the bootstrap copies the file before the readers first
    start.
- **Built in stage 3c** (below): the operator's action and the update with category `config`,
  which restarts the readers. Before it nothing wrote a version past 1.

### As built (stage 3c)

The operator's change and the update with category `config`, live and checked on 8.x since
0.1.122 (§10).

- **SY.admin's two actions** (`InternalActionRoute::Installation`), and one more category:
  - `get_installation`, `GET /installation`: a read, which Archi may ask for over the mesh
    (`MeshAccess::Archi`). It is a view of the motherbee's `/versions` (`installation_view`): the
    motherbee's file (`file`, `version`, `sha256`, `state`, `ai`, `not_written`),
    `expected_version`, the versions recorded with their values (`versions`: the newest 20 unless
    asked for others with `versions_limit` and `versions_before`, with `versions_total` and
    `versions_next_before`, FINDINGS A-198), and each hive's copy (`state`, `version`, `sha256`,
    `applied`).
  - `set_installation_ai`, `PUT /installation/ai`, body `{ai, expected_version,
    replace_hand_edit?}`: the operator's word, confirmed first and closed to the whole mesh, Archi
    included (`MeshAccess::Nobody`). The operator calls it on loopback, or Archi stages it as one
    step of a plan the operator confirms. The command log keeps it.
  - `update` takes category `config` for one hive, with `manifest_version` and `manifest_hash`
    from `get_installation`: it finishes a hive that did not apply a version.
- **The body is taken exactly** (`ai_settings::parse_operator_ai`): `ai` is the whole new `ai`,
  `default_provider` and `providers`, each provider with its `model` and nothing else, so a field
  no reader reads is refused rather than dropped, and the values must be ones every reader can use
  (`INVALID_AI_SETTINGS`). A provider given as null is not listed, as `get_installation` shows one.
- **1. The node check (A-11)**, in SY.admin, only when the change switches the default provider or
  drops a provider (`ai_change_needs_node_check`) against the values the motherbee's file holds
  now (its `/versions`):
  - it reads the AI nodes of the motherbee and of each connected worker
    (`installation_node_check_hives`; an ingress or an egress receives no runtime, so it runs
    none): each hive's managed nodes (`list_nodes` with `managed`), then each `AI.*` node's config
    (`get_node_config`), its `behavior.provider` and `behavior.model`; and SY.architect's own
    `config.ai.model`, a model with no provider. A node without a config names nothing;
  - it refuses, naming each node and why (`nodes_left_without_an_engine`, `409
    NODES_LEFT_WITHOUT_ENGINE`), a node that names its model and not its provider when the default
    provider switches, Archi's own model among them, and a node that names its provider and not its
    model when that provider is dropped;
  - a hive, or SY.architect's config, it cannot read refuses the change, naming it (`503
    HIVES_NOT_CHECKED`): retry once it answers. A worker whose join did not finish is named in
    `node_check.skipped`, never a refusal;
  - when the motherbee's current values are not known (its file is one no reader can use, and its
    last version recorded none), it skips the check, and the answer's `node_check` says so.
- **1b. The key check** (the review's F3), when the change switches the default provider: every
  reader of the default needs a key of the new provider, which A-11's names alone do not show.
  - The readers (`installation_key_readers`): each AI node that names no provider of its own, in
    its own tenant (no root fallback); and SY.admin's executor, SY.architect, SY.frontdesk.gov and
    each SY.cognition, in the root tenant, each as itself.
  - SY.admin lists each tenant's secrets of the new provider by names only, never values
    (`VAULT_LIST` with `tenant_id` and `resource_type`), and each reader must find one it reads:
    its own, one assigned to it, or its tenant's general one (`readers_without_the_key`, the order
    `VaultClient::resolve_resource` follows).
  - It refuses, naming each node, its tenant and the `resource_type` (`409 PROVIDER_KEYS_MISSING`);
    a tenant whose keys cannot be listed refuses too (`503 KEYS_NOT_CHECKED`).
- **2. The write**, by the motherbee's orchestrator alone: SYSTEM_UPDATE `{category: config,
  write: {ai, expected_version, replace_hand_edit}}` to `SY.orchestrator@motherbee`
  (`write_installation_operator_version`), admitted by policy rule (2) as every update, answered
  within 30 s (`JSR_ADMIN_ORCH_TIMEOUT_SECS`). It refuses, writing nothing, with the reason in plain
  words, and SY.admin passes the refusal through with its code:
  - `NOT_PRIMARY`: on a spoke, which never writes it;
  - `JOIN_IN_PROGRESS`: a join or a removal is running (`any_hive_topology_busy`, A-12), and a
    join copies the file to its spoke;
  - `INSTALLATION_NOT_WRITTEN`: the motherbee holds no file. While version 1 is the only one
    recorded, its start writes it from hive.yaml's `ai`: fix that `ai` (`/versions`'
    `config.not_written` says why it was not written), restart `sy-orchestrator`, then change it.
    A file missing after a later version was recorded is put back first, the same bytes from a hive
    that holds it;
  - `INSTALLATION_EDITED_BY_HAND` (A-7): the file is one no reader can use, or its hash is not the
    one its version was written with. `replace_hand_edit: true`, the operator's choice, makes this
    change the next version over it;
  - `VERSION_CONFLICT`: `expected_version` is not the motherbee's version, which changed since it
    was read. That version is the file's, or the last one recorded for a file no reader can use;
    `get_installation` shows it as `expected_version`;
  - `INVALID_AI_SETTINGS`: values a reader cannot use (`InstallationFile::render` validates);
  - `INSTALLATION_UNCHANGED`: the current version holds these values already. Values are compared,
    not bytes, which differ by the version number anyway;
  - `INSTALLATION_RECORDS_UNREADABLE`, `INSTALLATION_UNREADABLE`, `INSTALLATION_WRITE_FAILED`: the
    record or the file cannot be read or written.
  - No answer in 30 s (`TIMEOUT`) is not "nothing written" (the review's F6): the motherbee's
    orchestrator takes its system messages one at a time, and one ahead of the write (a node's
    purge, up to 120 s) can hold it. The answer says the write may still land, to read
    `get_installation`, and, when its `expected_version` moved, to run the update with category
    `config` on every hive (`installation_write_unanswered`).
- **The next version** is one past the last recorded, or past the file's when a hand edit raised it,
  so a version number never goes back. It is recorded first, with `source: operator` and its
  values, then the file is written atomically, mode 0644, as version 1 is; a record whose file could
  not be written is taken back. The answer: `{status: ok, version, sha256, previous_version,
  previous_sha256}`.
- **Each version's values** are in its record (`ai`, provider and model names only,
  `ai_settings::ai_values`; a file without `ai` records the fallback engine written out, on which
  every reader runs the same), so a previous version can be put back as a new one. Stage 3b
  recorded version 1 without them: the motherbee's start fills them in from the file while its hash
  is still the recorded one (`record_current_version_values_at_start`), and so does the first
  write. A file edited by hand fills in nothing.
- **Then the motherbee's Syncthing scans `fluxbee-dist-installation`** (`SYSTEM_SYNC_HINT`, 3 s,
  waited 5 s; a hint, never a gate), as a policy's write does, so the rename reaches the spokes
  without waiting for the watcher.
- **3. The apply, on every hive at once:** the motherbee and each hive the registry calls
  connected (`connected_hives_with`, as a policy's rollout) get SYSTEM_UPDATE `{category: config,
  manifest_version, manifest_hash}` (`apply_installation_version`):
  - a hive answers `sync_pending` until its copy is that version with that hash
    (`local_system_manifest_state`, `manifest_match`), and `VERSION_MISMATCH` for a copy past it;
    an orchestrator older than 0.1.122 takes no category `config` (`MANIFEST_INVALID`);
  - a copy of that very version with another hash is refused, `INSTALLATION_HASH_MISMATCH` with
    both hashes, never `sync_pending` (the review's F5): within one version the file does not
    change while it syncs (`config_hash_differs_within_version`). Category `config` only;
  - a hive the registry holds whose join did not finish (joining, failed, interrupted) is not
    asked: the answer names it in `skipped`, with its status, its join's phase and what finishes it
    (its join, or `remove_hive`, then the update with category `config`), since its join may have
    copied the file and started SY.cognition (the review's F4). It does not make the answer
    `partial`: `/versions` leaves such a hive out the same way;
  - SY.admin asks again 2 s later (0.5 s since stage 4: the 0.1.122 gate saw Syncthing bring the
    file in under 2 s), then half again as long each time, up to 5 s, for up to 90 s
    (`JSR_ADMIN_INSTALLATION_APPLY_TIMEOUT_SECS`). A hive it cannot reach is asked again within that
    bound; a timeout (an update's 60 s) ends the asking, since the hive may still be applying it;
  - each hive's apply also ends at the change's ceiling, `admin_action_timeout` of
    `set_installation_ai`: 210 s by default, 2 × 30 for the check and the write, then 90 and 60.
- **What a hive restarts** (`apply_config_update_local`, under the orchestrator's update lock as
  any update):
  - only its start readers, by role (`installation_start_readers`): SY.cognition and
    SY.frontdesk.gov on the motherbee, SY.cognition on a worker, none on an ingress or an egress.
    ai.generic takes the new version at its next request, SY.admin's executor and Archi at their
    next use;
  - only those its release runs and systemd has active: one that does not run reads the file at its
    next start;
  - only when their engine changed, or when nothing says what they run (`config_update_plan`).
    The engine is `ai_settings::engine_of` the version's values, the default provider with its
    model, against the one this hive applied last, recorded in
    `/var/lib/fluxbee/orchestrator/config-applied.json` (`{version, sha256, applied_at_ms, ai,
    restarted, active_since}`). Another provider's model restarts nothing. Nothing says what a
    reader runs when there is no record (a hive's first config update, or a record it cannot
    read), or when its unit became active since the record (the review's F2): the record keeps
    when systemd last made each running reader active (`ActiveEnterTimestampMonotonic`,
    `systemd_active_since`), and a reader active since another time read whatever file it found
    then. That covers a restart for another reason, a restart that failed (the others restarted),
    a record that could not be written, and a box joined again (`remove_hive` deletes the record).
    The same version and hash again restarts nothing, and answers ok;
  - each behind the core's health gate (`restart_core_service_with_health_gate`, 30 s). The record
    is written after the restarts, and only when they all succeeded: otherwise the answer is
    `error`, `RESTART_FAILED` with the reasons. A record that cannot be written once they all
    restarted answers `error`, `CONFIG_RECORD_FAILED`: the readers run the version. Either way the
    readers that restarted are active since another time than the record the hive holds, so the
    next update restarts them again;
  - inline: the hive's orchestrator answers its other system messages once the restarts end, a
    few seconds later (the review's F7, kept as it is).
  - The answer lists `restarted`, `unchanged` and `errors`, as the other categories do. A copy no
    reader can use stays `sync_pending`: no version has its hash.
- **`set_installation_ai`'s answer:** `{version, sha256, previous_version, previous_sha256, hives,
  skipped, node_check, timings_ms}`, and since stage 4 `update`, the installation update the change
  opened (`{id, state}`, §6, "As built (stage 4)").
  - Each hive's `status` is `ok` (with `restarted` and `unchanged`), `sync_pending` (its copy had
    not arrived at the bound; `local_version`, `local_sha256`), `error` (`error_code`,
    `error_detail`) or `not_answered`, with `attempts` and `elapsed_ms`. A hive not ok gets `next`,
    by its error code (`installation_next_step`, the review's F5): the update with category
    `config` run again for `sync_pending`, no answer, `RESTART_FAILED` or `CONFIG_RECORD_FAILED`;
    the core first for `MANIFEST_INVALID`; the copy restored for `INSTALLATION_HASH_MISMATCH`; the
    later version for `VERSION_MISMATCH`. None says "not applied" (P-13): a hive that did not
    answer may hold the version already.
  - `timings_ms`: `check`, `write`, `scan`, `apply`, `total`, and each hive's (`hives`).
  - `ok` (200) when every hive is ok; otherwise `partial` (202), with `pending` naming the hives and
    a `message`. The version is written either way.
- **SY.admin and Archi take a new version at their next use**, never restarted by a config update
  (above). Each records the stamp of the file it reached its last verdict on
  (`ai_settings::InstallationBuiltFrom`), and before each use reads that one small file and hashes
  it: SY.admin's executor before each plan it runs or reviews
  (`executor_takes_new_installation_ai`), Archi at each chat turn
  (`architect_takes_new_installation_ai`). The log says once where its `ai` comes from now.
  - Only another engine rebuilds the runtime (the review's F1): the executor's
    `ai_settings::reader_engine`, Archi's default provider with its own model or the default's
    (`architect_engine_of`). A changed file whose engine is the same (the gate's change of OpenAI's
    model) keeps the runtime with no key read.
  - A rebuild reads the key once from the vault (`ai_settings::provider_key`). A vault that does
    not answer (`vault_failure_is_transport`) is no verdict: the runtime stays, no stamp is
    recorded, and the next use asks again. A key the vault does not hold, or an `ai` no reader can
    use, takes the runtime down with the reason, as at a start.
  - Archi's own `config.ai.model` stays its node config.
- **Archi** waits 240 s more for each `set_installation_ai` step of a plan
  (`EXECUTOR_INSTALLATION_WAIT_SECS`, past SY.admin's 210 s), and verifies a run only once every
  hive applied the change: a `partial` answer names the hives in the run's reason
  (`installation_changes_not_applied`).
- **`/versions`** (§7): each copy's `ai` values; `applied` on every hive; `versions` on the
  motherbee.

## 5. `hive.yaml`, the hive's own

- **It holds** the hive's identity, its control path, and its local paths, ports and sections.
- **One writer**, under one lock, atomically: the join and the hive's own actions (D-14, P-17,
  A-13). As built:
  - **The motherbee's file:** past the install (the .deb copies the example only when there is
    none) and the operator's hand, `set_storage` alone writes it (§1). It writes atomically since
    0.1.125: a temporary file beside it, then a rename (`write_file_atomic`, mode 0644; FINDINGS
    A-224). It used to be a plain `fs::write`, which a crash or a full disk could leave truncated,
    and the orchestrator does not start on such a file. Its writes come one at a time: the
    orchestrator takes SY.admin's actions one after another (`run_admin_worker`).
  - **A spoke's file:** its join writes it, whole, over SSH and under the motherbee's lock on that
    hive (`lock_hive_topology`), with `sudo tee`: in place, not atomically.
  - **A worker's `egress`** (A-219, since 0.1.125): its egress switch writes it at its commit, on
    the worker, atomically as `set_storage` does (`write_hive_yaml_egress`: only `egress` changed),
    while the motherbee holds the same `lock_hive_topology` for the whole action. Both writers take
    one lock in the orchestrator (`lock_hive_yaml`).
  - **Not built:** a lock against a hand edit, so one made during a `set_storage` or an egress
    commit can be lost; and an atomic write at the join.
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
- **As built (stage 3c):** the per-hive update with category `config` (§4, "As built (stage 3c)").
  `set_installation_ai` runs it on every hive once, in parallel, and keeps no state: a hive not on
  the version is named in its answer, and `update` with category `config` finishes it. The one
  action for the core and the file, with its state on disk and the automatic step, came with
  stage 4 (below).

### As built (stage 4)

Live and checked on 8.x since 0.1.123, with its review round's fixes (F1 to F12, below where each
one changed the design) and its gates (§10). The machinery is SY.admin's
`src/bin/sy_admin/installation_update.rs`; the actions are wired in `src/bin/sy_admin.rs`.

- **The target is what the motherbee holds now** (`target_of`, read from its `/versions`):
  - its core, the reference: the manifest hash it runs (`core.installed.manifest_hash` when it
    records an install, otherwise what dist holds, `ops.py`'s `core_of`), and its version, its
    components';
  - its installation file's version and sha256, while the file is a version it recorded
    (`config.state` `current`). A file edited by hand names no config: every hive's config is then
    left as it is, saying why, until the operator restores the file or makes it a version.
- **A hive is on the target** when both hold:
  - it runs that core, with its update settled and nothing open (`core_settle`, `ops.py`'s
    `update_open_items`): no restarts under way, the orchestrator the update installed settled
    it, no settle pending, no `awaiting_update`. When the update has a settle, the settle judges
    it and `left_out` is not read (F4): a release that drops a node leaves it in `left_out`, which
    the settle never rewrites. Without one, nothing `left_out`. The motherbee's core is always on
    it;
  - its `config.applied` names the target's version and sha256.
- **One update at a time, on disk:** `/var/lib/fluxbee/state/sy-admin/installation-update.json`,
  written atomically (a temporary file, then a rename, mode 0640), as `acme.json` is.
  - It holds the open update: `id`, `opened_at_ms`, `origin` (`http` for loopback, `executor` for
    a confirmed plan, `set_installation_ai`), `reason`, `target`, `last_run_at_ms` (the last
    explicit run's start, on the motherbee's clock), `stale_target`, and per hive `role`, `core` and
    `config` (each `{state, reason, exclusion, outcome, next}`) and `automatic`; the registry's
    hives whose join did not finish, in `skipped`; and the last 20 closed updates, newest first
    (`done` or `superseded`).
  - **Its states** (F2): `open`; `held`, open while the only hives off the target are those the
    operator rolled back (their core excluded as `rolled_back`, their config on the target), A-5's
    one allowed divergence, named in `held`; `done`; `superseded`. Anything else off the target
    keeps it `open`: a copy edited by hand, a motherbee file that is not a recorded version, an
    error, a pending hive, one that did not answer. A held hive moved by its per-hive `update` is
    looked at at once, and the update closes `done`. **Why:** without `held`, an installation with
    one hive rolled back on purpose would keep its update open forever, read again every so often.
  - **A stale target** (F5): the motherbee may hold another target than the open update's (a
    downgrade, a new file). Before the background task acts (the config after a followed core, the
    automatic step), and at SY.admin's start, it compares them; while they differ the update
    carries `stale_target` (said once in the log), is never `held` or `done`, and nothing acts on
    it: the next explicit run supersedes it.
  - A run with the same target resumes the open update. A run with another target closes it as
    `superseded`, kept in the history, and opens a new one; a new update's hives start `unknown`.
  - A task of a closed update (superseded, or done) sends nothing more: the core update and the
    config apply each check, before they send, that their update is still the open one (the
    automatic step sends through them, and records its attempt only in the open update); a send
    already under way finishes, its outcome not recorded (since 0.1.124). **Why:** a pass of an
    explicit run going on after its answer, or a superseded update's automatic step, sent the old
    target's core or config, and held the hive from the new update meanwhile.
  - With no update open and every hive already on the target, a run answers ok with
    `nothing_to_do` and records nothing, so the history keeps real updates.
  - SY.admin's start picks an open update up from disk and looks at every hive again: a category
    in flight stays in flight and is followed, a core sent with no answer recorded included (F6:
    it becomes `not_answered` only when its `/versions` never shows the update within the settle
    bound); a config apply sent with no answer is sent again. Every step is idempotent: a core
    update that replaces nothing restarts nothing (A-190), and the same config version again
    restarts nothing.
  - A file that does not parse is moved aside (`installation-update.json.unreadable-<ms>`), and
    the state starts empty.
- **The hives** are the motherbee and each one the registry calls connected (`registry_hives`);
  the registry's others are named in `skipped`, as in stage 3c. A registry that cannot be read is
  no list (F1): the explicit run refuses, `503 HIVE_REGISTRY_UNREADABLE`, and the background task
  neither refreshes the update's hives nor closes it on that look (said once in the log). **Why:**
  read as "the motherbee alone", one failed listing closed the open update as done. A registry
  directory that does not exist yet (no hive ever added) is the motherbee alone.
- **The decision per hive and category** (`decide`) is a pure function of the hive's `/versions`,
  the target, the run's kind (explicit or automatic) and `include_rolled_back`:
  - **Core**, when not on the target: `SYSTEM_UPDATE` category core with the target's hash.
    Under way already (its services restarting, or the orchestrator it installed not settled yet):
    followed, not sent again. On the target with something open: sent again, since a core update
    installs what is pending.
  - **Config**, when its applied version is not the target's: `SYSTEM_UPDATE` category config,
    after the core when the core needs one (the readers restart with it). When the core failed and
    the hive's orchestrator answers, the config still goes: one version everywhere.
  - **The exclusions**, each with its reason and the per-hive action that moves it:
    - a hive the operator rolled back (`core.installed.source` `core_rollback`, A-5): no
      installation-wide run changes its core unless the explicit run names it in
      `include_rolled_back`; an explicit run still applies its config (a config apply replaces no
      binary); the automatic step does not touch it at all;
    - a hive ahead of the motherbee: the automatic step leaves it alone; an explicit run moves it,
      which is the installation's rollback (install the previous release on the motherbee, then
      run the update);
    - a copy edited by hand (`config.state` `edited_by_hand`): no run applies config to it, and the
      automatic step leaves the hive alone (§6's rule);
    - a hive whose orchestrator takes no category config (before 0.1.122: no `config.applied` in
      its `/versions`): its core first, and nothing when its core is excluded.
  - The per-hive `update` stays the operator's tool for one hive, and moves any of them.
  - **Why "ahead" uses SY.admin's own record:** a hive's `core.installed` names no version, and
    its dist follows the motherbee's. SY.admin knows the version of every core it recorded as a
    target, the open update's and the history's (`known_cores`): a hive whose installed hash is
    one of them with a newer version is ahead. A hash it never recorded counts as behind, never as
    ahead; that takes a downgraded motherbee and a hive on a release SY.admin never targeted, both
    at once.
- **`update_installation`, the explicit run** (`run_explicit`): one at a time (F8: a second, or a
  `set_installation_ai`, answers `409 INSTALLATION_UPDATE_RUNNING` naming the running one, its
  origin, since when and its update); the motherbee's `/versions` gives the target (F10: a
  motherbee whose own core is rolled back runs a hash its dist's version does not name, so the run
  refuses, `409 MOTHERBEE_ROLLED_BACK`: install a release on the motherbee first); every hive is
  read once, at once; then the update is opened or resumed, and each hive's pass runs in a task of
  its own (`run_hive`):
  - a hive another task holds (F7): one that only reads it, or one of a closed update (superseded,
    done), which sends nothing more, is waited for, up to a read and the send it may have under way
    (`holder_wait`: the read bound, an update asked again up to the apply bound with its last
    answer, and 5 s; 205 s); one that acts on it for this update (the follow applying its config,
    the automatic step) keeps it, and the hive is recorded as left to that task, naming it, with
    `next`: never skipped without a word. A holder that ended meanwhile is none: the hive is
    claimed (`claim_for_run`). **Why** (since 0.1.124): a run that left a hive to a closed update's
    task stranded it, since that task sends nothing more; and a holder ending between the run's
    two looks had the hive left "to another task, which has just ended". `set_installation_ai`'s
    apply waits for a task that acts on the hive too, while it keeps its own time (below);
  - it waits until the hive's orchestrator answers (120 s from the run's start,
    `JSR_ADMIN_INSTALLATION_ANSWER_TIMEOUT_SECS`): right after an install the motherbee's router
    may not have the spokes back (0.1.56);
  - each update is asked again while `sync_pending` or unreachable, first after 0.5 s, then half
    again as long, up to 5 s, for up to 90 s (`JSR_ADMIN_INSTALLATION_APPLY_TIMEOUT_SECS`); a
    timeout (an update's 60 s) ends the asking, since the hive may still be applying it;
  - a core update answers once its binaries are in place (`restarting`): the pass leaves it in
    flight, and the background task follows it to its settle, then applies its config. **Why:**
    the call stays short (about 20 s on 8.x, as the deploy's core calls were), Archi's plan and
    `ops.py` read the progress from the record, and the follow survives a restart of SY.admin.
  - a core update with no answer in time (F6) is followed the same way, since the hive may be
    applying it: it is `not_answered` only when its `/versions` never shows the update within the
    settle bound;
  - a core that did not settle (F9), its record unchanged since, gets its core update again: it
    installs what is pending.
  - The call waits for its tasks up to its ceiling, `admin_action_timeout` 550 s (30 for the
    motherbee's read; then 120 for an answer, two reads of 50, and a core and a config update, each
    90 and 60); a task past it goes on, and its outcome lands in the record.
  - Its answer: `ok` (200) when the update is done, or held (with `held` and a message naming the
    hives the operator rolled back); `partial` (202) while it stays open, with `pending` naming each
    hive not on the target and how it stands; `{update, opening, superseded, include_rolled_back,
    message}`.
- **Outcomes** per hive and category, each with its times (`started_at_ms`, `at_ms`, `elapsed_ms`;
  a core's `answered_ms`, `installed_ms`, `settled_ms`; a config's `restarted`): `ok`,
  `restarting`, `settled`, `sync_pending` at the bound, `error` with its code, `not_answered`
  (never "not applied", P-13), and `excluded` with its reason; a read that finds a category on the
  target, its last outcome saying otherwise, records that too, marked `seen` (the background task,
  below). A core watched past its bound (300 s, `JSR_ADMIN_INSTALLATION_SETTLE_TIMEOUT_SECS`) ends
  `CORE_NOT_SETTLED` (it runs the target, something open) or `CORE_NOT_INSTALLED`, and stays
  pending until its record changes (F9: another update's start, or a new settle): its `next` says
  to run the update again or the per-hive `update`, and nothing warns about it again in the
  meantime. One whose health gate rolled it back is `CORE_UPDATE_ROLLED_BACK`; a `/versions`
  silent past the bound, `not_answered`. Each `next` says what moves it; a hive that did not
  answer is promised the automatic step "unless it leaves the hive alone" (rolled back, ahead,
  edited by hand).
- **The background task** (`run_background`), in SY.admin, the motherbee only, while an update is
  open: it looks at each hive when its time comes, in a task of its own, and sleeps until the next
  one is due (F2):
  - **one read of a hive at a time, everywhere:** the explicit run, the follow, the re-read and the
    automatic step share each hive's read, so at most one `GET_VERSIONS` of a hive is outstanding,
    and the background task reads one hive at a time. **Why:** the reads go through the
    motherbee's orchestrator, whose single admin worker forwards each within its own 45 s
    (`JSR_ORCH_SYSTEM_FORWARD_TIMEOUT_SECS`), and whose queue keeps what SY.admin gave up. A read
    waits 50 s (`JSR_ADMIN_INSTALLATION_READ_TIMEOUT_SECS`), past that bound; one that timed out
    anyway keeps the hive unread for another 50 s, so a second is never queued behind it. The
    re-read (`recheck`) takes its turn among the background reads before it claims its hive
    (since 0.1.124): one waiting for its turn holds no hive, so an explicit run's pass waits only
    for the read under way. **Why:** woken at a run's open, the task re-read every hive not read
    yet, each holding its hive while queued behind one read, and a pass could be left to one;
    the follow and the automatic step still claim first (their hives are in flight, or did not
    answer);
  - a hive in flight is read every 3 s (`JSR_ADMIN_INSTALLATION_FLIGHT_CHECK_SECS`) until it
    settles; then its config goes, `by` the run that sent its core, or `by: background` for a core
    the task found under way (F12);
  - a hive that did not answer, or not read yet (`unknown`): a check of shared memory every 30 s
    (`JSR_ADMIN_INSTALLATION_RETRY_CHECK_SECS`), which asks no one; it is read only once shared
    memory shows its orchestrator (`connected_nodes_from_shm`), and read anyway when shared memory
    cannot be read;
  - a hive left as it is (excluded), in error, `sync_pending` at its bound, or pending: read every
    10 min (`JSR_ADMIN_INSTALLATION_SLOW_CHECK_SECS`). A read sets its states as a run would decide
    them, nothing sent: a hive the operator moves with a per-hive `update` counts, and the update
    closes;
  - **what such a read records** (`recheck`), category by category, since 0.1.124:
    - nothing, when the category's last outcome came in after the read began: that outcome stands.
      **Why** (FINDINGS A-205): an apply that answered while the read was under way is newer than
      what the read shows;
    - a category it finds on the target, its last outcome saying otherwise (`excluded`, an error,
      no answer), gets an outcome of the read: the category's own status on the target (`settled`
      for a core, `ok` for a config), marked `seen`, no attempt, the read's times, and a `detail`
      naming the outcome it replaces. Every read that decides does the same, `by` its own kind
      (`record_step`, `seen_on_target`): this one (`background`), an explicit run's pass, the
      automatic step, and the follow's config step once the core settled. **Why** (A-204): a hive
      moved by its own per-hive `update` read on the target with the last outcome a run had left,
      `excluded` by the automatic step on 8.x; a hive whose apply answered too late kept
      `not_answered` once its automatic step found it applied;
  - **each hive by the open update's cadence:** the task's first look at a new update (a run's, a
    change's) forgets when the looks of the update it superseded had each hive due next (since
    0.1.124). **Why:** the due time is kept per hive, so a hive the superseded update read every
    10 min, and which the new run recorded `not_answered`, waited out that cadence before its 30 s
    check of shared memory and its automatic step;
  - **an immediate look,** which never blocks the action: a per-hive `update` or `core_rollback`
    on a hive, the end of `add_hive`'s join and `remove_hive` make the task look at that hive at
    once; a read of `get_installation_update` or of `/versions` makes it look at each hive off the
    target it has not looked at for 30 s;
  - **the automatic step** (§6): a hive whose outcome in this update was `not_answered`, and that
    answers again, gets the update finished, once per hive and target, by the same pass as an
    explicit run's, core then config, with the same bounds. A core it finds under way is followed
    instead, and the attempt is not spent on it (F6). The attempt is recorded before it acts
    (`automatic`, with its start, end and result; the result says `followed` while what it started
    is in flight, and lands with it), so a second never happens, a restart of SY.admin included;
    its outcomes carry `by: automatic`. It never acts on an excluded hive: the decision leaves those
    alone. An error, or `sync_pending` at its bound, is left for the operator with its `next`. Its
    log says `deciding what it finishes`, then `decided` (each category's step and what it leaves
    alone), then `ended its pass` with its result;
  - every outcome is one line of SY.admin's log, `installation update: outcome`, with the update's
    id, the hive, the category, the outcome and its duration (`ops.py timeline` takes the line).
- **The reads go through the motherbee's orchestrator** (`get_versions` for one hive), which
  forwards `GET_VERSIONS`: the system policy admits `GET_VERSIONS` to a spoke from the motherbee's
  orchestrator only (rule (3)). The updates go from SY.admin to each orchestrator, under policy rule
  (2), as every update does; no new message type.
- **`get_installation_update`**, Archi's read: the current or last update, `{update, open}`, and
  with `history` the closed ones in short (each with its target and the hives it left off it). The
  history's 20 updates and the update, for three times 8.x's hives with every text at its
  longest (texts are cut at 400 characters), stay under 96 KiB, inside one mesh frame
  (128 KiB). `/versions` (`list_versions`, `get_versions`) and `get_installation` carry its
  summary: id, state, target, `stale_target`, `held` and each hive's state (§7).
- **`set_installation_ai`** is an explicit run too (one at a time), and opens the update with the
  version it wrote as the target's file and the motherbee's core as its core, the reference: a hive
  that comes back behind on its core gets its core too, once, by the automatic step, then the
  version. The target differs from an open update's, so it supersedes it, and the new update
  starts with every hive `unknown`: the background task reads each one again once the apply ended
  (the motherbee alone is recorded on its core at once). A hive that did not answer stays pending,
  and the automatic step finishes it when it answers again: the gate "egress1 down across a config
  change, then back". A hive that joins after the change has applied no version: it gets it at the
  next explicit run, whose config update restarts its readers once (the background task only reads
  it). A registry that cannot be read refuses the change before it writes anything, `503
  HIVE_REGISTRY_UNREADABLE`, as F1 refuses `update_installation` (since 0.1.124; before, it applied
  the version on the motherbee alone and answered ok). When the motherbee's own core is rolled
  back, no update opens and the answer says why. Its answer is 3c's, with `update: {id, state}`
  (or `{id: null, not_opened}`). Its pass is stage 3c's apply, as the update's
  (`apply_change_on_hive`, since 0.1.124):
  - each hive is claimed while it applies. A task holding it, one that reads it or one that acts on
    it (the follow applying its config, the automatic step or an explicit run's pass, of the update
    the change superseded), is waited for, and the hive is applied once it lets go: the operator's
    change reaches every hive in one go whenever it can. The wait ends while the apply still has
    its own time before the change's ceiling, one update's answer (60 s) and never less than 10 s:
    given less, a hive that applies the version would read `not_answered`. A task that holds the
    hive longer keeps it: the change leaves the hive to it, nothing is sent, the record says so,
    and the answer's line is `left_to_task`, naming the task in `detail`, with `next`. **Why**
    (FINDINGS A-205): unclaimed, the apply let the background task read a hive just before it
    applied the version; that read's answer, back after the apply's outcome, set the hive back to
    `pending`, and the update stayed open on a hive on the target (32 s on 8.x, up to 10 min when
    no one reads the update);
  - each hive's outcome is recorded as its own apply ends, from its own start to its own end, so
    its `elapsed_ms` is the answer's `timings_ms.hives`. **Why** (A-204): recorded once every apply
    had ended, every hive got the slowest one's time (85.5 s each on 8.x, for applies of about 2 s);
  - the background task is woken once the apply ended, not when the update opens: it would read
    the hives the apply is about to claim.
- **`ops.py deploy`** calls `POST /installation/update` with `{reason: "deploy <version>"}` once the
  motherbee is on the release and the spokes answer, from 0.1.123 on, and reads the update every 3
  s until it is done or held, or until nothing moves a hive any more (900 s at most; a hive that did
  not answer still moves, for its automatic step). It follows only this call's update (the one its
  answer names; before the answer, one the call opened or resumed, never one left from before) and
  prints each hive's transitions as this run's outcomes show them: answered, installed, settled,
  config applied, or once "on target" for a hive it did not move. An outcome `by: background` (a
  core the background task found under way and followed) or marked `seen` (a read, the run's
  included, that found the hive on the target) is never the run's (`this_run`, since 0.1.124):
  such a hive prints as "on target", never as settled or config applied with that outcome's times.
  The times are this script's from the call and SY.admin's own durations, never the two clocks
  compared (F11). A held update passes with a warning naming the hives the operator rolled back. A
  release before 0.1.123 keeps the core update per spoke: deploying 0.1.122 again, the rollback,
  needs it.
- **Archi** follows an update its plan ran past the executor's answer (F3): every 10 s, up to
  600 s, while its hives only move (a core in flight, its config after it, a hive not read yet). A
  run is verified once the update is done or held (the held hives named, the operator's
  divergence); a hive in error, without an answer, `sync_pending` at its bound or left as it is, a
  stale target or a superseded update is not verified, each hive named with its `next`. Its wait
  for each `update_installation` step is SY.admin's ceiling with a margin, 580 s.

## 7. `/versions`

- **Per hive:** the core, as today, plus `config` with:
  - the version;
  - the hash of the hive's copy;
  - its state: current, behind, pending or edited by hand.
- **Each hive is compared** with the motherbee's file.
  - **As built (stage 3b):** by the motherbee's orchestrator, which builds `/versions` for SY.admin
    (§4, "As built (stage 3b)"). A state no version record can decide is `unknown`.
- **As built (stage 3c):** each hive's `config` adds `ai`, a valid copy's values, and `applied`, the
  config update it applied last (`{version, sha256, applied_at_ms, ai, restarted}`, null before its
  first). The motherbee's adds `versions`: the versions it recorded, with their sha256,
  `written_at_ms`, `source` (`migration` or `operator`) and `ai`, so SY.admin and Archi read the
  history without reading the orchestrator's files. Since 0.1.123 (FINDINGS A-198) it lists the
  newest 20, oldest first, with `versions_total`, `versions_latest` (the newest recorded) and,
  while older ones remain, `versions_next_before`; `versions_limit` (1 to 100) and
  `versions_before` list others.
- **An update's result says `not_answered`** for a hive that did not answer, never "not applied",
  and points to `/versions` (P-13). As built (stage 3c): `set_installation_ai`'s answer, with
  `sync_pending` and `error` beside it.
- **As built (stage 4):** SY.admin's `/versions` answers (`list_versions`, `get_versions`) add
  `installation_update`, and `get_installation` adds `update`: the current or last installation
  update's `id`, `state`, `target` and each hive's state in each category (`{core, config}`), or
  null before the first. Each outcome, the automatic step's included, is in
  `get_installation_update` (§6, "As built (stage 4)").

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
    later. Talked over on 2026-11-10 (A-219): an explicit action per worker, `set_hive_egress`,
    from Archi with the operator's CONFIRM, and its reverse, `clear_hive_egress`
    ([edge-egress-nat-spec](edge-egress-nat-spec.md) §15, built for 0.1.125).
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
   - ✅ 0.1.117 (2026-11-08): nothing changed on the four hives; the throwaway worker's
     `hive.yaml`, nodes, system ILKs and folders came from the table. Its join failed at the
     finalize (FINDINGS A-190, not this stage's).
3. **The installation file and the operator's action** (the readers, SY.admin, the folder,
   `SYSTEM_UPDATE` category `config`, `/versions`).
   - Gate: a reversible change on 8.x, `ai.providers.openai.model` (not the default provider's)
     set and back. Every hive must be current, the readers restarted only where they read it,
     AI.pruebas still answering, and the action's reply arriving.
   - In three releases: 3a, the AI SDK alone (its type and reader); 3b, the file's first version,
     its folder, the readers and `/versions` (§4, "As built (stage 3b)"), with nothing changed in
     behavior; 3c, the operator's action and category `config`, with the gate above.
   - ✅ 3a live on 8.x (0.1.120); ✅ 3b live and checked on 8.x (0.1.121): the file current on
     every hive, every reader on it.
   - ✅ 3c live and checked on 8.x (0.1.122, 2026-11-09): the gate above, with the operator's OK.
     - A-11 refused a switch of the default provider, naming SY.architect, and wrote nothing.
     - Version 2 (openai's model to gpt-5.5-mini) through loopback: every hive applied it in
       3.2 s, the spokes on their second attempt (Syncthing under 2 s).
       - Each hive's first config update restarted its readers: the motherbee's SY.cognition and
         SY.frontdesk.gov, worker1's SY.cognition. Nothing restarted on the ingress or the
         egress.
       - SY.admin and Archi were not restarted.
     - Version 3, the undo, through Archi's confirmed plan: every hive current, nothing
       restarted anywhere.
     - AI.pruebas answered on both versions.
     - The numbers are in lab/DEPLOYMENTS.md's 0.1.122.
4. **The installation's update in SY.admin,** with the automatic step. `ops.py deploy` calls it.
   - Tests in CI, over temporary roots: the decision per hive and category as a table (on the
     target, core needed, config needed, settling, each exclusion for each kind of run,
     `include_rolled_back`, a hive behind 0.1.122); the state file (resumed after a restart, a
     different target supersedes, the same resumes, the history bounded); the automatic step
     against a fake mesh (once per hive and target, only for a hive that comes back after
     `not_answered`, never for an excluded one, its outcome recorded); the actions' wiring; and
     `set_installation_ai` opening an update that keeps a silent hive pending. The review round
     added one for each fix: a registry that cannot be read (no `done`, and a refusal), one read
     of a hive at a time, the cadence, the immediate looks, `held`, a dropped node, a stale target,
     a core update's `TIMEOUT` and a core found under way, a hive another task holds, one explicit
     run at a time, a core that did not settle, and a rolled-back motherbee; Archi's verdict and
     its follow; and `ops.py`'s deploy against a scripted SY.admin.
   - Gate: a deploy through it.
   - Gate: egress1 down across a config change, then back: finished once and reported.
   - Gate: a rolled-back egress1 is left alone, and the update is held.
   - Live and checked on 8.x, 0.1.123 (§6, "As built (stage 4)"): the three gates passed
     (lab/DEPLOYMENTS.md, 0.1.123).
5. **A-181's blob members, and A-188.**
   - A-72's tests for both blob folders, and a removal without flags.
   - Gate: a throwaway worker. Its device is taken out of `fluxbee-blob` through Syncthing's REST;
     the reconcile must restore it, and `remove_hive` must leave both folders. It needs A-190
     fixed first: a fresh worker cannot join today.

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

  Since stage 4, from 0.1.123 on, steps 2 and 3 are one `POST /installation/update`, followed in
  its record (§6, "As built (stage 4)").
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
  what is installed, the last update and the rollback generations. It shows nothing of the config
  (since stage 3b, each hive's `config`: §4, "As built (stage 3b)"; since stage 3c with `applied`,
  `ai` and the motherbee's `versions`: §7).
- **The join** writes a spoke's whole `hive.yaml` over SSH, from three templates:
  - the worker's: `:25968`, written at `:25992`;
  - the egress's: `:26785`;
  - the ingress's: `:27542`.

  Then `ADD_HIVE_FINALIZE` runs on the spoke (`:24236`).
- **After the join,** nothing writes a spoke's file (since 0.1.125, but a worker's `egress`:
  A-219, §5). `set_storage` writes the motherbee's own, and only `storage.path` (§1). It round-trips the YAML through serde, so the comments go; since
  0.1.125 it writes atomically (§5).
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
    for its check only until 0.1.124 (§4); ai.generic at each request (since stage 3b from the
    installation file, and from hive.yaml only while a hive holds none: §4, "As built (stage 3b)";
    since stage 3c, SY.admin's executor and Archi at their next use too, and a config update
    restarts SY.cognition and SY.frontdesk.gov when their engine changed: §4, "As built (stage 3c)");
  - `government.identity_frontdesk`: the router and SY.identity, at their start.
