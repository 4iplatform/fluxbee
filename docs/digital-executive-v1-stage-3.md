# The Digital Executive v1, stage 3: solutions from Archi's CI, and capabilities

**Status:** the build design, 2026-11-28, consolidated from the talk of the same day
([notes](digital-executive-v1.md) §33–§38). It waits for the operator's review (§11). Nothing is
built.

**What it builds:**

- **One kind of thing, the solution,** in three kinds: `executive`, `capability`, `infrastructure`.
- **Archi's CI holds the deployable packages,** the ones Fluxbee ships (`system`, locked) and the
  ones Archi makes; Archi publishes them to SY.deploy; Cloud reads the catalog from SY.deploy.
- **SY.deploy only deploys solutions:** the plain ED leaves its binary and becomes a system package.
- **Capabilities:** a set of agents behind the ED, with IOs only to external systems, and a WF at
  its door. The conscious finds its capabilities in identity, reads each door's help, and asks it
  within its turn.

**What it leaves out** (§10): the profile (stage 4), layer 1, SY.deploy's bounds, loops between
capabilities, upgrades and waves, an AI check of packages.

**What it leaves working:** every node and path of today, and stage 2's EDs. An ED with no
capability works as now.

---

## 0. What the code says (read on 2026-11-28)

- **Archi's manifests** (`SolutionManifestV2`, `src/bin/sy_architect/pipeline_types.rs`), saved per
  version at `state/manifests/<solution>/v<ms>.json`, have no blanks, no person they found, no node
  roles and no released mark. Archi keeps the last three and the newest executed one (0.1.160).
- **SY.deploy** (0.1.160) keeps its sources as copies under `state/sy-deploy/sources/<id>/
  <version>.json` and reads them through one reader. `plain-ed` 1 is built into its binary
  (`src/bin/sy_deploy/sources/plain-ed-1.json`). A deployment into an existing grouping is its
  model; its groupings are of kind `ed` only.
- **The package format** is SY.deploy's (`src/bin/sy_deploy/source.rs`): blanks with types,
  `founds`, nodes with their roles, configs and assets, vault mappings, templates `$blank`/`$ref`.
- **Identity's relation** (0.1.157): `work_role` 0 none, 1 conscious, 2 channel, 3 agent.
- **A WF** (`go/nodes/wf/wf-generic`, `go/sy-wf-rules`):
  - `start_from` never names `SY.*`, `RT.*` or `AI.*` (docs/wf-orchestration-v1.md §5.2): an agent's
    late answer would start a new instance (A-118);
  - a WF only sends `user` messages, and a message to an agent starts a turn of that agent;
  - it answers `HELP` with its `description`, states and `input_schema` (`handleWFHelp`);
  - its name is `[a-z][a-z0-9]*(\.[a-z0-9]+)*`, installation-wide; SY.admin deploys one with
    `wf_rules_compile_apply` and removes it with `wf_rules_delete`.
- **The AI agent's tools** are declared once in code, each a card in the model's tools, the catalog
  and `HELP` (docs/agent-tools-v1.md). Agents answer; they send nothing on their own (0.1.89).

---

## 1. Kinds and attributes

| Kind | What it is | Deployed into |
| --- | --- | --- |
| `executive` | the base of an ED: it founds the ED, its conscious and its channels | a new ED grouping |
| `capability` | an extension of an ED: agents behind it, IOs to external systems, a WF at its door | an ED's grouping |
| `infrastructure` | general for all of a tenant's EDs | the tenant's infra grouping (one per tenant) |

Two attributes, so that the kinds stay three:

- **origin:** `system` (ships with Fluxbee; Archi can read and derive it, never edit it),
  `catalog` (Archi's standard, for every tenant), `custom` (made by Archi for one tenant);
- **offered to:** every tenant (`system`, `catalog`) or one tenant (`custom`); and, for a
  capability, an optional flag that narrows it to all the tenant's EDs or to named EDs.

## 2. The package and its blanks

A package is a released version of a solution, in SY.deploy's format (§0), with:

- **`kind`, `origin`, `offered_to`,** and a `description` for Cloud's catalog;
- **blanks of two classes:**
  - **filled by SY.deploy from the grouping:** the ED's ILK, its tenant, its short id, its
    conscious's node name; for infrastructure, the tenant;
  - **filled by the user** in Cloud's form: names, keys in SY.vault, values;
- **nodes:** each with its kind (`AI`, `IO`, `WF`), its middle name, its runtime, its config with
  the blanks in place, its `work_role`, and for an agent its definition's assets;
- **`founds`** (an executive only): the person and its document;
- **vault mappings.**

**Roles in identity, two more** (the same byte, no layout change): `connector` (4), an IO to an
external system; `capability` (5), a capability's entry WF.

## 3. Archi's CI and its publish

- **Packages apart from the design:** Archi keeps designing in its manifests. Releasing one
  compiles it into a package (its blanks declared, its nodes' roles and assets), kept at
  `state/packages/<solution>/<version>.json`, written once.
- **System packages ship with Fluxbee** in the .deb, read-only (`/usr/share/fluxbee/solutions/`).
  Archi lists them as `system`; it may derive a `custom` copy, never edit one. `plain-ed` 1 moves
  there from SY.deploy's binary.
- **The publish** (`cd_publish`, through SY.admin, an event): Archi sends a released package;
  SY.deploy checks it (its format, its blanks, sections it can build) and keeps its copy, answering
  accepted or refused with the reason. A withdraw (`cd_unpublish`) stops offering it; running
  deployments keep their version.
- **System packages reach SY.deploy at Archi's start:** Archi publishes each shipped version
  SY.deploy has not got, and reports it in its log and its status (proposed, §11).
- **Versions kept:** three of each package in SY.deploy, never one a deployment runs (stage 2).

## 4. SY.deploy

- **Its catalog is what Archi published,** with its kinds and attributes; `cd_catalog` filters by
  kind and by what the caller's tenant (and ED) is offered.
- **The infra grouping:** one per tenant, made by its first infrastructure deployment.
- **A capability's deployment:** into an ED's grouping, its blanks from the grouping filled first;
  each node `works_for` the ED with its role; never `speaks_as`.
- **WF nodes in the step builder:** a WF is deployed with `wf_rules_compile_apply` (its
  definition with the blanks in place) and removed with `wf_rules_delete`; its name is
  `<middle>.<short_id>`, so it is unique per ED (`WF.collections.ed4qm7k2xa@worker1`). SY.deploy's
  row in SY.admin takes those two actions and `wf_rules_get_workflow`.
- **The removal of an ED** removes its capabilities first, then its executive (stage 2).

## 5. The capability's door: a WF

- **By convention, every capability's entry is a WF** (the operator, 2026-11-28). Depending on the
  request, it talks to the capability's agents and connectors and routes; no AI at the door.
- **Started only by the ED's conscious:** `start_from` takes exactly the conscious's node name,
  written by SY.deploy. A WF may name an `AI.*` in `start_from` only when it never sends to that
  node except through `reply`, which the compiler checks (A-118's loop cannot happen).
- **`reply`, a new action:** the instance answers the request that started it, as the response to
  that request (its trace), never as a new message. The conscious gets it as its tool's result.
- **Its interface is its help:** the definition's `description`, its `input_schema`, and each
  action's description, which `HELP` already answers. A change of the WF's program changes nothing
  else.

## 6. The conscious's tool

- **One base tool, `ask_capability`,** in ai.generic's catalog:
  - it lists the capabilities of the ED the node speaks as: the ILKs in the SHM whose `works_for`
    is that ED and whose role is `capability`, with their node names;
  - it reads each one's `HELP` (cached until the SHM moves) and gives the model their names,
    descriptions and input schemas;
  - it sends the request to the chosen door and waits for the `reply`, bounded (proposed 60 s,
    §11); a timeout or an error comes back as the tool's error, and the conscious answers the
    person with what it can.
- **Only for its own ED's capabilities,** and only while `speaks_as` is confirmed.

## 7. Cloud

- **The screen** (the operator): the tenant's EDs (`cd_tree`); "create an ED" from an executive of
  the catalog, its form from its blanks, a timer on `cd_get`; then, on an ED, "add a capability"
  from the capabilities offered to it, each its own deployment and timer; infrastructure for the
  tenant the same way; `cd_remove` for one part or the whole ED.
- **No new op:** the six of stage 2 cover it; `cd_catalog` gains its filters.

## 8. What a request looks like

A person asks the ED for an invoice's state. The conscious, in its turn:

1. lists its capabilities and reads `WF.billing.ed4qm7k2xa@worker1`'s help: "invoices: state of an
   invoice, by number";
2. calls `ask_capability` with `{capability: billing, input: {action: "invoice_state", number:
   "A-1032"}}`;
3. the WF starts an instance, asks its agent `AI.billing.ed4qm7k2xa@worker1`, which reads the CRM
   through `IO.crm.ed4qm7k2xa@worker1` (a `connector`), and `reply`s `{state: "paid", date: …}`;
4. the conscious answers the person, as the ED.

## 9. How it ships (METHOD §9)

- **3A, shared:** identity's two roles in the SHM's copies and the SDKs (no layout change); the AI
  SDK's `ask_capability` card and its call; the Go SDK's response to a request (for `reply`).
- **3B, the WF:** `start_from` with the ED's conscious, the `reply` action, the compiler's check,
  each action's description in `HELP` (wf-generic, SY.wf-rules).
- **3C, the packages:** Archi's release and CI, the system packages in the .deb, `cd_publish`
  and `cd_unpublish` (SY.admin, Archi); SY.deploy's catalog from publishes, kinds and attributes,
  the infra grouping, WF nodes in its step builder and their actions in its row; ai.generic's tool;
  `plain-ed` out of SY.deploy's binary.
- **Its gate:** an ED born from the published system package; a test capability (a WF door, one
  agent, a fake connector) built by Archi, released and published, added to the ED through Cloud;
  a person's question that the conscious answers through it; the capability removed; the ED
  removed.

## 10. Left for later

| What | Where it goes |
| --- | --- |
| The profile (position, boss, responsibilities) and the people tool | Stage 4 |
| Layer 1: the DID document and the card | The internet identity's stage |
| SY.deploy's bounds (a reserved namespace, its own nodes only) | After the flow |
| Loops between capabilities, a capability that asks another | When one needs it |
| Upgrades of packages, waves, rollback | With the first new version |
| An AI check of packages before a publish | Design §5.5 |

## 11. For the operator's review

1. **System packages reach SY.deploy at Archi's start,** each shipped version SY.deploy lacks,
   reported in Archi's log and status. The other way: the operator publishes them by hand after an
   install, and no ED can be created before.
2. **`ask_capability`'s bound:** 60 s, after which the conscious answers the person with what it
   has. It cannot come back later on its own: an agent only answers.
3. **Archi's release step** (a manifest compiled into a package): through Archi's chat, the
   operator's CONFIRM, like any write of Archi's.
