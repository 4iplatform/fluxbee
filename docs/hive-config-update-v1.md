# Hive config v1: a hive's shared config travels in the update

**Status:**

- The direction was agreed with the operator on 2026-11-08, in a design chat that started from
  FINDINGS A-181 (a blob folder turned on after the joins).
- This is the design's first draft. Its open points are in §8, for the operator's call before it
  is built.
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

## 7. A-181 in this design

- **The blob's topology goes by role, and the release decides it:** a worker shares `blob/active`,
  the ingress receives `blob/public`, and the egress gets neither. The operator has no switch for
  it.
- **The members:** each blob folder of the motherbee follows the members of the dist folder with
  its audience, as agent-assets does (A-72).
  - `fluxbee-blob` follows the workers' (`fluxbee-dist-runtimes`).
  - `fluxbee-blob-public` follows the ingress's core folder.
  - A worker that joined before the folder existed gets it at the next reconcile.
- **With both,** an installation whose spokes joined with the sync off gets it at its next update.

## 8. Open, for the operator

1. **The motherbee's own file at a release:** the release's keys (§2) are taken from the release's
   `hive.yaml.example` at the install, replacing the motherbee's, and the operator's keys stay.
   The previous file is kept, and the deploy's report lists what changed. Today the .deb never
   touches an existing `hive.yaml`.
2. **The list of keys in §2,** to confirm: `government.identity_frontdesk` and `blob.gc.*` above
   all.
3. **The blob's switch:** it goes, and the release decides it by role (§7). This reads *"todo lo
   define una versión de software"* that way.
4. **Related, but apart:** the worker that joined before the egress never gets
   `egress.gateway_ip` (the sweep of 2026-11-07). That key is the hive's default route, which §2
   keeps out of the update. It stays a call of its own.
