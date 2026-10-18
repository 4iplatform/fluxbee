# sy.orchestrator audit

Date: 2026-06-23
Commit audited: `78428c5`
Branch: `daily_onworking_coa`
Main file: `src/bin/sy_orchestrator.rs` (20562 lines)

## Resolution status (updated 2026-06-26)

Reconciliation of the work done after the audit. Priority agreed with the operator: **operational first, security later** — the network is internal with defined entry points, and SSH is used only for the initial bootstrap. That is why the operational batch + SSH-only-bootstrap was resolved first; the security batch has now been opened, starting with #1 of the suggested order (iface injection, F7/F12/F13).

**Resolved (19 of 24):**

- **F5 + F6 + F16 + F17** egress (NAT) batch — **empirically validated on a real VM (Ubuntu 24.04)**. **F16**: `conntrack_tuned` is no longer an unconditional `true` — `apply_conntrack_live()` returns a bool by verifying the read-back of `nf_conntrack_max` (it tries the canonical key **and** the alias); the VM journal confirms `false`→`true` when the module loads. **F17**: a new systemd unit `fluxbee-egress-nft.service` (oneshot, `Before=network-pre.target`) loads the nft table at boot — **conclusively proven**: with the orchestrator **disabled**, the table is present after a reboot (the boot unit loaded it, closing the ip_forward-without-table window). **F6**: the watchdog re-applies the idempotent reconcile every ~60s (not only when the table disappeared) — tested: a flush of one chain (partial drift, table present) is corrected in <70s. **F5**: `internet_reachable=false` degrades → a continuous `DEGRADED` WARN in the egress journal + the `add_hive` response reports `egress_internet_reachable: null` (it does not imply success; real transmission = T-VER-1). *Adversarial review (3 findings fixed: read-back of the conntrack alias, `systemctl enable` churn, `block_in_place` for the watchdog's blocking I/O) + 105 unit tests + empirical validation on a VM (F16/F17/F6/F5).*
- **F4 + F15** `system` origin allowlist — **resolved by moving origin authority to the ROUTER** (the operator's design decision: the router is the center of routing/authority; do not spread the check across nodes). The router resolves the authoritative `src_l2_name` (UUID→record) and gates the 18 protected SYSTEM actions in `serialize_for_local_delivery` (single choke point): `SY.orchestrator@*` (cross-hive, forwards), `SY.admin`/`SY.wf-rules`/`WF.orch.diag` `@<same-hive>` (closes F15: rejects cross-hive admin), `SY.config-routes`/`SY.vault` `@<same-hive>` only for `NODE_STATUS_GET` (read-only probe opened by the architect). `SY.` rules hardcoded in the router + published as **frozen** routes visible in SHM. The orchestrator's gate (`is_allowed_system_source_name`, not configurable because of the hardcoded prefixes — the F4 problem) **was removed**; future configurability goes to the **OPA-system** layer (noted). *Unit tests (router) + adversarial review (GO) + validated in the 2-hive lab (cross-hive live forwards + same-hive + router-only enforcement, 0 wrongful drops).*
- **F18** TOCTOU lock — a per-hive_id lock registry held during the whole `add_hive`/`remove_hive` flow. *Unit test + review (GO).*
- **F8** origin gate on the ADMIN channel — a 2-component change: the `SY.admin` relay stamps its identity `SY.admin@<hive>` and `handle_admin` gates the mutating actions to `== SY.admin@<its-hive>`. *4 unit tests + adversarial review (GO, no regression: all admin traffic routes to the local orchestrator). Residual: hop-1 not gated (separate finding).*
- **F11** NAT teardown in egress `remove_hive` — it reads `role`, sends `egress_teardown`, and the removed host cleans up nft+files+sysctl (idempotent, placed before the service kill); the response signals the offline orphan. *Unit test + adversarial review (cgroup race + offline orphan fixed). **Empirical validation on a VM still missing (Phase 2).***
- **F3** `hive_id` traversal — `valid_hive_id`/`valid_address` moved to the start of `add_hive_flow`/`add_egress_hive_flow` (before any `join`/`remove_dir_all`/`create_dir_all`) + a lexical backstop `hive_dir_is_direct_child` before each destructive op. *Unit test + adversarial review (GO).*
- **F7** root RCE via iface — `validate_iface_name` (allowlist `^[A-Za-z0-9._-]{1,15}$`, IFNAMSIZ, rejects `.`/`..`) applied to `wan_iface`/`lan_iface` in `resolve_egress_nat_config` (the only constructor of `EgressNatConfig`), before any sink. Also, `write_remote_file` was rewritten: the content goes through **stdin** to `sudo -n tee '<path>'`, with no shell interpolation. *Unit test (exhaustive accept/reject) + multi-agent adversarial review (GO, 0 blocking defects; reachability/charset/rewrite verified).*
- **F12** nft injection — closed by the **same** allowlist: the iface is embedded only in double-quoted nft strings, and the charset excludes `"` and `\`. *Covered by the F7 fix and test.*
- **F13** YAML injection/corruption — likewise: the iface is embedded only as a double-quoted YAML scalar from `nat.*` (validated); the charset excludes `"`/`\` and the scalar-coercion triggers. *Covered by the F7 fix and test.*

**Previously resolved (6):**

- **F1** remove_hive socket-only — operational SSH removed; online -> `socket_ok/socket`, offline -> `local_only/local_only`, no SSH fallback. *Empirically validated (lab, online + offline).*
- **F2** add_hive without SSH fallback — `WORKER_SOCKET_UNREACHABLE` retryable when the worker is online; classifier aligned with the real `RpcError` strings. *Empirically validated (lab).*
- **F9** add_hive idempotent — a retry on `pending` resumes via the socket fast-path; `connected` still returns `HIVE_EXISTS`. *Empirically validated (lab) + unit test.*
- **F10** egress hardening fatal — the `Err` arm returns `SSH_HARDEN_FAILED`/`SSH_KEY_FAILED`; on failure, info.yaml stays `pending` (resumable). *Code + adversarial review (SOUND); empirical validation still missing (needs the egress role, lab Phase 2).*
- **F14** watchdog panic-safe — `WatchdogRunGuard` (Drop) resets the flag even on panic. *Unit test.*
- **F20** SHM slice clamp — `shm_name_to_string` centralizes and clamps the 7 sites. *Unit test.*

Also (not an audit finding; a design change by the operator): **the bootstrap SSH credentials moved into the `add_hive` payload** (`ssh_user` required / `ssh_password` optional, key-first probe), removing the hardcoded `administrator`/`magicAI`; the secret is redacted in SY.architect; the admin schema was updated. *Empirically validated (lab: worker bootstrapped with the payload creds).*

**Pending (5 of 24)** — by severity:

- **High (0).**
- **Medium (0).** Egress batch (F5/F6/F16/F17) closed and validated on a real VM.
- **Low (5):** F19, F21, F22, F23, F25.

**Security note:** **F7/F12/F13 closed** — they were pending item #1. A single iface allowlist closes all three, because the accepted charset (`[A-Za-z0-9._-]`) shares no metacharacter with the three sinks (shell / nft / YAML), and the validation runs in the only constructor of `EgressNatConfig` before any sink (verified: no bypass; the reconcile paths re-validate the deserialized `hive.yaml`). The next highest-priority pending item becomes **F8** (origin gate on the ADMIN channel).

Code status: the resolved fixes live in `src/` (in a commit separate from the lab's). Validation: see `lab/STATUS.md` (a containerized environment where F1/F2/F9 + creds were tested over a real 2-hive mesh); F7/F12/F13 with unit tests + adversarial review (no egress role in the lab yet — Phase 2/VM for the empirical egress validation).

## Scope

`sy.orchestrator` was reviewed with a focus on the recent features: the v2 socket-first contract, `add_hive`/`remove_hive`, origin hardening for `system` actions, and `role=egress` / NAT support. The full diff against `main` is broad; this audit prioritizes the behavior of `src/bin/sy_orchestrator.rs` and the contracts documented around that binary.

Method: multi-agent adversarial review. Each existing finding was verified against the real code (line numbers confirmed at commit `78428c5`); in addition, a hunt was run by lens (add_hive worker/egress, remove_hive, SSH transport, concurrency, origin authorization, egress NAT, contract conformance), and each new candidate was verified adversarially. Three findings were reproduced empirically (`hive_id` path traversal, command injection in `write_remote_file`, nft rule injection).

Commands run:

```bash
cargo test --bin sy_orchestrator
bash scripts/router_dispatcher_guards/run_all.sh
```

Result:

- `cargo test --bin sy_orchestrator`: OK, 100 tests passed.
- `router_dispatcher_guards`: OK, 7/7 guards passed.

Important regarding the coverage of those commands: **none of the findings below is exercised by the current suite**. They all live in remote/operational paths (real hives, SSH, nftables, two NICs) that the unit tests and the guards do not cover. Each finding is verified by reading the code (and, in three cases, by empirical reproduction of the quoting/FS), not by running the suite. No E2E with real infrastructure was run.

## Executive summary

The binary compiles and its unit tests pass, but there are relevant security and contract defects. The two most serious were not in the first version of this audit:

- **Command injection as root** in `write_remote_file` via unvalidated `wan_iface`/`lan_iface` (F7, formerly classified as "low because of single quotes").
- **The `ADMIN_COMMAND` channel checks no origin allowlist** for destructive mutating actions (F8).

In addition:

- `remove_hive` and `add_hive` reintroduce/keep operational SSH against the v2 socket-first contract (F1, F2), contradicting backlog items marked as closed.
- `add_hive` validates `hive_id` after deleting directories (F3) and takes no lock (F18 TOCTOU).
- `add_hive` is not idempotent: a timeout leaves the hive `pending` with no retry path (F9).
- The egress boundary can be left with unhardened SSH yet report `ok` (F10), and `remove_hive` of an egress leaves the NAT orphaned (F11).
- The `system` origin allowlist has hardcoded prefixes that make it non-configurable (F4) and accept an arbitrary hive suffix (F15).
- The egress verification over-claims state (F5, F16), and the nft table is not loaded at boot (F17).

Also, three bugs in the new features share a single root cause: **the absence of an interface-name allowlist** (`wan_iface`/`lan_iface`). A single fix closes F7 (RCE), F12 (nft injection) and F13 (YAML corruption).

## Summary table

| ID | Sev | Area | Title | Verification | Backlog |
|----|-----|------|--------|--------------|---------|
| F7  | High | ssh-transport | ✅ RESOLVED — RCE as root in `write_remote_file` (unvalidated iface) | empirical | new (formerly Low) |
| F8  | High | origin-auth | ✅ RESOLVED — `ADMIN_COMMAND` without an origin gate | reading | new |
| F1  | High | remove_hive | Operational SSH in `remove_hive` | reading+git | contradicts `[x]` v2:21 |
| F2  | High | add_hive | Socket-only `add_hive` falls back to SSH bootstrap | reading | contradicts `[x]` v2:22 |
| F3  | High | add_hive | ✅ RESOLVED — Deletes dir before validating `hive_id` (traversal) | empirical | new (formerly Critical) |
| F4  | High | origin-auth | ✅ RESOLVED (router-authority) — `system` allowlist not configurable | reading+test | partial v2 |
| F9  | High | add_hive | `add_hive` not idempotent (stuck `pending`) | reading | new |
| F10 | High | add_hive | Egress SSH hardening non-fatal, reports `ok` | reading | new |
| F11 | High | remove_hive | ✅ RESOLVED (code; no VM yet) — Egress `remove_hive` does not deprovision NAT | reading | new |
| F12 | Medium | egress-nat | ✅ RESOLVED — nft rule injection (unvalidated iface) | empirical | new |
| F13 | Medium | ssh-transport | ✅ RESOLVED — YAML corruption/injection in remote `hive.yaml` | empirical | new |
| F5  | Medium | egress-nat | Egress reports `ok` with incomplete verification | reading | rewritten |
| F6  | Medium | egress-nat | Egress watchdog does not correct partial drift | reading | decision CR-5 |
| F14 | Medium | robustness | `watchdog_tick` is not panic-safe | reading | new |
| F15 | Medium | origin-auth | ✅ RESOLVED (router-authority) — Allowlist accepts an arbitrary hive suffix (cross-hive) | reading | new (extends F4) |
| F16 | Medium | egress-nat | Unconditional `egress_conntrack_tuned=true` | reading | new |
| F17 | Medium | egress-nat | nft table not loaded at boot | reading | new |
| F18 | Medium | concurrency | ✅ RESOLVED — TOCTOU: `add_hive`/`remove_hive` without a lock | reading | new |
| F19 | Low | inventory | `list_hives` labels egress as `worker` | reading | new |
| F20 | Low | robustness | SHM name slices without clamp (`node_name`) | reading | new |
| F21 | Low | egress-nat | Egress host without inbound filtering | reading | trade-off T-NET-4 |
| F22 | Low | add_hive | Egress without socket fast-path (re-provision re-runs SSH) | reading | new |
| F23 | Low | egress-nat | `ipv4_forwarding`/`ipv6_blocked` without read-back | reading | new |
| F25 | Low | egress-nat | nft delete-then-apply not atomic | reading | new |

Severities: 9 High, 9 Medium, 6 Low. (F24 was absorbed into F6; see the discarded items at the end for the false positives.)

## Findings - High

### F7 - High - Command injection as root in `write_remote_file`

> **RESOLVED (2026-06-26).** `validate_iface_name` (allowlist `^[A-Za-z0-9._-]{1,15}$`, IFNAMSIZ, rejects `.`/`..`) in `resolve_egress_nat_config` — the only constructor of `EgressNatConfig`, before any sink. `write_remote_file` rewritten: content through **stdin** to `sudo -n tee '<path>'`, no shell. Unit test + multi-agent adversarial review (GO).

Verification: **empirical** (the exact quoting chain was rebuilt and run against real bash).

Rewritten from the previous version, which classified it as "Low - can alter contents with single quotes". The suspicion raised in that earlier finding itself ("CAREFUL, IMPORTANT") was confirmed: it is not a matter of byte fidelity, it is arbitrary command execution as root.

Evidence:

- `write_remote_file` (`src/bin/sy_orchestrator.rs:16903-16906`):
  - `let escaped = contents.replace('\'', "'\"'\"'");`
  - `let cmd = format!("cat > {} <<'EOF'\n{}\nEOF", remote_path, escaped);`
  - `let sudo_cmd = sudo_wrap(&format!("bash -lc \"{}\"", cmd.replace('"', "\\\"")));`
- `sudo_wrap` (`src/bin/sy_orchestrator.rs:17107`) generates `sudo -n bash -lc "..."` (root). The heredoc body travels inside double quotes that the remote login shell expands **before** it processes the `<<'EOF'`. The only escape of that outer layer is `replace('"', "\\\"")`; `$`, backtick and `\` are never escaped.
- Reproduced: with `contents` = `x=$(touch PWNED && echo INJECTED)`, the file `PWNED` is created on the host and the written file ends up as `x=INJECTED`. The backtick variant (`y=` followed by `touch PWNED2` between backticks) also executes. `z=$HOME` is expanded.
- Unvalidated source: `wan_iface`/`lan_iface` come from the `add_hive` payload, and `resolve_egress_nat_config` (`src/bin/sy_orchestrator.rs:3801-3802`) only does `require_field` (= `trim` + non-empty, `:3773-3779`). There is no interface-name validation anywhere in the repo. They are interpolated into `hive_yaml` (`add_egress_hive_flow:15841-15851`) and passed to `write_remote_file(/etc/fluxbee/hive.yaml, hive_yaml)` (`:15853`).
- The correct primitive, `shell_single_quote` (`src/bin/sy_orchestrator.rs:13598`), already exists and is used correctly in other places (e.g. `:7378`); this path ignores it.

Impact:

An `add_hive role=egress` (motherbee-only, authenticated by admin) with `egress.wan_iface = "eth0$(...)"` executes arbitrary code as root on the remote egress host. It is a config-string -> shell trust-boundary crossing, not mere file corruption.

Mitigating factors (hence High and not Critical): `add_hive` is motherbee-only (`:1571`) and requires an authenticated `ADMIN_COMMAND`; it is not unauthenticated RCE from the internet. The `egress.gateway_ip` field is indeed validated as `Ipv4Addr` before interpolation (`render_worker_egress_yaml:3222-3224`), but `wan_iface`/`lan_iface` (and other free-form strings such as identity node names, paths) are not.

Recommendation:

1. Validate `wan_iface`/`lan_iface` against the real Linux interface format (`^[A-Za-z0-9._-]{1,15}$`, IFNAMSIZ<=15) in `resolve_egress_nat_config`, before any interpolation.
2. Rewrite `write_remote_file` so it does not build shell by interpolation: pass `contents` through stdin to `ssh ... sudo tee <path>` (or `scp`; `scp_with_key:16855` already exists), without the wrapping `bash -lc "..."` layer.

### F8 - High - `ADMIN_COMMAND` checks no origin allowlist for destructive actions

> **RESOLVED (2026-06-26) — a 2-component change.** The design revealed that **all** legitimate admin traffic arrived with `src_l2_name: None`, so the "obvious" gate would have broken the whole admin API. Two-part fix: **(A)** `send_admin_request` (sy_admin.rs) now stamps the relay identity `SY.admin@<hive>` instead of `None` (5 call-sites pass `&admin_node_name(&ctx.hive_id)`; the SDK's `send_admin_rpc` was left at `None` on purpose — architect/diag arrive via the relay, which re-stamps). **(B)** `handle_admin` gates the mutating actions: `admin_action_requires_origin` (reuses `classify_admin_action` + explicit `start_node`/`restart_node`) `&&` `is_allowed_admin_source_name` (`== SY.admin@<its-own-hive>`, exact, same-hive). All admin actions route to the **local** orchestrator (`action_routes_via_local_orchestrator`), so relay-hive == target-hive → no cross-hive regression. 4 unit tests (predicate + e2e `None`→FORBIDDEN + foreign→FORBIDDEN). Multi-agent adversarial review: **GO, 0 blocking defects** (regression / coverage+bypass / correctness verified). **Known residual (separate finding):** hop-1 (caller → SY.admin's internal socket) is still not gated; the gate is defense-in-depth over a self-asserted name, not a cryptographic barrier (that comes with the F4/F15 transport auth).

Verification: reading.

Evidence:

- `handle_admin` (`src/bin/sy_orchestrator.rs:1409`) goes straight from `msg.meta.action` into the action match. `kill_node` (`:1511`), `set_node_config` (`:1507`), `run_node` (`:1510`), `restart_node` (`:1509`), `remove_node_instance` (`:1499`) have no origin check at all. `add_hive` (`:1571`) and `remove_hive` (`:1546`) only check `state.is_motherbee` (a property of the local node, not of the sender).
- The `is_allowed_system_source_name` gate is only invoked in `handle_system_message` (`:1689`), never in `handle_admin` (grep confirms a single occurrence).
- The ADMIN channel is reachable from the mesh: `pre_pending_rule(RouteMatch::exact(ADMIN_KIND, MSG_ADMIN_COMMAND), RouteTarget::Command(RPC_CH_ADMIN))` (`:508-511`) -> `run_admin_worker` -> `handle_admin` with no filter.
- The same `kill_node_flow` is reachable through two paths: `KILL_NODE` (system, gated at `:1728`) and `kill_node` (admin, not gated at `:1511`).
- Aggravating factor: `run_node` over ADMIN forwards `SPAWN_NODE` to the target hive; the `src_l2_name` of the forwarded message is that of the relay orchestrator, which always matches `is_allowed_system_source_name` through `starts_with("SY.orchestrator@")` (`:1850`). An unauthorized entry through the admin path gets "laundered" through the system-path allowlist of the second hive.

Contract/documentation:

- `docs/history/onworking-coa/sy_router_tasks.md:216`: the origin check in SY.orchestrator is "in addition to policy/router" (defense in depth in the node, not only in the router).
- The guard `scripts/router_dispatcher_guards/origin_auth_gates_present.sh` requires `admin_origin_authorized`/`build_admin_forbidden_response` in SY.admin (the canonical `ADMIN_COMMAND` gateway). The orchestrator received the gate on the SYSTEM side but not its equivalent on the ADMIN side.

Impact:

A mesh node (or a compromised one) that can address `SY.orchestrator@<hive>` with a mutating `ADMIN_COMMAND` runs the operation without going through any allowlist. Unlike F4: here the ADMIN path does not even invoke the allowlist. Mitigating factor: it depends on how permissive the router's OPA policy is for `ADMIN_COMMAND`, and `add_hive`/`remove_hive` have the partial `is_motherbee` gate.

Recommendation:

Add to `handle_admin` an origin gate for protected admin actions (action predicate + `admin_origin_authorized` + `build_admin_forbidden_response`), on par with SY.admin, before the match at `:1416`. Tests: negative (origin not allowed -> FORBIDDEN) and positive (`SY.admin@<hive>`). Include the orchestrator in `origin_auth_gates_present.sh`.

### F1 - High - `remove_hive` uses operational SSH against the v2 contract

Verification: reading + git blame.

Evidence:

- `remove_hive_cleanup_via_ssh` exists (`src/bin/sy_orchestrator.rs:7397`); its only guards are an empty address (`:7399`) and a missing key (`:7403`). It uses `ssh_with_key(address, key, cleanup_cmd, BOOTSTRAP_SSH_USER)`.
- Socket OK path (`:7476-7499`): even with `socket_cleanup_ok=true`, if `address` is not empty, `remove_hive_cleanup_via_ssh(&address)` is called anyway (`:7479`) as a "verification" -> `remote_cleanup=socket_ok_ssh_verified`, `remote_cleanup_via=socket+ssh` (`:7494-7495`).
- Socket failure/timeout path (`:7500-7524`): SSH fallback -> `socket_timeout_ssh_ok`/`socket_failed_ssh_ok`, `remote_cleanup_via=ssh` (`:7518-7524`).
- There is no early-return and no config flag that turns SSH off. `address` is re-read from `info.yaml` (`:7441-7449`), so for a normal hive with a valid address and a key present (seeded by `scripts/install.sh`), SSH runs on every `remove_hive`.

Contract/documentation:

- `docs/history/onworking-coa/sy_orchestrator_v2_tasks.md:10-12`: SSH only for the `add_hive` bootstrap; "no operational SSH fallback".
- `:21`: `[x] remove_hive sin fallback SSH: online por socket o cleanup local` ("remove_hive without SSH fallback: online via socket or local cleanup") (marked closed).
- `:103-105`: E2E-3 offline -> `remote_cleanup in {socket_timeout,local_only}` and `remote_cleanup_via=local_only`.
- The code **breaks the E2E-3 gate itself**: `scripts/orchestrator_remove_hive_socket_e2e.sh:163-164` requires exactly `socket_ok` when online (it gets `socket_ok_ssh_verified`), and `:202-205` requires `local_only` when offline (it can get `ssh`). The script forces offline by stopping only the remote orchestrator (`:137`), without disabling SSH, so the fallback succeeds.

Nuance ("reintroduces"): git blame shows that commit `cd4967c` added the SSH "verification" on the socket-OK path (that is a genuine reintroduction); the SSH fallback on the offline path (`:7500-7524`) already existed before. The conclusion (operational SSH present against the contract) is correct in both cases.

Impact:

`remove_hive` mutates the worker over SSH even though the v2 operation should be socket-only or local-only, and it breaks the contract and its closing test. Mitigating factor: the result is idempotent (the cleanup already happened over the socket; the local directory is deleted anyway), and it degrades gracefully if the key/SSH fail.

Recommendation:

Remove `remove_hive_cleanup_via_ssh` from the flow. For online, report only `socket_ok`/`socket`. For a socket timeout/failure, continue with local cleanup and report `socket_timeout`/`local_only` with `remote_cleanup_via=local_only`. After that, the online and offline E2E-3 gates pass without touching the script.

### F2 - High - `add_hive` socket-only can fall back to the SSH bootstrap

Verification: reading.

Evidence:

- The fast path detects the remote orchestrator in LSA via `wait_for_remote_orchestrator_node` -> `socket_only_ready=true` (`src/bin/sy_orchestrator.rs:14821-14822`).
- Three paths reach the SSH bootstrap (`:14991`) even with `socket_only_ready=true`:
  1. Probe failure (`:14824`): the `else` is skipped, there is only a `tracing::warn!`, and control falls through to `:14991`. **It does not go through `is_socket_only_unreachable_error`** — any `GET_VERSIONS` failure (including a non-ok payload) falls back to SSH. This is the crudest violation.
  2. Finalize unreachable/timeout (`:14847-14856`): `is_socket_only_unreachable_error` (`:14586`, used at `:14849`) matches timeout/unreachable -> `finalize = None` -> the finalize block is skipped -> SSH.
  3. Finalize with a NON-unreachable error (`:14857-14876`): returns `FINALIZE_FAILED`. The only branch that meets the contract.

Contract/documentation:

- `docs/history/onworking-coa/sy_orchestrator_v2_tasks.md:10-12`: if the worker is already online over the socket, SSH is not used.
- `:22`: `[x] add_hive socket-only no cae a bootstrap SSH si falla finalize (FINALIZE_FAILED)` ("add_hive socket-only does not fall back to SSH bootstrap if finalize fails (FINALIZE_FAILED)") — with no exception for timeout/unreachable.
- `:101-102`: `[x] E2E-2` expects `bootstrap_mode=socket_only_existing_orchestrator`, zero SSH steps. No script references that mode (empty grep): the item is marked closed, but **no test guards it**.

Impact:

An already-operational worker can be re-provisioned over SSH (key/sudoers seeding + `sync_core_to_worker:15120`, which copies the core binaries) because of a transient probe/finalize failure. It contradicts the contract and rewrites the configuration of a host that was already up.

Recommendation:

Once `socket_only_ready=true`, block every SSH fallback at both points (probe and finalize-unreachable): return an explicit error (`FINALIZE_FAILED` or a probe code), without continuing to the bootstrap. Add an E2E-2 that asserts zero SSH steps.

### F3 - High - `add_hive` deletes directories before validating `hive_id`

> **RESOLVED (2026-06-26).** `valid_hive_id`/`valid_address` moved to the **start** of `add_hive_flow` and `add_egress_hive_flow`, before any `join`/`hive_exists`/`hive_partial_exists`/`remove_dir_all`/`create_dir_all`. A defense-in-depth lexical backstop, `hive_dir_is_direct_child` (requires a single `Normal` component under `hives_root`, rejects `..`), before each `remove_dir_all` **and** `create_dir_all`. Unit test `f3_hive_id_traversal_is_rejected` + adversarial review (GO; verified that no ungated destructive sinks remain and that legitimate flows do not regress).

Verification: **empirical** (rustc confirmed that `Path::join` does not normalize `..` and that `remove_dir_all` escapes the root).

Severity lowered from Critical to High compared with the previous version (see below).

Evidence:

- Worker `add_hive_flow`: `hive_dir = root.join(hive_id)` (`:14743`, root = `hives_root()` at `:14742`); `hive_exists` (`:14744`); `hive_partial_exists` (`:14760`); `fs::remove_dir_all(&hive_dir)` (`:14765`); and only then `valid_hive_id` (`:14773`).
- Egress `add_egress_hive_flow`: same order — `:15677`, `:15681`, `:15682`, validation at `:15686`.
- `valid_hive_id` (`:16064`) rejects empty, `len>64`, and any byte outside `[A-Za-z0-9_-]` (so `/`, `.`, `..` are rejected — but late).
- The dispatcher takes the raw `hive_id` from `msg.payload` without sanitizing it (`:1580-1584`); the only earlier barrier is `is_motherbee` (`:1571`).
- Reproduced: `root.join("../victim")` = `.../hives/../victim` (it keeps the `ParentDir`); `fs::remove_dir_all` on that path recursively deleted a sibling directory outside the tree.

Preconditions (which the previous version left out): the target must (a) already exist as a directory and (b) lack an `info.yaml` (if it has one, `hive_exists` short-circuits with `HIVE_EXISTS` before the deletion). This narrows the impact but does not eliminate it.

Impact:

An `add_hive(hive_id='../<existing-dir-without-info.yaml>')` deletes that directory outside `storage/hives`. It is destructive. Mitigating factor (hence High and not Critical): a double gate — `is_motherbee` + an authenticated `ADMIN_COMMAND`. It is path traversal by a privileged operator, not unauthenticated remote destruction. Contrast: `remove_hive_flow` validates first (`valid_hive_id:7415`) and only then does `root.join` — the correct pattern that `add_hive`/`add_egress` deviate from.

Recommendation:

Move `valid_hive_id` (and `valid_address`) to the start of both flows, before any `join`/`hive_exists`/`hive_partial_exists`/`remove_dir_all`, or sanitize `hive_id` in the dispatcher (`:1580`). In addition, before deleting, canonicalize the path and verify that it is still under `hives_root()`.

### F4 - High - The `system` origin allowlist is not really configurable

> **RESOLVED (2026-06-26) — router-authority.** Instead of making the orchestrator's allowlist configurable, **SYSTEM origin authority was moved to the router** (the routing center; do not spread the check around). The orchestrator's gate (with its 4 hardcoded prefixes — the root cause of F4) **was removed**. The router gates the 18 protected actions in `serialize_for_local_delivery` on the authoritative `src_l2_name`. Operator configurability goes to the future **OPA-system layer** (noted). See the details in "Resolution status".

Verification: reading + test.

Evidence:

- `load_system_allowed_origins` (`src/bin/sy_orchestrator.rs:2791-2805`) reads `ORCH_SYSTEM_ALLOWED_ORIGINS` (default `SY.admin,WF.orch.diag`) and expands entries without `@` to `@<hive_id>`.
- `is_allowed_system_source_name` (`:1845-1854`): after rejecting None/empty, it returns true if `system_allowed_origins.contains(name)` **OR** `name.starts_with` any of `SY.orchestrator@`, `SY.admin@`, `SY.wf-rules@` or `WF.orch.diag@`. The 4 prefixes are unconditional: no env var turns them off.
- Test `system_source_with_allowed_src_l2_name_passes_auth` (`:18391`): it inserts only `SY.admin@motherbee` but asserts that `WF.orch.diag@motherbee` passes (`:18401-18404`) without having inserted it — it pins down the hardcoded bypass. The fix must touch this test.

Contract/documentation:

- `docs/history/onworking-coa/sy_router_tasks.md:216,218`: hardening by an explicit allowlist, configurable via `ORCH_SYSTEM_ALLOWED_ORIGINS`.
- `:220`: production recommendation `ORCH_SYSTEM_ALLOWED_ORIGINS=SY.admin`, leaving `WF.orch.diag` only for controlled tests. **That recommendation is unattainable**: `WF.orch.diag@` is hardcoded at `:1853`, so the diagnostic tooling keeps access to `SPAWN_NODE`/`KILL_NODE`/`SYSTEM_UPDATE` in production even if the operator follows the guide.
- `SY.orchestrator@` and `SY.wf-rules@` do not even appear in the contract as system origins (the contract only mentions `SY.admin` and `WF.orch.diag`): 2 hardcoded origins missing from the spec.

Impact:

An operator cannot remove `WF.orch.diag@*` or `SY.wf-rules@*` from the accepted origins. Any node with one of those L2 names is authorized for protected actions if it manages to route the message. Mitigating factor: the check is "in addition to policy/router" (`:216,291`); there is defense in depth upstream.

Recommendation:

Move the 4 prefixes into defaults of the configurable allowlist (have `load_system_allowed_origins` seed them and `is_allowed_system_source_name` only check the set), so that `ORCH_SYSTEM_ALLOWED_ORIGINS=SY.admin` really excludes `WF.orch.diag`. If a wildcard is wanted, make it explicit config syntax, not a silent exception. Update the test at `:18391`.

### F9 - High - `add_hive` is not idempotent: a timeout leaves the hive stuck in `pending`

Verification: reading.

Evidence:

- In bootstrap, after the connectivity gates, `add_hive_flow` writes `info.yaml` **before** evaluating the result: `status = if wan_connected && orchestrator_connected { "connected" } else { "pending" }` (`:15426`) and an unconditional `write_hive_info` (`:15428`). Only afterwards does it return an error: `WAN_TIMEOUT` (`:15436`), `WORKER_ORCHESTRATOR_TIMEOUT` (`:15453`).
- `hive_exists` (`:16054`) only checks that `info.yaml` exists; it ignores `status`. `hive_partial_exists` (`:16059`) counts it as "partial" only when `info.yaml` does **not** exist. Since the `pending` state has an `info.yaml`, a retry enters through `hive_exists==true` and returns `HIVE_EXISTS` immediately, without retrying the bootstrap or the finalize.
- The finalize that the `pending` state never runs: `add_hive_finalize_via_socket` (`:15477`), `apply_add_hive_ssh_controls_after_finalize` (`:15582`), `status:connected` (`:15622`).
- No watchdog/reconciler reads `status=="pending"` to re-drive the finalize (grep: the only consumers of `status` are listing/inventory).
- Mirror in `add_egress_hive_flow`: it writes `pending` (`:15946-15953`), returns `WAN_TIMEOUT`/`ORCHESTRATOR_TIMEOUT` (`:15960/15971`), and applies the SSH controls only at `:15989`.

Impact:

The worker starts, but its WAN/orchestrator takes more than 60s to appear in LSA (slow network, first start, initial syncthing). `add_hive` writes `info.yaml=pending` and returns a timeout. The worker stays operational; the operator retries to complete the finalize (syncthing peer-link, `info=connected`, `restrict_ssh`) and gets `HIVE_EXISTS`: the hive is left permanently **without a syncthing peer-link or SSH controls**, with no recovery path except a manual `remove_hive` + re-add. Security-relevant in the egress case: it writes `pending` before applying the SSH controls.

Recommendation:

Make the entry guard tell `status=="pending"`/`"connected"` apart (or treat `pending` as partial), so that the retry lands on the idempotent socket-only fast-path and completes the finalize.

### F10 - High - SSH hardening failure on egress is non-fatal and reports `status:ok`

Verification: reading.

Evidence:

- `add_egress_hive_flow` (`src/bin/sy_orchestrator.rs:15989-16001`): the `Err` arm of `apply_add_hive_ssh_controls_after_finalize` only does `tracing::warn!("egress ssh hardening failed; continuing")` and sets `(restrict_ssh_applied=false, restrict_ssh_mode="error")`, falling through to the `status:ok` at `:16018-16035`.
- The worker flow treats the **same helper** as fatal (`:15582-15613`): it returns `SSH_HARDEN_FAILED`/`SSH_KEY_FAILED`.
- The helper returns `Err` both on a `from-only` restriction failure (`:16380-16385`) and on a `disable_remote_password_auth`/verification failure (`:16394-16399`). `disable_remote_password_auth_with_access` restarts the remote sshd (`:16457-16463`) — a real transient failure point.
- `harden_ssh=true` implies `restrict_ssh=true` by default (`resolve_add_hive_restrict_ssh:16151-16164`).
- The egress payload emits `harden_ssh: harden_ssh` (`:16028`, an echo of the request) and has no dedicated status field for the harden half.

Impact:

The egress host — the boundary exposed to the internet — can be left with `PasswordAuthentication` enabled and/or a bootstrap key without `from=` while the operator sees `ok`. The worker flow would have marked the identical condition as a hard failure. Mitigating factor: the `restrict` half does leave an observable signal (`restrict_ssh_mode="error"`, `restrict_ssh=false`); attentive automation can infer the failure.

Recommendation:

Align `add_egress_hive_flow` with `add_hive_flow`: return `SSH_HARDEN_FAILED`/`SSH_KEY_FAILED` and do not return `status:ok` when the helper fails on an egress boundary. At a minimum, add a real `harden_ssh_applied`.

### F11 - High - `remove_hive` of an egress does not deprovision the NAT state

> **RESOLVED in code (2026-06-26) — empirical validation pending (VM/Phase 2).** `remove_hive_flow` reads `role` from info.yaml (`is_egress_role`) and, if it is egress, sends `egress_teardown=true` in the forwarded `REMOVE_HIVE_CLEANUP`; the removed host runs the idempotent teardown (`nft delete table inet fluxbee_egress` + `rm -f` of the 4 `/etc` files + `sysctl --system`). The teardown is placed **before** the service-kill loop (the base cleanup does `systemctl kill -s KILL sy-orchestrator` with `KillMode=control-group`, which would kill the deferred bash itself → the teardown must run first). The response now exposes `role_egress` + `egress_teardown` (`not_applicable`/`applied_via_socket`/`not_confirmed_host_unreachable`) and warns when an **offline** egress was removed (possible orphan, manual teardown required). Unit test + adversarial review (the initial NO-GO caught the cgroup-kill race and the offline orphan without a signal; both fixed and re-verified CLOSED).

Verification: reading.

Evidence:

- The remote cleanup script (`src/bin/sy_orchestrator.rs:7366-7375`) only does stop/disable/kill of the fluxbee services + `rm -rf /var/lib/fluxbee/nodes/*` and `/var/lib/fluxbee/state/nodes/*`. It does not delete egress files.
- `remove_hive_flow` (`:7414-7603`) reads `address` and `syncthing_device_id` from `info.yaml` but never reads `role` or invokes a teardown (grep for role/egress/nft/sysctl/reconcile in that range = 0 hits).
- `delete_egress_nft_table_if_present` (`:3962`) has a single callsite: inside `reconcile_egress_nat` (`:4037`); never in remove.
- Persistent files that `reconcile_egress_nat` writes, with IPv4 forwarding ON + MASQUERADE: `/etc/sysctl.d/99-fluxbee-egress.conf` (`:3703`), `/etc/sysctl.d/99-fluxbee-conntrack.conf` (`:3704`), `/etc/modprobe.d/fluxbee-conntrack.conf` (`:3705`), `/etc/nftables.d/fluxbee-egress.nft` (`:3706`).

Contract/documentation:

- `docs/edge-egress-nat-spec.md` (L349/L371): the files are "persistent across reboots by reapplication" and live under `/etc/sysctl.d` and `/etc/nftables.d`.
- The remove contract (`docs/history/onworking-coa/edge_egress_nat_tasks.md`) does not include an egress teardown; the E2E only validates `remote_cleanup`, never the egress state.

Impact:

After `remove_hive` of an egress, the removed host survives a reboot with IPv4 forwarding enabled and LAN->WAN MASQUERADE active, with no orchestrator governing it: an orphaned, persistent router/NAT outside any governance. Mitigating factor: the box has already left the fleet, and the re-arming only happens at reboot.

Recommendation:

In `remove_hive_flow`, read `role` from `info.yaml` and, if it is egress, extend the cleanup (script + via socket) with `nft delete table inet fluxbee_egress` (reuse `delete_egress_nft_table_if_present`) + `rm` of the 4 files + `sysctl --system`. Alternative: an `EGRESS_TEARDOWN` in the handler.

## Findings - Medium

### F5 - Medium - Egress reports `ok` with incomplete verification

> **RESOLVED (2026-06-26) — validated on a VM.** Decision: `internet_reachable=false` **degrades** where the data lives (the egress host). The watchdog re-checks the reachability on every drift tick and emits a continuous `DEGRADED` WARN (not only the one-time bootstrap warn). The `add_hive role=egress` response no longer implies success: it reports `egress_internet_reachable: null` ("not verified by motherbee in v1") instead of letting `status:ok` suggest a healthy uplink; the `note` makes this explicit. The actual transmission of the 4 fields to motherbee is still T-VER-1 (future), as the finding itself scopes it. *Validated on a VM: injecting an unreachable fluxbee.ai (RFC5737), the journal showed `egress NAT healthy but internet unreachable — DEGRADED`; when restored, it recovered.*

Verification: reading. **Rewritten**: the earlier version based the finding on `edge_ip`, a premise that turned out to be wrong. The real problem is a different one.

Correction of the premise: spec §8.2 (`docs/edge-egress-nat-spec.md:411-435`) never asks to assign `edge_ip` to `lan_iface`; `edge_ip` is pre-existing host config that the orchestrator uses to validate (`:3814-3819`), to report, and as the gateway of the workers' route. That `egress_nft_ruleset` (`:3891`) does not use `edge_ip` is spec-compliant, not a defect. That axis of the earlier version is discarded.

What is indeed a problem:

- `add_egress_hive_flow` replies `egress_nat_applied: true` (`:16027`) and **omits** the 4 fields of spec §9: `egress_ipv4_forwarding`, `egress_ipv6_blocked`, `egress_conntrack_tuned`, `egress_internet_reachable` (they do not exist as JSON keys anywhere; empty grep). The payload points to "see the egress host journal".
- The inference "orchestrator active => NAT applied" is valid for `nat_applied`/`ipv4_forwarding`/`ipv6_blocked` because `reconcile_egress` is fatal at bootstrap (`bootstrap_local:790-797`, CR-1). **But** `internet_reachable` (`reconcile_egress_nat:4046`) only `warn`s if it is false (`:4047-4052`); it does not return `Err`. An egress with its WAN uplink down stays `active`, registers in the LSA, and motherbee returns `status:ok egress_nat_applied:true`.
- `check_internet_reachable` (`:3979`) tests the real uplink, unlike `wait_for_wan` (`:16909`), which only checks the hive's freshness in the LSA SHM (the overlay control plane). The gates of `add_egress_hive_flow` do not check the uplink.

Contract/documentation:

- `docs/edge-egress-nat-spec.md:471-488` (§9): requires the 4 fields and "No silent success". §3.5 (`:70-72`): "Fail loud, never silent".
- `docs/history/onworking-coa/edge_egress_nat_tasks.md:105` (T-VER-1): in v1 those details stay in the egress journal and do not travel to motherbee. The tasks doc takes precedence over the spec (`:25`), so the **non-transmission** of the fields is a documented v1 carve-out, not a contradiction.

Impact:

An egress whose `wan_iface` resolves but has no route to the internet (miswired) reports `ok` while the node's core function (NATing to the internet) does not work; the only evidence is a `warn` in the remote journal. The omission of the payload fields is by v1 design; the real problem is that `internet_reachable=false` does not degrade the status.

Recommendation:

Decide whether `internet_reachable=false` should degrade the status to `warn`/`pending` (consistent with §3.5) instead of `ok`. Going forward (T-VER-1/T-VER-3), have `add_hive role=egress` collect the 4 real fields from the remote host and include them in the payload. See F16 for `conntrack_tuned`.

### F6 - Medium - The egress watchdog does not fix partial drift of rules/sysctl

> **RESOLVED (2026-06-26) — validated on a VM.** The egress tick re-applies the idempotent reconcile every `EGRESS_RECONCILE_DRIFT_TICKS` (12 ticks — ~60s at a 5s cadence), in addition to the `!nft_table_loaded()` fast-path. So the partial drift that the mere presence of the table does not detect (a flushed chain, an edited rule, sysctl/conntrack drift) is fixed in <~60s, not only at reboot. On a worker, the IPv6 block is idempotently re-ensured on the periodic tick. The reconcile (blocking I/O: ping+curl+nft) runs under `tokio::task::block_in_place` so it does not starve the runtime. *Validated on a VM: `nft flush chain ... forward` (partial drift, table present) → the 4 rules came back in <70s.*

Verification: reading.

Evidence:

- `watchdog_egress_reconcile`, Egress branch (`src/bin/sy_orchestrator.rs:1291-1297`): re-applies NAT only inside `if !nft_table_loaded()`.
- `nft_table_loaded` (`:3939-3946`) only runs `nft list table inet fluxbee_egress` and looks at the exit code: it checks the table's **presence**, not its content. A present table with flushed/edited rules passes as healthy -> no re-apply.
- `reconcile_egress_nat` is the only place that applies sysctl (`:4017`) and conntrack (`:4030`) and rewrites the rules (`:4036-4040`); the watchdog only invokes it after `!nft_table_loaded()`, so sysctl/conntrack drift is not fixed while the table exists.
- Worker branch (`:1299-1323`): only `default_route_via` + `ip route replace`. The worker's IPv6 block (`worker_ipv6_sysctl_content` + `apply_sysctl_system`) lives in `reconcile_worker_egress` (`:4095-4098`), which only runs at bootstrap, never on the tick. (This absorbs what the analysis listed as a separate finding about the worker's IPv6.)
- The watchdog is silent on partial drift: if someone flushes a chain, there is no `warn` and no re-apply until the next reboot.

Contract/documentation:

- `docs/edge-egress-nat-spec.md:343` (§6.5): "On startup and on reconcile ... apply the network configuration described in §8" (§8 = sysctl + conntrack + nftables) — a real tension with the gap.
- `docs/history/onworking-coa/edge_egress_nat_tasks.md:145` (CR-5): describes **exactly** this behavior and declares it intentional ("egress re-applies NAT only if the nft table disappeared"). It is a documented design decision, not an oversight. (The `:143`/CR-1 citation in the earlier version is imprecise: it is about bootstrap-fatal, not about the watchdog.)

Impact:

The reconciliation detects the complete disappearance of the table, but not partial drift of rules/sysctl/conntrack. Manual changes can stay active until the next reboot. Mitigating factor: the egress node is dedicated infra; a reboot re-runs the full reconcile.

Recommendation:

On the egress tick, besides `nft_table_loaded()`, compare the table's content with the expected ruleset (or idempotently re-apply `write_file_if_changed` + sysctl + conntrack every N ticks). On a worker, idempotently re-apply the IPv6 sysctl. `reconcile_egress_nat` is already idempotent, so running it periodically breaks nothing except the millisecond window of the delete+apply (see F25).

### F12 - Medium - Injection of nft rules via unvalidated `wan_iface`/`lan_iface`

> **RESOLVED (2026-06-26).** Closed by the same `validate_iface_name` as F7: the iface is embedded only in double-quoted nft strings, and the accepted charset excludes `"` and `\`.

Verification: **empirical** (PoC with serde_yaml 0.9 + a simulation of the ruleset).

Root cause shared with F7 and F13: no allowlist for the interface name.

Evidence:

- `egress_nft_ruleset` (`src/bin/sy_orchestrator.rs:3905,3910`) interpolates `iifname "{lan_iface}"` / `oifname "{wan_iface}"` raw via `format!` (`:3915-3916`).
- `reconcile_egress_nat` (`:4035-4040`) writes the ruleset to `EGRESS_NFT_PATH` and runs it with `nft -f`.
- On the remote egress host, `add_egress_hive_flow` (`:15842-15852`) writes `wan_iface`/`lan_iface` verbatim inside the YAML scalar; the remote orchestrator loads `hive.yaml` with `serde_yaml::from_str` into `EgressSection` (with no post-deserialization validation) and passes it to `reconcile_egress_nat`.
- PoC: a **literal** newline only corrupts the YAML (a provisioning DoS), but laundering through YAML escapes (`eth0\"\n ip saddr 0.0.0.0/0 accept #`) produces valid YAML that serde_yaml decodes back into a real quote + newline -> it breaks the nft quoting and leaves an injected rule in the forward chain that defeats the `policy drop`.

Impact:

Injection of firewall rules executed as root via `nft -f` on the egress host, voiding the `forward policy drop` guarantee (spec §8.2). Same threat model as F7 (motherbee/admin), not an unauthenticated remote attacker.

Recommendation:

The same fix as F7 (allowlist `^[A-Za-z0-9._-]{1,15}$` in `resolve_egress_nat_config`) closes this vector.

### F13 - Medium - YAML corruption/injection in the remote `hive.yaml`

> **RESOLVED (2026-06-26).** Closed by the same `validate_iface_name` as F7: the iface is embedded only as a double-quoted YAML scalar from `nat.*` (validated); the charset excludes `"`/`\` and the scalar-coercion triggers.

Verification: **empirical** (YAML simulation). Root cause shared with F7/F12.

Evidence:

- Template `add_egress_hive_flow:15842`: `wan_iface: "{wan_iface}"` / `lan_iface: "{lan_iface}"` with direct interpolation, no escaping. `write_remote_file` uses a `<<'EOF'` heredoc (no shell expansion) but preserves the literal `"` and `\n` in `/etc/fluxbee/hive.yaml`.
- The remote parses with `serde_yaml::from_str` -> `HiveFile` (`load_hive:3036-3039`) without `deny_unknown_fields`.
- Simulation: `wan_iface = eth0"\n  enabled: false\n  injected_key: "pwned` produces `egress.enabled=false` + a parseable injected key; a dedent produces `ParserError` (corrupt hive.yaml -> remote startup failure).

Correction relative to the raw analysis: `storage_path` is **not** attacker-controllable (it comes from `state.storage_path:15832`, the operator's config, not from the payload). Only `wan_iface`/`lan_iface` are the untrusted input.

Impact:

An `add_hive role=egress` with `"`+newline in the interface corrupts the remote `hive.yaml` (a startup DoS of the remote orchestrator) or injects/overwrites keys of the egress mapping. It is defense-in-depth / input validation, with a concrete DoS of the egress node.

Recommendation:

Same fix as F7. A single interface allowlist closes F7 + F12 + F13.

### F14 - Medium - `watchdog_tick` is not panic-safe

Verification: reading.

Evidence:

- `src/bin/sy_orchestrator.rs:715-728`: `compare_exchange(false,true)` on `watchdog_running` as a gate; a detached `tokio::spawn` with no `catch_unwind`/`AssertUnwindSafe` and no Drop guard; the `watchdog_flag.store(false)` reset (`:727`) runs only **after** `watchdog_tick(&...).await` (`:726`) returns. The only writers are `:720` (true) and `:727` (false); there is no recovery. The gate's rejection only logs via `tracing::debug!` (`:730`), below the default `info` level (`:551`).
- Panic vector: `node_name` (`:7204-7205`) does `&entry.name[..len]` with `name:[u8;256]` and `name_len:u16` up to 65535 (`shm/mod.rs:150-151`); `read_router_snapshot` copies `NodeEntry` verbatim with no clamp (`shm/mod.rs:2827-2828`). If `name_len>256` -> slice out-of-bounds -> panic, the task dies, `:727` never runs. See F20.

Impact:

A panic inside the spawned task skips the `store(false)`, and the flag stays `true` forever; every later `compare_exchange` fails -> service restarts, runtime verify/retain, blob GC and the egress reconcile are lost, silently (only `debug`). Self-healing stops while the orchestrator keeps reporting itself healthy. Mitigating factor: the normal writer clamps `name_len` to 256, so triggering it via `node_name` requires a corrupt SHM/torn read; but `watchdog_tick` does more panic-prone work.

Recommendation:

Move the reset into a Drop guard (or `catch_unwind`/`AssertUnwindSafe` around the tick's body), so that a panic does not disable the watchdog forever. Also apply the F20 clamp.

### F15 - Medium - The allowlist accepts an arbitrary hive suffix (cross-hive bypass)

> **RESOLVED (2026-06-26) — router-authority.** The router's gate (`system_origin_allowed`) explicitly distinguishes: `SY.orchestrator@<any-hive>` (legitimate cross-hive, forwards) vs `SY.admin`/`SY.wf-rules`/`WF.orch.diag` `@<same-hive>` (exact same-hive match). A `SY.admin@<other-hive>` is rejected. This closes the cross-hive bypass that F15 describes.

Verification: reading. Extends F4 with the cross-hive angle.

Evidence:

- `is_allowed_system_source_name` (`src/bin/sy_orchestrator.rs:1849-1853`): the only exact match (`system_allowed_origins.contains(name)`) is confined to the local hive (`load_system_allowed_origins:2797-2802` expands a bare name to `name@<hive_id>`), but the 4 `starts_with(...@)` accept any hive suffix.
- `src_l2_name` is authoritative (stamped by the router, not spoofable by the sender; test `router_overwrites_spoofed_src_l2_name`). But `normalize_name` (`src/router/mod.rs:3712-3718`) keeps verbatim a name that already carries `@<hive>`, without validating that the suffix is the local hive and without protecting reserved names; a node can register as `SY.admin@<foreign-hive>`.
- `vpn_allows_between` (`src/router/mod.rs:4974-4976`) lets SYSTEM through to a system node (the orchestrator) cross-hive.

Impact:

A legitimate `SY.admin`/`WF.orch.diag` from another hive, or a node that registers one of those prefixes with `@<arbitrary suffix>`, passes `is_allowed_system_source_name` and can run `SPAWN_NODE`/`KILL_NODE`/`NODE_CONFIG_SET` on the victim orchestrator, voiding the same-hive confinement that the operator set via `ORCH_SYSTEM_ALLOWED_ORIGINS`.

Recommendation:

The allowlist must be an exact match on the full `name@hive` name, without the prefix `starts_with` checks; if any local instance of `SY.admin`/etc. should be allowed, compare against `base@<hive_local>`. In addition, `normalize_name`/registration should reject a node that registers with a reserved `SY.*`/`WF.orch.diag*` prefix or with an `@<hive>` other than the local one. Existing tests (`:18391`, `:18408`) pin the prefix behavior and would need to be updated.

### F16 - Medium - `egress_conntrack_tuned` is reported `true` unconditionally

> **RESOLVED (2026-06-26) — validated on a VM.** `apply_conntrack_live()` now returns `bool`: it applies `sysctl -w` and **verifies by reading `nf_conntrack_max` back** (it tries the canonical key `net.netfilter.nf_conntrack_max` **and** the alias `net.nf_conntrack_max`, symmetric with the write). `conntrack_tuned = apply_conntrack_live()` → `true` = verified live, `false` = pending-reboot (the persisted file applies it at boot). *Validated on a VM: the journal showed exactly the finding's scenario — the first reconcile at bootstrap, with the module not loaded yet, reported `conntrack_tuned=false` (where the old code falsely claimed `true`), and the next reconcile, with the module already loaded, `true`.*

Verification: reading.

Evidence:

- `apply_conntrack_live()` (`src/bin/sy_orchestrator.rs:3929-3937`) returns `()` and swallows errors with `tracing::warn!` ("conntrack max not applied live"); the doc-comment (`:3927-3928`) acknowledges that the keys only exist once the `nf_conntrack` module is loaded.
- `reconcile_egress_nat` (`:4030-4031`): `apply_conntrack_live(); verification.conntrack_tuned = true;` unconditionally — there is nothing to check because the function returns `()`.
- The hashsize is only written to `/etc/modprobe.d/fluxbee-conntrack.conf` (`:4026-4029`) = reboot-only. There is no read of `nf_conntrack_count`/`nf_conntrack_max` to verify.

Contract/documentation:

- `docs/history/onworking-coa/edge_egress_nat_tasks.md` (T-NET-5): requires reporting `egress_conntrack_tuned`, distinguishing "live" from "pending-reboot". The code implemented the writes but not the distinction.
- `docs/edge-egress-nat-spec.md:443` (§8.4): "the connection tracking table fills and packets are dropped silently".

Impact:

On the common path (the `nf_conntrack` module is not loaded yet at reconcile time -> `sysctl -w` fails -> `warn`), the field still ends up `true`, overstating the state. Under sustained load before the reboot, this is the silent-drop scenario that §8.4 warns about. It is more a misreport than a guaranteed outage (the persisted file takes effect on the next boot).

Recommendation:

Turn `conntrack_tuned` into an enum/string (`"live"`|`"pending-reboot"`) or add `conntrack_live_applied`, verified by reading `nf_conntrack_max` after the `sysctl -w`, instead of the unconditional bool.

### F17 - Medium - The egress nft table is not loaded at boot

> **RESOLVED (2026-06-26) — validated on a VM (conclusive test).** New systemd unit `fluxbee-egress-nft.service` (oneshot, `RemainAfterExit`, `DefaultDependencies=no` + `Before=network-pre.target` → it loads the table **before** any interface is configured, so no packet is forwarded without the table). `ensure_egress_nft_boot_unit()` installs+enables it from the reconcile (the absolute path of `nft` is resolved; `enable` only when the unit is written, not on every tick; `ConditionPathExists` makes it a no-op if the `.nft` was removed after a teardown). The F11 teardown also runs disable+rm on it. *Validated on a VM **by disabling the orchestrator** and rebooting: with `sy-orchestrator inactive/disabled`, after the reboot the table was PRESENT (4 forward rules + masquerade), `ip_forward=1`, `conntrack=262144` — the boot unit loaded it (`active/success`), not the orchestrator. This closes the ip_forward-without-table window.*

Verification: reading.

Evidence:

- The `ip_forward=1` sysctl is persistent and independent of the orchestrator: it lives in `/etc/sysctl.d/99-fluxbee-egress.conf` (`:3703`, content in `egress_sysctl_content:3848`), and `systemd-sysctl` applies it early on every boot.
- The nft table is not: `EGRESS_NFT_PATH = /etc/nftables.d/fluxbee-egress.nft` (`:3706`); `reconcile_egress_nat` writes it and applies it with `nft -f` (`:4036-4040`), but nothing in the repo enables `nftables.service` or adds an include or a unit that loads that file at boot (grep in `src/` `scripts/` `*.service` `*.sh` = 0 references except the const and its runtime use).
- Timing: `reconcile_egress` (for egress) runs in `bootstrap_local:790`, **after** `systemd_start("rt-gateway")` (`:782`) + `wait_for_router_ready` up to 30s (`:783`) + `wait_for_nats_ready` up to 20s (`:784`). The `sy-orchestrator` unit is `After=network.target rt-gateway.service` (`:15883`). Window = from `systemd-sysctl` (early boot) to `:790` = seconds to ~30-50s, and it reopens on every `Restart=always`.

Impact:

Between `systemd-sysctl` applying `ip_forward=1` and `sy-orchestrator` running `reconcile_egress`, the egress host has IPv4 forwarding enabled **without** the `fluxbee_egress` table: no masquerade (packets with a private saddr go out to the WAN, egress broken), no `forward policy drop` and no IPv6 forwarding drop. The controlled boundary is absent for the whole startup window after every reboot. Mitigating factor: the window is transient and closes by itself; the upstream normally discards packets with a private saddr.

Recommendation:

Persist the table load at boot (`enable nftables.service` + an include of `/etc/nftables.d/fluxbee-egress.nft`, or an `ExecStartPre`), or invert the order so that `ip_forward` is only ON after the table is applied — tying forwarding to the orchestrator's cycle instead of persisting it independently.

### F18 - Medium - TOCTOU: `add_hive`/`remove_hive` take no lock

> **RESOLVED (2026-06-26).** `OrchestratorState` has a registry of locks **per hive_id** (`hive_topology_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>`); `lock_hive_topology(hive_id)` returns an `OwnedMutexGuard` that `add_hive_flow`/`add_egress_hive_flow`/`remove_hive_flow` hold for the **whole** flow (after validation, before the `exists`/`remove_dir_all`/`create_dir_all`/`write_hive_info`). The lock is **per-hive** (not global) so that SSH bootstraps of different hives are not serialized. It closes the window between the existence check, the deletion and the creation for the same hive_id. Unit test + review (GO: no deadlock/re-entrancy, full coverage of the window, RAII on every early-return, no other writers of `storage/hives` outside the lock).

Verification: reading.

Evidence:

- The dispatch of `add_hive` (`src/bin/sy_orchestrator.rs:1571`) and `remove_hive` (`:1546`) takes no lock. `runtime_lifecycle_lock` (`:481`) exists for updates, but it is not used here.
- `add_hive_flow` runs `hive_partial_exists` -> `remove_dir_all` -> `create_dir_all` (`:14760-14817`) with no serialization.

Impact:

A concurrent `remove_hive` on the same `hive_id`, or two `add_hive` in parallel, can corrupt `storage/hives` (a race between the existence check, the deletion and the creation). Combined with F3 (deletion before validation), the window is exploitable through timing. Mitigating factor: it requires concurrent admin commands on the same `hive_id`.

Recommendation:

Serialize by `hive_id` (a per-hive lock) or reuse `runtime_lifecycle_lock` for the whole `add_hive`/`remove_hive` lifecycle.

## Findings - Low

### F19 - Low - `list_hives` labels an egress host as `role=worker` and omits `role` on workers

Verification: reading.

Evidence:

- The synthesized local entry (`src/bin/sy_orchestrator.rs:7340-7351`) branches only on `is_motherbee`: `let local_role = if state.is_motherbee { "motherbee" } else { "worker" };`. `OrchestratorState` has `role: HiveRole` (`:610`) with an `as_str()` that already returns `"egress"` (`:199-205`), but it is not used. On an egress host `is_motherbee==false` -> it reports itself as `worker`.
- This branch is reached on non-motherbee hosts because `write_hive_info` is only invoked for the *target* (remote) hive, never for the local hive.
- The workers' `info.yaml` payloads omit `role` (`:14946-14951`), while the egress one includes `role:"egress"` (`:15946-15952`); `list_hives` echoes them back without normalizing -> a `GET /hives` response mixes workers without `role` with an egress with `role`.

Impact:

Inventory inconsistency: a consumer (admin UI, filters by role) sees an egress as `worker` and workers without `role`. Display metadata only; no flow/security control reads this field (the orchestrator uses `state.role`/`state.is_motherbee` internally).

Recommendation:

Use `state.role.as_str()` for the local entry, and add `"role":"worker"` to the worker `info.yaml` payloads (or default a missing `role` to `"worker"` in `read_hive_info`).

### F20 - Low - Unclamped SHM name slices (`node_name` and siblings)

Verification: reading.

Evidence:

- `src/bin/sy_orchestrator.rs:7204-7205`: `let len = entry.name_len as usize; let name_bytes = &entry.name[..len];` with no clamp. Identical siblings at `:5585-5586`, `:5690-5691`, `:10449-10450`, `:11061`. `name:[u8;256]`, `name_len:u16` (`shm/mod.rs:150-151`), so `name_len` can be up to 65535 at the type level.
- The read path does not clamp (`read_router_snapshot:2828` and `read_lsa_snapshot:2958` copy the raw entry). The correct pattern already exists in 4 places (e.g. `read_string:2855` uses `len.min(buf.len())`, the router's `bytes_to_string` `:4944`).

Impact:

A latent robustness bug, **not reproducible in correct operation**: the legitimate writer (router) clamps `name_len<=256` via `copy_bytes_with_len` (`shm/mod.rs:2288-2293`), and the seqlock covers torn reads. Triggering it requires a buggy/compromised/desynchronized SHM writer. It is the panic trigger of F14.

Recommendation:

Change it to `&entry.name[..(entry.name_len as usize).min(entry.name.len())]` or skip entries with `name_len>256`, ideally centralized in a helper that reuses `bytes_to_string`/`read_string`.

### F21 - Low - Egress host without inbound filtering (input chain `policy accept`)

Verification: reading. **Documented v1 trade-off** (not a surprise defect).

Evidence:

- `egress_nft_ruleset` (`src/bin/sy_orchestrator.rs:3896-3901`): `chain input { type filter hook input priority 0; policy accept; }` with no allow rules.
- `ensure_core_firewall_local` (`:3660-3666`): early-return on `role==Egress` ("skipping ufw"); it opens no port; worker/motherbee do set up ufw rules (`:3667-3691`).

Contract/documentation:

- `docs/edge-egress-nat-spec.md:64` (§3.3) and `:465` (§8.5) describe inbound filtering (only WAN + control SSH from motherbee) that the implementation does not meet.
- `docs/history/onworking-coa/edge_egress_nat_tasks.md:78` (T-NET-4): real inbound filtering is deferred to §11 to avoid risking a lockout on the testing box; in v1, `policy accept`.

Impact:

An intentional, documented spec-vs-code divergence. Bounded latent exposure: the path that is really exposed (FORWARD/NAT toward the WAN) is filtered (`policy drop` + only ct established/related + LAN->WAN + ip6 drop, `:3902-3911`); the `input accept` is on a host in an internal LAN.

Recommendation:

Align §3.3/§8.5 with the real v1 state (note that inbound filtering is non-v1), or add the minimal allows (WAN listen + SSH from motherbee) to the input chain with `policy drop` when the boundary is closed.

### F22 - Low - The egress flow lacks the worker's socket-first fast-path

Verification: reading. **Not destructive** (the "undoes restrictions" framing was refuted).

Evidence:

- The worker `add_hive_flow` has a socket-first fast-path (`:14819-14988`); `add_egress_hive_flow` does not, and runs the SSH bootstrap unconditionally.
- The key re-seed only happens via the password channel (`:15729-15745`), and the restriction/hardening is applied only at `:15989`, after `write_hive_info` and after the gates that return early. In the partial case (interrupted before `info.yaml`) there is no earlier restriction to undo. The steps are idempotent (sudoers rewritten from a template, authorized_keys deduplicates, `systemctl restart`).
- Documented absence: `edge_egress_nat_tasks.md` T-ROLE-9 lists the helpers that `add_egress_hive_flow` reuses, and the socket-first ones are not on the list.

Impact:

Lack of parity: re-provisioning an online egress restarts `rt-gateway`/`sy-orchestrator` (restart, not wipe) instead of short-circuiting via the socket. The normal case (with `info.yaml`) is protected by `HIVE_EXISTS`. Worst case: a service restart in a very narrow window.

Recommendation:

Add the socket-first fast-path to the egress flow, for parity/idempotency with the worker. An improvement, not a security bug.

### F23 - Low - `ipv4_forwarding`/`ipv6_blocked` are set `true` without a kernel read-back

Verification: reading. Informational.

Evidence:

- `reconcile_egress_nat:4017-4019`: `apply_sysctl_system()?; verification.ipv4_forwarding = true; verification.ipv6_blocked = true;`. `apply_sysctl_system` (`:3921-3925`) only runs `sysctl --system` and looks at the exit code; it does not read `/proc/sys`. Same pattern in `reconcile_worker_egress:4096-4098`.
- Asymmetry: `nat_applied` does get verified with a real read-back (`nft_table_loaded:4041-4044`).

Impact:

`sysctl --system` exit 0 does not guarantee the live value (a higher-priority file, a rejected key, `disable_ipv6` not honored on ifaces that are already up). But the flags do not reach the operator (only the journal; they are not contract surface), and the load-bearing control for the IPv6 leak on the egress host is the nft rule `meta nfproto ipv6 drop` (`:3906`), which is verified. A logging-precision gap, not a security one.

Recommendation:

Read back `net.ipv4.ip_forward` and `disable_ipv6` before setting the flags, for consistency with `nat_applied`. Optional.

### F25 - Low - The nft re-apply does a non-atomic delete-then-apply

Verification: reading. The dangerous failure mode **is not reachable** (defense in depth).

Evidence:

- `reconcile_egress_nat:4037` runs `delete_egress_nft_table_if_present()?` before `nft -f` (`:4039-4040`), and `egress_nft_ruleset` includes no `delete table`/`flush` of its own, so the separate pre-delete makes the operation non-transactional.
- But the only callers are bootstrap (`:790`, fatal/loud crash-loop on egress) and the watchdog (`:1294`). The watchdog only invokes `reconcile_egress_nat` inside `if !nft_table_loaded()` (`:1292`): when the table is already absent. In that state `delete_egress_nft_table_if_present` (`:3962-3965`) does `if !nft_table_loaded() { return Ok(()) }` = no-op. So the watchdog never deletes a healthy table that is present.

Impact:

The non-atomicity is real as a quality observation, but the "silent partial on a live host" does not materialize: in the watchdog the delete is a guarded no-op and the recurring cycle retries; at bootstrap the failure is loud. The only dangerous state requires an extreme TOCTOU race between `:1292` and `:3963`.

Recommendation:

Prepend `delete table inet fluxbee_egress` inside the `-f` file itself (nft's implicit transaction) to make it atomic, and remove the separate pre-delete. A robustness improvement.

## Coverage gaps (not audited in depth)

- **v2 payload parsing**: `resolve_add_hive_role`, `resolve_add_hive_egress_section`, `resolve_add_hive_dist_sync_probe_timeout_secs`, `address` coercion (`unwrap_or_default`), range/limits of `dist_sync_probe_timeout_secs`/`harden_ssh`/`restrict_ssh`. Only `hive_id` (F3) was reviewed, and the ifaces partially (F7/F12/F13).
- **`info.yaml` persistence/atomicity**: `write_hive_info`/`read_hive_info`/`append_*_history`, and whether a partial write can leave an inconsistent `hive_partial` (central to F3/F9).
- **Signals/shutdown**: `shutdown_sequence` (`:1329`) sleeps a fixed 10s and does not interact with in-flight `add_hive`/`remove_hive` operations; behavior on SIGTERM midway through an SSH bootstrap or a `remove_dir_all`.
- **Observability**: `append_*_history` records `ok` even when there is an SSH fallback (F1/F2). It is worth auditing that the degraded paths stay traceable.
- **Network error classification**: `is_socket_only_unreachable_error` (`:14586`) decides fall-back-to-bootstrap vs `FINALIZE_FAILED` (F2) by substring matching; it deserves a dedicated test.
- **Egress bootstrap by password**: `add_egress_hive_flow` uses `ssh_with_pass_any` as the primary channel and seeds the key with `apply_remote_unrestricted_authorized_key_with_pass`; that password-first path and how it differs from the worker were not audited.

## Missing test coverage

- Concurrency: `add_hive`/`remove_hive` on the same `hive_id` (or two parallel `add_hive`) that detects the TOCTOU `hive_partial_exists` -> `remove_dir_all` -> `create_dir_all` (F18).
- Traversal: an invalid `hive_id` and `address` must not trigger `hive_partial_exists`/`remove_dir_all`/`create_dir_all` (F3); cover worker **and** egress.
- Injection: `wan_iface`/`lan_iface` with shell/nft/YAML metacharacters must be rejected in `resolve_egress_nat_config` (F7/F12/F13).
- Allowlist (F4): with `ORCH_SYSTEM_ALLOWED_ORIGINS=SY.admin`, `WF.orch.diag@<hive>` and `SY.wf-rules@<hive>` must be rejected. **Requires changing the existing test `:18391`** (which today asserts the opposite), not just adding the negative case.
- Cross-hive (F15): `SY.admin@<foreign-hive>` must be rejected.
- ADMIN gate (F8): `ADMIN_COMMAND` from a non-allowed origin -> FORBIDDEN; positive case from `SY.admin@<hive>`.
- `add_hive` socket-only (F2): if `socket_only_ready=true`, any probe/finalize failure ends without touching the SSH bootstrap; a test of `is_socket_only_unreachable_error`.
- `remove_hive` (F1): a unit test guaranteeing that online it replies exactly `socket_ok`/`socket` (no SSH) and offline `local_only`; run `orchestrator_remove_hive_socket_e2e.sh` in the lab to close the loop.
- Idempotency (F9): a retry on a `pending` hive must complete the finalize, not return `HIVE_EXISTS`.
- Egress (F5/F16): a unit test of the payload shape vs spec §9 (separate from the two-NIC reachability E2E); `conntrack_tuned` must distinguish live from pending-reboot.
- Drift (F6): with `nft_table_loaded()=true` but altered content, the reconcile must re-apply.
- panic-safety (F14): a panic in `watchdog_tick` must not leave `watchdog_running=true` permanently.

## Suggested fix order

1. **Validate `wan_iface`/`lan_iface`** in `resolve_egress_nat_config` (closes F7 RCE + F12 nft + F13 YAML in one stroke) and rewrite `write_remote_file` so it does not build shell commands by interpolation.
2. **Origin gate on `ADMIN_COMMAND`** (F8).
3. **Validate `hive_id` at the start** of `add_hive`/`add_egress` + canonicalize deletes (F3), and **lock per `hive_id`** (F18).
4. **Remove SSH from `remove_hive`** (F1) and **block the SSH fallback** once socket-only has been chosen in `add_hive` (F2).
5. **Make `add_hive` idempotent** (F9): a retry on `pending` completes the finalize.
6. **Fatal egress SSH hardening** (F10) and **NAT teardown in egress `remove_hive`** (F11).
7. **Effective, same-hive allowlist** (F4 + F15): no hardcoded prefixes, exact `name@hive` match.
8. **panic-safe watchdog** (F14) + **SHM slice clamp** (F20).
9. **Egress verification/persistence** (F5 internet_reachable, F16 conntrack, F17 load at boot, F6 drift).
10. Remaining Low findings (F19, F21, F22, F23, F25) as capacity allows.

## False positives discarded

Adversarially verified and discarded as non-bugs (documented so they do not reappear):

- **`remove_hive` local-only with an offline worker returns `status:ok`**: this is the deliberate, documented local-only degradation of the v2 contract (`sy_orchestrator_v2_tasks.md:21`, E2E-3), with a dedicated E2E gate that requires exactly `status:ok` + `remote_cleanup_via=local_only` and an explicit signal via `remote_cleanup`. The only legitimate observation (UX: the top-level `ok` can hide the degradation; there is no reconciliation of orphaned workers) is a matter for a design discussion, not a code bug.
- **A syncthing peer-link/dist_sync failure after a successful finalize does not roll back the remote finalize**: it causes no double provisioning and no inconsistency. Both the worker's finalize (`compute_local_core_update_sets`) and the mother's peer-link (`reconcile_syncthing_peer_xml`) are idempotent; the retry comes in through the idempotent socket fast-path, not through the SSH bootstrap; `hive_partial_exists`+`remove_dir_all` cleans up the empty local dir. Real residue: observability (there is no `worker_already_finalized` field), cosmetic.
