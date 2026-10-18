# sy.orchestrator audit - fresh look

Date: 2026-06-30
Repository: this one (`fluxbee`)
Branch: `daily_onworking_coa`
Commit reviewed: `25e883e`
Focus: `src/bin/sy_orchestrator.rs` and the immediate router/policy/identity boundaries that affect security or operability.

## Executive summary

`sy.orchestrator` improved clearly compared with the 2026-06-23 audit: there is an origin gate for mutating admin actions; `hive_id` is validated before destructive paths are touched; `add_hive/remove_hive` stopped using SSH as an operational fallback when the worker should already answer over the socket; `write_remote_file` no longer interpolates contents into a remote shell; and egress validates interface names before they reach YAML/nft/shell.

The new code surface, however, introduces significant risks in the lifecycle of dynamic nodes and in hive bootstrap:

- The `unit` field is not tied to the ownership of a managed node. `KILL_NODE` can stop arbitrary systemd units, and `SPAWN_NODE` can register/configure nodes against a unit chosen by the caller.
- `add_hive` leaves bootstrap SSH access with root capability in place by default, and also during partial failures that happen before the hardening step.
- `run_node` mutates `SY.identity` and the local mapping before checking whether a local config already exists and before confirming that `systemd-run` worked. This leaves partial states that are hard to reconcile.
- There is no per-node lock to serialize `run_node/start/restart/kill/remove/config_set`; the atomic config file is not enough to protect the effects on identity, systemd and timers.
- `SYSTEM` authority is centralized in the router, but `sy.orchestrator` keeps no local defensive guard for protected actions in case a future/bypass/test delivery path avoids that filter.

Local validation run:

- `cargo test --bin sy_orchestrator`: OK, 106 tests passed, 0 failed. The build emitted pre-existing `dead_code`/`unused` warnings, with no failures.

## Findings

### SO-2026-06-30-01 - High - `unit` allows controlling systemd units outside the managed node

Status: Open

Evidence:

- `run_node_flow` accepts `payload.unit`, sanitizes it, but uses it as the final unit if it is present (`src/bin/sy_orchestrator.rs:12518-12523`).
- The spawn command uses that value in `systemd-run --unit ...` (`src/bin/sy_orchestrator.rs:14157-14189`).
- `kill_node_flow` allows omitting `node_name` and using `payload.unit`; it too only sanitizes characters (`src/bin/sy_orchestrator.rs:13197-13244`).
- It then runs `systemctl kill/stop/reset-failed {unit}` (`src/bin/sy_orchestrator.rs:13264-13270`, `src/bin/sy_orchestrator.rs:13339-13340`).
- `KILL_NODE` and `SPAWN_NODE` are in the table of protected `SYSTEM` actions (`src/router/system_policy.rs:25-43`), but once the caller is authorized, nothing validates that the unit belongs to the node.

Impact:

A caller authorized for node lifecycle could stop the host's core services (`sy-orchestrator`, `sy-identity`, `rt-gateway`, `ssh`, `postgresql`, etc.) by sending `KILL_NODE` with a direct `unit`. This bypasses the managed-lifecycle invariants and can cause a local or cross-hive DoS, because `SY.orchestrator@<any hive>` is authorized for protected actions (`src/router/system_policy.rs:69-87`).

Recommendation:

- For the public/admin/system API, require `node_name` and always derive `unit` with `unit_from_node_name`.
- If `unit` is kept, allow only `fluxbee-node-*` and check that it matches an existing managed config before acting.
- Split control of core services into an explicit action with a reduced allowlist and no generic cross-hive authority.
- Add tests for `KILL_NODE { unit: "sy-orchestrator" }`, `KILL_NODE` without `node_name`, and `SPAWN_NODE` with a colliding `unit`.

### SO-2026-06-30-02 - High - Bootstrap SSH stays open by default and on partial failures

Status: Open

Evidence:

- The `harden_ssh` default is `false` (`src/bin/sy_orchestrator.rs:17040-17045`).
- The `restrict_ssh` default also ends up `false` if `harden_ssh` is not active (`src/bin/sy_orchestrator.rs:17129-17142`).
- The bootstrap seeds the key via password as an entry without `from=` or a forced command (`src/bin/sy_orchestrator.rs:15784-15823`, `src/bin/sy_orchestrator.rs:17579-17604`).
- The remote sudoers grants `NOPASSWD` to several root tools, including `/bin/bash` and `/usr/bin/bash` (`src/bin/sy_orchestrator.rs:17607-17611`).
- The SSH controls are applied only after the finalize (`src/bin/sy_orchestrator.rs:16385-16420`, `src/bin/sy_orchestrator.rs:16854-16888`).
- There are early returns after key/sudoers and before the closing step: WAN/orchestrator/finalize timeouts on worker (`src/bin/sy_orchestrator.rs:16239-16311`) and egress (`src/bin/sy_orchestrator.rs:16818-16847`).
- If `restrict_ssh` was not requested, the code logs that it leaves the bootstrap key unrestricted (`src/bin/sy_orchestrator.rs:17367-17371`).

Impact:

On a successful `add_hive` with defaults, the motherbee key keeps SSH access to the worker/egress and root escalation via sudoers. On a partial failure after the key/sudoers seeding, the host can be left partly managed with the same access open. On egress this is especially sensitive because it is a border host.

Recommendation:

- Change the defaults to `harden_ssh=true` and `restrict_ssh=true`, or require an explicit, audited opt-out.
- Better still: remove the bootstrap key and sudoers on a successful finish if the v2 contract says SSH is bootstrap-only.
- If break-glass is needed, install an `authorized_keys` entry with `from=`, no pty/forwarding, and a forced command with a small allowlist.
- On every return after key/sudoers, attempt a best-effort cleanup or mark a pending state with a visible flag such as `ssh_bootstrap_open=true`.

### SO-2026-06-30-03 - High/Medium - `run_node` mutates identity before knowing whether the node can be created

Status: Open

Evidence:

- `run_node_flow` registers the identity before computing/checking `config_path` (`src/bin/sy_orchestrator.rs:12627-12648`).
- It then applies an identity update if there are changes (`src/bin/sy_orchestrator.rs:12655-12689`).
- Only after that does it check whether the local config already exists, and it can return `NODE_ALREADY_EXISTS` with identity already mutated (`src/bin/sy_orchestrator.rs:12697-12724`).
- The registration function calls `SY.identity` and persists the local mapping (`src/bin/sy_orchestrator.rs:11246-11308`).
- `SY.identity` updates/creates the canonical ILK by `identification.node_name` (`src/bin/sy_identity.rs:885-944`).
- If the config write or `systemd-run` fails, the response returns an error but the earlier effects remain (`src/bin/sy_orchestrator.rs:12730-12752`, `src/bin/sy_orchestrator.rs:12794-12817`).

Impact:

A repeated `run_node` against an existing node can modify identity/mapping and end in `NODE_ALREADY_EXISTS`. A `systemd-run` failure can leave config + ILK + mapping persisted with no process running; the later retry collides with the existing config and does not resume cleanly.

Recommendation:

- Do the `config_path.exists()` check before any mutation in `SY.identity`.
- Turn spawn into a transaction or a resumable workflow: prepare a temporary config, register identity, run `systemd-run`, and do a final commit; if it fails, roll back config/mapping/ILK, or use a `pending_spawn` state with an explicit retry.
- Add tests that verify that `NODE_ALREADY_EXISTS` does not trigger an RPC to identity and that a failed `systemd-run` does not leave a state that blocks retries.

### SO-2026-06-30-04 - Medium - Missing per-node lock for lifecycle and config

Status: Open

Evidence:

- `OrchestratorState` has locks for runtime and hive topology, but no lock per managed node (`src/bin/sy_orchestrator.rs:466-498`).
- The per-hive lock applies to add/remove hive (`src/bin/sy_orchestrator.rs:513-525`, `src/bin/sy_orchestrator.rs:15434-15437`, `src/bin/sy_orchestrator.rs:8022-8024`).
- Node config creation does a `path.exists()` check and then `write_json_atomic`, but without a guard shared with identity/systemd/kill/remove (`src/bin/sy_orchestrator.rs:9499-9520`).
- `run_node_flow` runs identity, config and systemd inside a long window with no node lock (`src/bin/sy_orchestrator.rs:12627-12817`).

Impact:

Two simultaneous `run_node` calls for the same node can register/update identity on both paths and race for the config. A `kill_node purge_instance` or `remove_node_instance` can delete state while `run_node` is creating the config or starting the unit. The likely result is drift between identity, local config, timers and systemd.

Recommendation:

- Add a registry of locks keyed by normalized `node_name`, similar to `hive_topology_locks`.
- Hold the lock during `run_node`, `start_node`, `restart_node`, `kill_node`, `remove_node_instance` and `NODE_CONFIG_SET` for the local node.
- Cover this with concurrent tests for `run_node/run_node` and `run_node/kill_node purge_instance`.

### SO-2026-06-30-05 - Medium - `SYSTEM` relies only on the router for protected actions

Status: Open

Evidence:

- The RPC profile delivers any post-pending `SYSTEM` to the orchestrator's command channel (`src/bin/sy_orchestrator.rs:528-546`).
- `handle_system_message` states that it does not re-validate the origin because the router filters it centrally (`src/bin/sy_orchestrator.rs:2091-2099`).
- If a message reaches that handler, it runs protected actions such as `SPAWN_NODE`, `KILL_NODE`, `NODE_CONFIG_SET`, `ADD_HIVE_FINALIZE` and `REMOVE_HIVE_CLEANUP` (`src/bin/sy_orchestrator.rs:2119-2207`).
- The router currently filters before local delivery (`src/router/mod.rs:5628-5643`).

Impact:

The centralized design is reasonable on the normal path, but the receiver is left with no local defense against a test bypass, an old router version, a future alternative path, or a regression in `serialize_for_local_delivery`. If that boundary fails, the impact is total, because the handler runs critical mutations.

Recommendation:

- Keep the router as the source of truth, but add a defensive guard in `sy.orchestrator` for protected `SYSTEM` actions, using the same shared policy or an exposed helper.
- At a minimum, reject protected actions if `msg.routing.src_l2_name` is `None`, and log an alert.
- Add a test that invokes `handle_system_message` with a protected action and an unauthorized origin, without going through the router.

### SO-2026-06-30-06 - Medium/Low - `hive.yaml` fields go raw into the syncthing unit file

Status: Open

Evidence:

- `blob_runtime_from_hive` copies `blob.path`, `blob.sync.data_dir` and `blob.sync.service_user` from YAML with a trim, but with no character/control-char validation and no path normalization (`src/bin/sy_orchestrator.rs:3287-3335`).
- `resolve_syncthing_service_user` checks that the user exists, or falls back to root if fallback was enabled, but does not validate the value for use in unit files (`src/bin/sy_orchestrator.rs:3730-3748`).
- `syncthing_unit_contents` interpolates `User=`, `Group=`, `WorkingDirectory=`, `Environment=HOME=` and `ExecStart --home=` with raw strings (`src/bin/sy_orchestrator.rs:4774-4785`).
- It then writes `/etc/systemd/system/fluxbee-syncthing.service` as root (`src/bin/sy_orchestrator.rs:4789-4801`).

Impact:

It is not direct shell injection, but a path with a newline, an odd `%`/specifier or unexpected characters can produce an invalid unit file or inject systemd directives if someone controls `hive.yaml`. In the normal case the config is operator-owned, so the severity is lower; as operability hardening, it is worth closing the class.

Recommendation:

- Reject control chars/newlines and require normalized absolute paths for `blob.path`, `dist.path` and `blob.sync.data_dir`.
- Use correct escaping for systemd (`systemd-escape`/equivalent rules) or write unit fields through a validated representation.
- Add tests with a `sync.data_dir` that contains a newline, spaces and `%` to confirm rejection or stable escaping.

### SO-2026-06-30-07 - Medium/Low - `write_secret_file_0600` does not repair the permissions of existing files

Status: Open

Evidence:

- `write_secret_file_0600` opens with `.create(true).truncate(true).mode(0o600)` and writes, but does not call `set_permissions` afterwards (`src/bin/sy_orchestrator.rs:800-813`).
- `.mode(0o600)` only applies if the file is created. If the file already existed with broad permissions, the truncate keeps the previous mode.
- It is used for the hive's local TLS key (`src/bin/sy_orchestrator.rs:873-884`).

Impact:

If `cert.key` was ever left with `0644` or another broad mode, a later rewrite/rotation does not fix the permission and keeps the local exposure.

Recommendation:

- Call `fs::set_permissions(path, 0o600)` after writing, or write to a 0600 temp file and rename it atomically.
- Add a test with a pre-existing `0644` file.

### SO-2026-06-30-08 - Low/Medium - Egress reports sysctl as OK without readback and has a delete-then-apply window in nft

Status: Open

Evidence:

- `reconcile_egress_nat` runs `apply_sysctl_system()` and marks `ipv4_forwarding=true` and `ipv6_blocked=true` without reading back the real values from the kernel (`src/bin/sy_orchestrator.rs:4562-4569`).
- In nftables it deletes the existing table and then applies the new file (`src/bin/sy_orchestrator.rs:4584-4595`).
- Conntrack does have readback/real state (`src/bin/sy_orchestrator.rs:4580-4582`), which shows the pattern that is missing for sysctl.

Impact:

The verification can report a healthy egress even if another process/policy has rejected or reverted sysctl. During a reconciliation there is a short window without the `fluxbee_egress` table; with forwarding active, the border plane is temporarily left without that policy.

Recommendation:

- Read back `net.ipv4.ip_forward` and the relevant IPv6 sysctls after applying.
- Avoid delete-then-apply: use an nft transaction that replaces/flushes the objects inside `nft -f` without leaving an observable gap, or an equivalent staging table + swap.
- Add a unit test for the sysctl verifier and, if possible, an integration test in a network namespace.

## No regressions observed from the previous audit

This pass found no regression in the most critical fixes from the 2026-06-23 document:

- Mutating admin requires the origin `SY.admin@<hive local>` (`src/bin/sy_orchestrator.rs:1789-1811`, `src/bin/sy_orchestrator.rs:2221-2253`).
- `hive_id` is validated early, and there is a lexical guard for hive paths (`src/bin/sy_orchestrator.rs:16987-17004`, `src/bin/sy_orchestrator.rs:15417-15437`).
- `remove_hive` uses the socket or local-only, with no operational SSH fallback (`src/bin/sy_orchestrator.rs:8062-8111`).
- `write_remote_file` streams contents over stdin to `sudo tee`, with no heredoc/payload interpolation (`src/bin/sy_orchestrator.rs:17937-18005`).
- Egress validates interface names with an allowlist before the shell/nft/YAML sinks (`src/bin/sy_orchestrator.rs:4161-4215`).

## Audit gaps

- No E2E was run with real hives or a real egress, and no network/iptables/nft tests were run in a namespace.
- A live multi-hive topology with mixed router versions was not exercised.
- No fuzzing of admin/system payloads was done.
- The review was static + local unit tests. The concurrency findings must be confirmed with targeted tests, but the absence of a per-node lock and the order of side effects are visible in the code.

## Suggested remediation order

1. Close `unit` ownership in `run_node`/`kill_node` and cover it with negative tests.
2. Change the SSH bootstrap defaults/semantics: close or remove key/sudoers at the end, and clean up on partial failures.
3. Reorder `run_node` to avoid identity side effects before the local preflight, and define rollback/retry for a failed spawn.
4. Add a per-node lock for lifecycle/config.
5. Add a local defensive guard for protected `SYSTEM` actions.
6. Harden the syncthing unit file, the secrets' permissions and egress readback/atomicity.

## Remediation status (2026-07-01)

Prior verification: the 8 findings were confirmed against the current code (none stale/false); in addition, 3 new issues were found (B-1/B-2/B-3, below).

| ID | Severity | Status | Commit |
| --- | --- | --- | --- |
| SO-2026-06-30-01 | High | **Resolved** — `unit` is always derived from `node_name` (`fluxbee-node-*`); KILL_NODE requires `node_name`. It cannot touch host units. | `0f0400a` |
| SO-2026-06-30-02 | High | **Resolved** — revoke of the bootstrap SSH access (key + sudoers) when a successful add_hive finishes; validated on 2 VMs in the lab (sudoers GONE, 0 entries in authorized_keys, motherbee SSH→worker denied, worker operational over the socket). | `cc76814` |
| SO-2026-06-30-03 | High/Medium | **Resolved** — config preflight before mutating identity; best-effort rollback of the config on `SPAWN_FAILED`. | `0f0400a` |
| SO-2026-06-30-04 | Medium | **Resolved** — async lock per `node_name` in run/kill/start/restart/remove/config_set. | `57b9e71` |
| SO-2026-06-30-05 | Medium | **Resolved** — local defensive guard in `handle_system_message` with the same `system_policy` (now `pub`). | `0f0400a` |
| SO-2026-06-30-06 | Medium/Low | **Resolved** — control-chars/absolute-path validation before writing the syncthing unit. | `cec4195` |
| SO-2026-06-30-07 | Medium/Low | **Resolved** — `write_secret_file_0600` re-ensures 0600 post-write. | `0f0400a` |
| SO-2026-06-30-08 | Low/Medium | **Resolved** — real sysctl readback; atomic nft add+flush+define (no delete-then-apply). | `cec4195` |
| B-1 (new) | Low | **Resolved** — `ssh_user` validated by charset before touching shell/ssh. | `cec4195` |
| B-2 (new) | Low | **Resolved** — best-effort revoke of the bootstrap key on a failed add_hive (worker/egress) + `ssh_bootstrap_open` flag in the error result. | `d37d29a` |
| B-3 (new) | Info | **Resolved** — `validate_yaml_scalar` on the values interpolated into the worker/egress `hive.yaml` (rejects quote/backslash/control-char). | `d37d29a` |

Resolved: **11/11** — the audit's 8 original findings (SO-01..08) + the 3 new ones (B-1/B-2/B-3). SO-02 (SSH bootstrap revoke) was closed with a 2-VM re-validation in the Proxmox lab. None pending. Tests: `cargo test --bin sy_orchestrator` 109 passed / 0 failed. Commits: `0f0400a` (SO-01/03/05/07), `cec4195` (SO-06/08/B-1), `57b9e71` (SO-04), `cc76814` (SO-02), `d37d29a` (B-2/B-3).
