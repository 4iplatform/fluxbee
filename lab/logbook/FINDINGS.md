# FINDINGS — findings from the integrated deployment in PROD alpha

> **What this is.** The cumulative record of what the integrated test in production brings to
> light. The daily logbook (`YYYY-MM-DD.md`) tells *the journey*; this document gathers *the
> findings* so that a well-founded **code-change plan** can come out of them later.
>
> **The rule that governs it** (`METHOD.md` §3, rules 0b/0c): during the deployment **no code is
> touched and nothing is patched to make it work**. A failure is **data**, not an obstacle.
> Everything that shows up here is discussed with the operator **before** it becomes a change.

**States:** 🔴 confirmed in code, open · 🟡 partial, parked or observed (the heading says which) ·
🔵 to evaluate · ✅ resolved (the heading names the version and how it was validated) · 🟢 lab note or
no change required

---

## A. The fluxbee product — candidates for a code change

### A-1 ✅ VALIDATED ON 8.x (0.1.81, 2026-10-20) — The orchestrator does not configure secondary network cards

- **Fixed (0.1.81, `4e9f91e`):** the egress join reads `edge_ip` from `lan_iface` on the host
  (an explicit one must be on it); the LAN leg, IPv4 forwarding and the IPv6 block are checked at
  boot (fatal), on every drift tick (alert) and by the join on the box (`EGRESS_NAT_NOT_READY`,
  `EGRESS_LAN_LEG_INVALID`). PROD's egress1 had `edge_ip` 10.10.10.1, add_hive's guess, on a leg
  that is 10.10.10.40: corrected by hand before the upgrade. Validated: `lan_leg_ok=true`.

> **Reframed in the 2026-08-03 audit ([PB-5](PENDING-BUGS.md#pb-5)):** addressing the cards is the
> operator's job by contract, in every role. What stays open is that the egress reports
> `nat_applied: true` without checking its LAN leg.

- **What happens:** `add_hive role=ingress|egress` **requires** the interface names (`wan_iface`,
  `lan_iface`) and assumes they **already exist and are addressed**. Not a single line in the
  orchestrator assigns IPs to interfaces.
- **Evidence:** `src/bin/sy_orchestrator.rs` — `resolve_add_hive_egress_section` validates
  `lan_cidr`/`wan_iface`/`lan_iface`; `reconcile_egress_nat` applies nftables **on interfaces it
  assumes are configured**. Zero address handling.
- **Impact:** every deployment with ingress/egress needs **manual or external** network
  configuration (in this case, cloud-init). On bare metal without cloud-init, someone does it by
  hand → a non-reproducible step, outside the recipe.
- **Detected by:** the operator anticipated it; confirmed by reading the code.
- **Status:** a real product gap. **To discuss:** should the orchestrator accept the configuration
  of the secondary legs in the `add_hive` payload and apply it?

### A-2 ✅ RESOLVED (0.1.81: the join warns; PB-6 option b) — `harden_ssh` defaults to `false`

- **Fixed (0.1.81, `4e9f91e`):** when the join's password fallback ran and the box was not hardened,
  the join answers a `warnings` entry saying password logins stay on and how to close them (PB-6
  option b; option a went with D32). Covered by tests; no join ran in the gate.

> **Rewritten in the 2026-08-03 audit ([PB-6](PENDING-BUGS.md#pb-6)):** flipping the default
> would leave boxes with no way in at all. What stays open is that the `ssh_password` channel does
> not close by itself.

- **What happens:** with an `ssh_password` bootstrap, if `harden_ssh:true` is not passed
  explicitly, when the join ends `add_hive` **removes its key and its sudoers entry but leaves
  `PasswordAuthentication yes`**. The machine is left with password login open.
- **Evidence:** `resolve_add_hive_harden_ssh` → default `false`. The hardening
  (`disable_remote_password_auth_with_access` + verification) only runs if it is `true`.
- **Impact:** the correct mental model is *"the box opens for a few seconds and `add_hive` closes
  it"*. With the current default **that does not hold** unless the operator remembers the flag.
- **Status:** **to discuss.** Options: flip the default, or make it loud (warn in the response
  when the bootstrap used a password and no hardening ran).

### A-3 ✅ CLOSED (→ PB-7, `9f86c12`) — The admin timeout (180 s) can fall short for `add_hive`

> **Closed with [PB-7](PENDING-BUGS.md#pb-7):** `add_hive` answers `202` right away and the join
> runs in the background, with its phase in `info.yaml`. Validated live on 2026-08-05.

- **What happens:** `JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS` defaults to **180 s**, but the flow's
  internal waits can add up to more (30 s health + 60 s WAN + 60 s LSA + finalize).
- **Evidence:** `src/bin/sy_admin.rs` (timeout) vs. the gates of `add_hive_flow` in
  `sy_orchestrator.rs`.
- **Existing mitigation:** the hive stays in `status: pending` and **retrying is idempotent**.
- **Impact:** on slow boxes the client sees a *timeout* even though the join carries on and ends
  well → it confuses, and it can lead someone to "fix" something that was working.
- **Status:** **to discuss.** Raise the default, or have the response say explicitly
  "in progress, retry to see the status"?

### A-4 ✅ CLOSED (→ PB-8) — `egress.gateway_ip` propagates to the workers, but **not to motherbee**

> **Closed with [PB-8](PENDING-BUGS.md#pb-8):** it was not an oversight. `egress.gateway_ip` is, by
> contract, the workers' way out, and propagating it to the motherbee would leave it without a
> control plane. The motherbee reports it with `EGRESS_MOTHERBEE_BYPASS` and does not touch its
> route. Validated live.

- **What happens:** when MB declares `egress.gateway_ip`, **each worker** routes its default
  through the egress (`reconcile_worker_egress` → `ip route replace default via <gw>`). **MB is not
  on that list.**
- **Impact:** the outbound traffic of **motherbee** —which is precisely the one that calls the
  external APIs (OpenAI, Slack, Meta)— **does not go out through the egress node**, but through its
  own default route. In this deployment that means it would go out through the management network,
  which is exactly what the design wants to avoid.
- **Status:** **observed in the code, not yet confirmed live.** It is one of the points to look at
  once the egress is working. **To discuss:** is it intentional (MB must have its own way out) or
  is it a gap?

### A-5 ✅ RESOLVED (0.1.60 with A-52, validated 0.1.61) — A freshly booted clone **is not ready** for `add_hive`

- **Status (2026-10-19 review):** `wait_for_host_to_settle` runs in the three join flows; a fresh clone waited 8 minutes (ledger 0.1.61) and joined again on 0.1.62.

- **What happens:** the first boot of a VM cloned from the cloud image triggers automatic updates
  (`unattended-upgrades`), **reboots by itself**, and during that time the `qemu-guest-agent`
  (and potentially sshd) become intermittent.
- **Measured:** `netin` 37 MB → 52 MB, `diskwrite` 2.5 GB, a spontaneous reboot, the agent down for
  several minutes.
- **Impact:** running `add_hive` "as soon as the VM boots" can hit a box in the middle of an
  upgrade → an intermittent failure, hard to diagnose, and on top of that combined with **A-3**
  (timeout).
- **Status:** observed in this deployment. **To discuss:** should `add_hive` have an explicit
  *readiness gate* (wait until the box is quiet), or is documenting it in the recipe enough?

### A-6 ✅ RESOLVED — `build-deb.sh` **never compiled `ai_node_runner`** (divergence from `install.sh`)

- **What happens:** the `[1/5] build rust` step of `packaging/build-deb.sh` does:
  ```bash
  cargo build --release --bins
  cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov
  cargo build --release --manifest-path nodes/io/Cargo.toml -p <io crates>
  ```
  and a comment claims: *"ai.generic (nodes/ai) is a root workspace member already built by
  `--bins`"*. **That is false.**
- **Why:** the root `Cargo.toml` has **`[package]` (json-router) AND `[workspace]`**. With a root
  package present, `cargo build --bins` **without `--workspace`** compiles **only the root
  package's bins**. `nodes/ai/ai-generic` (package `fluxbee-ai-nodes`, bin `ai_node_runner`) is a
  **member** → **it is never compiled**. Note that `sy-frontdesk-gov` —another member— **does** have
  its explicit line: the correct pattern is already applied there; `ai-generic` was missed.
- **Symptom:** the build reaches `[3/5] stage files` and aborts:
  `Error: binary does not exist: target/release/ai_node_runner` → **no `.deb` is produced**.
- **Why it was never noticed:** on the **lab** build box the binary already existed in
  `target/release/` because an earlier manual `build2.sh` **does** compile it explicitly
  (`cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner`). `build-deb.sh` found it and
  carried on. **On a clean box —like prod— it fails.**
- **Impact:** the **canonical** build path is not self-sufficient. Any new build box (or a
  `cargo clean`) breaks the `.deb` build.
- **Proposed fix (one line, pending approval):** add it next to the `sy-frontdesk-gov` one:
  ```bash
  cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner
  ```
- **What makes it a divergence (not a design decision):** **`scripts/install.sh` line 246 DOES
  have** `cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner`. And `base-nodes.json`
  states explicitly that it is *"the single source of truth read by BOTH packaging/build-deb.sh and
  scripts/install.sh (**they must not diverge**)"*. In other words: `install.sh` was right and
  `build-deb.sh` was the outlier → **de-diverge, don't invent**.
- **✅ RESOLVED** (approved by the operator): `build-deb.sh` got the **same line** that
  `install.sh` already had, and **the two false comments were corrected**, the ones that claimed
  `--bins` built it (those comments are the reason the bug survived). **The two scripts ended up in
  exact parity** on the three explicit builds.
- **Shortcut rejected:** the error itself suggests `-buildvcs=false` for B-8, and here compiling
  the binary by hand would have been enough — both would have made the build pass **while hiding
  the problem**, leaving the next clean box just as broken.

### A-7 ✅ RESOLVED (0.1.41–0.1.42) — User OPA did not reach the spokes (CONFIG_CHANGED never left the motherbee)

- **What happens:** the admin sends OPA writes as CONFIG_CHANGED; the router intercepts it before
  routing, ignores the destination and only distributes it inside the host. A clear aimed at
  worker1 was run by the motherbee (2026-09-28). No spoke has anywhere to get the policy from;
  ingress and egress do not even run SY.opa.rules. And the compiled wasm (140–155 KB) does not fit
  in a message (128 KiB cap).
- **Fix (the operator's design, logbook 2026-09-30; `docs/opa-distribution.md`):** SY.opa.rules on
  every hive; only the motherbee compiles and publishes the wasm in the Syncthing folder
  `fluxbee-dist-policy`; a CONFIG_CHANGED `{opa, sync}` announces it; each hive installs it if the
  sha256 matches; one single global policy; eventual convergence; no encryption. CONFIG_CHANGED now
  travels over normal routing (it crosses hives).
- **Validated on 8.x:** apply on the 4 hives in 16 s; a hive cut off from Syncthing caught up by
  itself, 27 s after reconnecting; global clear (slow until 0.1.43: A-19).

### A-8 ✅ RESOLVED (0.1.36) — One hive's config applied on another: `add_route` on a spoke overwrote the motherbee's routes

- **Confirmed live** on 8.x (logbook 2026-09-30): two adds on worker1, and the motherbee ended up
  with those routes. The broadcast after each add/remove carried the spoke's list, and the admin's
  counter ran one ahead of SY.config.routes'.
- **Fix:** `ConfigChangedPayload.hive`; the admin fills it in, and SY.config.routes / SY.opa.rules
  ignore another hive's config.

### A-9 ✅ RESOLVED (0.1.36) — Any node could rewrite its hive's routes, taps or OPA

- **What happened:** CONFIG_CHANGED was not a protected action, and the router's intercept
  distributed it without looking at the origin; SY.config.routes accepted `add_route`/`add_tap`
  from anyone (the `admin` type does not go through the router's gate); SY.opa.rules accepted
  `compile/apply/rollback_policy` from anyone. A user node could, for example, add itself a tap and
  mirror traffic.
- **Fix:** CONFIG_CHANGED protected (router + `system.rego`, current rules, no recompilation);
  SY.config.routes mutations and SY.opa.rules commands only from `SY.admin@motherbee`.

### A-10 ✅ RESOLVED (0.1.36) — Nodes outside the motherbee pointed at `SY.vault@<their hive>` / `SY.admin@<their hive>`

- **What happened:** vault and admin run only on the motherbee. Cognition, the AI runners and the
  IO nodes derived the vault's destination from their own hive; SY.wf-rules published via
  `SY.admin@<their hive>`. On a worker, none of that exists. SY.edge had already solved it with
  `vault_hive`.
- **Fix:** `VaultClient::for_primary` and `PRIMARY_HIVE_ID` in the SDK (the router re-exports it);
  `PrimaryAdminNode` in the Go SDK for SY.wf-rules.

### A-11 ✅ RESOLVED (0.1.36) — The identity replica did not catch up on reconnect

- **What happened:** the full sync ran only at startup. If the delta stream was cut, or the worker
  started with the motherbee down, the replica kept old data (or only the system ILKs) until the
  next restart, even though the log said it would converge on reconnect.
- **Fix:** each subscription starts with a full snapshot; a sequence gap reconnects and resyncs
  instead of `exit(0)`.

### A-12 ✅ RESOLVED (0.1.37) — A WF's first spawn on a worker ran before Syncthing

- **What happened:** with the publish fixed (A-10), the deploy reached the spawn before the package
  had synced to worker1 (~11 s measured) → `RUNTIME_NOT_AVAILABLE`, a single attempt.
- **Fix:** wf-rules retries (backoff 1→4 s, up to 20 s) while the orchestrator answers
  `RUNTIME_NOT_AVAILABLE` / `RUNTIME_NOT_PRESENT` / `BASE_RUNTIME_NOT_AVAILABLE`.

### A-13 ✅ RESOLVED (0.1.37) — The SDK masked the vault's verdict

- **What happened:** `VaultClient::get`/`list` parsed the response as a success before looking at
  the `status`; a `KEY_NOT_FOUND` arrived as `json error: missing field key`, and the
  `KEY_NOT_FOUND` branches of io-slack and io-wapp never ran.
- **Fix:** read the verdict from the raw JSON before parsing.

### A-14 ✅ RESOLVED (0.1.37) — OPA's SHM was `0666`

- **What happened:** SY.opa.rules created `/dev/shm/jsr-opa-<hive>` `rw-rw-rw-` (and set it back to
  that if it existed): any process could read the policy or write one for the router. Its Rust
  peers use `0600`. Policy files `0644` in `0755` dirs.
- **Fix:** region `0600` (corrected at startup), files `0600`, dirs `0700`.

### A-15 ✅ RESOLVED (0.1.38–0.1.40) — wf-rules deleted packages from a worker's `dist/` mirror

- **What happened:** after apply/rollback/delete, wf-rules purged versions and rewrote the manifest
  in its **local** `dist/runtimes`. On a worker that is a receive-only Syncthing mirror: the local
  deletion is never restored. worker1 was left with 15 local changes (13 deletions); the
  `wf.w1probe` package disappeared from the mirror and the WF node went into a crash-loop because
  `flow/definition.json` was missing. On top of that, the orchestrator accepted the spawn with the
  manifest already synced but without the package's files.
- **Fix:** on a hive that is not the motherbee, the purge asks the admin for
  `remove_runtime_version` (it is deleted at the origin and the sync carries that over); the
  orchestrator answers `RUNTIME_NOT_PRESENT` until the package's `package.json` is on the hive. In
  0.1.38 the request went out without a `target` (`INVALID_REQUEST: missing target`); 0.1.39 aims
  it at the motherbee; 0.1.40 purges leaving the current version for last and takes
  `RUNTIME_NOT_FOUND`/`RUNTIME_VERSION_NOT_FOUND` as done.
- **Validated (0.1.40):** create → re-apply → delete a WF on worker1: node HEALTHY on 0.0.1 and
  0.0.2 (18 s each), delete OK, motherbee with no versions and no manifest entry, worker1's mirror
  with 0 local changes.
- **One-off repair:** worker1's mirror was restored with Syncthing's `POST /rest/db/revert`
  (15 → 0 local changes); after that `WF.w1probe@worker1` became HEALTHY: the first WF running on
  a worker.

### A-22 🟡 PARTIAL (0.1.44 + 0.1.83: sockets 0600 with a peer-uid check) · binding a name to its process (P-1) PARKED with A-60 — A local node could send messages as another node

- **What happened:** the router looked up a frame's sender by the `routing.src` the frame declared,
  and from there stamped its L2 name, which is what the gate uses to decide protected actions.
  Confirmed live on 8.x: a test node on the motherbee sent `NODE_STATUS_GET` (protected) with
  SY.admin's UUID, and SY.config.routes answered the admin; with its own UUID, the gate dropped it.
  UUIDs travel in every message (a tenant node sees its orchestrator's in every `node_config`
  signal), so any local node could act as the motherbee's admin or as its hive's orchestrator,
  which under rule 3 can perform every protected action on any hive.
- **Fix:** the router drops the frame if `routing.src` is not the UUID that socket used in its
  HELLO, which is what `02-protocolo.md` already required ("the router stamps `src_l2_name` from
  the authenticated session"). The Rust, Go and IO senders were reviewed: all use their own UUID.
- **Validated:** the same probe is now dropped (`routing.src is not the sending node`); no
  legitimate traffic dropped on the 4 hives.
- **What stays open (DTAP panel 2026-10-01, P-1):** 0.1.44 closed the case of a node using
  another's UUID inside its own connection, but the **name** is declared by the process itself in
  the HELLO and the router accepts it: socket `0666`, no reserved names, no rejection of duplicates
  or of a foreign `@hive`. Any local process can connect with a new UUID as `SY.admin` and write
  the global policy, or as `SY.orchestrator` on a spoke.
- **The operator's decision (2026-10-01): DEFER** the security package (today it costs more than it
  yields). It includes: authenticating the name in the HELLO (P-1), the direction of rule 3
  (A-20 / D-2), signing the published manifest (P-8), `get_policy` exposing the rego (P-16), and
  encrypting the synced copy (already deferred in the design).

### A-23 ✅ RESOLVED (0.1.48) · composing several solutions' policies and a registered owner PARKED — The architect modeled OPA per hive; there is one single global policy

- **Status (2026-10-19 review):** what stayed "to review" is still not built: the last solution's policy wins and its owner is deduced, not stored. That was the operator's decision of 2026-10-01, not a defect.

- **What happens (seen in code, 2026-10-01):** a solution declares `opa_deployments` per hive
  (`hive`, `policy_id`, `rego_source`) and the plan emits one `opa_compile_apply` for each.
  - If the hive is not the motherbee, the admin rejects it with 400.
  - If it is the motherbee, the policy becomes the one for every hive. Two deployments overwrite
    each other.
  - The snapshot looks for `policy_id`/`rego_hash` in the state, which does not have them: the
    reconciler never sees the policy that is running and re-applies it on every run.
  - Removing it is blocked (there is no remove action).
  - The plan compiler (AI) receives only the `delta_report`: nobody passes it the declared rego.
- **The operator's decision (2026-10-01):**
  1. The solution declares one single global policy, with no hive.
  2. It is compared with the rego running on the motherbee; if they are the same, nothing is done.
  3. It is applied with one single `opa_compile_apply`; a `pending` hive is not a failure.
  4. When the owning solution stops declaring it, `opa_clear`.

  If two solutions declare OPA, **the last one wins**. The "owner" is the solution whose policy is
  running now; it is recognized by comparing the rego with the stored manifests. A solution only
  does a clear if the running policy is its own, and the plan says whose policy it replaces.
- **Fix (0.1.48, `1486807`):** `desired_state.opa` (one policy, no hive); the snapshot reads the
  motherbee's policy with `opa_get_policy`, and the owner from the stored manifests; one single
  `opa_compile_apply`, and `OPA_REMOVE` = `opa_clear`; the plan compiler receives the rego, and the
  plan's OPA steps are pinned to it; the confirmation says whose policy is being replaced.
- **Validated:** tests (177 of the architect's, including the real response from the 8.x admin).
  The full pipeline was not run live: it needs the architect's AI (A-26).
- **Pending review (a deliberate simplification, like the security package):** composing the rules
  of several solutions into one single policy, instead of the last one winning; and an owner
  registered together with the policy, instead of deducing it by comparing the rego.

### A-24 ✅ RESOLVED (0.1.49 + 0.1.81, the WF half) — The architect's snapshot asked an admin that does not exist for each hive's state

- **The WF half (0.1.81, `4e9f91e`):** the snapshot reads wf-rules' real answer (`workflows`,
  `current_hash` reproduced exactly); a DMZ hive is not asked for runtimes or workflows; workflow
  ownership comes from executed manifests (external by default) and removal reaches every hive
  the solution used; the plan compiler gets the declared definitions and the steps are pinned.
  The review found more in the same reconciler: see A-77.

- **What happens (seen in code, 2026-10-01; not reproduced live):** `build_actual_state_snapshot`
  sends each read to `SY.admin@<hive>`. Only the motherbee has an admin (on worker1, `sy-admin` is
  inactive), so in a solution with more than one hive those reads fail, the snapshot is left
  incomplete and the reconciler blocks the whole run.
- **Fix (0.1.49):** the snapshot and the plan compiler's `query_hive` query ask
  `SY.admin@motherbee`, with the hive as the target (the admin is, and will be, only on the
  motherbee; operator, 2026-10-01). A source-code test prevents naming a per-hive admin again.
- **Still open:**
  - The plan compiler does not receive the definition of the declared workflows
    (`wf_deployments`) either: the same gap A-23 had with the rego.
  - On ingress and egress, `list_runtimes` answers `RUNTIME_MANIFEST_MISSING` and there is no
    SY.wf-rules (that is how it is, by role). A solution that names a DMZ hive still ends up with
    an incomplete snapshot, and that blocks the run. On worker1 the five reads come back ok.

### A-25 ✅ RESOLVED (0.1.49, the handbook found on the motherbee) — The `.deb` did not install the architect's handbook (`install.sh` did)

- **Status (2026-10-19 review):** `packaging/build-deb.sh` installs `docs/archi/handbook_fluxbee.md`; the 0.1.49 gate found it on the motherbee.

- **What happens (2026-10-01):** the architect appends `/etc/fluxbee/handbook_fluxbee.md` to its
  prompts if it exists. `install.sh` copies it, `build-deb.sh` does not: it is not on the 8.x
  motherbee, so the PROD architect works without a handbook. The same class of divergence as A-6.
- **Fix (0.1.49):** `build-deb.sh` installs it in the same place (a static document: it is
  replaced on every upgrade, it is not a conffile).

### A-26 ✅ RESOLVED (0.1.49, validated) — The architect read its secrets from the vault only once, at startup

- **Status (2026-10-19 review):** `resolve_resource_awaiting_vault` waits for the vault at boot (`VAULT_BOOT_WAIT`); re-reads after a vault notice or a `CONFIG_SET` are still one-shot, as in the sibling nodes.

- **What was seen (2026-10-01, when installing 0.1.47 and 0.1.48):** at startup, the architect
  could not read its AI key or the URL of its message database (`vault unreachable`): during an
  upgrade the whole hive restarts, and the vault was not answering yet. It stayed listening with
  `ai_configured=false` and `messages_db_configured=false`.
- **Correction of the diagnosis:** the vault's startup notice (`VAULT_SECRET_CHANGED`, one per
  secret) **did** arrive: the message database reconnected 0.4 s later. The AI stays off because
  the 8.x vault has no AI key (it has 3 secrets: edge, TLS and postgres). Even so, that notice goes
  out only once, and the SDK documents that a node that does not hear it in time stays degraded
  (that is what happened to SY.storage).
- **Fix:** like SY.storage and SY.identity, the two startup reads retry until the vault answers
  (`resolve_resource_awaiting_vault`, up to `VAULT_BOOT_WAIT`, both at once). Re-reads on a notice
  or on `CONFIG_SET` are still one-shot.

### A-27 ✅ RESOLVED (`802faf5`) — The parity guard of the admin's catalog (CI) had already been red before

- **What happens (2026-10-01):** `scripts/admin_action_catalog_parity_check.sh` runs on GitHub
  Actions on every push that touches `sy_admin.rs`, and it fails: its expressions do not recognize
  how 20 actions that do have an HTTP route, or are internal-only, are dispatched today:
  `get_runtime`, `list_cloud_actions`, `publish_artifact`, `publish_cloud_endpoint`,
  `unpublish_artifact`, the 8 `timer_*` and the 8 `wf_rules_*`. Every push in the OPA stretch that
  touched the admin set it off red.
- **Fix:** the script also follows `handle_admin_query_with_payload`, the timer RPCs, the generic
  internal dispatch, the wf-rules handlers and the cloud endpoint one, and it declares the
  exceptions on purpose: actions that only arrive over the mesh (`publish_artifact`,
  `list_cloud_actions`) and actions with a dedicated arm (`externalize`, `unexternalize`,
  `list_externalized`); both lists fail if they go stale. In CI it passes since `802faf5`.

### A-28 ✅ RESOLVED (`3eeadf2`) — Three compiled Go binaries were versioned in git

- `go/sy-opa-rules/sy-opa-rules`, `go/sy-timer/sy-timer` and `go/nodes/wf/wf-generic/wf-generic`.
  They dirtied every `git status` after a local build and were one `git add -A` away from a commit.
- **Fix:** they leave the index (the files stay on disk) and `.gitignore` ignores the four that the
  build writes (also `sy-wf-rules`). Approved by the operator in the small-topics batch
  (2026-10-02).

### A-29 ✅ RESOLVED (0.1.51) — sy-timer and wf-generic: SQLite waited for the lock on only one connection

- **What happened:** the first Go CI showed a flaky sy-timer test (20 concurrent `TIMER_SCHEDULE`
  with the same `client_ref`): it failed 10 out of 15 times on fb-build with
  `TIMER_STORAGE_ERROR: database is locked (SQLITE_BUSY)`. The cause was in the product:
  `busy_timeout` (and `synchronous` in wf-generic) is a **per-connection** pragma, and it was
  applied with `db.Exec` on the `database/sql` pool. Only one connection waited for the lock; the
  others failed instantly. In sy-timer the scheduler writes while requests are being served, so a
  scheduling or a firing could fail intermittently in production.
- **Fix:** the pragmas go in the DSN (`path?_pragma=...`), which modernc.org/sqlite applies to
  every connection it opens. New tests check three connections at once; the sy-timer suite passed
  15 times in a row with `-race`.

### A-30 ✅ RESOLVED (0.1.51) — The admin did not decode the query's `%xx`

- **What happened (while validating 0.1.50):** a `DELETE /hives/{h}/taps` with `@` sent as `%40`
  (the way Python's `urlencode` sends it) answered `NOT_FOUND`: the admin compared the value
  without decoding it.
- **Fix:** query keys and values are decoded; `+` stays literal (raw `+` signs are more common
  here, like `Etc/GMT+3`) and a stray `%` is kept.

### A-31 ✅ RESOLVED (0.1.57) — The services write ANSI color codes into the journal

- **What happens:** levels come out as `\x1b[33m WARN\x1b[0m` in `journalctl`. A `grep ' WARN '`
  finds nothing, and while validating 0.1.52 an error count gave a false 0 until the codes were
  stripped with `sed 's/\x1b\[[0-9;]*m//g'`.
- **Why:** each binary builds its own `tracing_subscriber` (36 files), and none turns color off
  when the output is not a terminal.
- **Proposed fix:** a tracing init in the SDK with `with_ansi` only if the output is a terminal,
  used by every binary. It is a mechanical change in 36 files: it goes with the mass-formatting
  pass (item 8 of the batch), at the operator's decision.
- **Fix:**
  - `fluxbee_sdk::logging::fmt()` and `ansi()` only add color when stdout is a terminal.
  - The 36 tracing startups go through there: the core binaries, the IO/AI/gov/test nodes, the
    diags and the SY.edge layer.
  - The mass formatting stays separate; it is the operator's decision.
- **Verified:** a diag with its output to a pipe went from 48 ESC sequences to 0, and its levels
  come out as ` WARN `.
- **Validated live (0.1.57, 2026-10-06):** zero ANSI codes in the new processes on the 4 hosts.
  Before the upgrade there were hundreds per service on the motherbee (rt-gateway: 564).


### A-32 ✅ RESOLVED (0.1.53) — Personal data in logs and responses (frontdesk and identity)

- **What happened:**
  - SY.frontdesk.gov wrote to the log, at INFO level, the `ILK_REGISTER` payload (name, email,
    phone, company), the company name the person typed (`TNT_CREATE`), the first 240 characters of
    each message and the sender's id (on WhatsApp, their phone number).
  - SY.identity's errors traveled with their free text. For a duplicate email that text carries the
    Postgres DETAIL with the email.
  - SY.identity logged the `ILK_PROVISION` payload (the person's address) and returned the Postgres
    DETAIL to the caller.
- **Fix:**
  - For a registration, it logs `ilk_id`, `tenant_id` and which fields came in, never their values.
  - For an identity error, only its code.
  - SY.identity logs `ich_id` and `channel_type`, and answers with the Postgres message without the
    DETAIL.
  - Three tests capture the logs of the frontdesk's three entry points; with the old fields they
    fail.

### A-33 ✅ RESOLVED (0.1.53) — SY.frontdesk.gov: definitive rejections from identity reported as retryable

- **What happened:** errors were classified by searching their text. TENANT_PENDING, ILK_DELETED,
  SYSTEM_ILK_PROTECTED, DUPLICATE_* and others ended up as `IDENTITY_UNAVAILABLE` with
  `retryable: true`.
  - **Also:** the fallback target's response was not checked. A `status:"error"` came out as
    `registered: true`, a false REGISTERED. It could only happen with
    `GOV_IDENTITY_FALLBACK_TARGET` configured.
- **Fix:**
  - The bridge returns the SDK's `IdentityError`, just like the orchestrator, and SY.identity's
    code is kept as is.
  - Only transient errors are retryable: no response, NOT_PRIMARY, DB_NOT_READY and
    DB_WRITE_FAILED.
  - Both responses go through the same status check.

### A-34 ✅ RESOLVED (0.1.53) — Cognition: the cold start never ran, and it rebuilt wrongly

- **What happened (seen in PROD and in a code review):**
  - **The rebuild never ran.** The Postgres credential arrives from the vault after startup, and
    the rebuild was only attempted at startup.
  - **It rebuilt with other keys.** It indexed by entity id while the live path indexes by label.
    After a restart, the next turn duplicated contexts, reasons and memories with the same id.
  - **The live state was never pruned.**
  - **A scope was never cut:** a sustained topic change renamed it instead of cutting it.
  - **`storage.enabled: true` was hardcoded** in CONFIG_GET.
  - **The deterministic text of the episodes was never sent.**
- **Fix:**
  - **Rebuild:**
    - It waits for the vault at startup (the A-26 pattern, up to 60 s).
    - If it still does not load, the first turn retries it, only once and inside the turn task.
    - It builds the state with the same keys as the live path, and it only installs over an empty
      state.
    - Episodes get their real instance back.
  - **Pruning:** the live state is trimmed to the same set that goes into the `jsr-memory` SHM.
  - **Scopes:** a sustained topic change cuts the scope; one that still binds is drift, and it
    relabels the scope. The dominant one is chosen deterministically.
  - **CONFIG_GET:** reports the real `storage.db_configured`.
  - **Episodes:** their text is the AI summarizer's.
  - New tests for each case; each one was verified to fail without its fix.

### A-35 ✅ RESOLVED (0.1.53) — ai.generic asked for the AI key by name

- **What happened:** since July (`ai-engine-selection.md` D2/D3), an `AI.*` node had to name its
  key (`behavior.vault_key`). Without that key, its config was rejected.
- **The operator's decision (2026-10-02, D4):** *"La clave de IA la debería tomar del vault con la
  clave general (sin asignación de nombre)."* ("It should take the AI key from the vault, with the
  general key (no name assignment).")
- **Fix:**
  - The provider is the hive's (`hive.yaml` `ai`), and the key is that provider's general key in
    SY.vault: the one of the node's tenant and, if there is none, the root's. It is the same thing
    the SY.* nodes and the frontdesk do.
  - `behavior.vault_key` is rejected with a message that names it.

### A-36 ✅ RESOLVED (0.1.53) — The base install brought up a test instance, and Archi knew of runtimes that do not exist

- **What happened:**
  - `AI.chat@motherbee`, the example node from March, started with every install as if it were
    base.
  - The cookbook seeds told Archi that `config_only` packages should use
    `runtime_base: "AI.common"`, which does not exist.
  - The handbook that Archi loads said that `ai.chat` is a runtime.
  - An architect example launched a node with no runtime, which would have been derived to
    `ai.chat`.
  - An architect help text had double-encoded UTF-8 ("catÃ¡logo").
- **Fix:**
  - `ai.generic` stays baked in, with no startup instance. Instances are created for something
    concrete; test ones are ephemeral (operator, 2026-10-02).
  - Everything points to `ai.generic`: admin, architect, handbook, seeds, docs and scripts.
  - `install-ia.sh` and `ai-nodectl.sh` (March's per-node systemd unit model) were deleted, along
    with the deprecated flags of the publishing scripts.
- **PROD:** the `AI.chat@motherbee` instance that already exists stays in FAILED_CONFIG, because its
  config has `vault_key`. It gets deleted or reconfigured when the operator asks.

### A-37 ✅ RESOLVED (0.1.54 + 0.1.55) — SY.frontdesk.gov: the tenant path and the merge by email

**The operator's decisions (2026-10-02):**

- On tenants: *"no da para crear un tenant por un mensaje que llegue de un humano que no esté
  registrado; ahora el tenant se crea desde el cloud y tiene un proceso"* ("it makes no sense to
  create a tenant because of a message from a human who is not registered; now the tenant is
  created from the cloud and it has a process").
- On the merge: *"haría merge sobre datos que no están… si no coinciden los datos y mandan un update
  queda lo último"* ("I would merge over data that is missing… if the data doesn't match and they
  send an update, the latest stays").

**The case's tenant:**

- It is the temporary ILK's, which SY.identity registered when it provisioned it under the IO
  node's tenant. The frontdesk reads it from identity's SHM.
- A handoff's `tenant_id` only has to match; if it does not, `INVALID_REQUEST`.
- With no readable tenant it answers `TENANT_UNRESOLVED` and does not register.
- The frontdesk no longer creates tenants: SY.identity took away its permission for `TNT_CREATE`,
  `TNT_UPDATE` and `TNT_SET_SPONSOR`.
- Neither the person nor the LLM chooses the tenant.

**The tenant's state:** `ILK_REGISTER` on a suspended tenant is now rejected (`TENANT_SUSPENDED`),
just like on a pending one.

**Merge by email**, inside SY.identity's `ILK_REGISTER`:

- **When:** the email already belongs to another active human of the same tenant, and the one
  registering is a temporary of that tenant.
- **What it does:** the channels are moved (before, they were copied: a bug) and the temporary
  stays as an alias.
- **The data:** only empty fields are filled in; a value is never overwritten.
- **The response:** `merged:true`; the frontdesk answers `MERGED`.
- **The latest wins:** an `ILK_REGISTER` from the ILK itself, for example a repeated
  `register_human`, replaces the identification.
- **io.cloud:** `register_human` reports the final ILK, `merged` and the source temporary.

**Also:**

- Email lookups are per tenant, like the database index.
- A failed handoff keeps the case's tenant (G7).
- The tests of logs without personal data were flaky; they became stable.

**Closed in 0.1.55:**

- **Root tenant:** nobody registers there. The operator's decision (2026-10-02): *"nadie se puede
  registrar en tenant raíz"* ("nobody can register in the root tenant").
  - SY.identity rejects a person's `ILK_REGISTER` in the root tenant with
    `TENANT_ROOT_NOT_REGISTRABLE`, no matter who it comes from. Node ILKs (`agent`) are still
    registered there.
  - The frontdesk cuts it off earlier, with no LLM and no SY.identity, and answers
    `TENANT_NOT_REGISTRABLE`. The person stays temporary.
  - IO nodes launched from the root tenant remain as A-43.
- **The frontdesk gate of io.slack and io.wapp** (`io-common`) built the handoff as a `data`
  message, but the frontdesk only takes handoffs from `user` messages. The handoff never reached
  its deterministic path. Now it goes as `user`, like io.cloud's.
- **The identity lab e2e tests:** four diags created their tenants with the frontdesk's name and
  got `UNAUTHORIZED_REGISTRAR`. They were `identity_merge`, `identity_negative`,
  `identity_provision_complete` and `identity_replica_sync`.
  - Now the script creates the tenants through SY.admin, passes them to the diag and purges them at
    the end. The shared parts went into `scripts/lib/identity_e2e.sh`.
  - The replica one also checks through SY.admin that the tenant and the ILK reach the replica.
  - The `gov_frontdesk_identity_e2e.sh` wrapper no longer brings old hives (`sandbox`,
    `worker-220`).
- **The diags impersonate the frontdesk:** they connect with its name and with the UUID the
  frontdesk persisted.
  - While they run, the router delivers the frontdesk's traffic to them.
  - When they leave, the real frontdesk stays connected but with no route until it reconnects
    (A-44).
  - The scripts now stop `sy-frontdesk-gov` while the diag runs, and start it again afterwards.

**Still open:**

- 🔴 **Security:** there is no proof that the email belongs to whoever writes it (A-42, deferred).
- **G6 and G8:** the §12 counters and the conversational retries.

### A-38 ✅ RESOLVED (0.1.54) — Cognition: design questions

The operator's answer (2026-10-02): *"sí a todo"* ("yes to everything").

- **Thresholds:** `context_open`/`reason_open` are renamed after what they do,
  `config.thresholds.context_close` / `reason_close`, with a default of 0.25 (the same effect as
  before). CONFIG_SET rejects the old keys by name, and any other unknown key. The persisted PROD
  config with the old ones starts with the defaults, which give the same behavior, and leaves a
  warning.
- **Full region:** the recent wins. The current turn's thread is never dropped, and after it comes
  the most recently seen.
- **Scope cut:** it stays at the 6th sustained divergent message (5th if the scope has one turn,
  7th if it is heavily reinforced), written in the spec §8.3 and pinned by a test.
- **Still not assembled** (a separate chapter): cross-thread memory, LanceDB, getting what is on
  the workers to reach the motherbee, and readers of the tables.

### A-39 ✅ RESOLVED (0.1.54, to be validated in the deploy) — On every release, the motherbee's managed nodes went down for 20 to 80 s (`203/EXEC`)

- **What happens (seen while validating 0.1.53, and pre-existing: AI.chat racked up 104 failures
  in 30 days):**
  1. When unpacking the new version, dpkg deletes the old package's files, and the
     `dist/runtimes/<rt>/<old version>` directories came in that package.
  2. During the update, the managed nodes' units restart with the old `ExecStart`, which points to
     a directory that no longer exists: a `status=203/EXEC` loop.
  3. Only when the new orchestrator starts does it see that `current` moved, and it repoints them.
- **Evidence 0.1.53:**
  - AI.chat, IO.api, IO.blob, IO.cloud and IO.slack.default: 5 failures between 20:32:10 and
    20:32:31. Repointed at 20:32:34 ("runtime 'current' pointer moved; rebinding node").
  - IO.wapp.default: 17 failures, until 20:33:33; the reconcile had skipped it as "visible".
- **Impact:** IO.api and IO.cloud, the public-facing ones, are left without service on every
  upgrade.
- **Who restarted them:** `needrestart`, Ubuntu's apt hook. After dpkg, it restarts the services
  that run deleted binaries, and their old `ExecStart` points to the directory that no longer
  exists.
- **The other side of the bug:** the startup reconcile skipped the nodes that were running. One
  that nobody restarted stayed on the old binary indefinitely; it repaired them only because
  `needrestart` had knocked them down.
- **Fix (`779df4c`):**
  - The startup reconcile, which runs after every upgrade and every core-update, relaunches on the
    new version a node that follows `current` when the pointer moved: a controlled restart. The
    60 s loop does not do it, so a hot publish behaves as before.
  - The package installs `/etc/needrestart/conf.d/fluxbee.conf`, which leaves the
    `fluxbee-node-*` units to the orchestrator (verified against `needrestart`'s code).


### A-40 ✅ RESOLVED (0.1.55) — IO.slack.default did not have its ILK in identity (PROD)

- **What happened:** its config pointed to `ilk:9cd1bb44…`, but SY.identity answered `NOT_FOUND`,
  so every `CONFIG_SET` failed when registering its own channel, and the node stayed in
  FAILED_CONFIG. It was probably deleted by the first run of the factory reset on 28/09.
- **The operator's decision (2026-10-02):** *"io.slack es parte del sistema pero funciona
  instanciándose, no corre solo… corre con su propio tenant que lo lanza"* ("io.slack is part of
  the system, but it works by being instantiated, it doesn't run on its own… it runs with its own
  tenant, which launches it").
- **How it was closed:**
  - `io.slack` stays baked in, with no startup instance in `base-nodes.json`;
  - each binding is launched by its tenant;
  - the PROD instance, which ran in the root tenant, was deleted with a purge. Its config was saved
    outside the repo, to recreate the binding from a tenant.

### A-41 ✅ RESOLVED (0.1.54) — Purging a node left its UUID persisted

- **What happened:** `purge_instance` deleted the directory, the ILK and the mapping, but not
  `state/nodes/<name>.uuid`. In PROD there were 12, from nodes already deleted: AI.chat, the
  factory reset test ones and old Slack bindings. A new node with the same name inherited the old
  UUID.
- **Fix (`7c9f781`):** both purge paths delete it and report it (`uuid_file_removed`). The 12 in
  PROD were deleted.


### A-42 ✅ VALIDATED ON 8.x (0.1.83, 2026-10-22) — The merge by email does not verify that the email belongs to whoever writes it

- **Resolved (0.1.83, design §7, rule (b)):** only an email the channel vouches for (the IO
  node's handoff, Cloud, io.api's integrator) goes in `identification.email` and can merge; a
  typed one is `claimed_email`, never indexed or merged; the frontdesk takes handoffs only from
  IO nodes with the `frontdesk_handoff` mark. Live: Cloud's `register_human` registered a person
  with the vouched email. An explicit merge by the operator is not offered yet.

- **What happens (since 0.1.54, A-37):** if someone writes the email of another person in the same
  tenant, SY.identity does the merge, and their channel becomes associated with that person's ILK.
  They are routed as if they were that person, and they can fill in the fields that are empty.
- **The operator's decision (2026-10-02):** *"levántalo y dejamos para después"* ("raise it and
  we'll leave it for later"). It goes with the security package (A-22).
- **Planned fix:** a proof of ownership before the merge, for example a one-time code sent to the
  email.

### A-43 ✅ RESOLVED (0.1.56) — IO nodes launched from the root tenant

- **What happened:** the base install brought up channel IO nodes in the root tenant
  (`IO.api@motherbee`, `IO.wapp.default@motherbee`). A person who arrived through one of them
  stayed temporary, because since 0.1.55 nobody registers in the root tenant.
- **The operator's decision (2026-10-02):** *"algunos nodos IO tienen que correr con el tenant raíz,
  ej io.cloud io.blob los demás no"* ("some IO nodes have to run with the root tenant, e.g.
  io.cloud io.blob, the rest don't"); *"todos quedan para poder usarlos pero no corriendo default.
  Solo io.cloud y io.blob por ahora"* ("they all stay so they can be used, but not running by
  default. Only io.cloud and io.blob for now").
- **Fix (`4a9249f`):**
  - The SDK defines `ROOT_TENANT_IO_RUNTIMES` (io.cloud, io.blob).
  - The orchestrator rejects any other IO node in the root tenant with `TENANT_ROOT_NOT_ALLOWED`
    in the three places where it launches a node:
    - `run_node`, which covers admin, Cloud, Archi, firstboot and the forwards;
    - start/restart;
    - the relaunch at startup. If a node is already running, it leaves it running.
  - io.api and io.wapp stay baked in, with no base instance; firstboot only brings up IO.cloud and
    IO.blob.
  - Archi never picks the root tenant for an IO node.
  - Docs: `715143a`.
- **PROD:** the operator purged `IO.api@motherbee` and `IO.wapp.default@motherbee` before the
  0.1.56 deploy; from the session, the permissions classifier blocked it. In 30 days they had not
  been used, and their configs were saved outside the repo.
- **Validated live (0.1.56):** a `run_node` of io.api in the root tenant gives
  `TENANT_ROOT_NOT_ALLOWED` and leaves nothing created.
- **io.web (operator, 2026-10-03):** *"io.web también va en el core, la idea es que los IO que se
  requieren como recursos compartidos y no porque una solución lo requiera, corren en el core"*
  ("io.web also goes in the core; the idea is that the IO nodes that are required as shared
  resources, and not because a solution requires them, run in the core").
  - It went into `ROOT_TENANT_IO_RUNTIMES`, even though it does not exist yet: there is only a
    spec.
  - Its spec declares it in `system_nodes`; when it is built, it goes like IO.blob and IO.cloud
    (managed runtime, `boot=true`).
- **Noelia's LinkedHelper runbook** launches the node in the root tenant. The operator is looking
  into it.

### A-44 ✅ RESOLVED (0.1.55 + 0.1.56) — Two connections with the same UUID: the second one takes the route

Seen while fixing the A-37 e2e tests, reading `src/router/mod.rs` and `src/shm/mod.rs`.

- **What happened in the router:**
  - A HELLO with a UUID that already had a live connection replaced its entry in the node
    table.
  - When the new one closed, the old one stayed connected but with no route.
- **The operator's decision (2026-10-02):** *"rechazar la nueva, no veo porque deba aparecer una en el
  esquema normal"* ("reject the new one, I don't see why one should show up in the normal
  scheme").
- **What checking it first showed:**
  - **A pre-existing bug:** if a connection ended with an error (a read error, an invalid
    OPA_RELOAD, a forward to peers, `handle_message`), the router did not clean it up. The entry
    stayed in the table and the socket open: a live node stayed connected with nobody reading its
    messages. With "rechazar la nueva" ("reject the new one") that turned into a permanent block.
  - **Transient duplicates in the normal scheme:**
    - every Go node sends two HELLOs at startup;
    - a node can reconnect before the router processes the close of the previous connection.
- **Fix (`068014d`, 0.1.56):**
  - Every connection ends by cleaning up its entry; a message that cannot be processed is logged
    and skipped.
  - A HELLO with a UUID in use waits up to 1 s for the previous connection to go away; if it is
    still alive, it is rejected with no ANNOUNCE and with a WARN. The check and the insert run
    under the same lock.
  - The SDK logs the rejected handshake.
  - The negative identity diag uses one connection per name.
  - The identity e2e tests run with `timeout`: their retries waited for an error that no binary
    prints.
  - Three of the four new tests fail with the previous code.
- **The router's SHM (0.1.55):**
  - `register_node` duplicated a node that registered again.
  - The reader lost the last node when there was a gap.
- **Noted for later:**
  - the Go SDK's double HELLO: the wait absorbs it, but the SDK could reuse the first
    connection;
  - the Rust SDK has no handshake timeout: a router that accepts and does not answer leaves it
    hanging.

### A-45 ✅ RESOLVED AND MEASURED (0.1.57) — Every motherbee upgrade shuts down the whole core before unpacking

- **What happens (measured in the 0.1.55 deploy):**
  - The package's `prerm` (`packaging/deb-prerm`) stops every core service before unpacking, also
    on an upgrade.
  - The `postinst` starts only the orchestrator, which then brings up the rest.
- **Measured outage:** rt-gateway 70 s, sy-admin 76 s, sy-vault 78 s, sy-storage 84 s, sy-identity
  86 s.
- **Impact:** meanwhile the motherbee has no router, identity or admin.
  - IO.api and IO.cloud do not serve: the public sees 502.
  - The identity replicas retry every second (~2 warnings per second).
  - Nobody can be provisioned.
- **Why it is like this:** the `prerm` says "stop + disable services before removal", but it runs
  on an upgrade anyway.
- **Proposal:** have the `prerm` stop the services only on uninstall, and on an upgrade have the
  `postinst` restart them after unpacking. Linux keeps the old binary running until the restart,
  so the outage would shrink to what each restart takes.
- **To decide:** the order of the restarts, and whether the `postinst` or the orchestrator does
  them.
- **Operator (2026-10-02):** *"no me parece mal"* ("that seems fine to me").
- **Fix (`1a10cab`):**
  - On an upgrade the `prerm` stops only the ones that write the runtimes manifest:
    sy-orchestrator, sy-admin and sy-wf-rules. The `postinst` counts on that when it registers the
    runtimes.
  - The rest keep serving with the old binaries.
  - The new orchestrator's boot restarts each service that runs a replaced binary, in its startup
    order: rt-gateway first and SY.vault last. That way, ordering the restarts is handled by the
    orchestrator, not by a script.
  - The result is recorded in `/versions` as `core.last_update`, with `via: package`.
  - needrestart also leaves rt-gateway and sy-* to the orchestrator.
  - dpkg runs the old package's `prerm`: the upgrade to 0.1.56 still stops everything, and the
    improvement shows from 0.1.57 on.
- **Measured on the upgrade to 0.1.57 (2026-10-06):**

  | Service | 0.1.55 | 0.1.57 |
  |---|---|---|
  | rt-gateway | 70 s | 0 s (restart) |
  | sy-vault | 78 s | 1 s |
  | sy-storage | 84 s | 5 s |
  | sy-identity | 86 s | 5 s |
  | rest | — | 0 s |

  Only the ones that write the manifest stop: the orchestrator was down for 44 s.

### A-46 ✅ RESOLVED (0.1.57: the ingress no longer reaches Archi) · stage 4 minimal LIVE (0.1.83: SSH on the DMZ hosts) · the full stage-4 firewall and Archi's API auth PARKED — Archi's HTTP API has no authentication and is reachable from the ingress

- **Status (2026-10-19 review):** stage 1 (loopback binds) shipped in 0.1.57 and stages 2-3 closed with A-47; the stage-4 firewall and authentication for Archi's API are parked in the posture table (2026-10-07).

- **What happens (verified on 2026-10-03, in the node survey):**
  - SY.architect's axum router (`sy_architect.rs`, `Router::new()`) has no authentication layer at
    all. Open: `/api/chat`, `/api/executor/plan`, `/api/package/publish`,
    `/api/software/upload|publish`, `/api/agent-assets`, `/api/messages`, `/api/sessions`.
  - In PROD it listens on `0.0.0.0:3000`. The code's default is `127.0.0.1:3000`, but the packaged
    `hive.yaml.example` uses `0.0.0.0`.
  - The motherbee's firewall is inactive (`ufw status`: inactive).
  - From `fb-ingress`, which has a public interface (`eth1`), port `10.10.10.10:3000` is open. The
    admin's 8080 is not.
- **Impact:** whoever compromises the ingress can ask Archi's executor for plans and publish
  packages or software. Archi's identity can also write to the vault. It is a path from the DMZ to
  the control plane.
- **Options:**
  - Immediate: have Archi listen on `127.0.0.1` (and be used through a tunnel), or a firewall that
    lets 3000 through only from the operator's network.
  - Long-term: authentication on the API, with the security package (A-22).
- **Extension (2026-10-05): why 3000 was reachable.** It is not just Archi:
  - **No hive filters inbound traffic.** `ufw` is inactive on all 4. The input of the egress's nft
    table is at `policy accept` ("future hardening").
  - **The orchestrator's rules are inert.** `ensure_core_firewall_local` and the Syncthing one run
    `ufw allow` (9000, 9100, 22000, 21027, from any source) on a ufw that is off: they stay stored,
    with no effect.
    - It is a trap: if someone turns ufw on, SSH on every host and the ingress's 443 get blocked.
    - The core rules are never closed.
  - **They listen on external interfaces:** the ingress's SSH and Syncthing, also on `eth1`
    (public), and the egress's on the office network.
  - **The admin also accepts any address** (`admin.listen` / `JSR_ADMIN_LISTEN`), and it has no
    HTTP auth either.
  - **Fixing the example is not enough.** The postinst only copies it if `hive.yaml` does not
    exist, so it does not fix PROD or DEV.
- **Decided with the operator (2026-10-05):**
  - the host posture (nft firewall per role, binds, Syncthing) is declared, applied and verified by
    the orchestrator;
  - Archi and the admin, on loopback only;
  - the only public thing is the ingress's 443;
  - from outside, Archi is used via Cloud → IO.cloud → IO.web, with a URL that Cloud hands out.
- **Design and staged plan:** `docs/host-posture-and-exposure-spec-v1.md`. Stage 1 = binds.
- **DTAP panel (2026-10-05):**
  - It failed revision 1 on all 4 lenses: 75 findings, none refuted (record in
    `docs/audits/2026-10-05-host-posture-dtap-panel.md`).
  - Revision 2 adds everything technical.
  - Operator decisions remain: O1–O4 before stages 1–5, O5–O8 before 6–7.
  - O1–O4 answered the same day (D16–D19 of revision 3):
    - SSH with nothing configured: open to everyone except the hives;
    - Archi on loopback only through config;
    - io.linkedhelper through the edge, at the end;
    - for the ingress, only what `add_hive` and the orchestrator handle.
  - Work order (D20): core → connection with Cloud → implementation nodes.
  - Panel round 2 (on Part A, revision 3): failed again, 84 findings. The cause was the per-hive
    IP filtering and everything around it.
  - Revision 4 simplifies: rules per port and interface according to the role, an automatic mode
    (observe for 24 h and then block), no configuration and no probes.
- **What stays open even if stages 1–5 are closed:**
  - the control mesh: any orchestrator, the ingress's included, can send SPAWN, KILL, etc.
    (A-20, A-22);
  - stage 7 reopens a path to Archi with a session: a compromised ingress can issue and use the
    session (O5).
- **Stage 1 (0.1.57, 2026-10-06), validated in PROD:**
  - Archi and the admin listen only on `127.0.0.1`. The motherbee's `hive.yaml` was migrated by
    itself during the upgrade (`fluxbee-migrate-config`).
  - From worker1, ingress, egress and fb-build, the motherbee's 3000 went from `open` to `refused`.
  - The orchestrator warns if any of them listens outside again. A positive control proved it: an
    alert at 33 s.
- **Status:**
  - Panel round 3 done.
  - The firewall (stages 4–5) and Syncthing (stages 2–3) are still in design: revision 6 and
    round 4 before building them (§8 of the doc).
  - Access from Cloud (stage 7) belongs to the Cloud phase.

### A-47 ✅ RESOLVED (0.1.59, validated on 8.x) — Syncthing used the public infrastructure, and worker1 depended on discovery

- **What happens (verified on 2026-10-05 on the 4 PROD hosts):**
  - `globalAnnounce`, `localAnnounce`, `relays`, `nat` and `crashReporting` are on, and the
    orchestrator never configures them.
  - Each hive announces its device ID and its IPs to the public discovery servers, can go through
    public relays, tries UPnP/NAT-PMP and sends crash reports.
  - Local discovery goes out on every interface: on the ingress through the public one, on the
    egress through the office network.
  - It listens on every interface.
- **The trap:** motherbee ↔ worker1 are set as `dynamic` on both sides, so today they sync only
  because discovery finds them. Ingress and egress have the motherbee's address fixed.
  - Cause: `add_hive` takes `syncthing_peer_address` as optional, and with no address it writes
    `dynamic` (`sy_orchestrator.rs:7340`).
  - Turning discovery off without fixing this cuts dist, blob and policy to worker1.
- **Decided:**
  - no public infrastructure at all;
  - listen only on the internal IP;
  - the side that dials always uses a fixed address (the spokes dial the motherbee, like WAN and
    identity);
  - `add_hive` does not create peers without an address;
  - the change goes in two steps: first the addresses, checking the connections, and only after
    that turn discovery off.
- **Design:** `docs/host-posture-and-exposure-spec-v1.md` §1.4 and §3.5 (stages 2 and 3).
  - The order is enforced in the code, so skipping versions cuts no one off (panel T-1).
  - The addresses are reconciled on every startup, not only at the join.
  - The orchestrator is the sole owner of the options.
- **Also found:** `vendor/syncthing/config.xml` already ships with the public parts off, but only
  the dev install path uses it. The `.deb` and the spokes run Syncthing's defaults.
- **Status:**
  - **Stage 2 (addresses and folders) validated in 0.1.58** (2026-10-06), after four adversarial
    code reviews (`docs/host-posture-and-exposure-spec-v1.md` §3.5, "as built"). The spokes end up
    with the motherbee at a fixed address, and the motherbee only accepts; it also removes each
    spoke from the folders its role must not have (A-50) and deletes the devices that no hive
    claims.
  - Before the deploy, `worker2` (VM 104) joined with 0.1.57 and was left powered off, for stage
    3's version-skipping test (T5-1).
  - **Stage 3 (Syncthing options: no discovery, relays, NAT, STUN or reports; listen only where
    needed) validated in 0.1.59** (2026-10-06), after two adversarial reviews. Each spoke switched
    on its own; the motherbee waited for `worker2`, which jumped from 0.1.57 to 0.1.59 and
    switched, and only then did the motherbee switch. Zero Syncthing UDP sockets on the 5 hives.

### A-48 ✅ VALIDATED ON 8.x (0.1.81, 2026-10-20) · leaf pinning PARKED — `remove_hive` revokes nothing: the removed hive is still a valid peer

- **Fixed (0.1.81, `4e9f91e`):** the registry (`/var/lib/fluxbee/hives/<id>/`) is the one list. The
  motherbee's router rejects a hive it does not list (`HIVE_NOT_REGISTERED`) and closes a removed
  hive's session within ~10 s; a rejected uplink backs off to 60 s. `remove_hive` deletes the
  hive's identity key (and restarts sy-identity when the box was not reached); a motherbee boot
  sweep deletes keys with no registry entry and refuses on an empty registry; the spoke's cleanup
  runs in its own transient unit, credentials first, and only from SY.orchestrator@motherbee.
  `wan.authorized_hives` is gone. Validated: the sweep deleted PROD's stale `worker2.key`; VM 104
  (worker2, which kept its certificate and key) was rejected on :9000 and :9100.
- **Parked:** leaf pinning (host-posture spec §3.8, superseded). A removed box that kept its leaf
  is admitted again if a hive with the same id is added later.

- **What happens (DTAP panel 2026-10-05, D-1, verified):**
  - `remove_hive_flow` (`sy_orchestrator.rs:10635`) deletes `hives/<id>`.
  - It unlinks the Syncthing device only if `info.yaml` has `syncthing_device_id`, and the egress's
    never has it (`:20989-20996`).
  - It does not delete the identity HMAC key, which stays in `/var/lib/fluxbee/identity/keys`
    (`src/mesh_hmac.rs:33`).
  - It does not revoke the mesh certificate: there is no CRL, and with `wan.authorized_hives` empty
    the router accepts any hive with a valid certificate from the CA (`src/router/mod.rs:2853`).
- **Impact:** a removed hive (or its disk) can reconnect to the mesh and to identity. Today the
  only brake would be the firewall, which does not exist.
- **Proposed fix:** have `remove_hive`:
  - revoke (or keep `authorized_hives` with the live hives);
  - delete the HMAC key;
  - unlink Syncthing in every role.

  It goes as a track separate from the posture design (§6 of the doc).
- **The same, on the motherbee's side (DTAP round 3, D3-3/P3-3):**
  - `REMOVE_HIVE_CLEANUP` and `ADD_HIVE_FINALIZE` sent to the motherbee were an off switch.
  - The policy lets any orchestrator send them, the ingress's included.
  - The first stops and disables the whole core; the second restarts the core and links to
    Syncthing a device chosen by whoever sends it.
- **Status:**
  - **0.1.57: the motherbee rejects both actions** (`FORBIDDEN`, with the origin in the log;
    covered by a unit test).
  - The rest of A-48 (certificate pinning, the router's view, HMAC keys, SSH host keys) is still
    in design: round 3 found that, as it was written, it cut off every existing hive (§8 of the
    doc).

### A-49 🟡 PARKED (waits for O9) — The apt repo is not signed (`[trusted=yes]`): whoever impersonates it is root on the motherbee

- **What happens (DTAP panel 2026-10-05, P-14, verified):**
  - The motherbee installs from `http://10.10.10.50:8900` with `[trusted=yes]`, because the repo
    is not signed (HANDBOOK:419, :434; `docs/packaging-and-build.md:157`).
  - The 10.10.10.0/24 network is flat: the ingress (DMZ) shares a segment with the motherbee and
    with fb-build.
- **Impact:** a compromised ingress that impersonates 10.10.10.50 serves its own `.deb`, and the
  next upgrade installs it as root on the motherbee.
- **Proposed fix:**
  - sign the `Release` (`InRelease`) and remove `[trusted=yes]` (the `.deb` does not change);
  - isolating the ingress at the network level waits until after the core: it needs infra config
    (D19).
- **Status:** recorded. Nothing was touched.

### A-50 ✅ RESOLVED (0.1.58, validated on 8.x) — The motherbee shared `blob/active` with the egress

- **What happens (verified on 2026-10-06 in PROD, read-only, with `lab/posture-check.py`):**
  - On the motherbee, the `fluxbee-blob` folder (`/var/lib/fluxbee/blob/active`, sendreceive: the
    live blobs of every tenant) is shared with worker1 **and with egress1**.
  - The egress never accepted it: its `hive.yaml` says `blob: enabled: false`, and it has it as a
    pending folder offered by the motherbee. No data traveled.
  - But the authorization exists on the motherbee's side: whoever has root on the egress (a host
    with an address on the office network) can accept the folder through its Syncthing's API and
    receive every active blob.
  - Side effect: the motherbee sees `fluxbee-blob` at 0% for egress1 forever.
- **Cause:** `d73687f` (2026-07-30, U-2) put the egress in the Syncthing mesh so it would receive
  the core, and the blob gate stayed at `public_only = spoke_role == HiveRole::Ingress`. From then
  on the motherbee shares `blob/active` with every spoke that is not an ingress, and the egress
  neither runs nodes (it does not receive `runtimes`) nor declares blob.
- **Fix (stage 2):**
  - The join shares `blob/active` only with workers.
  - The motherbee's reconciliation removes each spoke from every folder its role must never have
    (`blob/active` and `runtimes` outside the workers, `blob/public` outside the ingress, another
    role's core). The rule is the role alone, never the `hive.yaml` flags: the second adversarial
    review found that deriving it from the flags left every spoke without folders forever if
    someone turned a sync off for a while.
  - A test pins that the join never adds one of those folders, with any combination of flags.
- **A second way in, found in the third review:** creating a missing folder cloned another one
  with its members (creating `blob/active` after the spokes had joined shared it with every peer
  of the first folder), and the "isolated" folder was seeded with the first `<device id=>` of
  `config.xml`, which can be a peer. Now every new folder is born with no members (Syncthing adds
  the local device when it loads the config).
- **Known limit:** a folder created after the spokes joined (turning on the blob sync or the
  public one later) stays empty until `add_hive` is run again for them. The proposed fix is a
  backfill per role, like the policy one from vendor. Today it does not apply in PROD: blob has
  been on since the install.
- **Status:** validated in 0.1.58 (2026-10-06): the first round removed egress1 from
  `fluxbee-blob`, and the pending offer disappeared from the egress.

### A-51 🟡 FIXED ON THE MOTHERBEE (0.1.60) · the spokes PARKED with O9 (a signed manifest) — root wrote into the `fluxbee` user's directories following names that user controls

- **What happens (adversarial review of stage 2, 2026-10-06):**
  - `ensure_owned_tree` leaves all of `/var/lib/fluxbee/dist` owned by the Syncthing user
    (`fluxbee`), and `sync_data_dir` too.
  - `write_file_atomic` writes there (the core materialized per role, the manifest, the Syncthing
    binary in vendor) with a fixed-name temporary (`.<name>.tmp`) and `fs::write`, which follows
    symlinks.
  - `copy_exec_file` does a `chmod` by path after the rename, and `ensure_owned_file` does a
    `chown fluxbee:…` by path, which also follows symlinks.
- **Impact:** whoever holds the `fluxbee` user (Syncthing listens on the network) can plant a
  symlink and make root write to a root file, or change a file's owner (`chown` of
  `/etc/sudoers` or similar → root). It requires compromising the `fluxbee` user first; it is
  defense in depth. It has existed since before the posture work.
- **Proposed fix:** the same primitive the `config.xml` writer already uses (an `O_EXCL` temporary
  with a random name, owner and mode set by descriptor, rename, fsync of the directory) for
  `write_file_atomic`, `copy_exec_file` and `ensure_owned_file`. It is done in its own step, with
  its own review, because it touches the core update path.
- **Fix (0.1.60, four adversarial reviews):**
  - **Writing:** an `O_EXCL` temporary with a per-run token and the `.syncthing.` prefix (Syncthing
    never sends it), the mode set on the descriptor, fsync of the directory with `O_NOFOLLOW`.
    Temporaries from another run are swept right away; this run's, never.
  - **Managed directories:** they are opened `O_DIRECTORY|O_NOFOLLOW`, and the chmod/chown is done
    on the descriptor. Whatever is not a directory (a symlink or a planted file) is left as it is
    and reported as a posture alert, without bringing down the startup.
  - **`.stfolder`:** it is created exclusively; a symlink in its place is replaced; a folder that
    is a symlink gets no marker. The ingress join no longer chmods the blob directories.
  - The (dead) copy of the installed Syncthing into `dist/vendor` **was deleted**.
  - **The motherbee's `dist/` belongs to root** (`root:<Syncthing group>`, 0750, `g+rX,go-w`).
    Links inside it are deleted without following them, in the preinst and on every startup;
    Syncthing only reads it (on the motherbee it is send-only). The operator: *"Si root PERO!!!
    cuidado que hay muchas carpetas de syncthing, no todas pueden ser root"* ("Yes, root, BUT!!!
    careful, there are many syncthing folders, not all of them can be root"): only `dist/`;
    `blob/active` (send and receive), `blob/public`, Syncthing's home and the spokes' `dist/` stay
    `fluxbee`'s.
- **What stays open:**
  - **The spokes install and run whatever Syncthing delivers to them** (core, vendor, runtimes).
    Whoever holds the motherbee's Syncthing user (its `key.pem`) gets root on every spoke. The fix
    is a signed manifest, with a root key that the root of each spoke verifies.
  - **Root writes by path in the Syncthing user's `blob/`** on the motherbee: A-56.
  - **A re-join** installs with `install -m` inside the spoke's `dist/` by name (a race on a used
    box); the same class as the first point.
- **Status:** in 0.1.60.

### A-52 ✅ RESOLVED (0.1.60, validated on 8.x) — `add_hive` on a freshly created clone collides with cloud-init's `dist-upgrade`

- **What happened (2026-10-06, join of `worker2` on 8.x):** the first join failed with
  `CONFIG_FAILED` ("identity HMAC key distribution failed … port 22: Connection refused").
  - cloud-init, on the clone's first boot, runs an `apt-get dist-upgrade`. When it upgrades
    `openssh-server`, it stops `ssh.service` and closes `ssh.socket`, in the middle of the join.
  - After cloud-init finished and the VM rebooted (new kernel), the resumed join went fine.
  - HANDBOOK §3.4 already documents it ("A freshly booted clone is NOT ready": wait for the cycle
    and reboot before joining it). This time it was not followed; the error was an operational
    one, not the HANDBOOK's.
- **Also seen:** the throwaway VM (10.10.10.60) has internet access (it downloaded packages from
  `archive.ubuntu.com`). The 2026-07-29 logbook says the 8.x machines should not have it; it is the
  operator's network configuration.
- **Proposed fix:** have the join's bootstrap protect itself and wait for cloud-init
  (`cloud-init status --wait`, bounded) and for the dpkg lock before starting: the orchestrator
  owns the host's environment. Meanwhile, the HANDBOOK §3.4 recipe holds.
- **Fix (0.1.60):** as soon as it verifies the seeded key, and before installing the sudoers entry,
  the join waits while systemd has not finished booting (`is-system-running` in `starting`,
  `initializing` or `stopping`; it covers the gap between two cloud-init stages) or while a
  cloud-init stage or a daily apt job (unattended-upgrades) is `activating`. It does not use
  `cloud-init status`, which says "error" while stages are still running and "not started" forever
  if cloud-init does not run. Up to 15 min in total, retrying if the box reboots; the join shows
  the `waiting_for_host` phase. If it is still busy: `HOST_NOT_SETTLED` (retryable, the key stays,
  503); if the check itself fails: `HOST_SETTLE_CHECK_FAILED` (502).
- **Validated (2026-10-06):** a new clone of VM 104, joined as soon as it booted, waited 8 min in
  `waiting_for_host` while cloud-final ran, and then completed the join (ledger 0.1.61).

### A-53 ✅ RESOLVED (0.1.58; exercised by the joins of 0.1.61 and 0.1.62) — `local_syncthing_device_id` always failed with Syncthing v2 and fell back to the first device in `config.xml`

- **Status (2026-10-19 review):** the code no longer reads `config.xml` for the local device id; the worker2 joins of 0.1.61 and 0.1.62 linked the peer. PROD's motherbee id sorts first, so a join cannot tell the old fallback from the fix: the code is the proof.

- **What happens (verified on 2026-10-06 on the PROD motherbee, read-only):**
  - The orchestrator asked for the ID with `syncthing --home … --device-id`; the v2.0.14 binary
    answers `unknown flag --device-id` (in v2 it is the `device-id` subcommand).
  - So it always fell back to "the first `<device id=>` of `config.xml`". The journal since
    September shows the fallback 2 times.
  - That first device can be a peer: Syncthing sorts the folder members by ID. A join would have
    handed a spoke the wrong ID as "motherbee".
- **In PROD there was no damage:** the motherbee's ID (`BBJBKBB…`) is the lowest, so the fallback
  happened to give the local one; the 4 spokes have the motherbee's device right.
- **Fix (stage 2):** the `device-id` subcommand, only if `cert.pem` exists (without a certificate
  the binary would generate one as root); otherwise, the running Syncthing's `myID`. Never
  `config.xml`.
- **Status:** built; the next join exercises it (the A-48 one).

### A-116 🔴 OPEN (found 2026-10-27) — A WF in front of a conversation loses the person and the conversation for the agent

- **What happens** (go/nodes/wf/wf-generic/node/):
  - **What an instance sees** of a message (`messageToMap`, instance.go): `trace_id`, `msg`,
    `type`, `thread_id`, `src`, `context` and `payload`. It does not see `src_ilk`, `dst_ilk` or
    `ich`.
  - **Its `send_message`** (actions.go) builds a new message from the definition, with `type`,
    `msg`, `thread_id` and `context`:
    - it sets no `src_ilk`, so the agent keeps one memory for everyone the WF brings (its memory is
      keyed by `src_ilk`);
    - its `thread_id` is the instance's, so every message is a new conversation for the agent (its
      cognition is keyed by thread and agent).
  - **`thread_id_ref`** sends on the person's thread, but only as a terminal reply: an answer comes
    back correlated by `thread_id == instance_id` (correlate.go). So the agent's answer to that
    message finds no instance, and the WF tries to start a new one.
  - **Instances:** each message from a person starts a new instance. An instance is found only by
    its own id, never by the conversation's thread.
- **Why it matters:** the operator's direction (agent-tools-v1 §6.2, §6.3) puts a WF between an IO
  node and an agent whenever a solution needs timers or routing. For that the WF must forward the
  message as received, and keep the person and the conversation.
- **Next:** the WF review of §6.3, case by case.

### A-115 🟡 OPEN (found 2026-10-27, iteration 2 design) — SY.timer: no retry, no caps, an open listing, and a drifted doc

- **What happens:**
  - **A fire is sent once.** If its owner is down, SY.timer ignores the router's `UNREACHABLE`
    and the fire is lost.
  - **A one-shot can fire twice** if SY.timer stops between sending it and saving it (read in the
    code).
  - **After a hive reboot, overdue timers fire before the agents are relaunched.**
  - **No limits:** no maximum delay, no cap per owner or in total, no payload size check (only
    the 128 KiB frame).
  - **Open listing:** `TIMER_LIST` and `TIMER_GET` answer any node of the hive, payload
    included, and SY.admin shows them.
  - **The doc drifted:** docs/sy-timer.md disagrees with the code in ten places. Among them: the
    Rust SDK's `TimerClient`, recurring catch-up, the history GC (at start only), the purge order
    and its failure, the size limits, and the envelope's keys.
- **In 0.1.89:** the doc is corrected.
  - The reminders that would have covered a lost fire in the agent moved to WFs, by the operator's
    decision ([agent-tools-v1](../../docs/agent-tools-v1.md) §6.2).
  - A WF reconciles its own timers at start (go/nodes/wf/wf-generic/node/recover.go). Whether that
    covers a fire lost while it was down is part of the WF review (§6.3).
- **Later:** delivery retries and caps in SY.timer.

### A-114 🟡 OPEN (found 2026-10-27, iteration 2 design) — An agent cannot write first to a person, and a WF does not answer an agent by default

- **What happens:**
  - **No IO node writes to a person first.** Each answers only inside the conversation it brought
    in, by its `reply_target`:
    - io.slack posts in its bound conversation;
    - io.wapp drops a message that has none.
  - **Nothing routes by person:** the router never maps an ILK or a channel to a node, and no IO
    node reads `dst_ilk`.
  - **io.api drops an unsolicited message,** so a reminder's or an answer's turn cannot reach its
    client.
  - **A WF:**
    - answers on its instance's thread, unless its definition sets `thread_id_ref` and
      `target_ref`;
    - starts only on a message its `input_schema` accepts;
    - sets no `src_ilk`.
- **Since the operator's decision (agent-tools-v1 §6.2, §6.3):** agents only answer, and WFs do
  the routing.
  - **Answering later a person who wrote before** is the WF's job: it keeps that message's
    `context.io` (its `reply_target`) and later sends to the IO node with it. The WF review
    confirms the engine allows it.
  - **Writing first to a person who never wrote** is what remains here.
    - The person is registered (the frontdesk or cloud identity), so the ILK is known.
    - The WF sends to the IO node by the name written in its definition.
    - The IO node must deliver to that ILK on its channel without an inbound message.

    That is the IO family's work.
- **Later:**
  - the IO track: addressing a person, and delivering without a pending request;
  - Archi's WF design.

### A-113 ✅ DECIDED by the operator (2026-10-27) — OPA never sees a message that has a destination

- **What happens:**
  - **Only for a message without a destination:** the router runs the user policy only on
    `Resolve` (src/router/mod.rs). Its output is a target, not an allow or a deny.
  - **A unicast**, by UUID or by name, passes only:
    - the VPN check;
    - the system-authority gate on protected verbs.
  - **PROD has no user policy loaded:** `GET /opa/policy` is empty.
  - **The policy's input** has no destination and no tenant. Its `src_ilk` is whatever the sender
    wrote.
- **So:** the operator's rule for `send_message`, *"que OPA decida"*, has no place to run.
  - In 0.1.89 the tool applies its own rule: agents and WFs of its own tenant.
  - The router's VPN check still applies.
- **Decided by the operator:** OPA stays for rules of substance: security, general systems, and
  implementations that need clear rules. It does not gate agents' messages.
  - A rule for each pair of nodes that talk would end as *"una enredadera de reglas OPA imposible
    de debug"*.
  - Agents do not send at all: the AI node stays a pure responder, and routing belongs to WFs
    (agent-tools-v1 §6.2).

### A-112 🔴 OPEN, probable (found 2026-10-27, iteration 2 design) — The AI node answers router notices

- **What happens:**
  1. A router notice (`UNREACHABLE`, `TTL_EXCEEDED`) that no waiter takes reaches
     `GenericAiNode::on_message`.
  2. Its control plane answers any system message that is not a control verb, with a
     `CONFIG_RESPONSE` `unknown_system_msg` to the sender. Here the sender is the router's own
     UUID; a unit test answers a notice like that.
  3. The router cannot deliver to its own UUID, so it sends `UNREACHABLE` back, and the node
     answers it again.

  This was read in the code, not seen running. Today it needs a reply that cannot be delivered,
  because its sender is gone. With `send_message` and reminders it becomes common.
- **Fix (0.1.89):**
  - a test confirms the loop first;
  - the node never answers a router notice, and logs it;
  - `send_message` reads the notice as its failure.

### A-111 🔴 OPEN (found 2026-10-27, iteration 2 design) — A CSV or HTML attachment fails the whole message on Claude

- **What happens:**
  - ai.generic reads as text only the `text/plain`, `text/markdown` and `application/json`
    attachments (crates/fluxbee_ai_sdk/src/text_payload.rs).
  - Every other attachment reaches the model as a document part, CSV and HTML included.
  - The Anthropic provider accepts only PDF documents, so a message with a CSV or HTML attachment
    fails there. On OpenAI it goes as an `input_file`.
- **Fix (0.1.89, agent-tools-v1 §6.2.2):** CSV and HTML become text, and an attachment shows its
  blob name, for `read_blob`.

### A-110 🟡 OPEN (found 2026-10-27, 0.1.88 gate) — publish-ia-runtime.sh moves `current` before its last step

- **What happens:**
  - `scripts/publish-ia-runtime.sh` calls `publish-runtime.sh`, which writes the manifest, `current`
    included, and only then writes the version's `tools.json` from the repo's
    `docs/reference/agent-tools.json`.
  - Run outside a full checkout, the catalog step fails after `current` has already moved. The
    version is live without its catalog, so `get_runtime` answers `tool_catalog: null` to Archi.
  - Running nodes were safe: the reconcile checks the version's files.
- **Fix:** write every file of the version first and move the manifest last; fail before touching
  the manifest when an input is missing.

### A-109 ✅ VALIDATED ON 8.x (0.1.88, 2026-10-27) — A release that does not change the orchestrator leaves running nodes on the old runtime

- **What happens:**
  - A node that follows `current` moves to the new runtime only when its hive's orchestrator
    starts: "running node follows 'current' and the pointer moved; relaunching it".
  - A core update restarts only the services whose binary changed. In 0.1.87 the orchestrator's
    did not change, so worker1's orchestrator never restarted.
  - `AI.pruebas` kept the 0.1.86 binary, whose directory dpkg had removed (`/proc/<pid>/exe` read
    "(deleted)"). It went on answering with the old tools until `restart_node`.
- **Fix:** when `current` moves, the orchestrator relaunches the running nodes that follow it after
  every core update, and on its periodic reconcile, not only at its own start. With it, save
  `_system.runtime_version` (A-101).
- **Fixed (0.1.88, `d01ba1d`):** see the ledger.
  - One reconcile pass, at boot and every 60 s, relaunches a running node that follows a moved
    `current`, or whose unit does not run what its runtime names (live ExecStart plus a version
    stamp). Package nodes move with their base.
  - Only once the target is ready; otherwise it waits, logged once.
- **Validated:**
  - a runtime published without a .deb moved `AI.pruebas` within 28 s;
  - a node pinned to 0.1.88 stayed;
  - a same-version republish relaunched nothing.

### A-108 🔴 OPEN (found 2026-10-27) — io.slack fails a whole reply that carries an HTML file

- **What happens:** io.slack's MIME list has no `text/html`. A reply with a
  `generate_html_artifact` file is refused whole, the text and every other file, and Slack shows
  "Error (unsupported_attachment_mime)". Since 0.1.87 a reply may carry several files, so one
  unsupported file now takes the others down too.
- **Fix (the IO track):**
  - send what Slack accepts, and name what was left out in the text, rather than failing the
    reply;
  - or accept `text/html`.

  io.wapp sends the text only and drops every attachment, an older deferral.

### A-107 🔵 PENDING by the operator's choice (2026-10-27) — A published page cannot be updated under the same link

- **What happens:** a link is content-addressed. IO.blob curates `public/<sha256>`, the edge binds
  the key to that sha256 and size, and it refuses to rebind a key to different facts. Updating a
  page means publishing again (a new link) and taking the old one down (`unpublish_html_page`,
  0.1.89).
- **A stable link would need:**
  - an `update_html_page` tool;
  - a SY.admin action that curates the new content for the same publication;
  - an explicit edge operation that replaces the row's facts, from SY.admin only, for the
    publisher's own publications.
- **The operator:** *"dejalo pendiente, al final creo que es lo correcto pero tampoco es demasiado
  importante ahora"*.

### A-106 🟡 OPEN, interim decision (found 2026-10-27) — The blob store has no tenant; agents read the whole hive

- **What happens:** a `BlobRef` carries no tenant, and the store has no per-tenant directory. From
  0.1.89, `read_blob` lets an agent read any text blob of its hive by reference.
- **The interim decision:** *"si no es por tenant, hoy prefiero que lea todo lo del hive"*. A blob's
  name carries 16 hex of its content's hash, so a reference has to have reached the agent; it
  cannot be guessed.
- **Also open:**
  - a per-tenant directory, or a tenant on the reference;
  - reading a PDF from the blob (the model gets a PDF only when it arrives as an attachment);
  - finding a blob: search, in a future hive of its own (the operator: *"todo un tema… nada para
    ahora"*).

### A-105 ✅ VALIDATED ON 8.x (0.1.86, 2026-10-27) — What the 0.1.86 review found and the release fixed

Found by the code review round of [agent-tools-v1](../../docs/agent-tools-v1.md) (security,
operations); fixed in `89848d8`. Covered by tests; the ones marked *live* were seen on 8.x.

- **Any node could reconfigure an AI agent.**
  - **The problem:** ai.generic and the frontdesk accepted their control plane as `system` or
    `admin`, verbs in any case, from any sender. The router gates only `system`, in exact case.
  - **Fixed:** the control plane is accepted only from the real authorities: CONFIG_* from
    `SY.admin@motherbee`, and NODE_STATUS_GET and NODE_HELP from the node's hive orchestrator.
  - *Live:* before, an intruder got the whole config; after, `unauthorized`.
- **Tool descriptions over 1024 characters,** which OpenAI rejects. The cards are now bounded
  (the longest is 970), and so are Archi's and the executor's tools (`fluxbee_system_get` went from
  1277 to 790). *Live:* gpt-5.5 took the definitions.
- **A-95 had a second path:** the final-output log line.
- **`node_help` held the orchestrator's one-at-a-time workers** for up to 7 s. It now runs in its
  own task, and answers at once for a node that ai.generic does not run. *Live:* `IO.web` in 1.3 s.
- **`tool_catalog`'s read** followed symlinks and was unbounded: now a regular file, read bounded.
- **The frontdesk received the delete-state card,** which contradicts its flow. It now keeps get
  and put only.
- **CSV cells could run as formulas** in a spreadsheet: they are neutralized per OWASP.
- **Cost:**
  - the larger definitions are cached on Anthropic, with breakpoints on the tools and the system
    prompt, and the cache tokens are counted;
  - the registry is in name order, so OpenAI caches the prefix;
  - *Live:* Anthropic cache writes and reads of 6,376 tokens; OpenAI read 2,560 from its cache.

### A-104 ✅ VALIDATED ON 8.x (0.1.87, 2026-10-27) — An AI node's model must match a provider it may not know

- **What happens:** since 0.1.86, `behavior.provider` is optional and defaults to the hive's
  provider, but `behavior.model` is required and free text. SY.admin's examples say
  `"model": "gpt-5.5"` with no provider. Copied on an Anthropic hive (8.x), the result is an OpenAI
  model sent to Anthropic, and every message fails.
- **Fix:** without `behavior.model`, the node uses the hive's model for its provider
  (`hive.yaml ai.providers.<provider>.model`). The examples then name neither, unless the node
  needs another model.
- **Fixed (0.1.87, `6772df4`):**
  - the default model;
  - CONFIG_GET and CONFIG_SET show `ai_model`;
  - CONFIG_SET refuses a model the hive cannot supply;
  - the examples, Archi's prompts and its versioned cookbook seeds name neither provider nor model.
- **Validated:** a `config-set` with neither was accepted, with `claude-sonnet-5-5` from the hive.

### A-103 🔵 TO EVALUATE (found 2026-10-26, 0.1.86 review) — The tool cards restate constants by hand

- **What happens:** the cards repeat values that live elsewhere in the code:
  - the 128-byte filename limit;
  - the 24 h publication expiry (SY.admin's `PUBLICATION_DEFAULT_EXPIRES_SECS`);
  - the 120 s publish wait;
  - the 64–2400 image clamp.

  The catalog's golden test compares the cards with themselves, so it cannot catch a change to any
  of those values.
- **Fix:** build the card text from the constants, or test the text against them.

### A-102 ✅ RESOLVED (0.1.88; covered by tests) — A stopped node's first start can race the T9 conversion

- **What happens:** `start_node` saves the relaunch flag into config.json after launching the node,
  while the node converts the same file at boot. If the orchestrator's read-modify-write lands
  first, `behavior.capabilities` comes back, and the node boots FAILED_CONFIG. A restart fixes it.
- **Fix:** save the flag before launching, or convert in the orchestrator's launch step under the
  node's lock.
- **Fixed (0.1.88):** start and restart write the version and the relaunch flag before launching,
  in one write, under the node's lock.

### A-101 ✅ VALIDATED ON 8.x (0.1.88, 2026-10-27) — NODE_HELP's runtime_version goes stale after a restart

- **What happens:** `start_node` and `restart_node` launch the current version without saving
  `_system.runtime_version`; only the boot reconcile saves it. A node restarted onto a newer
  version reports the old one until the orchestrator's next boot.
- **Fix:** save the version in start and restart, before launching.
- **Seen on 8.x (0.1.87, 2026-10-27):** after `restart_node AI.pruebas`, the node ran 0.1.87 and
  `node_help` said 0.1.86. To fix with A-109.
- **Fixed (0.1.88):** every launch path records `_system.runtime_version` before launching, and the
  unit carries `FLUXBEE_NODE_RUNTIME_VERSION`.
- **Validated:** after the deploy, and after the A-109 relaunch, `node_help`, the binary, the stamp
  and `_system` all agreed.

### A-100 🔴 OPEN (found 2026-10-26, 0.1.86 review) — The router gates protected verbs only for `msg_type` system, in exact case

- **What happens:** the router's delivery gate checks protected verbs only on `msg_type: system`,
  and only in exact case. A node that took `msg_type: admin`, or a verb in lower case, received a
  CONFIG_SET from any sender.
  - 0.1.86 closes it on the AI nodes: `ai.generic` and the frontdesk accept their control plane
    only as `system`, in exact case, and only from their real authorities.
  - The IO nodes already checked their callers.
- **Fix, as defense in depth:** the router compares `meta.msg` without case, and gates protected
  verbs on any `msg_type`.
- **Seen on 8.x (0.1.85, 2026-10-27):** an `AI.intruso` client on worker1 sent `config_get`, in
  lowercase, as `system`. The router let it through, and the agent answered its whole config.
  Since 0.1.86 the agent refuses it.

### A-99 ✅ VALIDATED ON 8.x (0.1.87, 2026-10-27) — When a file generator runs, the model's own text is dropped

- **What happens:** the reply is the generator call's `text`, or a fixed Spanish line. Whatever the
  model writes after the call is lost.
- **Fix:** decide which text the user gets, and keep the model's own.
- **Fixed (0.1.87):** the reply is the model's text after its last tool call, else its text
  alongside a call. It carries every file of the turn, and a published link the text lacks is
  appended.
- **Validated:** a PDF and a CSV came in one reply, with the model's own sentence, and a page's
  link came with a CSV.

### A-98 ✅ VALIDATED ON 8.x (0.1.87, 2026-10-27) — A generated file's bytes flow back into the model's context

- **What happens:** each `generate_*_artifact` result carries the file's base64 back to the model.
  That costs tokens on every later turn, and fills the context with bytes the model cannot use.
- **Fix:** return a reference (the file name, its size, the blob reference), never the bytes.
- **Fixed (0.1.87):** a per-message collector keeps the file. The model gets `{status, filename,
  mime, size_bytes}`. Tested on 8.x through the two-file reply.

### A-97 ✅ VALIDATED ON 8.x (0.1.87, 2026-10-27) — The PDF generator garbles non-ASCII text

- **What happens:** `generate_pdf_artifact` prints only ASCII correctly, so accented Spanish comes
  out broken. The users of 8.x write in Spanish.
- **Fix:** embed a Unicode font, or use a PDF library that encodes text properly.
- **Fixed (0.1.87):**
  - WinAnsiEncoding (Windows-1252), with octal escapes;
  - lines wrapped by Helvetica's widths;
  - new pages.
- **Validated:** a 2-page Spanish PDF had every accent, ñ, ¿ and ¡ escaped, and no raw byte
  ≥ 0x80.

### A-96 🔵 TO EVALUATE (noted 2026-10-26) — How the SY nodes' tokens are measured per message

- **What happens:** the SY nodes that call a model (cognition, Archi, the frontdesk) keep the
  hive's single provider (agent-tools-v1 T8). How their tokens are measured per message is still
  open.
- **The operator (2026-10-26):** *"después vemos cómo se miden los tokens por mensaje"*.

### A-95 ✅ VALIDATED ON 8.x (0.1.86, 2026-10-27) — ai.generic logs the full public link of a page it publishes

- **Fixed (0.1.86, `89848d8`):** the publish tool logs the publication id. The final-output line
  logs only the file names and the text's length; the review found that this second path also
  carried the link (A-105).
- **Validated:** after two pages were published (on Claude and on OpenAI), no line with `/public/`
  was in the agent's journal.

- **What happens:** after `publish_html_page`, ai.generic writes an INFO line with the whole URL,
  capability key included (`published HTML page tool="publish_html_page" url=…/public/<key>`).
  Anyone who reads the worker's journal can open the page until it expires.
- **Fix:** log the publication id only, as the edge does since A-82.

### A-94 ✅ RESOLVED for the AI node (0.1.86; the refusal seen on 8.x) — "capabilities" means six different things

- **What happens:** the word names six unrelated things, none of them what an agent can do:
  1. identity's `capabilities` arrays: retired and never implemented (identity addendum §2.4);
     SY.identity rejects them;
  2. `behavior.capabilities.multimodal`: whether an AI node's model takes images;
  3. the "capability" URL and key of `/public/`: a link that is itself the permission;
  4. the "capability query" of `list_cloud_actions`: what Cloud may ask SY.admin;
  5. SY.timer's HELP catalog: its own capabilities;
  6. the router's WAN HELLO, `capabilities: ["lsa", "forwarding"]`: protocol negotiation.
- **Decided** (operator, 2026-10-26): what an agent can do is called **tools**
  ([agent-tools-v1](../../docs/agent-tools-v1.md) T7, T9).
  - **The AI node gets one meaning:** `behavior.capabilities.multimodal` becomes
    `behavior.multimodal`, and the default role stops saying "capabilities".
  - **Infra nodes keep their own uses (2–6):** *"cada nodo puede tener las suyas"*.
  - **Identity's dead arrays (1) stay refused.**
- **Fixed (0.1.86, `89848d8`):**
  - `behavior.multimodal` in ai.generic and the frontdesk;
  - the old key refused;
  - a saved config converted once at boot (0600, a `config.json.pre-0.1.86` copy, one INFO line).
- **Seen on 8.x:** a `config-set` with `behavior.capabilities` was refused with a clear message.
  The boot conversion is covered by tests, since no PROD config held the key.

### A-93 ✅ VALIDATED ON 8.x (0.1.86, 2026-10-27) — An AI node cannot choose its provider

- **What happens:** a node chooses its model (`behavior.model`) and its endpoint (`behavior.base_url`,
  `api.openai.com` or `api.anthropic.com` only). Its provider, and with it the API's format and the
  vault key it looks for, is always the hive's: `hive.yaml ai.default_provider`, anthropic on 8.x.
  `behavior.provider` is refused. So a tenant with only an OpenAI key cannot run an agent on 8.x.
  This follows the operator decision of 2026-10-02 (`ai.generic` the only runtime, the key the
  hive provider's).
- **The operator (2026-10-26):** "no veo qué problema hay al instanciar el código usar un proveedor
  u otro". The model client already speaks both protocols. A node would need to choose the provider
  in `behavior`, and the vault lookup would follow that provider.
- **Decided** (operator, 2026-10-26): *"el proveedor de AI queda restringido al tenant del nodo, pero
  no al proveedor"*. The provider is per node, and the key comes from the node's tenant.
- **Checked:** nothing in the code stops two nodes from running different providers. The change is
  in agent-tools-v1 (T8), for the proposed 0.1.86.
- **Fixed (0.1.86, `89848d8`):**
  - `behavior.provider` is optional and defaults to the hive's provider;
  - the key comes from the node's own tenant;
  - `base_url` is accepted only with the provider, and only that provider's host.
- **Validated on 8.x:** `AI.pruebas` published a page on Claude, the hive's provider, then on
  gpt-5.5 after one `config-set` with `provider: openai`, using its tenant's OpenAI key.
  `llm_usage` charged both to the tenant, by provider.

### A-92 ✅ VALIDATED ON 8.x (0.1.85, 2026-10-24) — What the 0.1.85 review found and the release fixed

Found by the code review round of [io-web-v2](../../docs/io-web-v2.md) §11.2 (security,
operations); fixed in `95bacd3`. Covered by tests; the ones marked *live* were seen on 8.x.

- **TLS resumption skipped io.web's role check:**
  - **The problem:** on a resumed TLS 1.3 session, rustls restores the client chain without
    calling the verifier. A hive removed from the registry, or demoted, stayed admitted through
    the edge's tickets.
  - **Fixed:** the SDK's role-gated server config keeps no sessions and issues no tickets; the
    named client config never resumes; io.web checks the role again right after the handshake.
  - **Tested:** the test failed on the old code. *Live:* every probe got a full handshake, and
    worker1 and egress1 were refused.
- **A tenant's io.web could take the web door:**
  - **The problem:** the orchestrator admitted a core IO runtime in any tenant and under any
    `IO.*` name, so a tenant (through Cloud's `provision_node`) could bind 9200 first.
  - **Fixed:** core IO runtimes run only in the root tenant (`TENANT_ROOT_REQUIRED`) and only as
    `IO.<x>@motherbee` (`CORE_IO_NAME_MISMATCH`); io.web refuses to run under any other name or
    tenant.
  - *Live:* three attempts got `CORE_IO_NAME_MISMATCH`.
- **`web_status` could not see the edge's upstream:** a missing upstream only reached the edge's
  log. The `EDGE_WEB_ROUTES` answer now carries it, shown as `routes.upstream` and as `upstream=`
  in the ops line (*live*: `upstream=ready`).
- **Port 9200 and the edge's direct path to the motherbee were undocumented:** added to the port
  tables, the exposure section and the firewall alert.
- **One WARN line per failed proxied request:** now aggregated per interval. *Live:* one line,
  `failed=1`, with the request id of the 502 before io.web started.

### A-91 🔵 TO EVALUATE (found 2026-10-24) — io.blob and io.cloud boot UNCONFIGURED while they work; io.web boots CONFIGURED

- **What happens:**
  - The SDK's boilerplate rule reads a seed-only `config.json` as UNCONFIGURED. So io.blob and
    io.cloud answer CONFIG_GET with `node_not_configured`, while they curate and relay with their
    defaults, and their status reads HEALTHY.
  - io.web (0.1.85) applies its contract to `{}` at boot and runs CONFIGURED.
- **To decide** with the IO family's review: whether the siblings should materialize their
  defaults the same way, so that a node's state says whether it works.

### A-90 🔴 OPEN (found 2026-10-24, 0.1.85 review) — One client can exhaust the edge's way to io.web

- **What happens:** the proxy opens a TCP and TLS connection to io.web for each request and closes
  it first. Each one leaves a TIME_WAIT on the ingress.
  - One internet client pipelining GETs to the web host faster than about 470 per second runs out
    of the ingress's ephemeral ports toward the motherbee. Every web-host request then gets a 502
    until about 60 s after the flood ends.
  - The proxy's 128 slots are global, so one client can also hold them all.
  - `/e/` and `/public/` are not affected.
- **Impact today:** none that matters; the web host serves a placeholder page. It matters once
  Archi is behind the proxy (stage 2).
- **Fix, before stage 2:**
  - a per-client share of the proxy's slots (`PeerSlots`, as `/public/`);
  - a small keep-alive pool to io.web, so connections and their handshakes are not per request,
    or at least io.web closing first (`Connection: close` upstream);
  - a deadline for a stalled io.web body (docs/nodes/sy-edge.md, "Not there yet").

### A-89 🔴 OPEN (found 2026-10-24, 0.1.85 review) — io.web's connection slots fill before any authentication

- **What happens:** io.web takes one of its 256 slots at accept, before the TLS handshake, which
  may take 10 s.
  - Any host that reaches the motherbee's 9200 can keep the slots full with idle connections,
    about 26 new ones per second. That means any spoke on the internal network, or the
    motherbee's LAN address.
  - The edge's connections are then closed at accept, and the web host answers 502.
  - No certificate is needed. It is not reachable from the internet.
- **Fix, before stage 2:**
  - count against the 256 only connections past their handshake;
  - give connections still in their handshake a small budget per source address and a shorter
    timeout (2–3 s on the internal network).

### A-88 ✅ RESOLVED (0.1.84; covered by tests, the fixed bodies seen on 8.x) — An `/e/` error hands internal detail to the internet

- **Fixed (0.1.84, `5be9e7b`):** one fixed body per error code; the detail goes only to the edge's
  log. Seen on 8.x: an unknown `/e/` endpoint answers `NOT_FOUND` "no such endpoint". A failed
  reply is covered by tests.
- **What happens:** when the handler's reply fails, `/e/` answers with `err.to_string()`
  (src/bin/sy_edge.rs ~3717), which can carry a timeout's `trace_id` and the request's message
  family.
- **Fix (io-web-v2 §11.1):** a fixed error body per code, the detail only in the edge's log.

### A-87 ✅ VALIDATED ON 8.x (0.1.84, 2026-10-23) — One byte holds an edge connection forever

- **Fixed (0.1.84, `5be9e7b`):** hyper's HTTP/1 builder with a header-read timeout that starts
  before the first byte; no protocol sniffing, no HTTP/2. Validated from outside: a connection
  that sent one byte was closed after 30.2 s, and an h2 preface in 0.3 s.
- **What happens:** the edge serves every connection with hyper-util's `auto::Builder` and
  `serve_connection_with_upgrades` (src/bin/sy_edge.rs ~2734-2744), which sniffs for the HTTP/2
  preface. The sniff has no timeout after the first byte (hyper-util ignores `http1_only()`
  there), and hyper's h2 server has no idle timeout. The 4,096 connection permits are taken at
  accept, so 4,096 connections that each send one byte refuse every later client: Cloud's `/e/`,
  the webhooks, `/public`, and Let's Encrypt's validation. No credential is needed.
- **Fix (io-web-v2 §11.1):** hyper's HTTP/1 builder with its head-read timeout, which starts before
  the first byte; no protocol sniffing and no HTTP/2 (reachable today only by prior knowledge);
  tests for one byte and for an h2 preface.

### A-86 ✅ RESOLVED (0.1.84; covered by tests, the idle cut seen on 8.x) — Stalled readers can block the edge's downloads

- **Fixed (0.1.84, `5be9e7b`):**
  - every connection's stream is paced (30 s of idle credit, then 1 KiB/s), with a 30 s body
    deadline and `TCP_NOTSENT_LOWAT` 16 KiB;
  - per-IP caps: 256 connections (`edge.max_connections_per_ip`) and 8 downloads
    (`edge.public_max_inflight_per_ip`);
  - downloads keep their own 128 slots, apart from `/e/`'s.
- **Validated:** the idle cut on 8.x (the A-87 test); the caps and the pacing by tests only.
- **What happens:** `/public` streams with no write timeout, so 128 readers that stop reading hold
  every download slot (`PUBLIC_BUSY` for everyone), and every route shares one 4,096-connection cap.
- **Fix:** a minimum rate, an idle deadline, a per-IP cap, and download slots kept apart from
  `/e/` (with the edge's work in [io-web-v2](../../docs/io-web-v2.md)).

### A-85 🔴 OPEN (found 2026-10-22) — io.slack's file fetch has no timeout and buffers the whole body

- **What happens:** `download_private_file` (nodes/io/io-slack/src/main.rs) uses a client with no
  timeout, reads the whole body into memory and checks the size only afterwards.
- **Fix:** stream to staging under a byte cap, with a connect timeout, a total deadline and a
  minimum rate; HTTPS only, no redirects, no non-global addresses.

### A-84 🔴 OPEN (found 2026-10-22) — `bundle_upload` follows symlinks and extracts without caps

- **What happens:** `runtime_package_safe_blob_rel_path` (src/bin/sy_admin.rs) refuses only
  absolute paths and `..`, and the file is opened following symlinks; Syncthing replicates symlinks
  from the workers into `active/`. `materialize_bundle_runtime_package` has no size or count caps
  and keeps the zip's mode bits.
- **Fix:** `O_NOFOLLOW` at each step, regular files only, a path under a root-only directory, and
  an extraction with caps that sets its own modes.

### A-83 ✅ RESOLVED (0.1.84; covered by tests) — The edge answers an empty 200 when it cannot build a response

- **Fixed (0.1.84, `5be9e7b`):** a response that cannot be built is a 500, and a layer puts the
  security headers on every response. 8.x never had to build one.
- **What happens:** three places in src/bin/sy_edge.rs fall back to
  `Response::new(Body::empty())` when a response cannot be built (for example an invalid header
  value): a 200 with no body and no security headers.
- **Fix:** answer 500 with the security headers.

### A-82 ✅ VALIDATED ON 8.x (0.1.84, 2026-10-23) — The edge logs capability keys

- **Fixed (0.1.84, `5be9e7b`):** the edge stores and logs only `SHA-256(key)`; its
  `publications.json` is converted once, and the conversion is reported. Validated: a publication
  made in the gate was logged with `key_sha256`, its link was served, and no raw key was left in
  the edge's state directory.
- **What happens:** src/bin/sy_edge.rs logs `key = %row.key` when it publishes a public artifact
  and when it drops an expired one; those records leave the DMZ through the router stream, and the
  keys sit in plaintext on the edge's disk.
- **Fix:** the edge stores and logs only `SHA-256(key)`.

### A-81 ✅ VALIDATED ON 8.x (0.1.84, 2026-10-23) — An agent on a worker cannot publish a page

- **Fixed (0.1.84, `5be9e7b`):** as below, with a 110 s budget for the whole call.
- **Validated (the agent's path without the model):**
  - a mesh client carried a test agent's identity on worker1;
  - it wrote a page into worker1's `active/` and sent `publish_artifact` 1 s later;
  - the answer was `ok` after 12.8 s and 13.3 s (two runs);
  - the edge served the page from outside, and answered 404 after the unpublish.
  - The model's own tool call was not run: the test tenant had no AI key, and a tenant's agents
    read only their own.
- **What happens:** ai.generic's `publish_html_page` promotes the blob on its worker and calls
  `publish_artifact` at once; IO.blob opens the motherbee's `active/` with no wait, before
  Syncthing has brought the file (~10 s), and answers `BLOB_NOT_FOUND`, which the model receives
  as final. PROD's agents run on worker1.
- **Fix:** before taking its publication lock, SY.admin sends the producer's hive a `sync_hint`
  on `fluxbee-blob` (`wait_for_idle: false`) and waits up to 30 s for the file in the motherbee's
  `active/`; IO.blob never waits in its message loop; any curate error releases what was curated.

### A-80 🟡 OPEN (found 2026-10-22) — A restore is refused when another tenant holds the same email

- **What happens:** `check_ilk_restorable` (src/bin/sy_identity.rs) looks the person's vouched
  email up across every tenant, while emails are unique per tenant. Restoring a marked person is
  refused with `ILK_KEY_CONFLICT` when someone in another tenant holds the same email.
- **Fix:** look the email up in the person's own tenant, as registration does.

### A-79 ✅ VALIDATED ON 8.x (0.1.83, 2026-10-22) — What the hardening reviews found besides A-62, A-20, A-22 and A-42

Found by the design review and the code review round of
[control-path-hardening-v1](../../docs/control-path-hardening-v1.md); fixed in `5fb8dab`.
Covered by tests; the ones marked *live* were seen on 8.x.

- **The router:**
  - a WAN frame's sender was looked up in every hive's bucket, not its peer's, so root on a spoke
    could send as another spoke's node (it now resolves only in the peer's bucket, or among the
    hub's vouched nodes when the peer is that hub; *live*: the few frames of a node not yet or no
    longer listed are counted as `foreign_source`, which 0.1.82 dropped without a log line);
  - a hive's LSA could carry any node name, routes matching other hives' names and taps on other
    hives' traffic (it now carries only `*@P`, routes for P's destinations and taps P is party to
    and that target P's nodes), and a UUID it advertised could shadow another hive's node (a UUID
    two hives list now resolves to neither);
  - a tap copy carried SYSTEM authority (it no longer does), and a sibling router's shared-memory
    region was read whoever owned it (it must be the router's user's, with no group or world
    write).
- **SY.admin:** wf-rules could publish `wf.engine` itself, which every workflow runs; the teardown
  purged the ILK before deleting its secrets, so they could not be tied to a hive (now one
  `teardown_node`; *live*); `test_hold_ms` and an unbounded `sync_hint` wait reached the mesh; the
  WF ownership check waited on the orchestrator's queue (now read from shared memory).
- **SY.orchestrator:** its admin inbox answered reads to any caller and forwarded them under its
  own name (*live*: FORBIDDEN); non-canonical ILK ids were stored as given.
- **SY.identity:** an orchestrator could delete, purge or rewrite any person; a non-canonical
  `ilk_id` slipped past the scope check and overwrote another ILK at the database's upsert; a
  channel another ILK held could be taken over; a node could register an email.
- **The frontdesk:** a handoff was recognized by its shape alone and from any sender; a handoff
  without an email inherited the typed one; a vouched email was lost when the handoff had no
  name; the registration tool acted on an ILK the model named.

### A-78 ✅ VALIDATED ON 8.x (0.1.82, 2026-10-21) — The vault's change notice never reaches the edge

- **Validated (0.1.82, `d2980cf`):**
  - A throwaway secret owned by the edge was put and deleted, and both notices reached the edge,
    the first ever seen there.
  - A vault rollback of the edge's certificate made the edge swap to the previous certificate on
    the notice alone, without a restart and without `/acme/reload`. A second rollback brought the
    first one back the same way.

- **What happens:**
  - SY.edge reloads its TLS certificate when SY.vault announces a change (`VAULT_SECRET_CHANGED`,
    `src/bin/sy_edge.rs` ~465-657).
  - The router's edge gate (`vpn_allows_between`, `src/router/mod.rs` ~5976) comes before the global
    SYSTEM bypass. It admits only the `EDGE_*` commands and their answers, the IO data path and
    `VAULT_GET`, and every broadcast fan-out applies it. The notice is dropped.
- **Evidence in PROD (2026-10-21):** SY.vault has sent 146 notices. Neither sy-edge nor rt-gateway
  on ingress1 logged one since 2026-08-05, the start of their journals.
- **Impact:**
  - PB-1's mitigation ("a key change in the vault restarts the edge") never ran: the 2026-07-30
    logbook saw the old certificate served after a put until a manual restart.
  - Channel secrets are not affected: a rotation reaches the edge through a new `EDGE_OPEN_URL`.
- **How it showed up:** verifying the code for the ACME design (docs/acme-edge-certificates-v1.md §2).
- **Fix (0.1.82, design §5):**
  - SY.vault also sends the notice as a unicast to the owner when the owner is an `SY.edge`.
  - The edge gate admits only that unicast from SY.vault on the primary hive.
  - The broadcast, which carries every stored key's name and owners, stays out of the DMZ.
  - The edge reloads without restarting (PB-1).

### A-77 ✅ RESOLVED (0.1.81, 2026-10-20) — What the closing pass and its review found besides A-72..A-76

Found on 2026-10-19 while verifying the open findings and in the review round of 0.1.81; fixed in
`4e9f91e`. Covered by tests; the ones marked *live* were seen on 8.x.

- **Archi (SY.architect):**
  - the reconcile never saw existing runtimes, nodes, routes or VPN rules (it read only a top-level
    array, `items` or `responses`), so every declared node or route was a Create on every run;
  - an AI node's config never converged (whole-value compare against the filled-in config);
  - a solution could own, and then delete, a workflow it never applied (ownership counted unexecuted
    manifests; the default was "solution", the docs said "external");
  - a lowercase or unknown route action turned a working route into a failed replace;
  - the VPN model (`{vpn_id: text, hive}`) could never match SY.config.routes' pattern → numeric id;
  - its artifact auditor refused valid workflow definitions (it wanted a `steps` field);
  - a hive the manifest adds blocked the snapshot; a WF on a DMZ hive or a declared WF./SY./RT. node
    failed only after the operator confirmed;
  - plans with `add_hive` timed out in Archi while the executor still ran them;
  - its cookbook seeds were never shipped by the `.deb` (*live:* seeded now) and taught old shapes;
  - the Messages view would fail on rows whose message has no sender name.
- **SY.admin:** its plan executor counted an accepted `add_hive` as done; it now waits for the join.
- **SY.orchestrator:**
  - `remove_hive`'s cleanup ran inside sy-orchestrator's own unit, so everything after it stopped the
    orchestrator never ran; it now runs in a transient unit;
  - a join's record was lost on its own rewrites, so a restart mid-join never ended it.
- **SY.timer:** answered every system message it did not handle, router notices included, so an
  UNREACHABLE could bounce between it and the router forever (no storm was seen in PROD's logs).
- **AI runtime:** a message over 60 s was retried up to 3 times, billing again and re-running tools,
  then dropped without an answer; provider calls had no timeout at all.
- **Documented, not built (operator's decision, 2026-10-19):** Archi never removes nodes, routes or
  VPN rules a solution stops declaring (workflows and the OPA policy are removed); an extra one goes
  through SY.admin's API.

### A-76 ✅ CLOSED (0.1.81, 2026-10-20) · the several-routers items DOCUMENTED (unsupported topology) — Smaller defects found while rewriting the docs from the code

- **Outcome (0.1.81, `4e9f91e`), item by item:**
  - routers: a restarted router takes its region over (no stale nodes); the identity region is
    recreated when its limits change, and SY.identity refuses to start if its store does not fit;
    the two several-routers defects (broadcast copies, WAN UNREACHABLE) are documented as limits
    of an unsupported topology (operator's decision, 2026-10-19);
  - SY.admin: `/hives/{hive}/status` and the per-hive inventory summary answer, `/inventory/hive`
    is gone, a vault call for a hive without a vault answers 400, vault metadata follows the list
    rule, `get_node_status` reads SY.* and RT.gateway from their units, the CONFIG_* allowlists
    follow the system policy;
  - OPA: a check is a read again (its own `check_id`, a scratch compile, no version, nothing
    staged), and back in Archi's read tool;
  - SY.orchestrator: `remove_hive` (A-48); Syncthing's firewall rule is 22000/tcp on the
    motherbee only;
  - AI nodes: `behavior.instructions`, `instructions.source` and the unread `runtime.*` keys are
    gone and refused; an OpenAI tool turn checks for a cut answer; ai.generic and the frontdesk
    report their real state; `deploy-ia-node.sh` is deleted;
  - comments and leftovers: corrected or removed.
- **Left to the IO-family track:** the managed IO nodes still answer NODE_STATUS_GET with a fixed
  HEALTHY (io.slack does not answer it); the SDK helper they can use exists.

Found by the docs rewrite of 2026-10-17 (the reference set and the node pages read every path they
describe). None was tried live. To be sorted with the operator; most belong to the IO-family and
router reviews.

- **Routers:**
  - in a hive with several routers, the gateway's nodes receive a broadcast twice;
  - a restarted router keeps entries for nodes that never reconnect;
  - an UNREACHABLE coming back over the WAN reaches only nodes on the gateway router;
  - lowering the identity limits leaves readers on the old SHM offsets.

  PROD runs one router per hive, so only the last one can show there.
- **SY.admin:**
  - the catalog lists `GET /hives/{hive}/inventory/summary` and `/inventory/hive`, but neither has a
    handler (405);
  - `GET /hives/{hive}/status` looks broken;
  - `get_node_status` looks for `fluxbee-node-*` units, so it misreports `SY.*` nodes;
  - the HTTP vault metadata route passes no `tenant_id`, so a non-root secret answers
    `UNAUTHORIZED`;
  - `/hives/worker1/vault/...` goes nowhere: SY.vault runs only on the motherbee;
  - the allowlists of SY.admin and Archi name `SY.config-routes`; the node is `SY.config.routes`,
    so that entry never matches.
- **OPA:** a policy check is not a read. SY.admin bumps the OPA version for it
  (`next_opa_version`) and SY.opa.rules compiles it into `staged/`, so a check run between the
  operator's `opa_compile` and `opa_apply` replaces what was staged (found by the 0.1.80 review;
  Archi's read tool no longer runs checks).
- **SY.orchestrator:**
  - `remove_hive` revokes neither the spoke's identity key nor its TLS certificate;
  - `syncthing_firewall_rules` still opens 22000/udp and 21027/udp, though Syncthing uses TCP only
    since host-posture stage 3.
- **AI nodes:**
  - `behavior.instructions` does nothing for a managed agent: its ILK definition, or the
    "unconfigured" prompt, replaces it (the scaffold no longer writes it);
  - an OpenAI tool turn does not check for a truncated answer (its usage record says `truncated`);
  - ai.generic launched by the orchestrator ignores the `runtime.*` timeout, pool and queue
    settings, and reads `instructions.source` file/env as literal text;
  - `scripts/deploy-ia-node.sh --update-existing` restarts with a local `systemctl` (old binary,
    same host only), and `--kill-first` does not purge (the re-spawn hits
    `NODE_ALREADY_EXISTS`);
  - SY.frontdesk.gov's `NODE_STATUS_GET` always answers HEALTHY.
- **Comments and leftovers:** SY.identity's comment on sync auth says the default is "disabled"
  (the code defaults to required); the `IlkGetRequest` comment says Cloud reads are tenant-scoped
  (they are not); SY.storage still creates `storage_migrations`, which nothing writes since the
  cleanup.

### A-75 ✅ RESOLVED (0.1.81, 2026-10-20) — The Go SDK still has the reply-matcher bug Rust fixed after the 2026-08-26 outage

- **Fixed (0.1.81, `4e9f91e`):** family-wide success matchers are never reply shapes;
  `SendSystemRPC` defaults to TTL 16; a frame over 128 KiB is refused with `MessageTooLargeError`
  and the link stays up. SY.timer, SY.wf-rules and the WF engine answer a request whose reply is
  too large with an error. Covered by tests (each failed on the old code); the Go nodes run 0.1.81
  in PROD and the OPA check went through SY.opa.rules.

- **What happens (go/fluxbee-go-sdk, found while writing docs/reference/protocol.md):**
  - Go registers any-message-of-a-kind matchers as reply shapes, the family-wide drop the Rust SDK
    fixed in 0.1.31 (the `any_msg_type` matcher).
  - `SendSystemRPC` defaults to TTL 1, so a system request cannot leave its router.
  - A send over the frame limit drops the whole connection.
- **Impact:** every Go node (SY.timer, SY.wf-rules, SY.opa.rules, the WF engine) carries them. No
  incident is known.
- **Proposal:** align the Go SDK with Rust (matchers, TTL, an oversize send refused without dropping
  the link). To decide with the operator.

### A-74 ✅ VALIDATED ON 8.x (0.1.80, 2026-10-17) — One invalid route, VPN or tap blocked every later change in SY.config.routes

- **What happens:** SY.config.routes gets no reply for an invalid route, VPN or tap, and later
  changes stay blocked until that entry is deleted or the node restarts
  (src/bin/sy_config_routes.rs; found while writing docs/reference/routing-and-policy.md).
- **Impact:** one bad rule from the operator, Archi or a plan stops all routing changes on that
  hive, silently.
- **Proposal:** answer the invalid entry with an error and keep applying the rest. To decide with
  the operator; not tried live.
- **Fixed (0.1.80, `58db83a`):** a change is built on a copy and committed only when the config
  region accepts it (`commit_config`); the region is put back after a failed write or save; the
  file is saved through a rename; every refusal is answered (`INVALID_CONFIG`, `INVALID_REQUEST`,
  `INVALID_CONFIG_SET`). Validated on 8.x: an invalid route answered `422 INVALID_CONFIG` in 0.01 s
  and changed nothing; the next valid one committed.

### A-73 ✅ VALIDATED ON 8.x (0.1.80, 2026-10-17) — Archi's read tool could change the system without the operator's confirmation

- **What happens:**
  - `fluxbee_system_get` is Archi's read tool. It accepts GET and POST, translates the path with
    `translate_scmd` and runs the action (`ArchitectSystemGetTool::call` in
    src/bin/sy_architect.rs).
  - It never checks whether the action changes something. `POST /hives` is `add_hive`, and
    `POST /hives/{hive}/nodes` is `run_node`. Both run at once, without the plan compiler and
    without the operator's CONFIRM.
  - The only guard is the prompt ("never call admin mutation endpoints directly").
  - The operator's own SCMD, a pasted plan and `POST /api/executor/plan` also run without CONFIRM,
    but those come from the operator.
- **Impact:** a model mistake, or text Archi reads (a message in `turns`, a node's output), can make
  it change the installation. Found while writing docs/nodes/sy-architect.md; not tried live.
- **Proposal:** make the read tool refuse every action that is not a read (an allowlist of read
  actions, plus the safe POST checks: the OPA policy check and a node's live CONFIG_GET). Small.
  To decide with the operator.
- **Fixed (0.1.80, `58db83a`):** the read tool runs a GET that names no change and the POSTs that
  change nothing (a node's live CONFIG_GET, the timer's conversions); an OPA check is not one (it
  stages the policy it checks, see A-76). Validated on 8.x: asked to start a node through the read
  tool, Archi got the refusal and no node was created.

### A-72 ✅ VALIDATED ON 8.x (0.1.81, 2026-10-20) — An agent's role does not reach the workers: `blob/agent-assets` is not replicated

- **Fixed (0.1.81, `4e9f91e`):** a worker-only Syncthing folder, `fluxbee-dist-agent-assets`
  (motherbee send-only, workers receive-only), backfilled for hives that joined before. Archi
  writes an asset outside the folder and renames it in (mode 0644); ai.generic reads only a
  regular file, bounded, and keeps its definition until a complete new one is readable.
  Validated: worker1 got the folder with its core update; a new role reached it in ~14 s; an
  agent on worker1 composed its definition from it.

- **What happens:**
  - An agent's role, skills, handbooks and personality are JSON assets in the motherbee's
    `blob/agent-assets/`; Archi writes them. The agent's ILK stores their hashes.
  - ai.generic reads them from the local disk, `<blob_root>/agent-assets/<hash>.json`
    (`load_cognitive_asset`, `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`).
  - Syncthing replicates only `blob/active` to the workers. `agent-assets` is in no folder
    (`src/bin/sy_orchestrator.rs`, `peer_shares_active_blob` and the `agent-assets` permissions).
- **Impact:**
  - An agent on a worker does not find its assets: it is left without a definition and answers the
    fixed "unconfigured" JSON.
  - Today an agent with a role only works on the motherbee. However, per-interlocutor cognition
    (A-70) stayed on the workers so that the agents would run there.
- **How it showed up:** in the review of the developer docs (2026-10-17). It was not tested live:
  the gates' agents ran on the motherbee.
- **Proposal:** share `agent-assets` with the workers, read-only like `blob/active`, or serve them
  over the mesh. To be decided with the operator.

### A-71 ✅ VALIDATED ON 8.x (0.1.80, 2026-10-17) — ai.generic truncated the user's message, together with its memory, to 1,200 characters

- **What happens:**
  - With `capabilities.multimodal: false`, ai.generic prepends the communication's memory block to
    the user's text: `Conversation memory: … Current user message: …`
    (`inject_memory_package_into_text_input`, `ai_node_runner.rs`).
  - The AI SDK runner trims that whole text to 1,200 characters
    (`DEFAULT_INTERACTION_MAX_CHARS`, `crates/fluxbee_ai_sdk/src/function_calling.rs`).
- **Impact:**
  - With a memory of more than about 1,100 characters, the model does not see the current message:
    it answers without knowing what it was told.
  - A long message, for example an email, also arrives cut off.
  - The gates did not show it: their conversations were short.
- **How it showed up:** in the review of the developer docs (2026-10-17).
- **Proposal:** do not trim the current message (the limit stays for the immediate history), or
  bound the memory on its own. To be decided with the operator.
- **Fixed (0.1.80, `58db83a`):** the AI SDK no longer cuts the message being answered (the
  1,200-character limit stays for the history around it); ai.generic keeps the person's own words,
  without the memory block, in its immediate history. The cut also gave Archi's pipeline stages
  (designer, auditor, programmer, plan compiler) cut inputs since March: they now get their whole
  input, so expect more tokens per stage. Validated on 8.x: a ~3,000-character message to Archi
  with the question at its end was answered correctly.

### A-70 ✅ VALIDATED ON 8.x (0.1.75 to 0.1.79, 2026-10-16/17): NATS DELETED AND LLM USAGE RECORDED — The records' "NATS" lived inside the router, lost data and only stored the motherbee's

- **How it came up:** while designing stage 5 (tokens), the record was going to travel over NATS.
  The operator asked to review NATS in general and between hives. Design:
  `docs/router-stream-v1.md`.
- **What happens (verified in the 0.1.74 code and measured in PROD on 2026-10-16):**
  - **It is not NATS:** it is an in-house broker that speaks part of the NATS text protocol and
    runs inside the router process, one per hive, with no link between hives.
  - **Messages do not travel through it.** The router publishes a copy of each user message as a
    turn, with a new TCP connection for each publish.
  - **Its durability is weak:**
    - each topic is an in-memory queue of up to 50,000 messages which, past the cap, deletes the
      oldest ones even if nobody has read them, silently;
    - one in-flight message per consumer;
    - the disk is a single JSON file that is rewritten whole every 500 ms.
  - **Only the motherbee's data reaches the database,** because SY.storage runs only there:
    - worker1 had 67 turns that were never stored;
    - the ingress (DMZ) kept on disk 597 turns of edge traffic that nobody consumes, and they were
      going to grow up to 50,000.

    It is the gap A-38 left noted ("getting what is on the workers to reach the motherbee"), and it
    covers every hive except the motherbee.
  - **Three topics with no producer** (`storage.events`, `storage.items`, `storage.reactivation`):
    only an E2E script uses them.
  - **The log plane lives in the router's fast path.** The operator: *"todo lo que va a DB es
    de log, alta cantidad de datos, tiempos largos... eso choca contra la dinámica de fluxbee
    principalmente en los routers, que es alta velocidad"* ("everything that goes to the DB is log:
    a high volume of data, long times... that clashes with fluxbee's dynamics, mainly in the
    routers, which is high speed").
- **The operator's decisions (2026-10-16, in the doc §0):**
  - NATS dies (*"quedó como algo inicial viejo"* ("it was left over as an old, early thing")).
  - The router gains a simple stream, one per router, `RT.<router>.stream@<hive>`. The pipe is
    the Fluxbee socket.
  - Best effort for everyone.
  - A small window: 2 minutes and 32 MB per router, in visible constants. When it piles up: an
    alarm and a reduced log.
  - The router never waits.
  - Order:
    1. the stream with SY.storage;
    2. cognition and policy, and NATS deleted;
    3. the tokens.
- **Review (one round, three views: the router, the data path, the tokens):** everything
  incorporated into the doc.
  - **Live bugs it found** (fixed in stage 1a, on today's NATS):
    - **the AI's responses are not stored in `turns`:** the `(ctx, seq)` key comes from the
      `trace_id` and the response reuses the request's, so `ON CONFLICT DO NOTHING` throws it away;
    - **SY.cognition processes each turn several times:** it only confirms when it finishes, and
      the broker redelivers every 2 s. A test turn cost 3 or 4 calls to the model;
    - **a frame over 128 KB kills a link forever** in the node, WAN and IRP writers;
    - **a message from another hive to a node on a router that is not the gateway is dropped;**
    - **SY.storage does not reconnect to postgres;**
    - **`storage_inbox` is never purged,** and its retry runs only at startup.
  - **From the design:**
    - `RT.` reserved, with verification of the router's PID;
    - the stream's disk in its own thread;
    - separate queues for turns and for nodes;
    - authenticated acks;
    - the cursor and the sequence made safe against trims and restarts;
    - an ack on receipt for cognition and policy, without reprocessing repeats;
    - a dedicated SY.storage connection for what arrives in batches.
  - **For the operator:** six questions (Q1 to Q6 of the doc). None of them holds up stage 1.
- **Stage 1a, the code (2026-10-16):** the live bugs fixed on today's NATS. A code review (two
  views) found the following, and it was fixed before the build:
  - **The `storage_inbox` purge would have deleted history:** Archi's messages page reads the
    inbox as the message log, and the responses that `turns` discarded exist only there. No purge;
    where the log lives is up to the operator (Q7 of the doc);
  - **the inbox retry did not count attempts** and always retried the same rows: now it adds one
    attempt per failure and moves forward with a cursor;
  - **SY.storage stayed without a database if postgres was not there at startup:** now it exits
    and systemd retries;
  - **the router's remote origin** comes only from the LSA, with the canonical name (EDGE-01), and
    not from the frame. The fallback through the reachability table was dead code outside the
    gateway;
  - **the damage from the big frame was smaller than written:** node and IRP reconnected, losing
    what was in between; only the WAN link stayed dead in one direction.
- **Stage 1a, validated on 8.x (0.1.75):**
  - a request and its response both end up in `turns`;
  - a turn costs one call to the labeler, versus 3 or 4 before;
  - SY.storage comes back on its own when its database is cut;
  - `RT.` stays reserved.

  Detail in `lab/DEPLOYMENTS.md`.
- **Q7, decided by the operator (2026-10-16):** Archi reads `turns` (*"que lea turn es integrado
  mas que lo otro, si es asi hacerlo"* ("having it read turn is more integrated than the other
  thing; if so, do it")). `turns` becomes the message log, and the inbox gets purged.
- **Stage 1b, the code:** the stream on each router, with SY.storage as the destination. It
  includes:
  - `turns` as the message log, with a one-time backfill and a purge of the inbox;
  - Archi on `turns`;
  - `list_streams` in the admin;
  - `streamlost` in `ops.py health`.

  A code review (one round) found no HIGH. Everything was fixed before the build:
  - **the loss counters lied in both directions:**
    - the trim removed what had expired without counting it;
    - each retry of a batch counted the skipped items again.

    Now each one is counted once: when the cursor passes it, or when it leaves the log without the
    destination having taken it. What is in an in-flight batch waits for its ack;
  - **the upgrade order revived the old consumer:** the new router starts before the old
    SY.storage, which subscribed again. It would have held on to `storage.turns` and stored every
    turn twice in the meantime. Now the broker rejects that consumer;
  - **minor:**
    - the disk is written outside the lock that the router's threads use;
    - a failed write opens a new segment instead of cutting the one that was there;
    - the queue has a cap in bytes;
    - the last segment is deleted when there is nothing left to deliver;
    - SY.storage's processing moves out of the ack path;
    - each record has to come from a node in its stream's hive;
    - Archi reads the tail cursor before the list.
- **Stage 1b, validated on 8.x (0.1.76):**
  - **the four hives reach the database:** worker1, the ingress and the egress for the first time;
  - **the backfill left the 582 rows of `turns` with their message,** and the inbox deleted 435 old
    rows;
  - **Archi shows `turns`,** with the live tail;
  - **SY.storage absent for 71 s:** the backlog was seen, and everything was delivered once;
  - **absent for more than 3 min:** 10 expired, counted once, with the alarm, and routing carried
    on;
  - **a router restarted with pending items** recovered them from disk;
  - **the motherbee restarted under traffic** lost nothing;
  - **clean health** apart from the induced expiry.

  To test it, `sy-storage` had to be masked: SY.orchestrator's watchdog brings it up 2 s after a
  `stop`. Detail in `lab/DEPLOYMENTS.md`.
- **Stage 2, discussed with the operator (2026-10-17, doc §6.1):**
  - **cognition belongs to the interlocutors** (*"la parte cognitiva es de los interlocutores no
    de que hive está o que nodo es... los nodos son mecanismos, igual que el router"* ("the
    cognitive part belongs to the interlocutors, not to which hive it is on or which node it is...
    the nodes are mechanisms, just like the router")): memory and references per communication
    between ILKs, never per hive or node;
  - **the owner of the turn is the owner of the communication:** the agent serving it;
  - **cognition stays next to the agents** (motherbee and workers), so they read their memory
    through local SHM (*"no quiero cambiar eso"* ("I don't want to change that")). The agent's
    router passes both directions to its local cognition;
  - SY.policy goes away (*"si no hace nada sacalo. molesta"* ("if it does nothing, take it out.
    it's in the way")); NATS is deleted entirely, with no legacy; the ingress's 597 turns are thrown
    away; test data can be deleted;
  - the LLM cost stays as it is; what cuts across hives is for later.

  A design review (one round) found one HIGH: removing SY.policy in a single version blocked the
  core-update of every worker. It goes in two versions. The MEDIUMs were incorporated: a
  communication is `(thread_id, agent ILK)`, the turn number replaces `thread_seq`, the stream's
  epoch, the packed envelopes, the region that survives restarts, the reload over the mesh, and
  equal weights for the two ILKs.
- **Stage 2a, the code:** cognition per communication, SY.policy retired (first version) and the
  router without NATS for turns. A code review (one round), no HIGH. Fixed before the build:
  - **MEDIUM:** a loaded communication went stale if its agent went to another hive and came back,
    and the other cognition had continued it. Now, after 5 min without turns, it is requested
    again from SY.storage, and the one that is further ahead wins;
  - **LOW:**
    - the agent's node is pinned as soon as the state is fetched;
    - the reload keeps what SY.storage does not have yet, and it runs outside the main loop;
    - without a thread, the memory region is not read;
    - the `hive.yaml` is written atomically;
    - scopes are also protected by the turn number;
    - SY.frontdesk.gov answers with its ILK;
    - old comments and doc passages.
- **Stage 2a, validated on 8.x (0.1.77):**
  - **two hives, one communication:** an interlocutor on worker1 and the agent on the motherbee.
    The motherbee's cognition received both directions, worker1's nothing; one single
    communication in the database, written by one cognition, with the agent's ILK;
  - **the responses go out with the agent's ILK;**
  - **the agent receives its communication's package,** and the one the sender sends is dropped;
  - **a cognition restart** reloads over the mesh and the memory stays; the numbering continues;
  - **the inactivity refetch** asks for the state once, after 5 min without turns;
  - **an agent on worker1:** worker1's cognition writes to postgres through the stream. Before, it
    had no database;
  - **SY.policy** runs on no hive; NATS received nothing; clean health.

  Detail in `lab/DEPLOYMENTS.md`.
- **Stage 2b, the code:** NATS deleted, some 9,000 lines: the broker, the SDK client, the diags,
  eight scripts, SY.policy and `thread_seq`. SY.storage's metrics and the orchestrator's wait for
  the database move to the mesh (`STORAGE_METRICS_GET`), and there are one-time transitions at
  startup. A code review with two views, no HIGH:
  - **MEDIUM:** on the motherbee, the old router wrote the broker's state again after the cleanup,
    when it stopped during startup. The cleanup now runs after the router runs the new binary;
  - **LOW, all fixed:**
    - retired nodes stay out for everyone who reads a roles section;
    - the admin's commands, which stayed queued during startup, get `ORCHESTRATOR_BOOTING`;
    - one metrics scan at a time;
    - SY.policy leftovers on a motherbee installed with `install.sh`;
    - old comments;
    - the admin catalog's parity check, broken since 1b and A-68.
- **Stage 2b, validated on 8.x (0.1.78):**
  - **nothing of NATS or SY.policy is left on any hive:** nothing on 4222, no directory, no
    section, no unit, no binary;
  - **the ingress recorded its 597 pending turns** before the broker was deleted;
  - **the metrics and the database wait work over the mesh;**
  - **with SY.storage held back,** the admin answered `ORCHESTRATOR_BOOTING` in 0.01 s during
    startup, instead of hanging;
  - **the cognition regression passes;**
  - **clean health.**

  Detail in `lab/DEPLOYMENTS.md`.
- **Stage 3, discussed with the operator (2026-10-17, doc §7.1):**
  - a tenant's node is paid for by its tenant; cognition, by the tenant of the communication's
    agent; the frontdesk, by the case's (*"OK todo"* ("OK, all of it"));
  - a call that its caller abandons stays `abandoned`, with no tokens (*"después vemos qué
    cambiamos si no sirve así"* ("later we'll see what we change if it doesn't work like this"));
  - Archi and the admin's executor pay with the root (*"cobranza será a raíz"* ("billing will go
    to root")). Each record stores which Archi work it belongs to: session, pipeline run and
    solution. That is the place for the customer's tenant, later on;
  - nothing is deleted for now; a general date-based policy for the database, later;
  - `ai_local_probe` is deleted;
  - the reads for Cloud, approved (*"en todo caso evolucionará después"* ("in any case it will
    evolve later")).
- **Stage 3, the code:**
  - **what it does:**
    - every call to a model leaves a record, charged to a tenant, in `llm_usage`;
    - the AI SDK writes it to the router's stream;
    - SY.storage checks it against identity;
    - the admin, Archi and Cloud read it.
  - **The code review,** with two views:
    - **HIGH:** the router compared the names of the `SY.*` nodes that charge other tenants
      case-sensitively, and SY.storage case-insensitively. A process with `sy.frontdesk.gov` from
      any hive could charge any tenant;
    - **MEDIUM:** a record written while the node's router was restarting was lost without being
      counted. Now the write waits until the node is connected;
    - **LOW, fixed:** Archi's failure classifier without a session, a finished pipeline named in
      the work of later turns, and the index of the per-tenant sync;
    - **LOW, stays with A-62:** the executor lets any node of the mesh read every tenant's usage,
      like any other action.
- **Stage 3, validated on 8.x (0.1.79),** with an agent in the test tenant `pruebas`:
  - **each node charges whoever it should:**
    - the agent, to `pruebas`;
    - cognition, to `pruebas` through the agent;
    - the frontdesk, to the case's tenant, or to the root as a fallback;
    - Archi, to the root with its session.
  - **The probes:**
    - a foreign tenant that was claimed ended up as a mismatch;
    - a foreign `SY.*` was rejected;
    - `sy.frontdesk.gov` in lowercase from worker1 was rejected by the router;
    - a node with no identity stayed unverified at 10 minutes.
  - **`abandoned`:** cognition with 50 ms left `abandoned` records with no tokens.
  - **Reads:** the operator, Archi and Cloud read them. Cloud sees only its tenant, and the root
    gives it `TENANT_ROOT_PROTECTED`.
  - **Health:** `usagelost` at 0. The only things rejected were the probes.
  - **Cleanup:** everything from the test was deleted, including the keys and the `pruebas`
    tenant.

  Detail in `lab/DEPLOYMENTS.md`.
- **Stages:**
  1. **1a:** the live bugs ✅;
  2. **1b:** the stream with SY.storage ✅;
  3. **2a:** cognition per communication, SY.policy retired ✅;
  4. **2b:** NATS deleted, and SY.policy gone completely ✅;
  5. **3:** LLM usage, recorded per call and charged to a tenant ✅.

### A-69 ✅ VALIDATED ON 8.x (0.1.73 and 0.1.74, 2026-10-16) — SY.cognition failed on every turn with both providers: it sent `temperature`

- **How it came up:** the operator asked to test their Anthropic key across the whole infra
  (2026-10-16).
  - Archi, the admin and the frontdesk worked with Claude: Archi answers and uses its tools.
  - SY.cognition failed when labeling a test turn, with Opus 5.5 and with Sonnet 5.5.
  - Back on OpenAI, it failed the same way.
- **What happened:**
  - **`temperature`:** the semantic labeler and the narrative summarizer sent it (0.1 and 0.2).
    The current models of both providers reject it:
    - Claude 5: "`temperature` is deprecated for this model";
    - gpt-5.5: "Unsupported parameter: 'temperature' is not supported with this model".

    Each turn was left without semantic derivation. Only a WARN and the
    `semantic_tagger_invalid_outputs_total` counter showed it, because the SDK reports the
    provider's 400 as a protocol error.
  - **The output caps** (256 and 220 tokens) were tight for a model that reasons before
    answering.
  - **The status lied after every startup:** it said `degraded_no_ai_provider` until its first
    turn or a key change, even though the key was there. It did not resolve it at startup, unlike
    Archi and the frontdesk. On worker1 it showed after every core-update.
- **Fix:**
  - no `temperature`, like its peers (Archi, the admin, ai.generic);
  - caps of 1024 tokens: the cap only bounds a runaway response, it does not change what is spent;
  - the key is resolved at startup, waiting for the vault like the rebuild from the database;
  - the secret-change log said "openai" hardcoded; now it names the key's type.
- **The 0.1.73 gate (with Claude) passed:**
  - cognition started ready on both hives, also on the freshly updated worker1;
  - a test turn came out labeled (`billing`, `duplicate charge`, `october invoice`…) and
    summarized.
- **The same turn with OpenAI found a second bug, in the SDK:**
  - Its tool-less path read the text from `output[0]`. A reasoning model (gpt-5.5) puts a
    `reasoning` item first, and the message comes after it: "missing text output".
  - Archi did not see it because its path with tools walks through all the items.
  - Fix for 0.1.74: one single reader for both paths. It takes `output_text` if the server sends
    it; if not, the first `message` item.
- **The 0.1.74 gate:** cognition worked with OpenAI (labeler and summarizer) and with Claude. With
  Opus 5.5, one summary out of three went past the 8 s timeout (`semantic_tagger.timeout_ms`): that
  turn is left without memory synthesis. Raising it or using Sonnet 5.5 is the operator's call.
- **What is left out (noted):**
  - the SDK reports a provider HTTP error as a protocol error, and that mixes two cognition
    counters. It goes with the AI SDK review;
  - `config-get` on SY.admin answers with its origin rejection (`CONFIG_GET_RESPONSE`) to a query
    from the admin itself. Not investigated.
  - **SY.cognition on a worker does not reach the database:** each hive has its embedded NATS, and
    SY.storage runs only on the motherbee. What a worker1 node publishes (its turns, what its
    cognition writes) stays in worker1's NATS, and its rebuild fails ("error connecting to
    server"). It predates this; it matters for stage 5 (where a worker node's consumption gets
    recorded).

### A-68 ✅ VALIDATED ON 8.x (0.1.70 to 0.1.72, 2026-10-16) — WF nodes were outside the standard: SY.wf-rules handled them directly with the orchestrator, and the Go node had its own control plane

- **How it came up:** the operator asked whether the WF node starts like the others (2026-10-15).
  The orchestrator starts it the same way: `systemd-run`, `config.json` with `_system`, the 60 s
  reconcile. What was different was around it. Design: `docs/wf-standard-path-v1.md`.
- **What happened:**
  - **Who launches it.**
    - SY.wf-rules sent SPAWN, RESTART, KILL and NODE_CONFIG_GET/SET directly to
      `SY.orchestrator@<hive>`, and NODE_STATUS_GET to the node, with the authority of rule (4) of
      the system policy.
    - It broke "every action goes through the admin".
    - Every WF node ended up with `launched_by: system`.
  - **The Go node.**
    - It read `_system` strictly and rewrote it from its struct: a new key from the orchestrator
      brought it down (it happened with `launched_by` in stage 4).
    - With a broken config or definition it exited, systemd relaunched it every 5 s, and the
      reason stayed only in journald.
    - It always answered HEALTHY.
    - Its CONFIG_SET errors carried `detail`, which the admin does not read.
    - It wrote an extra `config_version` at the root, with no atomic write and with 0644.
  - **The deploy from Archi was broken.**
    - The admin served the mesh's requests one at a time, and Archi's `wf_rules_*` waited for
      SY.wf-rules.
    - SY.wf-rules called the admin to publish the package, and that request stayed in the queue:
      at 5 s `PACKAGE_PUBLISH_FAILED`, with `current` already rotated.
    - Deleting failed the same way, and the executor's plans also ran in that loop.
    - Only the operator's HTTP worked, which serves each connection in its own task.
  - **`WF.orch.diag`.**
    - Rule (4) gave almost-admin authority to that name from the E2E tests.
    - A workflow called `orch.diag` would have been deployed with that name, and a workflow can
      send system messages.
- **Fix:**
  - **The admin serves each mesh command in its own task,** like the HTTP. The whole class of
    deadlock goes away: the 0.1.69 one, Archi's and the plans'.
  - **SY.wf-rules handles its nodes through the admin.**
    - It uses `get_node_config`, `set_node_config`, `run_node`, `restart_node`, `kill_node` and
      `get_node_status`.
    - The admin lets it act only on the `WF.*` nodes of its hive.
    - The only thing it asks the node directly is its running instances (WF_LIST_INSTANCES),
      which is a read.
  - **`launched_by`:** the admin records who asked for the deploy (`operator` over HTTP, `archi`)
    and SY.wf-rules carries it to the `run_node`.
  - **The policy:**
    - rule (4) stays only for SY.admin, with both wasm recompiled;
    - config-routes' authority markers and the table in `docs/history/04-routing.md` are brought up to
      date.
  - **The Go SDK has the managed nodes' control plane,** a port of the Rust one:
    - `_system` untouchable;
    - FAILED_CONFIG with its reason;
    - an atomic write that keeps the file's mode;
    - health according to the config's state.
  - **The WF node uses it,** and it validates the workflow definition as part of the config
    (`invalid_definition`). With a config it cannot use, it stays up, answers the control messages
    and drops workflow traffic until a restart applies a good config.
  - **A `wf_rules_*` query over the mesh without a hive** answers with an error instead of nothing.
- **Review (one round): 1 blocker and 7 to fix, all fixed before the build.**
  - **Blocker: a half-synced workflow package.**
    - Syncthing can bring the manifest and the `package.json` before the `flow/definition.json`.
      Before, the WF node crash-looped until it arrived, and it fixed itself; with the change it
      waited in FAILED_CONFIG and nobody retried it.
    - Now the orchestrator answers `RUNTIME_NOT_PRESENT` for a workflow package without its
      definition: in `run_node`, on every start and restart (the reconcile included) and before
      `set_node_config` binds the version. SY.wf-rules retries while it syncs.
  - **The runtimes manifest.**
    - With the admin serving in parallel, publishing and deleting versions could overwrite each
      other; a lock serializes them.
    - SY.wf-rules no longer writes the motherbee's dist: it deletes the old versions through the
      admin on every hive.
  - **The timings.**
    - SY.wf-rules gives each request a 35 s deadline; the admin waits 45 s for it, and Archi 60 s.
    - Before, Archi and the admin waited the same, 30 s, and a response close to the limit got
      lost.
  - **Who asks SY.wf-rules.**
    - It accepts operations only from the admin; before, any node could ask it to deploy or
      delete.
    - The admin accepts only `operator` or `archi` as its `launched_by`. Never `cloud`, which would
      give Cloud the management of the node.
  - **The content:** the admin requires the node's own `wf.<x>` runtime, the `wf.engine` base and
    a `package_path` of that package.
  - **The re-binding sends only the binding** (the orchestrator merges), instead of copying the
    node's config, which dragged along keys written by old nodes.
  - **SY.wf-rules reads a WF node's refusal correctly** (`node_not_configured`) and stops taking
    it as a timeout.
  - **Minor:**
    - a mesh command that fails before responding answers `INTERNAL_ERROR`;
    - the WF node's CONFIG_SET response carries its real state, unchanged until the restart, and
      `pending_restart`;
    - the SDK does not write over a `config.json` it cannot read.
- **The 0.1.70 gate on 8.x found two things, fixed for 0.1.71:**
  - **An update on worker1 did not wait long enough.**
    - The new package arrived more than 20 s after it was published.
    - The cause: just before, another workflow had been deleted. worker1's Syncthing failed to
      delete its directories ("not empty", parent before children), marked the runtimes folder as
      failed and retried it.
    - Now SY.wf-rules waits for the package up to 45 s, with a 60 s deadline per request; the
      admin waits 75 s and Archi 90 s.
  - **A deletion on worker1 left its packages on the motherbee.**
    - SY.wf-rules deletes the current version last, according to its copy of the manifest, which
      after a rollback was out of date. The admin rejected it (`RUNTIME_CURRENT_CONFLICT`) and the
      purge stopped there.
    - Now a version rejected as current is retried once the others are gone.
    - The test's orphaned packages were deleted by hand.
- **The 0.1.71 gate found that waiting longer was not enough, fixed for 0.1.72.**
  - **A first deploy on worker1 waited 45 s, and the package did not arrive.**
    - From the motherbee to the worker: the manifest arrived at 12 s and the package's files at
      62 s.
    - The motherbee's watcher batches changes. With the runtimes folder churning (the core-update
      just done, the previous E2E's deletions), it takes a while to announce them.
    - Deleting a workflow (and the version cleanup of a core-update) also makes the worker's
      folder fail once ("not empty") and wait before retrying.
  - **Fix:** on a worker, after publishing, SY.wf-rules asks the admin for a `sync_hint` on the
    motherbee's `fluxbee-dist-runtimes`.
    - The motherbee scans right away and announces the package. The worker pulls it as soon as
      the announcement arrives, even if it is waiting to retry a failed pull.
    - While the package is not there, it asks for the same scan before each retry.
    - Scanning on the worker does not help: Syncthing 2.0 does a pull after a scan only if it
      found local changes. The first version of the fix (`a053102`) did that, and it also waited
      up to 20 s for the worker's folder to settle, which does not happen after a failed delete.
      It was corrected before it was published.
    - It is an explicit, bounded step of the deploy.
- **The 0.1.72 gate passed everything,** in the worst case: right after the core-update, and
  worker1's E2E right after the deletion of the motherbee's E2E.
  - **The motherbee and worker1:** 35/35 each.
    - On worker1 the first deploy took 4 s: the package arrived 1 s after it was published.
    - The v2 arrived in 2 s, and the deletion left no packages.
  - **Through Archi:** deploy and deletion in 1 s; `launched_by: archi`; an unknown `_system` key
    stays in the file, and the node stays `HEALTHY`.
  - **Nothing from the test was left,** and the test tenant was purged.
- **What is left out (noted):**
  - **The admin's surface over the mesh** is still open to any node for almost every action: only
    IO.cloud and SY.wf-rules have a gate. A workflow does not reach it: the engine rejects every
    `SY.*` or `RT.*` destination when sending. It is A-62, parked along with A-60.
  - **All node actions go through the motherbee orchestrator's serial worker.** A long rollback or
    a hive that does not answer delays the others, a WF deploy too.
  - **WF configs written before 0.1.70** with `config_version` at the root (a direct CONFIG_SET)
    stay in FAILED_CONFIG with the reason. They are fixed with `set_node_config?replace=true` and a
    restart. In PROD there are no WF nodes.
  - **The OPA version counter** has no lock either. Only operator actions touch it.
  - **The health of IO nodes** ignores the config's state, and io.slack does not answer
    NODE_STATUS_GET. It goes to the IO family review.
  - **In the WF node,** opening the database and the recovery are still fatal.
  - **The diagnostic tools that used `WF.orch.diag`'s authority** lose it and go to the cleanup
    catalog:
    - `orch_system_diag` and the `scripts/orchestrator_*_e2e.sh` that use it;
    - `blob_sync_diag`, whose SYSTEM_SYNC_HINT was already denied.

### A-67 ✅ VALIDATED ON 8.x (0.1.68 and 0.1.69, 2026-10-15) — Cloud could not see or manage its nodes, nor suspend a tenant, and a stopped node came back by itself

- **The frame:** `docs/ilk-tenant-model-v1.md` §2.4 and D14, stage 4, decided with the operator on
  2026-10-15.
  - Cloud is the business layer: it associates companies with tenants and users with people's
    ILKs, and it manages credentials.
  - It launches IO nodes only to have somewhere to load the token: *"el nodo lo lanza porque sino el
    token no se puede cargar"* ("it launches the node because otherwise the token can't be
    loaded").
  - The infrastructure belongs to the operator, through Archi.
  - Everything goes through the admin.
- **What happened:**
  - **Nodes:** Cloud created nodes and could not see, stop or delete them. The linkedhelper runbook
    queried the status through the admin directly, which Cloud no longer has.
  - **Tenants:** Cloud could not suspend or reactivate a tenant, nor edit its sponsor or its
    settings. `get_tenant` only said whether it existed.
  - **Stopping did not last:** a kill without a purge stopped the process, and the relaunch every
    60 s brought it up again. It also happened to the operator.
  - **Who launched a node** was not recorded.
- **Fix:**
  - **Tenants:**
    - `update_tenant` accepts `status` (`active` or `suspended`), `settings` and
      `sponsor_tenant_id`;
    - `get_tenant` reads the full record through the admin, with `people_count` and `node_count`;
    - `list_tenants` brings all of them except the genesis, with the deleted ones marked.
  - **Nodes:**
    - the admin records who launched each node (`_system.launched_by`: `cloud`, `archi`,
      `operator`);
    - Cloud starts, stops and deletes only the IO nodes it launched in its tenant. The admin puts
      that condition on the request and the node's hive checks it; for anything else the answer is
      `NODE_NOT_FOUND`;
    - `list_nodes` gathers the tenant's nodes from every hive in parallel, and `get_node_status`
      gives the status of one;
    - Cloud sees the status, not how the node runs: no process, unit or paths.
  - **Stopping lasts, for everyone:** stopping removes the relaunch mark, and starting puts it back.
    A node that crashes on its own keeps being relaunched.
  - **The word:** Cloud's contract says tenant, never company (85 places).
  - **`a5f1b2a`** goes out in this release: the `set_ilk_tenant` rejection answers with the code
    only.
- **Review (one round): 3 major, 4 minor and details, fixed before the build.**
  - **Major 1:** every WF node launched with this version crash-looped. The Go WF node reads the
    `_system` block strictly and did not know `launched_by`. It learned it, and a Rust test pins
    the block's keys against the Go one. The build in progress was stopped.
  - **Major 2:** with a spoke on an old core, Cloud could stop or delete other people's nodes, and
    `list_nodes` handed it the whole router inventory of that spoke. Now the motherbee checks the
    condition before forwarding, reading the node's config there, and `list_nodes` filters by
    tenant and marks the hive as outdated.
  - **Major 3:** stopping a node in a crash loop (waiting for systemd's next restart) answered
    "stopped" without stopping it. Now that path also stops the unit.
  - **Minor:**
    - the relaunch re-reads the mark after taking the node's lock;
    - the start, stop and delete responses no longer show the unit or paths;
    - a node's status carries `launched_by` and `relaunch_on_boot`;
    - the actions that Cloud shares stay open to the other callers on the mesh (the lab's
      diagnostic nodes had been left out);
    - the lab's restart script brings the node down like a restart, not with a stop.
  - **Documented:** nodes launched before 0.1.68 are not managed by Cloud; the `launched_by` of a
    direct request to the orchestrator is taken as it comes (A-22).
- **Found in the 0.1.68 gate (8.x, 2026-10-16), fixed for 0.1.69:**
  - **`delete_node` from Cloud got stuck.** The admin serves the commands arriving over the mesh
    one at a time. The kill with purge that Cloud asks for waits for the orchestrator, and the
    orchestrator, to clean up, asks the admin to delete the node's identity and keys; that request
    stays queued behind it. Result: a timeout for Cloud (`ADMIN_UNAVAILABLE`), the identity marked
    without being purged (the name reserved) and the node's own key deleted only when the admin
    freed up. It did not happen to the operator: their requests arrive over HTTP, each in its own
    task. Now the admin runs the commands that call it back (node kill and remove) in their own
    task, and IO.cloud gives `delete_node` 25 s.
  - **An SY from another hive reading a genesis key while identity restarts** (the deploy) got
    `UNAUTHORIZED` once: the vault only knows the SYs of its own hive by heart, and the others
    through the SHM. Now it answers `IDENTITY_UNAVAILABLE`, which is retried, without giving away
    anything extra. It predates this; in this deploy it hit `SY.cognition@worker1`, which read
    fine 42 s later.
- **Validated on 8.x:**
  - **0.1.68:** tenants through Cloud (settings, sponsor, reads, `pending` and genesis rejected);
    its nodes (`list_nodes` with only the status, `NODE_NOT_FOUND` for a node of another tenant
    and for one the operator launched in the same tenant); stopping lasts longer than one relaunch
    pass; suspending through Cloud cuts off new nodes and credentials; `a5f1b2a` answers with the
    code only; a test workflow starts clean.
  - **0.1.69:** `delete_node` answers ok, the node's own key goes away, the assigned one stays and
    the name is freed; on the restart, the SY from another hive got `IDENTITY_UNAVAILABLE` instead
    of `UNAUTHORIZED`.

  Detail in `lab/DEPLOYMENTS.md`.

### A-66 ✅ VALIDATED ON 8.x (0.1.67, 2026-10-15) — A suspended tenant kept starting nodes, and deleting a tenant marked its people

- **The frame:** `docs/ilk-tenant-model-v1.md` §2.3, stage 3. The operator's decisions of
  2026-10-14: *"no quiero borrar ni desaparecer ilk de humanos"* ("I don't want to delete human
  ILKs or make them disappear"); *"sobre nodos y recursos sí vale la pena actuar"* ("on nodes and
  resources it is worth acting"); restoring means reactivating the tenant.
- **What happened:**
  - **Nodes:** the orchestrator started and relaunched nodes of a suspended or pending tenant.
    Since stage 2 they got no secrets, but they ran.
  - **Deleting a tenant** cascade-marked the people who had written to it through a channel
    (2026-09-28), and it was rejected if it had Cloud accounts (`TENANT_HAS_ACCOUNTS`, stage 1). A
    person deleted on their own did not come back while their tenant was deleted.
  - **Purging a tenant** deleted every person charged to it.
  - **`set_ilk_tenant`** moved only complete accounts: a company's contacts could not be taken out
    of there.
  - **io.api `by_ilk`:** a company's API acted only on people charged to that company. A person
    who wrote to it but was charged to another one could not be used.
- **Fix:**
  - **Orchestrator:**
    - `run_node`, start and restart answer `TENANT_NOT_ACTIVE` if the node's tenant is pending or
      suspended;
    - the relaunch at startup and the periodic one skip it, without relaunching it or moving it to
      another version, until the tenant is active again;
    - a node that is already running is not killed;
    - if the hive does not know the state (identity still starting), it carries on as before and
      says so in the log.
  - **SY.identity:**
    - deleting a tenant marks only the tenant itself, with no cascade, and answers
      `people_charged`; it is rejected if the tenant has nodes or sponsors tenants;
    - restoring it brings it back as it was;
    - purging it is rejected while it has people charged to it (`TENANT_HAS_PEOPLE`); the ones
      marked on their own go with it;
    - `set_ilk_tenant` also moves temporaries, but only out of a deleted tenant: the frontdesk
      registers a temporary in the case of the tenant it wrote to;
    - `restore_ilk` brings a person back even if their tenant is deleted;
    - the v4 migration unmarks, on every startup, whoever an old cascade marked (their id stays in
      the log).
  - **io.api `by_ilk` (D12):** people charged to the tenant or with a channel in its space.
  - **Admin and Cloud:** `TENANT_HAS_PEOPLE` and `TENANT_NOT_ACTIVE` are 409; the texts of the
    actions and Cloud's catalog.
- **Review (one round): 2 major, 2 minor, 4 details.**
  - **Major 1:** the people an old cascade had marked (up to 0.1.66) went away with the purge as if
    they had been deleted on their own, and restoring their tenant no longer brought them back.
    Fixed with the v4 migration. In PROD there were none.
  - **Major 2:** moving a temporary out of an active tenant cut off its registration: the frontdesk
    rejects the case if the person's tenant is not the one of the IO serving them
    (`tenant_mismatch`). Now a temporary is moved only out of a deleted tenant.
  - **Also fixed:**
    - the relaunch reads the tenant from the config if it is missing from the map, like start and
      restart;
    - `start_node` of a node that is already running answers `already_running`;
    - a temporary merged into another ILK does not count as a person of the tenant;
    - the HANDBOOK and io.cloud texts that described the cascade;
    - a test that pins the order: the check comes before registering, forwarding, stopping or
      starting.
- **No code, documented:**
  - the router does not cut off a suspended tenant's traffic: it would be a lookup on every
    message, on the busiest path, and it adds little to the above;
  - a workflow deployed on a suspended tenant: wf-rules writes the new version into the config,
    the restart is rejected and wf-rules reports `restart_failed`. On reactivation nothing restarts
    it, so it carries on with the old package until a restart;
  - a node running in a suspended tenant during an upgrade stays on the old version. If it
    crashes, systemd cannot relaunch it (its directory is gone), and the periodic relaunch brings
    it up on the new one once the tenant is active again.
- **Open (D13):** joining a contact to an account, for example a customer of A who becomes the
  owner of B, needs an explicit step (the person proves the channel from their account). It is not
  designed.
- **Validated on 8.x (0.1.67):**
  - **suspended:** the restart of its node gives `TENANT_NOT_ACTIVE` and the node keeps running
    without secrets; stopped, two passes do not relaunch it; reactivated, the next pass relaunches
    it;
  - **deleted:** nobody gets marked, people still resolve, a deleted person comes back with the
    tenant still deleted, and restoring it leaves it as it was;
  - **purge:** it is rejected first because of the key and then because of the people, and it
    goes through once they are moved (a temporary included, out of the deleted tenant);
  - **found:** the rejection of moving a temporary arrived as `CODE|detail` in `error_code`; fixed
    in `a5f1b2a`, it goes out with the next release.

  Detail in `lab/DEPLOYMENTS.md`.

### A-65 ✅ VALIDATED ON 8.x (0.1.67, 2026-10-15) · the residual is P-1, tracked in A-22 — The vault believed the ILK the node itself declared

- **What happened:** SY.vault authorized by `meta.src_ilk`, which the node itself sets. The router
  did not bind it to the sender. A compromised node could:
  - claim another node's ILK and read its secrets, own or assigned;
  - claim SY.admin's and write or delete any secret.

  It was noted as phase 0 of io.web (`docs/history/io-web-spec-beta-v1.md` §21).
- **The operator's decision (2026-10-14):** fix it now.
- **Fix:** the vault binds the ILK to the sender name that the router stamps
  (`routing.src_l2_name`). It compares them exactly: no lowercasing, no trimming of spaces.
  - **A system node** proves the ILK derived from its name, without the SHM, so the core reads its
    keys at startup.
  - **Any other node:** it has to be the ILK that identity registered for its name.
  - **Someone else's ILK, or a request with no stamped name:** `UNAUTHORIZED`.
  - **An ILK the SHM does not have:** an error that gets retried (`IDENTITY_UNAVAILABLE`), because
    a degraded identity startup writes only the system ones.

  The refusal is answered and audited, never dropped.
- **What it closes:**
  - putting another ILK in `meta.src_ilk` is no longer enough, and a subject that io.api forwards
    does not read secrets;
  - between hives, the name is bound to the hive's certificate (EDGE-01): a node from another hive
    cannot be `SY.admin@motherbee`.
- **What it does not close:** within the same hive, the name is declared by the process in its
  HELLO, and the router accepts it (P-1, see A-22). A local process can open its own connection
  with the name of another node of that hive and use its ILK; on the motherbee, SY.admin's. It is
  the hole P-1 already leaves for everything else, not a new one, and the nodes run as root
  (A-60).
- **Reviewed and not built:** two cheap checks in the router, which remain for P-1:
  - rejecting a local HELLO with a foreign `@hive`: it adds little while any name of one's own
    hive can be chosen;
  - rejecting a name that already has a live connection: it can block the restart of a node whose
    old socket has not closed yet.
- **The router does not need it:** in a user message, `src_ilk` is the person, not the node.
- **Before the change:** every (node, ILK) pair that the vault's audit had ever seen was reviewed
  in PROD. All the SYs pass with their derived ILK; the IO/AI ones that show up were test nodes
  already purged.
- **Validated on 8.x (0.1.67):** after the deploy, all the SYs read their keys without a single
  rejection, also those of other hives; the admin requesting with IO.cloud's ILK got
  `UNAUTHORIZED` (audited), and with its own it passed; an IO.slack node read with its own ILK.

### A-64 ✅ VALIDATED ON 8.x (0.1.66, 2026-10-14) — Secrets were shared only per tenant, a suspended tenant kept handing them out, and any node could be born in the genesis

- **The frame:** `docs/ilk-tenant-model-v1.md` §2.2, stage 2. The operator decided it on 2026-10-14:
  *"tenant no es identidad ni seguridad ... el secret se guarda con el tenant para filtrar el uso del
  que paga"* ("tenant is not identity or security ... the secret is stored with the tenant to filter
  the usage of whoever pays").
  - A secret's tenant is who pays, and the only thing the vault checks is that it is active.
  - Who uses it:
    - its owner (a node, or a person);
    - the assigned nodes;
    - or, if it has neither, every node of its tenant: the general secret.
  - A node is born and dies in its tenant.
  - Only the core is born in the genesis.
- **What happened:**
  - **Sharing:** a secret belonged to one node or to the whole tenant; sharing it among some was
    not possible.
  - **Tenant state:** it was not checked. A suspended or deleted tenant kept handing out its
    secrets.
  - **The root as a fallback:** A-35 promised that a company's AI node fell back to the root's key.
    The vault never allowed it (only the SYs read it). The promise lived in the SDK, in ai.generic,
    in Archi's handbook, in an admin note and in the firstboot banner.
  - **The genesis:** the orchestrator rejected only IO nodes there. An AI, a WF or an `RT.*` with
    any runtime could be launched in the genesis, and from there read the Postgres URL.
  - **A node's tenant:** a temporary could still take another tenant when registering, and a
    replica could push the change.
  - **Unfinished sign-ups:** one from Cloud could not be told apart from someone who wrote through a
    channel (D11, already resolved in `80b479c`).
- **Fix:**
  - **SY.vault:**
    - a list of assigned nodes in the secret (`assigned_ilks`);
    - `VAULT_ASSIGN`, which changes that list without touching the value;
    - the active-tenant check;
    - the change notice reaches whoever had the secret and whoever has it now (the previous owner
      too), and the whole tenant if it is or was general.
  - **SDK:** the resolution looks first for an own one, then an assigned one, then the tenant's
    general one; never the root's.
  - **SY.admin:**
    - `vault_assign` (over HTTP and over the mesh);
    - the nodes of `vault_put` and `vault_assign`, which have to be charged to the secret's tenant
      and never be a person.
  - **IO.cloud:** `nodes` in `put_token` and `assign_token`.
  - **Orchestrator:** the definition of the core (SY.\*, RT.\* with an `rt.*` runtime, the core's
    IO), with which it rejects everything else in the genesis.
  - **SY.identity:** a node's tenant does not change, neither through registration nor through a
    replica.
- **Review (one round):** 1 major and 9 minor.
  - **The major:** refreshing a secret's value without naming nodes made it general and gave it to
    the whole tenant. It happened with every token refresh from Cloud. Now a new value keeps who
    uses it.
  - **Fixed:**
    - a tenant the SHM does not have is answered with an error that gets retried, so an identity
      outage does not leave the nodes without credentials;
    - an invalid tenant id is rejected;
    - the previous owner gets the notice;
    - `RT.*` only passes in the genesis with an `rt.*` runtime;
    - the edge no longer restarts its public door because of an `assign` of its TLS key;
    - `nodes: null` no longer unassigns everything;
    - the texts that promised the root;
    - tests of the validation of assigned nodes and of the decision about the tenant.
  - **Documented, no code:**
    - a deleted node stays on the lists, because removing it could make a secret general on its
      own;
    - io.linkedhelper re-reads its credential only at startup and on CONFIG_SET;
    - the vault trusted the `src_ilk` that the node itself declared: A-65 (0.1.67) closes it
      between hives and for the header; within the same hive, P-1 remains.
- **Validated on 8.x (0.1.66):** the vault's audit shows what each node reads. With four IO.slack
  nodes from two companies:
  - with the key assigned to two nodes, only those two read it;
  - when a node is taken off it, that one is out at once;
  - as general, all of the company's nodes read it and none of the other's;
  - with the company suspended, nobody; reactivated, they come back;
  - a person's secret is read by its assigned node even after the person is moved to another
    company;
  - the genesis rejects an AI node;
  - the core read its keys after the restart.

  Detail in `lab/DEPLOYMENTS.md`.

### A-63 ✅ VALIDATED ON 8.x (0.1.65, 2026-10-14) — Identity depended on the tenant: duplicated people, keys that drifted and merges across tenants

- **The frame:** `docs/ilk-tenant-model-v1.md`. The operator, Claude and Astra decided it on
  2026-10-14: the ILK is the actor and is never duplicated, the tenant is a cost center and
  permissions are explicit.
- **What happened:**
  - A channel's key was `(channel, address, ILK's tenant)`, always recalculated from the ILK's
    current tenant. So:
    - the same person in two companies was two ILKs;
    - re-registering a temporary in another tenant left the primary's lookup out of date until a
      restart;
    - a merge stored the moved channels with one tenant in memory and another one in the database.
  - `merge_from_ilk_id` did not check the tenant.
  - Cloud's `delete_ilk` could mark node ILKs.
  - Deleting a tenant marked every person charged to it.
- **Fix (stage 1):**
  - **The channel's scope:**
    - each channel stores its `scope`, fixed when it is created;
    - every key uses it: memory, database, SHM and the fast path;
    - the SHM's `IlkEntry` carries the cost center;
    - there is no layout change.
  - **One Cloud account per email:**
    - the `cloud` channel lives in the installation's scope, and only IO.cloud writes it;
    - `register_human` returns the existing account (`existing: true`);
    - an abandoned temporary is recharged to the tenant that asks, with history;
    - `ACCOUNT_AMBIGUOUS` and `ACCOUNT_IN_ROOT_TENANT` cover the old data.
  - **`ILK_SET_TENANT`:**
    - only complete people, with history in the same transaction;
    - the admin stamps whom the change is for;
    - registration no longer changes a person's tenant.
  - **Merges** within the same tenant.
  - **Cloud** deletes and restores only people, and reads by id with no tenant filter.
  - **Deleting a tenant** with accounts charged to it gives `TENANT_HAS_ACCOUNTS`; the rest still
    cascades.
  - **v3 migration in the schema:** before loading the memory, it moves the unambiguous accounts and
    reports the rest in the log.
- **Design review (one round):** 7 major findings and 5 minor, all incorporated into the design
  before writing the code.
- **Code review (one round):** 1 major and 8 minor.
  - **The major:** purging a tenant that a person had left always failed. The FK from their
    channels to the tenant was still alive. Now the purge takes along the channels left in that
    scope (`dropped_channels`).
  - **Fixed:**
    - **A rejected replica push:** it is no longer taken as a removal.
    - **The `cloud` channel and the installation's scope**, also closed through the replica
      (snapshot and delta), through `ILK_UPDATE` and through the merge from an IO node of the
      tenant.
    - **Recharging a temporary:** only with IO.cloud and in the shared scope.
    - **A node's own channel** does not answer a person sign-up.
    - **A foreign `ich_id` is rejected** (`ICH_ID_CONFLICT`).
    - **An ambiguous email:** it is also ambiguous if another ILK has it in the installation's
      scope. It is checked before searching.
    - **The restore of an old account** goes back to the installation's scope if the key is free.
    - **An unfinished sign-up neither blocks nor gets marked** when its tenant is deleted
      (`pending_signups`).
    - **Tests:** the admin's `on_behalf_of` stamp, plus 9 in identity.
  - **Documented, no code:** the version skew between the core and io.cloud during the upgrade. An
    old core rejects `scope`, and a new admin rejects an old `delete_ilk` without `ilk_type`. They
    are the seconds of the A-39 rebind. The v3 SQL is validated on 8.x with data made in 0.1.64.
- **Validated on 8.x (0.1.65):** old data made with 0.1.64 through Cloud's door. The migration
  moved what was unambiguous and reported what was ambiguous. 30 checks went through the public
  door. On the operator's side:
  - the history;
  - worker1's SHM;
  - the purge of the tenant a person had left (`dropped_channels=1`);
  - a restart of sy-identity, which resolved the ambiguous email live.

  Detail in `lab/DEPLOYMENTS.md`.
- **Pending for the next stages:** reassigning nodes (with the vault's permissions, stage 2), and
  the tenant's state as a gate and the rest of D8 (stage 3).

### A-62 ✅ VALIDATED ON 8.x (0.1.83, 2026-10-22) — Any node on the mesh can ask the admin for any action

- **Resolved (0.1.83, [control-path-hardening-v1](../../docs/control-path-hardening-v1.md) §2):**
  SY.admin admits a mesh caller only by its allowlist (`mesh_access`, `authorize_mesh_caller`);
  a caller on an ingress or egress hive gets nothing, roles come from the registry and fail
  closed. Live: a node on ingress1 and a `WF.*` node on worker1 were refused; Archi, IO.cloud,
  SY.wf-rules (a deploy) and the orchestrator's `teardown_node` were admitted. What root on a
  spoke can still do is the design's §9.

- **What happens (seen in the code on 2026-10-07):** the admin's internal gateway (`ADMIN_COMMAND`)
  does not look at who is calling, except for a few actions with their own gate: Cloud's relay
  (`authorize_cloud_relay`), `externalize` and `publish_artifact`. The rest (`add_hive`,
  `vault_get`, `kill_node`, `opa_*`, `publish_cloud_endpoint`, which issues a new Cloud bearer and
  returns it) it runs for any node connected to the router. It has been like that by design since
  the internal gateway (`docs/history/02-protocolo.md` §7.10: AI/WF/IO callers).
- **Why it is not urgent:** the tenants' logic does not reach it. A WF cannot target `SY.*`
  (`go/nodes/wf/wf-generic/node/actions.go`), and ai.generic only sends `publish_artifact`, with
  the action fixed. It takes a compromised node process, and the nodes already run as root (A-60):
  whoever compromises one is already root on the host.
- **When to look at it:** together with A-60 (nodes without root). That is when who can ask for
  what starts to matter. Likely form: a list per role prefix, with the system actions only for
  `SY.*`.
- **Since 0.1.79 (A-70, stage 3):** over the mesh, `get_llm_usage` is requested only by Archi and
  IO.cloud, but the executor (`executor_execute_plan`) runs any plan as the operator for any node.
  A one-step plan reads the LLM usage of every tenant. The stage 3 code review found it, and it
  stays here: reading usage changes nothing about what the executor already allows.

### A-61 ✅ RESOLVED (0.1.64, validated on 8.x) — Cloud could overwrite another tenant's keys and read identities of any tenant

- **What happened (review of the Cloud team's cards, 2026-10-07):**
  - `put_token` overwrote an existing key of **any** tenant, infrastructure ones too (the AI
    provider's key): SY.vault did not look at the tenant when writing. With the root tenant it was
    even "its own".
  - `get_ilk_details` by id read the ILK of any tenant, and the fast reads took the tenant from
    `params`.
  - `edge_fanout_verify:<ich>` was not among the reserved keys.
- **Fix (approved by the operator, 2026-10-13: *"primero io.cloud"* ("io.cloud first")):**
  - **SY.vault, for every caller:** a put of a key that belongs to another tenant is `KEY_TAKEN`
    (even if the value is the same), and it never moves it. Delete, rotate and roll back require
    `tenant_id`, and another tenant's key answers `KEY_NOT_FOUND`. The admin sets the root if none
    comes, just like in the put; the orchestrator passes each key's tenant when it cleans up a
    node's secrets; Archi's plans have the field.
  - **SY.identity:** ILK_GET accepts `tenant_id` and answers `NOT_FOUND` for an ILK of another
    tenant. `update_tenant` rejects the root (`TENANT_ROOT_PROTECTED`; it is rewritten on every
    startup) and a name or domain of another tenant (`TENANT_NAME_TAKEN` /
    `TENANT_DOMAIN_TAKEN`), and it validates everything before changing anything (before, a bad
    `status` left the new name set).
  - **IO.cloud (rule 3):** the tenant goes only in the root of the envelope (one in `params` is
    rejected), it is never the root, and everything the op touches belongs to that tenant. New
    operations `list_tokens`, `delete_token` and `update_tenant`. Every error carries
    `error_code`.
  - **SY.admin:** it enforces all of that again on the server side for `IO.cloud`
    (`enforce_cloud_relay_content`).
- **What almost broke:** adding actions to Cloud's surface means that, over the mesh, only io.cloud
  can request them (it already happened with `delete_ilk` on 2026-09-28). `delete_ilk`,
  `vault_list` and `vault_delete` were specifically left open to the orchestrator (cleanup of a
  node), and the whole surface to `SY.architect@motherbee`. Over the mesh, Archi was already
  getting its `run_node`, `get_ilk` and `delete_ilk` commands rejected.
- **Adversarial review (one round, 2026-10-13):** one MAJOR: the .deb's factory reset deleted the
  secrets without naming the tenant, so with the new rule it left the tenants' secrets behind and
  then could not purge them (TENANT_HAS_SECRETS). Now it deletes each key in its tenant. Six minor
  ones fixed:
  - a key of another tenant reads the same as a nonexistent one (same code and same text; only the
    audit says "denied");
  - Cloud's tenant is accepted only in its canonical form (the other spellings of the root's UUID
    passed the exact check);
  - the Cloud emulator no longer uses the root by default, and its test deletes its key;
  - `KEY_TAKEN` and the tenant `*_TAKEN` codes answer 409, `TENANT_ROOT_PROTECTED` 403;
  - two doc blocks that were out of place;
  - a sentence in the doc.
- **Out of scope (the operator's decision):** a person in several companies and parent/child
  tenants go to the identity talk. Restricting Cloud's `delete_ilk`/`restore_ilk` to human ILKs
  was left as a question: the operator hesitated, because tokens are assigned to the node's ILK.
  That last part does not change the ILK, only the secret's metadata.
- **Contract:** `docs/io-cloud-api.md`, rewritten to the real state (it was at 3 operations and
  configuration through `.env`).
- **Validated on 8.x (0.1.64, 2026-10-13):** 53 checks through the real public door with Cloud's
  bearer, plus the ones on the operator's side (the node purge deleted its secret in its tenant,
  the HTTP `DELETE` with and without a tenant, Archi over the mesh), all PASS (ledger 0.1.64).
  Minor pending item: the admin's HTTP maps `TENANT_NOT_FOUND` to 500.

### A-60 🟡 PARKED (to evaluate, 2026-10-07; no design yet) — The managed nodes run as root

- **What happens:** the orchestrator launches the nodes (IO, AI, WF) with `systemd-run` without
  `User=`, so they run as root on every host. Seen in PROD on 2026-10-06 (read-only): `io-blob`,
  `io-cloud` and `sy-wf-rules` run as root. A compromised node is root on its host, and everything
  it writes ends up owned by root (hence A-56).
- **What 0.1.63 does (the minimal step, approved):** the nodes stay as root, but the SDK hands each
  blob over to the owner of the folder where it leaves it, and the postinst's `chown -R` goes away.
- **What has to be evaluated (operator: *"deja documentado el tema de evaluar correr los nodos como
  fluxbee de usuario"* ("leave the topic of evaluating running the nodes as the fluxbee user
  documented")):** running them as `fluxbee`, or with one user per node. First we have to see
  what they use today that only root can reach. On the motherbee, on 2026-10-06:
  - the SHM regions (`/dev/shm/jsr-*`: router, config, identity, lsa, memory, opa) are
    `root:root 0600`, and the SDK reads them for the ILK lookups, the config and the policy;
  - the router's socket is `0666`, but the `irp-*.sock` is `root:root 0600`;
  - `state/nodes/` is `root:root 0755`, with each node's `.uuid` owned by root;
  - `dist/runtimes` is `root:fluxbee 0755` (readable).
  We also have to measure what each base node (io.api, io.blob, io.cloud, io.slack, io.wapp,
  ai.generic, wf.engine, io.linkedhelper) does with the filesystem and the network.
- **Status:** to evaluate after 0.1.63; it has no design yet.

### A-59 ✅ RESOLVED (0.1.62, validated on 8.x) — The identity replica waited forever on a subscription cut without a FIN

- **What happened (DTAP panel round 7, P7-1, confirmed in the code):** a replica's delta
  subscription is a long-poll. The replica only writes to confirm a delta, it read with no timeout
  and the socket has no keepalive. If a stateful firewall stops tracking the connection, it drops
  the following deltas and also the FIN, and the replica never finds out. It happens with a table
  loaded after the connection is opened (the first apply of stage 4, or a join where sy-identity
  starts before the posture) or after 5 days without traffic (the conntrack timeout). Additions,
  removals and revocations stop reaching that hive until its sy-identity is restarted.
- **Fix:** the primary writes a heartbeat (`IDENTITY_SYNC_HEARTBEAT`, with no seq and no ack) when
  20 s go by without sending anything. The replica cuts the subscription after 60 s without
  hearing anything, says so in the log ("no frame from the primary for 60s (heartbeats stopped)")
  and subscribes again from a full snapshot. It is the same pattern as the peers: the WAN sends an
  LSA every 10 s and the identity publication reconciles every 30 s.
- **Tests:** three tests with real sockets: the primary beats and keeps delivering deltas; the
  replica cuts a silent stream; and the two together stay subscribed. Without the heartbeat two
  fail, and without the timeout one fails.
- **From the adversarial review (two more fixes; they predated this):**
  - a delta could be lost silently when a subscription was opened: the primary could build the
    snapshot before registering the subscriber, and a delta broadcast in between was in neither of
    the two. Now the handler queues the registration before responding, and the main loop
    registers what is queued before building each snapshot. With P7-1, re-subscriptions are more
    frequent, which is why it matters;
  - dead subscriptions were cleaned up only when a change was broadcast; now also when a new one is
    registered, so the cap of 256 counts only the live ones.
  - Rolling the motherbee back from 0.1.62 while leaving the spokes up makes the worker
    re-subscribe every 60 s: the HANDBOOK ("The rollback") asks to roll back the spokes first.
- **Seen in PROD before the fix (0.1.61):** worker1's subscription had gone 15,929 s without a
  packet in either direction; the publication channel, every 30 s.
- **Validated (2026-10-06, ledger 0.1.62):**
  - On worker1, the subscription went from 15,929 s without a packet to a `lastrcv` of 8, 3 and
    18.5 s.
  - A 100 s cut of what arrives from :9100: the replica reported it 60 s after the last heartbeat,
    and it re-subscribed 5 s after the cut was lifted.
  - A worker joined from scratch (worker2) also receives heartbeats.

### A-58 ✅ RESOLVED (0.1.63, validated on 8.x) — On the workers and the egress, Syncthing ran as root

- **What happens (seen on 2026-10-06 in PROD, read-only):** worker1 and egress1 do not have the
  `fluxbee` user, so `resolve_syncthing_service_user` falls back to root: Syncthing and its whole
  `dist/` belong to root. The ingress bootstrap creates the user (`useradd`); the worker and egress
  ones do not (`worker_remote_base_dirs_command`).
- **Impact:** a compromised Syncthing on those hosts is already root. Since stage 3 it listens only
  on loopback and its only peer is the motherbee, so the motherbee would have to be compromised
  first.
- **Fix (approved by the operator, 0.1.63):**
  - The orchestrator creates the `fluxbee` user at startup, in any role. Syncthing always runs as
    `fluxbee`: the root fallback and the `blob.sync.service_user` and
    `blob.sync.allow_root_fallback` options are gone.
  - On a spoke that runs it as root today, the change is a visible, one-time step: the log says
    "Syncthing moves to its own user (A-58)", Syncthing stops, its home, its blob folders and the
    spoke's `dist/` go to `fluxbee` (`chown -R` with the service stopped, so it does not keep
    creating root files), the new unit is written and it starts.
  - The home becomes `fluxbee`'s whenever Syncthing runs; before, the egress (which only syncs
    dist) never had it.
  - What the adversarial review added:
    - the binary is installed before anything is stopped: a vendor that cannot be installed leaves
      the Syncthing that delivers it running;
    - the change is decided by the unit, by the home's files and by the process's user, so a
      change left half done runs again;
    - it leaves a `syncthing_user_switched` alert, only once Syncthing is healthy as `fluxbee`;
    - `useradd` uses a `fluxbee` group that already exists;
    - second round:
      - the process's uid only counts when `/proc/<pid>/comm` is `syncthing`, because while
        systemd is starting it the process still runs as root;
      - `daemon-reload` on the change, so it does not bring up the old unit definition.
  - Rolling a box back to 0.1.62 leaves Syncthing as `fluxbee`, because the user already exists.
    But the 0.1.62 nodes write root blobs that this Syncthing cannot read. To bring it back to
    root: stop `fluxbee-syncthing`, `userdel fluxbee` and restart `sy-orchestrator` (HANDBOOK,
    "The rollback").
- **Remaining:** the ingress bootstrap still creates the user and does its `chown`. It is
  redundant, but it is not touched until an ingress join is validated (the ingress branch of the
  A-48 gate).
- **Validated (2026-10-06, ledger 0.1.63):**
  - worker1 and egress1 created `fluxbee` and switched once (`syncthing_user_switched` alert with
    the service healthy);
  - their folders stayed `idle` with no errors;
  - a blob written on worker1 reached the motherbee in ~12 s, and one from the motherbee reached
    worker1 in ~10 s;
  - the motherbee and the ingress were not touched.

### A-56 ✅ RESOLVED (0.1.63, validated on 8.x) — Root wrote by path in the Syncthing user's `blob/`

- **What happens (third adversarial review of A-51, 2026-10-06; verified step by step in the code,
  not run end to end):** on the motherbee, `blob/` and `blob/staging` belong to the Syncthing user,
  and root processes write there by path: the blob SDK (`fs::write`, chmod by path,
  `ensure_dir_mode_0750`) from io.blob, the IO and AI nodes and sy_architect.
  - The name in staging is predictable (`staging/<hash16[..2]>/<name>_<hash16>.<ext>`). That user
    can plant a symlink there, and root writes a third party's bytes (for example, an incoming
    attachment) wherever it points.
  - io.blob does a chmod 0750 of a planted `blob/public` on every startup.
- **What it also hid (seen on 2026-10-06):** the nodes run as root (A-60) and the SDK left each
  blob `root:root 0640`. On the motherbee, Syncthing runs as `fluxbee`, so a new blob could not be
  read or synced. Nobody noticed because the postinst did `chown -R fluxbee blob/` on every
  install.
- **Fix (approved by the operator, 0.1.63):**
  - Owners: `blob/` is `root:fluxbee 0750`; `staging/` belongs only to root, the whole tree, with
    no links inside; `active/` and `public/` belong to `fluxbee`. The orchestrator leaves it like
    that on every startup.
  - SDK (`blob::safe_fs`): staging is written with an exclusive temporary and a rename by
    descriptor. `promote` opens `active/<prefix>` without following links, hands the blob over to
    the owner of `active/` with `fchown` on the descriptor and does a `renameat`.
  - io.blob: the public copy is written the same way inside `blob/public`, with the folder's owner,
    an exclusive temporary and `linkat`. It no longer does a chmod by path, nor does it change the
    folder's mode at startup.
  - The postinst's `chown -R` and the `blob` ones in `install.sh` were deleted.
- **What the two adversarial reviews added (same step):**
  - **Reading, which predated this:** `resolve` handed out a path that io.slack, the AI SDK, the
    architect and `io_common` read with `fs::read`. Syncthing replicates symlinks, so a
    compromised worker could make the motherbee read a root file and send it to Slack or to an
    LLM. Now:
    - `open_blob` opens the whole chain without following links and only accepts a regular file of
      the `BlobRef`'s size;
    - `read_blob` also verifies that the SHA-256 starts with the name's hash;
    - the four readers and io.blob's curator use this;
    - `resolve` stays only to know where the blob lives.
  - **`promote`:**
    - it only considers a regular file promoted; a link planted with the name is replaced, never
      followed;
    - the blob in staging has to be a regular file with a single link, from this process;
    - opening does not block on a FIFO;
    - after the move, the inode that arrived is verified: a concurrent `put` could change the file
      between the handover and the `renameat`, and a root blob was left behind.
  - **Temporaries:**
    - the staging GC deletes the ones a crash left;
    - io.blob deletes at startup the `.curate-*` files left in `public/`.
  - **`blob/agent-assets`:** it becomes root-only. It had stayed owned by the Syncthing user
    because of the old `chown -R`, and the architect and ai.generic write and read there by path:
    that user could change an agent's system prompt.
  - **What root already left in `active/` and `public/`:**
    - on every startup, after the rebind, it is handed over to the Syncthing user (`chown -R`,
      which walks by descriptor), and an alert says how many entries there were. It only runs with
      `fs.protected_hardlinks=1`;
    - in addition, the watchdog alerts when Syncthing reports per-file errors in the blob folders.
      A file it cannot read never makes the folder "unhealthy".
  - **Markers:** the `.stfolder` files take their folder's owner.
- **Second round of the reviews (same step):**
  - **Concurrency on the same blob:** a concurrent `put` + `promote` of the same blob no longer
    gives a false integrity error. A staging file that another `put` replaced (with no links) or
    that another `promote` already handed over is accepted, and if another `promote` already moved
    it, it is Ok. There is a test with 4 threads.
  - **Bounded read:** `read_blob` reads at most the `BlobRef`'s size plus one byte, even if the
    file grows while it is being read.
  - **Async:** the reads run in the blocking pool (`read_blob_async`) for callers from async code.
  - **ai.generic:** it verifies that each asset in `agent-assets` has the SHA-256 of its name. If
    someone edited it before the first startup of 0.1.63, it is not loaded.
  - **The startup handover:** it tolerates races (`-ignore_readdir_race`, `-execdir chown -h`). A
    recount judges it, and it alerts per folder with what was left.
  - **`protected_hardlinks`:** the pass that returns staging and `agent-assets` to root also
    requires `fs.protected_hardlinks=1`.
- **Tests:**
  - no planted link is followed, not in staging, not in `active/<prefix>`, not in `public/`;
  - `read_blob` rejects a link, a different size, different bytes and a FIFO, without hanging;
  - the blob takes the group of `active/` (a secondary group of the test user, so it can be seen
    without root);
  - the GC deletes temporaries;
  - io.blob cleans up `.curate-*`.
- **Against the previous code:** according to the review, the link tests in staging and in the
  `active/` prefix failed, and so did the group one. The `public/` one also failed, but only
  because of the temporary it left.
- **Validated (2026-10-06, ledger 0.1.63):**
  - the new owners are in place on the 4 hosts, with 0 foreign entries;
  - the ingress handed over its `.stfolder`;
  - a blob written as root with the new SDK ended up `fluxbee:fluxbee 0640` and crossed between
    worker1 and the motherbee in both directions.
- **Residue:** the `active/` GC still lists, measures and deletes by path. If someone swaps a
  prefix folder for a link between the listing and the deletion, root could delete blob-shaped
  names outside. It ships off by default (`gc.enabled` and `gc.apply` at `false`); it moves to
  descriptors before it is turned on.

### A-57 ✅ RESOLVED (0.1.62, validated on 8.x) — `update category=vendor` installed the new Syncthing but did not restart it, and answered that it had

- **What happened (verified in the code on 2026-10-06):** the vendor update called
  `ensure_blob_sync_runtime`, which reinstalled the binary if the hash changed and then did
  `systemctl start`, which does nothing if the service is already running. The running Syncthing
  kept the old binary until the next restart, and the response said
  `restarted: [fluxbee-syncthing]` anyway.
- **Fix:** installing the binary and writing the unit now say whether they changed anything (the
  unit had the same problem: `daemon-reload` without a restart). If the service is running and
  either of the two changed, or if its process runs a deleted binary (`/proc/<pid>/exe` ends in
  `(deleted)`, the same check the core uses), it is restarted at that moment, before the health
  checks, which already use the new unit's port and home. Reading it from the process makes a
  restart that was lost because of an error in between happen on the next call (adversarial
  review, F1). The response says what happened: `updated: [syncthing]` only with a new binary,
  `restarted` only if there was a restart. Tests of the rule and of the response.
- **Validated (2026-10-06, ledger 0.1.62):**
  - With no new binary, the update answers `unchanged` and with no restart, and no host restarted
    Syncthing when the new orchestrator started.
  - With the binary replaced under the running process (an identical copy with another inode), the
    update restarted Syncthing and said so (`restarted: [fluxbee-syncthing]`). The next one did not
    restart.
  - Only the case of a vendor with new bytes is missing, and it follows the same path.

### A-54 ✅ RESOLVED (0.1.61, validated on 8.x) — The motherbee used SSH outside `add_hive`

- **What happens (verified in the code on 2026-10-06):** all the SSH lives inside the three
  `add_hive` flows, with one exception. On every motherbee startup, `reconcile_hive_tls_material`
  logs into each `connected` spoke over SSH to see whether it has its mesh certificate and, if it
  is missing, reissues it.
  - In PROD the four spokes joined in `revoke` mode: those attempts fail authentication and are
    discarded at debug level. It is noise on every startup.
  - With the optional `ssh_access=key_only_persist` mode (it leaves one SSH key per spoke in the
    vault after the join) it does get in. P6-2 of round 6 showed the risk: only an authentication
    failure cuts the attempt, so a machine that takes the spoke's IP and accepts any key receives a
    new leaf and its private key.
- **The operator's decision (D32, 2026-10-06):** *"no deberíamos tener comunicaciones NUNCA por ssh a
  no ser cuando se hace un add_hive y es totalmente temporal"* ("we should NEVER have
  communications over ssh, except when an add_hive is done, and it is completely temporary"). The
  startup reconciliation and the `key_only_persist` mode are deleted (no PROD spoke uses it). A
  spoke that loses its certificate joins again (`remove_hive` + `add_hive`), like a new machine.
- **Fix (0.1.61, one adversarial review):** the reconciliation and the mode were deleted; a request
  with `ssh_access` is rejected (`INVALID_REQUEST`). Sending the leaf became fatal for the join
  (`TLS_PUSH_FAILED`, retryable if it was an SSH cut): now the join is the only one that issues it.
  Each join's revoke also deletes a `fluxbee-spoke-recovery` key the old mode may have left, and
  the factory reset no longer preserves `ssh:*` secrets. A guard test walks the code from every use
  of ssh/scp/sftp/rsync/sshpass/ssh-keyscan up to `accept_add_hive` and fails with the chain if
  anything else reaches it (tested with probes: from startup, `sh -c "ssh …"`, `Command::new`
  without a literal, relaunching the join from `main`).
- **On the record:** a join that fails in a retryable way leaves the bootstrap key on purpose (so
  that the retry gets in by key); if the operator abandons it, the key and the sudoers entry stay
  on the box until they complete it or remove them through the console. A box joined at some point
  with `key_only_persist` (DEV only, July) loses its recovery key on its next join; the vault's
  `ssh:<hive>` secret is deleted with `vault_delete` or with a factory reset.
- **Validated (2026-10-06):** zero SSH attempts from the motherbee to the spokes since the deploy
  (before, one per startup); a new join completed with the fatal leaf push and the revoke (ledger
  0.1.61).
- **Follow-up (0.1.62, from the D32 review):**
  - each flow issues the leaf before its first SSH session: if the CA cannot, the join fails with
    `TLS_ISSUE_FAILED` before installing anything on the box (the failure path's revoke still
    tries to get in, as in any non-retryable failure);
  - the revoke deletes the `fluxbee-spoke-recovery` key only if that name is the line's comment
    (before, any line that mentioned it), with a test that runs the filter with bash;
  - the guard test also covers ssh-copy-id and autossh, looks for the program in command position
    inside a shell (after `sudo -n -u x`, `timeout 5`, `env A=b`, `bash -lc`, `su -c`,
    `runuser`, `systemd-run`, `if`, `{`, with the source's escaped quotes), rejects renaming
    `Command` and code after `mod tests`, and requires `accept_add_hive` to have a single caller,
    `handle_admin`. Tested with seven probes that make it fail and one control that does not;
  - along the way, a worker's join fails with `VENDOR_SYNC_FAILED` if the vendor does not reach
    it, as egress and ingress already did: without vendor it never gets Syncthing, and nothing
    repairs it.

### A-55 🟡 PARKED BY DECISION (D31) · a switch since 0.1.83 (`egress.ssh_from_wan: false`) — The egress's SSH door

- **What it is:** the operator keeps SSH (tcp 22) open on `egress.wan_iface` for operation and
  maintenance during this development. In PROD it is 192.168.8.240, an internal network behind a
  firewall (*"no es la NSA pero es red segura"* ("it's not the NSA, but it's a secure network")).
  Nothing else gets in through there.
- **Risk:** the code cannot tell a secure office network apart from the internet. If a customer
  connects the egress's WAN directly to the internet, that door is left open to the internet.
- **Status:** close it before a customer's first installation (a code change in a release; the
  design has it in the declaration of the egress role, §3.2).

### A-19 ✅ RESOLVED (0.1.43) — The published policy waited up to 60 s for the Syncthing watcher

- **What happened:** with 0.1.41 the apply reached the 4 hives in 16 s, but the clear took 63 s,
  and the admin answered with the three spokes pending. The motherbee's Syncthing events show it:
  the two applies were scanned 10 s after publishing (`fsWatcherDelayS`), the clear 60 s after.
  The watcher holds back the changes that are renames and deletions until its timeout (6 × 10 s),
  and the publication writes by rename. With 0.1.42 it also held back an apply: at 33 s only the
  motherbee was running v8.
- **Fix:** after applying on the motherbee, the admin sends the motherbee's orchestrator a sync
  hint for `fluxbee-dist-policy`, which scans the folder right then (the same step as a runtime's
  publish), and only then the notice (0.1.42). In 0.1.42 the orchestrator rejected it (invalid
  `folder_id` for the `dist` channel: it only accepted `fluxbee-dist`, even though the handler
  already knew how to honor a specific folder) → 0.1.43 accepts any `fluxbee-dist*`.

### A-20 ✅ VALIDATED ON 8.x (0.1.83, 2026-10-22: the direction) — With CONFIG_CHANGED crossing hives, its receivers applied any origin the router admitted

- **Resolved (0.1.83, design §3):** rule (3) admits orchestrator mutations only from
  `SY.orchestrator@motherbee` and reads also from the same hive; rule (4) never admits
  `ADD_HIVE_FINALIZE`/`REMOVE_HIVE_CLEANUP` on the motherbee; the orchestrator answers admin
  messages only from its own hive's SY.admin. Live: a SPAWN from ingress1 was dropped and an
  admin read to the motherbee's orchestrator answered FORBIDDEN; spawns on worker1 still work.

- **What happened:** the router's gate for CONFIG_CHANGED admits the primary admin **and the
  orchestrators** (rule 3 of the system policy). SY.opa.rules applied a compile/apply/clear from
  any of them, and what is applied on the motherbee is now published to every hive.
  SY.config.routes applied the routes/VPN/taps list of a global CONFIG_CHANGED, whose only sender
  was `PUT /config/*` (removed in 0.1.41): now it would reach every hive and overwrite their
  config.
- **It is not a new capability:** rule 3 already lets an orchestrator write the policy or the
  routes of any hive through `CONFIG_SET`. The orchestrator does not use it: `CONFIG_SET`/
  `CONFIG_GET` are sent only by SY.admin; the orchestrator forwards between hives
  SPAWN/KILL/NODE_CONFIG_*/NODE_STATUS/LIST_NODES/GET_*/ROLLBACK/ADD_HIVE_FINALIZE/
  REMOVE_HIVE_CLEANUP and sends CONFIG_CHANGED `node_config` only to nodes of its hive.
  **Decided (operator, 2026-10-01): shrink it** → 0.1.45: rule 3 is the list of the 16 actions
  the orchestrator forwards, and the config signals that nobody used were deleted (`node_config` +
  `notify`, storage's `CONFIG_RESPONSE`, sy-wf-rules' handler, io-slack's branch). Validated: a WF
  cycle on worker1 and reads across hives without a single gate rejection.
- **What it does NOT close (DTAP panel, D-2):** the list has no direction. The orchestrator of any
  hive, the ingress's included, can still send another hive the list's mutations
  (`NODE_CONFIG_SET`, `SPAWN`/`KILL`/`RESTART`/`REMOVE`, `SYSTEM_CORE_ROLLBACK`, finalize/cleanup).
  And as long as the HELLO name is not authenticated (A-22), any local process can present itself
  as an orchestrator. Deferred with the security package (A-22).
- **Fix:** SY.opa.rules takes CONFIG_CHANGED only from `SY.admin@motherbee`, just like its command
  path and the protocol; SY.config.routes no longer applies lists that arrive through
  CONFIG_CHANGED.

### A-21 ✅ RESOLVED (0.1.50) — Router status without a user policy: `error` at startup, `ok` after a clear

- **What is seen:** in `/hives/{h}/opa/status`, a router that started without a policy reports
  `load_status=1, error`; after a clear, `load_status=0, ok`. In both cases there is no policy and
  the routing is the same (a message with no destination that does not go to the frontdesk comes
  out as `OPA_ERROR`).
- **Cause:** the resolver is born in `OPA_STATUS_ERROR`, and `unload()` leaves it in
  `OPA_STATUS_OK`. It is only informational.
- **Fix (0.1.50):** the resolver is born in the same state a clear leaves (`ok`, no policy).

### A-17 ✅ RESOLVED (0.1.39) — An identity replica that starts without a primary deletes its hive's ILKs

- **What happened:** the replica re-publishes its set of own ILKs every 30 s, and the primary takes
  it as authoritative (it deletes the hive's ILKs that do not come in it). If the worker starts
  with the primary unreachable, in memory it only has its system ILKs: when the connection came
  back it published that, and the primary **deleted the ILKs of the hive's nodes**. Seen on 8.x:
  the primary went from 25 to 24 ILKs, and `AI.vaultprobe@worker1` was left without an identity
  (`ILK_NOT_FOUND` when killing it). It is the case of a full reboot in which a worker comes up
  before the motherbee.
- **Fix:** the own set is not published until it has a base from the primary (the startup full
  sync or the subscription's first snapshot). Loose local deltas still go out.
- **Validated:** the same degraded startup → the replica logs `holding its self-owned snapshot`;
  the primary stays at 25 ILKs, with the probe's.

### A-18 ✅ CLOSED (0.1.50) — config-routes: static routes, VPN and taps

- **The need (operator, 2026-09-30):** static routes are rarely used and can stay local. The VPN
  is going to be used globally (separating tenants, security: a worker1 node talks to a worker2
  one). Taps are the most used tool (echo to other nodes) and have to be global. Decided not to put
  them in OPA: they are fixed in config-routes.
- **How it is today (verified in code, 2026-10-01):**

  | Tool | Loaded | Reaches the rest | Global? |
  |---|---|---|---|
  | Static routes | on one hive | LSA → FIB of every router | yes |
  | Taps | on one hive | LSA; the origin router applies the local ones and the LSA ones, only once | yes |
  | VPN | on one hive | LSA carries them, but `assign_vpn` uses only the local config | **no** |

- **The operator's decision (2026-10-01): the VPN stays per hive.** Each router assigns it with its
  hive's rules; for a VPN between hives, the same rule is loaded on each one (documented in
  `06-regiones.md` §5.1). With that, nothing is left open; *"resolvé los detalles por
  costo/beneficio... prefiero empezar lo próximo con estos temas cerrados"* ("resolve the details
  by cost/benefit... I'd rather start the next thing with these topics closed").
- **Closed in 0.1.50 (`aa206ac`):**
  - One single surface: `/hives/{h}/routes|vpns|taps`. `/routes`, `/vpns` and `/taps` with
    `?hive=` were deleted (nobody used them).
  - Rule 5 of `system.rego` no longer names `SY.config-routes`.
  - The router re-reads the routing config on every heartbeat, with version control (panel D-12):
    a router with no local nodes is no longer left with an old config. Today it did not apply (one
    router per hive).
- **Accepted:**
  - If the hive where a route or a tap was loaded gets cut off, it stops being applied on the
    others during the cut.
  - Taps match by exact name (v1).
  - On a host with several routers, the peer router does not do the taps fanout (same case as
    D-12).
- **Already resolved:**
  - `PUT /config/*`: 0.1.40–0.1.41.
  - The broadcast after each add, and the router's special treatment of CONFIG_CHANGED: 0.1.44.
  - Storage's broadcast: 0.1.41; its CONFIG_RESPONSE: 0.1.45.
  - Hive-scoped config and origin control: 0.1.36 (A-8, A-9).
  - Cross-hive taps with a destination by UUID: July (`18c4dd8`).
- **Accepted:** if a hive is deleted or rebuilt, its routes, taps and VPN are lost (nobody keeps a
  copy).

### A-16 ✅ RESOLVED (0.1.56) — The receive-only mirrors pile up local changes silently

- **What happens:** in a receive-only folder, Syncthing neither propagates nor reverts local
  changes. If something touches the mirror on a spoke, it diverges from the motherbee forever and
  nobody finds out. The orchestrator manages Syncthing but does not look at or revert these
  changes.
- **To discuss:** have the orchestrator revert (`/rest/db/revert`) the local changes of the
  receive-only folders and log it, so that the mirror converges to the origin by itself.
- **Fix (`10c97b1`):**
  - The watchdog, which already read each folder's state on every round, also reads
    `receiveOnlyTotalItems`.
  - If a receive-only folder has local changes, it leaves a WARN and reverts it.
  - It covers the spokes' `dist/` mirrors and the ingress's blob `public/`.
  - On 2026-10-02, the 12 receive-only folders in PROD were at 0.
- **Validated live (0.1.56, 2026-10-03, with the operator's OK):**
  - A test file in worker1's `dist/vendor` raised `receiveOnlyTotalItems` to 1 at 11 s.
  - The watchdog left the WARN and reverted the folder.
  - At 16 s the file was gone, and the counter was back at 0.

---

## B. Infrastructure and tooling — no product code change required

### B-1 🟢 LAB NOTE — The Proxmox API does not allow writing cloud-init *snippets*

- Neither `POST /nodes/{n}/storage/{s}/upload` nor `download-url` accepts `content=snippets`
  (enum: `iso, vztmpl, import`), even though the storage **does** support the type.
- **Impact:** **a template cannot be built 100% through the API**; the operator has to drop the
  file in place once. It is the Level-3 limit of `METHOD.md` §2 in action.
- **Mitigation applied:** the operator wrote it just once; the template ended up with the agent
  baked in, and **the clones no longer depend on the snippet**.

### B-2 🟢 LAB NOTE — `cicustom` replaces Proxmox's user-data (it overrides `ciuser`/`cipassword`)

- When `cicustom=user=...` is used, Proxmox uses that file **instead of** the user-data it
  generates, which is the one that creates the user. Result: the user **is not created**, and
  `add_hive`'s bootstrap would have no one to log in as.
- **Rule for the handbook:** *either* `cicustom` *or* `ciuser`/`cipassword`; **they do not
  coexist**.

### B-3 🟢 The Ubuntu cloud image is qcow2 even though it is published as `.img`

- `download-url` rejects `filename=*.img` (`invalid filename or wrong extension`). It is downloaded
  by renaming it to `.qcow2`.

### B-4 🟢 Ubuntu 24.04 uses **socket activation** for sshd

- `ssh.socket` active, listening on `:22`, `ssh.service` inactive. **It is normal** and does not
  affect `add_hive` (which only needs port 22 answering). Noted so it is not misdiagnosed.

### B-5 🟢 VM operations in Proxmox are serialized by a *lock*

- Launching `resize`/`start` right after a `qmcreate` that imports a large image fails with
  `can't lock file … got timeout`. **Our own mistake, made and corrected.**
- **Rule:** chain operations by **task state** (`/tasks/<UPID>/status` → `stopped OK`), never by
  `sleep`.

### B-7 🟢 The Proxmox guest-agent runs **without `HOME`** defined

- **What happens:** `agent/exec` runs the command without `HOME` in the environment. With
  `bash -lc`, that makes `/root/.profile` evaluate `. "$HOME/.cargo/env"` as `. "/.cargo/env"` →
  an error on **every** command.
- **Real impact (not cosmetic):** that noise on `stderr` **polluted the output of every command**
  and **broke two of our own pollers** that extracted numbers from the output (the message contains
  `line 10`). Two false positives: the problem **was not the VM, it was the helper**.
- **Solution:** invoke `/usr/bin/env HOME=/root /bin/bash -lc '<cmd>'` in the guest-agent helper.
- **Rule for the handbook:** the guest-agent channel **is not a normal login shell** — define
  `HOME` explicitly, and **never parse numbers from an output that can carry stderr**.

### B-8 🟢 Cloning as one user and building as another breaks the Go build (`buildvcs`)

- **What happened:** the repo was cloned as `fluxops` (to use the deploy key), but `build-deb.sh`
  runs as `root`. Git rejects the foreign repo (`fatal: detected dubious ownership in repository`)
  and the Go step fails when stamping the VCS information:
  ```
  error obtaining VCS status: exit status 128
      Use -buildvcs=false to disable VCS stamping.
  ```
  The build got as far as `[2/5] build go` and exited with `rc=1` **without producing a `.deb`** —
  after ~55 min of an already successful Rust compilation.
- **Cause:** our own setup mistake (two different users to clone and to build), **not** a fluxbee
  problem.
- **Solution applied (git's standard one, not a shortcut):**
  `git config --global --add safe.directory /opt/fluxbee` for root.
  *(The `-buildvcs=false` alternative would have hidden the problem and lost the version stamping
  in the Go binaries: it was discarded.)*
- **Rule for the handbook:** **the same user who clones must build**, or declare the repo as
  `safe.directory`. Check it **before** launching a long build.

### B-9 🟢 Proxmox on VMware: **the portgroup security breaks ALL bridged networking of the nested VMs**

- **Symptom:** the VMs with a card on `vmbr0`/`vmbr1` **could not even ARP** their gateway
  (`ARP INCOMPLETE`, 100% packet loss), even though the Proxmox **host** worked perfectly on the
  **same bridge**, with correct IP/routing/carrier on the VM's side.
- **Cause:** the Proxmox is a VMware VM. The portgroups come by default with
  **`Forged transmits: Reject`** (+ `MAC address changes: Reject`, `Promiscuous mode: Reject`),
  which **drops every frame whose source MAC is not the one assigned to the Proxmox's vNIC**. The
  nested VMs transmit with **their own** MAC (`BC:24:11:…`) → VMware throws them away.
- **What made the diagnosis conclusive** (and ruled out fluxbee, the firewall and the
  configuration):

  | Observation | Explanation |
  |---|---|
  | ✅ the Proxmox host works | it uses **its** assigned MAC |
  | ✅ the worker goes out to the internet through `fbint`+SNAT | **routed** traffic → goes out with the **host's MAC** |
  | ❌ a VM on `vmbr0` cannot even ARP | **bridged** traffic → **the VM's MAC** → dropped |

  That is: **the internal SDN network worked precisely because it is routing with NAT, not
  bridging.** That asymmetry was the clue that closed the case.
- **Solution (operator, in ESXi, live and per portgroup):**
  `Promiscuous mode: Accept` · `MAC address changes: Accept` · **`Forged transmits: Accept`** ← the
  one that enables **sending**.
- **Costly operational detail:** there is **one portgroup per physical card**. When it was applied
  only to `nic0`'s, `vmbr0` started working and `vmbr1` **was still broken** — same symptom,
  another card. **It has to be applied to ALL the portgroups** of the nested hypervisor's NICs.
- **Verified after the change:** ingress with **its own public IP `71.182.182.80`, visible from
  the internet** + an internal leg to the mesh, at the same time. Egress going out through its WAN
  leg.
- **For the handbook:** on any prod on VMware, **this is configured BEFORE deploying**; otherwise
  hours are lost diagnosing a network that is flawless on the guest's side.

### B-6 🟢 `build-deb.sh` could produce a truncated `.deb` without failing

- With the disk full, `dpkg-deb` exited with code 0 but wrote a ~1.8 KB package without
  `data.tar`. **Already fixed** (commit `01db2cc`): a space preflight + an integrity check of the
  `.deb` with a loud failure.

### B-10 🟢 The apt repo died with every reboot and looked empty during every publish

- **What happened (two defects in `scripts/apt-repo-publish.sh`):**
  1. `--serve` brought up the server with `systemd-run` → a **transient** unit: it died with every
     reboot of the build box, and its errors were swallowed (`>/dev/null 2>&1 || true`), so a
     re-run could leave the repo down **without warning** (it happened in August, after a VM110
     reboot).
  2. The index was regenerated *in place* (`dpkg-scanpackages > Packages`): during the re-hash of
     every `.deb` (~5.5 min with 33 × ~240 MB) the repo served **0 packages** — a client that ran
     `apt-get update` in that window saw the repo empty.
- **Evidence:** `FragmentPath=/run/systemd/transient/fluxbee-apt.service`; polls during a publish:
  0 packages from 14:47 to 14:52 (logbook 2026-09-25).
- **Solution:** `--serve` writes a **persistent, enabled** unit (idempotent; it replaces an old
  transient one and fails loudly if it does not start); the index is built in `.new` and goes in
  through atomic renames (`Release` last); the `.deb` is copied as `.partial` + `mv`. The
  one-liner prints all the box's IPs.
- **Validated in prod:** VM110 reboot → repo up on its own; a real publish with 32 samples from a
  client → minimum 33 packages.

### B-11 🟢 A character above U+00FF in `agent/exec` jams the guest-agent — it was the "flakiness under load"

- **What happens:** the Proxmox API (Perl) does not encode the `guest-exec` arguments with wide
  characters: a single `—` (U+2014), `→` or `✓` in the command leaves the **qemu-guest-agent
  jammed** — even `guest-ping` times out ("QEMU guest agent is not running") until the agent is
  restarted. Latin-1 characters (`é`, `ñ`, `¿`) go through. `agent/file-write` with `encode=1`
  fails for the same reason (`Wide character in subroutine entry`, without jamming anything). In
  addition, the output comes back with mojibake (UTF-8 bytes read as Latin-1: `—` → `â€”`).
- **Evidence (controlled A/B, VM110):** with the agent healthy, `exec echo acento-é` → OK and the
  agent stays alive; `exec echo guion—largo` → `guest-exec failed - got timeout`, and the next
  `ping` dies. Load (host cpu 0%), disk (34%) and kernel (6.8.0-142 runs fine) were ruled out.
- **Impact:** it explains the "flaky qga" episodes of the whole August campaign (the agent's
  commands carried `—`, `→`, `✓` in the `echo`s) — **the cause was not the load**. Each one cost
  reboots/resets of prod VMs.
- **Solution (in `lab/pve.py`):** a non-ASCII argv travels in base64, and an ASCII bootstrap
  decodes it and runs the exact original argv; `push` encodes the bytes locally (`encode=0`, which
  also works for binaries); the output is de-mojibaked. Along the way, the **B-7** solution
  (`env HOME=/root`), which was not in the helper, was applied. Validated live: accents/`—`/`✓`,
  exit codes propagated, the agent survives.

### B-12 🟢 All the PROD VMs had a pending reboot for kernel + libc6 — applied

- **What was observed:** `unattended-upgrades` installed kernels 6.8.0-137…142 and libc6 on the
  VMs; until they reboot, they stay on the old kernel (fb-egress: runs 6.8.0-136,
  `reboot-required`). Today's VM110 reboot activated **6.8.0-142** without problems (guest-agent,
  network and repo OK).
- **Resolution (2026-09-25; the operator delegated the infra: "hacé lo que creas conveniente" ("do
  whatever you think is best")):**
  - Reboot one VM at a time, with a prior snapshot `pre-kernel-reboot-20260925`, in the order
    egress → worker1 → ingress → mb.
  - All 4 ended up on **6.8.0-142**, without `reboot-required`, with identical IPs and routes and
    0 `failed` units. Mesh 4/4, 9/9 runtimes, public 200.
  - The spokes withstood the hub going down without restarting anything.
  - The boot-time load of the egress's NAT (F17) was validated in PROD: the ruleset came back
    identical.
  - Detail in the 2026-09-25 logbook.

### B-13 ✅ RESOLVED (2026-10-02) — The PROD VMs did not start on their own after a host reboot (`onboot` not defined)

- **What happens:** `onboot` is not defined on fb-mb, fb-worker1, fb-ingress, fb-egress or
  fb-build. If the host reboots (power cut, update), **PROD stays powered off** until someone
  starts the VMs by hand.
- **Evidence:** it happened on 2026-07-30 ("the 4 VMs were left powered off. I started the 4 VMs").
  The HANDBOOK recorded it as "hypervisor reboot: validated", but what was validated was that the
  mesh re-forms, not that the VMs start on their own.
- **Proposed solution:** `onboot=1` on 100, 101, 102, 103 and 110 (fb-build, because of the
  decision to keep it always on). No start order, because the simultaneous start of the 4 is what
  was validated on 30/07. `PUT /nodes/pve/qemu/<id>/config onboot=1`, or in the GUI: VM → Options
  → *Start at boot*.
- **Status:** resolved. The operator set `onboot=1` on 100, 101, 102, 103 and 110; verified
  through the API on 2026-10-02. It remains to be seen live on the next host reboot.

### B-14 🟡 LAB ONLY, OPEN — Slow boot of the PROD VMs (userspace from 53 s to 1 min 48 s)

- **What was observed (reboots of 2026-09-25):** userspace of ingress 53 s, egress 60 s, worker1
  89 s, mb 108 s (fb-build: 31 s).
- **What it is not:**
  - It is not the journal flush, even though `systemd-analyze blame` puts
    `systemd-journal-flush.service` at the top (mb 74 s, worker1 58 s): journald reports that the
    flush itself took ~1 s (mb: 1.4 s for 752 entries).
  - Nor is it the size of the journal: mb 1.8 G; egress 109 M and it took 27 s; fb-build 112 M and
    0.6 s.
- **Clue:** in mb's log, unrelated jobs (flush, apparmor, binfmt) finish at the same instant
  (94.6 s). Something common holds them back.
- **Impact:** it adds 1 to 1.5 min to every recovery. With mb it is ~3 min 20 s from the reboot to
  the 9 runtimes; do not diagnose before that.
- **Untested candidate:** the serial console (`console=ttyS0` with `serial0=socket` and
  `vga=serial0`).
- **Pending:** a diagnosis (`systemd-analyze plot`, what holds those jobs back) before touching
  anything.

### B-15 ✅ RESOLVED (2026-10-02) — Memory headroom of the PROD host with fb-build always on

- **What was observed:** 26.0 G configured in the powered-on VMs over 27.4 G physical, with no
  ballooning. Real usage stable at ~22 G (a peak of 81% during the week, 0 swap). mb reaches 5.8 G
  of its 10 G; fb-build, 7.6 G of 8 G.
- **Risk:** if mb were to use its 10 G while fb-build compiles, the host runs out of headroom, and
  the first thing to go down is a QEMU process.
- **Options (the operator's decision):**
  - Ballooning with a minimum on fb-build (e.g. `balloon=2048`): Proxmox reclaims memory from it
    when the host goes over 80%.
  - Lowering its RAM outside of builds.
- **Status:** resolved. The operator enabled ballooning on fb-build with a 2 GiB minimum
  (`balloon=2048`); verified through the API on 2026-10-02. To give the host more RAM it has to be
  shut down, because it is a VMware VM.

### B-16 ✅ RESOLVED (2026-10-03) — `ops.py deploy` reported spokes as updated when they were not

- **What happened (0.1.56 deploy):**
  - The core update fired a few seconds after the motherbee's admin came back, before its router
    had the spokes back. The three requests failed with `TRANSPORT_ERROR` and none arrived: no
    spoke service was restarted.
  - Even so, the deploy said "worker1 on 0.1.56". `ops.py` judged by `core.manifest_hash` and the
    component versions in `/versions`, which on a spoke are what dist brought. Syncthing updates
    them before any update. What is installed is in `core.installed`, and it was still on the
    0.1.55 hash.
- **How it was seen:** the binaries check after the deploy showed `not_restarted` on every unit of
  the spokes.
- **Fix (`lab/ops.py`):**
  - `core_of` uses the installed hash when the hive reports it, just like the `/versions`
    comparison (X1). `ops versions` says where each hash comes from.
  - `deploy` waits for each spoke's orchestrator to answer before the update, and retries a
    request that did not arrive up to 3 times.
  - The update was sent again by hand, and the three spokes finished fine (U-8b validated).
- **To consider:** in a spoke's `/versions`, `core.manifest_hash` and the component versions
  describe what is available, not what is running. The fleet comparison already solves it, but
  read on its own it is confusing.

---

## How this document is used

1. During the deployment: **findings are added, not fixed**.
2. At the end: section **A** is reviewed with the operator, and the **code-change plan** comes out
   of it (what changes, why, in what order, and what stays as a design decision). That plan goes
   to [`PENDING-BUGS.md`](PENDING-BUGS.md), which is where the state of each task is tracked.
3. Section **B** feeds `HANDBOOK.md` (recipes) and, where it applies, the infra scripts.
