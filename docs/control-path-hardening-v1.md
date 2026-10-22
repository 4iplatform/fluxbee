# Control-path hardening (v1)

**Status:** design, 2026-10-22, after its review round (security and operations). **Findings:**
A-62, A-20, A-22 (partial), A-46 stage 4 (minimal), A-55 (a switch), A-42, and three found by the
review (§3–§5). **Decisions:** the operator, 2026-10-22: the batch, the A-62 table, D-2 un-parked,
A-42 rule (b), SSH on the DMZ hosts, the technical debt in the same release. After the review: the
three doors it found go in this release, and SSH to the ingress is allowed only from the motherbee
(other networks only when add_hive lists them).

**The rule for every item:** the product applies it on every installation. The code, the baked
policy, or the orchestrator applies it at join and on every reconcile. Nothing is configured by hand
on a hive.

---

## 1. Why now

Root on any spoke (a worker, the egress, or the DMZ ingress) is enough to command the whole
installation, through five doors:

- **A-62, SY.admin.** Its mesh inbox runs `ADMIN_COMMAND` (`msg_type: admin`, outside the SYSTEM
  policy) for any caller. That includes executor plans, `publish_cloud_endpoint` (a fresh Cloud
  bearer), `add_hive`, and the vault and OPA actions.
- **A-20, the system policy.** Rule (3) has no direction: any SY.orchestrator, the ingress's included,
  may send SPAWN, KILL, NODE_CONFIG_SET or SYSTEM_CORE_ROLLBACK to any hive.
- **The orchestrator's admin inbox** (found by the review). Read actions answer any caller, and the
  motherbee's orchestrator forwards them to other hives under its own name.
- **SY.identity** (found by the review). It takes ILK_REGISTER, UPDATE, DELETE and PURGE from the
  orchestrator of any hive, on any ILK, people included.
- **The router's LSA** (found by the review). The motherbee installs whatever a spoke advertises:
  - nodes with any name;
  - routes that match other hives' names;
  - taps on other hives' traffic. For example, a copy of SY.admin's VAULT_PUT that stores the edge's
    private key.

**A-42** is reachable from the internet with nothing compromised. From a public channel, anyone who
types a registered person's email is merged into that person.

---

## 2. A-62: SY.admin answers mesh callers by an allowlist

- **The dispatcher knows where each command came from:**
  - the HTTP API on loopback;
  - the executor running the steps of a plan already approved;
  - the mesh, with its router-stamped `src_l2_name`.

  The first two are trusted, as today. A mesh caller is admitted only by this table, checked first
  in `dispatch_internal_admin_command`. Anything else gets `403 UNAUTHORIZED` and a log line.
- **The table lives next to the action catalog,** so a new action is closed until it is listed.

| Caller | Admitted |
|---|---|
| any caller on an ingress or egress hive | nothing (checked before every row below) |
| `SY.architect@motherbee` | everything (the operator's agent) |
| `IO.cloud@motherbee` | the Cloud-exposed actions; externalize, unexternalize and list for its own channel |
| `SY.wf-rules@H` | node actions; package actions; `sync_hint` (detailed below) |
| `SY.orchestrator@H` | `teardown_node` for a node `*@H` (new, below) |
| `IO.*@H` | externalize, unexternalize and list for its own channels (checked in the handler, as today) |
| `AI.*@H`, `IO.*@H`, `WF.*@H` | `publish_artifact`, and `unpublish_artifact` for `AI.*` (checked in the handler, as today) |

**The `SY.wf-rules@H` row in detail:**
- **Node actions:** `run_node`, `restart_node`, `kill_node`, `get_node_config`, `set_node_config`
  and `get_node_status`, only for `WF.*@H`.
- **`publish_runtime_package`:**
  - only an inline package whose `package.json` says `type: workflow` and `runtime_base: wf.engine`;
  - its name must be `wf.<x>` and never `wf.engine`, since every workflow runs the engine's code;
  - no `sync_to`, `update_to` or bundle upload;
  - refused when a `WF.<x>` node runs on another hive (from the inventory).
- **`remove_runtime_version`:** under the same rule.
- **`sync_hint`:** with target `motherbee` and the folder `fluxbee-dist-runtimes`, as a worker's
  deploy sends it.

**Callers and roles:**
- **Only from Archi and the HTTP API:** `executor_*` and `publish_cloud_endpoint`.
- **A hive's role comes from the registry** (`hives/<id>/info.yaml`). It fails closed: no file, no
  role, a hive still joining, or a removed one gets nothing. The local hive is the motherbee.

**`teardown_node {node_name, ilk_id}`** replaces the four calls a node teardown makes today:
`delete_ilk`, `purge_ilk`, `vault_list`, `vault_delete`.
- **Why:** today the ILK is purged first, so the vault calls can no longer be tied to a hive.
- **What SY.admin does:**
  1. checks that the node is on the caller's hive and that the ILK is that node's;
  2. deletes the secrets dedicated to the ILK;
  3. marks the ILK, then purges it.
- **What it reports:** each step. `status: error` when any step fails. Today a teardown answers
  `ok` over partial failures.
- **During the update:** a spoke not yet updated still sends the old calls. They are refused, so a
  teardown in those minutes leaves the ILK and its secrets, and says so. Its retry after the update
  works.

---

## 3. A-20 and the orchestrator's admin inbox

**Rule (3) gets a direction.** This is `policy/system.rego`; both wasm files are rebuilt.
- **Mutating orchestrator actions come only from `SY.orchestrator@motherbee`:** `SPAWN_NODE`,
  `KILL_NODE`, `START_NODE`, `RESTART_NODE`, `REMOVE_NODE_INSTANCE`, `NODE_CONFIG_SET`,
  `SYSTEM_CORE_ROLLBACK`, `ADD_HIVE_FINALIZE` and `REMOVE_HIVE_CLEANUP`. The last two are never
  admitted on the motherbee itself.
- **Reads come from the motherbee's orchestrator or the same hive's:** `NODE_CONFIG_GET`,
  `NODE_STATE_GET`, `NODE_STATUS_GET`, `LIST_NODES`, `GET_VERSIONS`, `GET_RUNTIMES` and
  `GET_RUNTIME`.
- **Verified:** every cross-hive orchestrator message goes from the motherbee to a spoke, and the
  watchdog acts locally. Updating the motherbee first breaks nothing a spoke sends.

**The orchestrator answers `msg_type: admin` only from `SY.admin@<its own hive>`, reads included.**
F8 today covers only mutations. SY.admin addresses only its own hive's orchestrator (verified), so a
spoke's orchestrator takes no admin messages at all.

---

## 4. SY.identity: an orchestrator acts only on its own hive's nodes

From `SY.orchestrator@H`:
- `ILK_REGISTER` is accepted only for a node: `identification.node_name` ends in `@H`, and the ILK
  type is never a person's.
- `ILK_UPDATE`, `ILK_DELETE` and `ILK_PURGE` are accepted only on an ILK whose `node_name` ends in
  `@H`.

Spawn and its rollback stay on their own hive, so they keep working. Teardown goes through SY.admin
(§2).

---

## 5. The router: who may connect, and what other hives may advertise

**Local socket:**
- created `0600`;
- a connection whose peer is not uid 0 is refused (`SO_PEERCRED`), which also covers the moment
  between bind and chmod;
- every unit, managed node, Go node, e2e script and lab path connects as root (verified).

**From hive P's LSA:**
- **Nodes:** only those named `*@P` are installed. Others are dropped and counted.
- **Routes:** apply only to destinations on P, or to an `@*` destination.
- **Taps:** apply only to a message P is a party to, either as its source or its destination.
- **Impact:** a hive can no longer redirect or copy traffic it does not already see. PROD has no
  routes and no taps on any hive (checked 2026-10-22). `docs/reference/routing-and-policy.md` changes
  to say so.

**The HELLO suffix check from the first draft is dropped.** Names arriving over the WAN are already
rewritten to the authenticated hive, so the check adds nothing.

---

## 6. A-46 stage 4, minimal: SSH on the DMZ hosts

Each DMZ host's orchestrator applies its rules at boot and on every reconcile. It loads them through
a boot unit, the same machinery as the egress's table. No hand edits.

**The ingress.** A `table inet fluxbee_ingress` accepts SSH (the ports `sshd -T` reports, IPv4 and
IPv6) only from:
- the motherbee's address, the one in the hive's own uplink configuration;
- the networks listed in `ingress.ssh_allow_from`, which add_hive writes when the operator passes
  them.

All other SSH is dropped. This works the same with one NIC or several.
- **The join checks this before installing anything.** Over the bootstrap session it reads the
  motherbee's source address (`SSH_CONNECTION`). If that address is not allowed, the join stops and
  says which address to add.
- **Afterwards:** the join's later SSH steps (hardening, key revoke) arrive from an allowed address,
  so the join cannot lock itself out.

**The egress.** `egress.ssh_from_wan: true` stays the default (the operator's decision; D31). With
`false`, its existing table drops SSH arriving on its WAN interface.

**Removed:** the ufw and firewalld writes (`open_firewall_rules_local` and its peers). They protect
nothing today, and turning ufw on would lock SSH and :443 out (D25).

**New:** an active ufw or firewalld raises a posture alert (`host_firewall_active`), since it may close
9000, 9100, 22000 or 443 without anyone seeing it.

**Cleanup:** remove_hive and the factory reset drop the new table and its unit.

**Recovery:** the hypervisor console, or `ingress.ssh_allow_from` in the ingress's `hive.yaml` with an
orchestrator restart.

Full default-deny on every role stays parked.

---

## 7. A-42: only an email the channel vouches for identifies a person

**The frontdesk:**
- accepts a handoff only from a router-stamped `IO.*` sender;
- treats the handoff's `subject.email` as vouched;
- treats an email typed in the conversation as claimed;
- never upgrades a typed email to vouched when a handoff arrives without one.

**SY.identity:**
- **A vouched email** goes in `identification.email`. It stays the unique key, and the merge with an
  existing person works as today. This covers:
  - the IO node's handoff;
  - the Cloud;
  - io.api, where the integrator holding the bearer vouches for its users.
- **A claimed email** goes in `identification.claimed_email`. It is stored and shown, but it is never
  indexed, never a lookup key and never merged. So a typed email can neither take over a registered
  person nor reserve an address for a later merge.
- **The registration completes either way,** so there is no DUPLICATE_EMAIL loop with a person stuck
  temporary.
- **When a claimed email matches someone,** the frontdesk registers a new person anyway. Its reply is
  the same whether or not the email is known, so a channel cannot probe which emails are
  registered. If it is the same person, the operator merges the two explicitly through SY.admin.
- **Records in PROD stay as they are.**

---

## 8. Technical debt in the same release

**Separate files, built alongside the items above:**
- **rcgen 0.14 and x509-parser 0.18** for the whole workspace.
  - **Test:** take a CA certificate and key made by today's code as a fixture; the new code must
    issue a leaf from it that verifies.
  - **The gate:** spokes on the old code must reconnect to the updated motherbee before their own
    update.
- **`turns.ctx`/`seq` in two steps,** so the apt rollback to 0.1.82 keeps working. 0.1.82 creates the
  `(ctx, seq)` index at start and would exit without the columns.
  - **0.1.83:**
    - stops writing them;
    - removes them from the INSERT;
    - stops creating the index;
    - keeps them as nullable columns (`DROP NOT NULL`).
  - **The next release:** drops both columns and the index.
  - **Every DDL** runs under `lock_timeout`.
- **Stale docs:** the IO docs that still describe `ctx`; two future designs move to `docs/history/`.
- **Dead code** the compiler does not flag.

**Last, alone, once the rest is merged:**
- `cargo clippy --fix` and the trivial fixes;
- the remaining Spanish comments translated;
- a clippy gate in CI (`-D warnings`, allowing the structural lints such as `too_many_arguments`).

---

## 9. What root on a spoke can still do

These remain after the batch and are recorded, not fixed:

- **The ingress terminates TLS,** so its root sees every bearer and can act as SY.edge toward IO.cloud.
  The Cloud surface is as strong as the ingress host.
- **Its own SY.\* nodes can read the root tenant's general secrets,** such as the AI provider key the
  workers' agents already use.
- **It can speak as any person,** because the router does not bind `src_ilk` to the sender.
- **It can write into the motherbee's `blob/active`,** a two-way Syncthing folder.
- **It can publish public artifacts** as its own nodes.
- **On the LAN, it can impersonate the unsigned apt repository** (A-49).
- **On the motherbee, a local non-root user can call the loopback HTTP APIs** (SY.admin :8080,
  Archi :3000).
- **A node name is not bound to its process** on its own hive. That waits for A-60.

---

## 10. Tests and gate

**Unit tests:**
- **A-62:** every row of the table, and the refusal of everything else, including a caller on the
  ingress and `wf.engine` from wf-rules.
- **teardown_node:** its order of steps and its report.
- **The rego truth table:**
  - mutations from a spoke are refused;
  - reads from the same hive are admitted;
  - the spoke-only actions are refused on the motherbee.
- **The orchestrator:** its admin inbox from a foreign caller.
- **SY.identity:** the scoping by `node_name`.
- **The router:**
  - the socket mode and the peer-uid refusal;
  - the LSA filters for nodes, routes and taps.
- **The nft rules rendered per role,** with and without the settings.
- **A-42:** vouched versus claimed email, and the handoff sender check.
- **The rcgen fixture.**

**Live gate on 8.x:**
- **Refusals:** from a process on ingress1,
  - an `ADMIN_COMMAND` to SY.admin is refused;
  - a SPAWN on the motherbee is refused;
  - a read to the motherbee's orchestrator is refused;
  - an ILK_PURGE to SY.identity is refused;
  - a foreign node name in its LSA is dropped.
- **What must keep working:**
  - Archi;
  - a WF deploy on worker1;
  - a node purge on worker1, with every step reported `ok`;
  - Cloud;
  - an AI publish.
- **The admin log** shows no UNAUTHORIZED from a legitimate caller during the gate.
- **Spokes** reconnect to the updated motherbee before their own update.
- **`POST /acme/test` passes,** which exercises the ported challenge certificate and CSR.
- **SSH on the ingress:** refused from outside, accepted from the motherbee.
- **A new hive joins** (VM 104).
