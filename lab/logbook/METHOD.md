# METHOD — how the Fluxbee infrastructure is worked on

> Method document. It defines **how** the infra is operated (lab and prod), **what the agent can do**,
> and **how each change is recorded**.
>
> Sibling documents in this directory:
> - `YYYY-MM-DD.md` — **the logbook**: the journey, change by change, with date/time/evidence.
> - `FINDINGS.md` — **the accumulated findings** of the integrated deployment; the
>   **code change plan** comes out of it at the end. During the deployment they are **added**, not fixed.
> - `HANDBOOK.md` — **the validated recipes** (portable, ready for the next prod). A recipe
>   is written **only after** it has been run successfully here.
> - [`../DEPLOYMENTS.md`](../DEPLOYMENTS.md) — **the audit ledger** of versions in PROD: which
>   version was left installed, when, which commit, what changed and how it was verified. It is written on EVERY
>   deploy to prod (rule in HANDBOOK §12) — with no entry, the deploy is not done.
>
> It complements —does not replace— [`lab/PROXMOX.md`](../PROXMOX.md) (daily lab operation) and
> `docs/packaging-and-build.md` (build/packages). This doc is the one that rules when prod is involved.

---

## 1. Environments

| | **LAB** | **PROD** |
|---|---|---|
| Proxmox | `192.168.4.165` (PC-004-165), `192.168.4.157` (PC-004-157) | **`192.168.8.207`** (node `pve`) |
| Use | experiment, break, validate | real service |
| Build box | fb-build VM210 @165 · apt repo `192.168.4.200:8900` | fb-build VM110 (outside the cluster) · `10.10.10.50` (fbint) + `192.168.8.180` (vmbr0, DHCP) · apt repo `:8900` — devs: `http://192.168.8.180:8900` |
| Hives | 240–243 @157 | VMs 100 `motherbee` · 101 `worker1` · 102 `ingress1` · 103 `egress1` |
| Network | 192.168.4.0/24 | 192.168.8.0/24, gw `192.168.8.1`, bridge `vmbr0` |
| Host timezone | — | `America/New_York` (**the host journal is in EDT, the logbook in ART −03**) |

**The two networks cannot see each other.** The bridge is the operator/agent machine, which reaches both:
to take a `.deb` or a binary from lab→prod, it is downloaded from the build box and uploaded to prod. The networks are not connected.

---

## 2. Agent capabilities (verified empirically, not assumed)

Prod token `ai-agent@pve!vscode` → ACL `/ = Administrator, propagate=1`, `privsep=0`.
**It is full admin of the datacenter.** Concretely:

### Level 1 — Datacenter via the REST API: **YES, everything**
- **VMs**: create, clone, start/stop/destroy, snapshots, rollback, migrate.
- **Templates**: create (VM + `template=1`) and clone from them.
- **Storage**: create/modify/delete pools, upload ISOs/cloud-images, download by URL
  (`/nodes/{n}/storage/{s}/download-url`).
- **Network**: bridges, bonds, VLANs (`/nodes/{n}/network` + apply). **SDN**: zones, vnets, subnets.
- **Firewall** (datacenter/node/VM), **users/tokens/realms/ACL**, **host apt**,
  **host services**, **host power** (reboot/shutdown), **syslog**.

### Level 2 — Inside the VMs: **YES, full**
`VM.GuestAgent.Unrestricted` + FileRead/FileWrite → command execution as root, file push/pull,
**with no SSH or tunnels**. It is the standard mechanism (`lab/pve.py exec|push|pull`).

### Level 3 — Arbitrary shell *on the Proxmox host*: **NOT direct** ← the only real limit
The REST API does not expose "run this command on the host". There are `vncshell`/`termproxy` (an interactive
websocket, the privilege is available), but they are not convenient to script.

**In practice it hardly gets in the way**: network, storage, services, apt, firewall and power **have an API endpoint**.
What would be left out is something very specific (editing a single file in the host's `/etc`, running an
ad-hoc script). If that need comes up, **it is raised and decided together** — an access
mechanism is not improvised.

### Who decides on each infrastructure (the operator, 2026-10-29)
- **8.x, the PROD host `192.168.8.207`: the agent administers it.** It decides and acts on its own
  judgment (VMs, storage, snapshots, network, users), the irreversible included, and records each
  change in the day's logbook (§5) with its reason. The rules of §3 still hold. When the host is
  shared with the dev team, how it is split gets decided then.
- **4.x, the DEV cluster: outside the agent's scope for now.** It is where the dev team works
  (Noelia's VMs 270 and 271; VM 260 is not ours).
- **In practice,** Claude Code's permission classifier can still stop an irreversible action (a
  deleted snapshot or VM). The agent then asks the operator, and never works around the block.

---

## 3. Golden rules

### Prod
0. **ACCESS SCOPE (the operator's rule, 2026-07-28):** the agent operates **only on the
   machines it created itself**, plus the Proxmox host it administers. The administration network
   `192.168.8.0/24` is **shared and flat**: there is third-party equipment on it. **The network is not scanned,
   and no IP that does not belong to one of our own VMs is accessed.** Faced with an unknown IP: it is not touched.
0b. **CODE: BUGS GET FIXED, PROPERLY** (the operator's rule, updated 2026-07-29): the agent
   **does fix the bugs it finds**, on the condition that they are fixed **correctly** —
   never a patch "to make it work". The original version of this rule (2026-07-28) forbade touching
   code; the operator broadened it after finding **A-6**.
   In practice: (a) **diagnose the root cause**, not the symptom; (b) **check the peers** —a
   divergence from a sibling script/node often *is* the bug, and the correct sibling is the
   reference; (c) **reject the shortcut** even if the error suggests it (e.g.: `-buildvcs=false`);
   (d) **record** the fix in `FINDINGS.md` with its evidence; (e) for a **design** change
   (not a bug), it still applies: **it is discussed first**.
0d. **NO FINDING IS REPORTED WITHOUT AN ATTEMPT TO REFUTE IT** (the operator's rule, 2026-08-04).
   It comes from a concrete mistake: U-12 was reported proposing to build an atomic
   "take the node down + purge its directory" operation that **already existed** (`kill_node` with `purge_instance`, with an
   example in the self-documentation) — and the inferior sequence had been written in the
   `HANDBOOK` as *the* recipe. The operator cannot verify every detail; **the adversary has
   to be the agent**.

   The failure pattern, measured over that session's mistakes: **the findings produced
   alone had no refuter; the ones that went through adversarial review were corrected in
   time.** It is not a matter of reading faster, it is a matter of having a contradictor.

   Before writing "X is missing" / "Y must be built", **it is mandatory**:
   1. **Ask the system.** `GET /admin/actions` and `GET /admin/actions/<action>` expose
      routes, the full schema and examples — including **optional parameters that are not visible by reading
      the function's signature**. That is where `purge_instance` was.
   2. **Search with other names.** The repo uses synonyms: purge/remove/delete/kill,
      ref/reference/key, glob/family/fanout, hot/reload/refresh/reconcile.
   3. **Compare with the siblings.** If the three nodes of the family do the same thing, **it is not one node's
      bug: it is the family's contract**, and calling it a bug is the mistake. (It happened with `configured`.)
   4. **Review the tests and `docs/`.** There are capabilities that are only exercised there.

   And when reporting, **separate what was verified from what was inferred**: what was read in the code (with
   `file:line`), what was tested live, and what is an assumption. A finding without that distinction is not
   finished.

   **Corollary for the HANDBOOK:** a recipe written there is going to be followed without question. Nothing goes
   into the handbook without having been run, and if a finding is corrected, one has to **check whether it contaminated
   a recipe**.

0c. **Do not invent mechanisms parallel to the ones the product already provides.** If `add_hive` (or any
   fluxbee flow) solves something, **that** path is used, even if there is a shortcut from the agent's
   privileged position. Reason: the recipes must be **portable** (the next prod may be
   bare-metal, with no Proxmox or guest-agent) and must exercise **the path that is tested**.
1. **Snapshot before every destructive or doubtful change.** Name: `pre-<action>-YYYYMMDD`.
   **One per VM, the backup** (the operator lowered it from 3 on 2026-10-29): the old one is deleted
   first. `lab/pve.py` enforces it (HANDBOOK §12).
2. **One change at a time**, verified before the next one. No blind batches.
3. **Every change goes into the day's logbook** (§5), including the one that goes wrong.
4. **What is irreversible is asked first** (§4). Even if the token can do it. On the 8.x host it
   is the agent's call since 2026-10-29 (§2, "Who decides").
5. **Verify with evidence**, not with assumption: if a service was brought up, its state is shown.
6. **No lab credentials in prod** — see §6.
7. When in doubt between "test in prod" and "test in lab": **test in lab**.

### Lab
It can be broken. Even so, whatever is useful as learning is recorded (the findings are the product).

---

## 4. Change classification

| Type | Examples | Requires approval | Snapshot |
|---|---|---|---|
| **Read** | inventory, `config-get`, logs, permissions | no | no |
| **Reversible** | create VM, snapshot, start/stop node, `config-set` | no (give notice) | recommended |
| **Irreversible** | destroy VM, delete storage pool, change host network, delete data | **YES** (on 8.x the agent's call, §2) | **YES** |
| **State-destructive** | `cleanall`, drop DB, reinstall hive | **YES** (on 8.x the agent's call, §2) | **YES** |

Rule: *can do it* (permission) is not *should do it*. The token is Administrator; the method sets the criteria.

---

## 5. Logbook protocol

One file per day: `lab/logbook/YYYY-MM-DD.md`. Each change, one entry:

```markdown
## HH:MM — <short title of the change>
- **Environment:** prod (192.168.8.207) | lab (…)
- **Type:** read | reversible | irreversible | destructive
- **Goal:** why it is done (one line)
- **Executed:** the EXACT command/endpoint (reproducible)
- **Result:** relevant output / evidence
- **Status:** OK | FAILED | REVERTED
- **Rollback:** how it is undone (or "N/A")
```

**Why this way:** the logbook has to make it possible to **rebuild** the environment and **audit** what happened.
It is the same principle as SY.admin's *command audit log* (`list_recent_commands`): record the
mutation with enough data to replicate it. Secrets **never** go in the logbook — the key's
*name* and its `resource_type` are recorded, never the value.

At the end of the day: a **State at close** block (what was left alive) and **Pending**.

---

## 6. Operational security

- **Secrets**: they go to SY.vault (or to the appropriate manager). In the logbook/commits/logs, only the key's
  *name*. Temporary files with credentials are deleted (local and remote) as soon as they are used.
- ⚠️ **`lab/template-prep.sh` bakes in the `administrator` user with password `magicAI`** (the default of
  the `add_hive` scripts). **It is fine for the lab; in prod it has to be changed.** The script itself
  warns about it. → For prod: a variant with its own credential and `harden_ssh: true`; recovery is through the
  console and a new join (D32: Fluxbee uses SSH only inside `add_hive`; `key_only_persist` was
  removed in 0.1.61).
- **Proxmox tokens**: they are not pasted into commands that end up in logs; they go through an environment variable.

---

## 7. Verification (definition of "done")

A change is not done until there is **evidence**:
- VM created → it appears in `list` and the guest-agent responds (`wait-agent`).
- Service brought up → `systemctl is-active` confirms it.
- Fluxbee node spawned → active unit **and** `CONFIG_GET` returns the expected state
  (`UNCONFIGURED` is correct until the secret is loaded).
- Network change → applied **and** connectivity tested (not just written to `interfaces.new`).

If something fails: the failure is recorded, it is diagnosed, and **the diagnosis also goes into the logbook**
(the findings are the valuable part).

---

## 8. Fluxbee deploy model (a reminder, do not re-decide)

- **Only the motherbee is installed by `.deb`.** Worker / ingress / egress are **clean Linux boxes** that the
  motherbee bootstraps via `add_hive`. Installing postgres on a worker = bug.
- **Compiling is ALWAYS done on the build box** (fb-build), never on a hive or in prod.
- **Growing does not require a new `.deb`**: the runtime/package is published and an `update` is run.
- **`base-nodes.json` is the single source** of what goes into the `.deb` (`build-deb.sh` reads it).
