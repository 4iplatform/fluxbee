# sy.orchestrator audit - third pass after remediation

Date: 2026-07-01  
Commit reviewed: `fa08233` (`daily_onworking_coa`)  
Base: follow-up of [`2026-06-30-sy-orchestrator-audit.md`](./2026-06-30-sy-orchestrator-audit.md), after its 11 items were marked as resolved.  
Focus: a fresh look at `src/bin/sy_orchestrator.rs` and the immediate boundary of `system_policy`, looking for regressions or new problems introduced by the fixes.

## Executive summary

The second pass found no regressions in the critical fixes for unit ownership, the local `SYSTEM` guard, per-node locks, 0600 permissions, or nftables idempotency. `cargo test --bin sy_orchestrator` passes with 109/109 tests.

3 new/residual issues did appear:

1. **Medium/High:** worker `add_hive` still interpolates `blob/dist` paths into the remote shell and into YAML, with incomplete quoting/validation coverage.
2. **Medium:** `run_node` still leaves identity/ILK/mapping persisted if it fails after the registration, especially `CONFIG_WRITE_FAILED` and `SPAWN_FAILED`.
3. **Low/Medium:** the cleanup of a failed SSH bootstrap depends on `sudo -n`; on sudo setup failures the key can stay open, even though it could be removed as a normal user.

## Findings

### SO-2026-07-01-01 - Medium/High - Worker `add_hive` interpolates `blob/dist` paths into shell/YAML without full coverage

Status: Resolved in the 2026-07-02 remediation

Evidence:

- `blob_runtime_from_hive` and `dist_runtime_from_hive` copy `blob.path`, `blob.sync.data_dir`, `blob.sync.tool`, `dist.path` and `dist.sync.tool` from `hive.yaml` with `trim`/lowercase, but without global validation of absolute paths, control chars, quote/backslash or shell metachars (`src/bin/sy_orchestrator.rs:3379-3499`).
- In the worker provisioning, the remote `mkdir -p` interpolates `state.blob.path`, `blob_active_dir`, `blob_staging_dir` and `state.blob.sync_data_dir` inside hand-written single quotes, without `shell_single_quote` (`src/bin/sy_orchestrator.rs:16102-16113`).
- The same file already has a quoting helper (`shell_single_quote`, `src/bin/sy_orchestrator.rs:14340-14342`) and uses it in other remote commands, which leaves this block as an outlier.
- The B-3 loop of `validate_yaml_scalar` for workers validates `hive_id`, uplink, `storage.path` and identity, but does not validate `desired_blob.path`, `desired_blob.sync_tool`, `desired_blob.sync_data_dir`, `desired_dist.path` or `desired_dist.sync_tool` (`src/bin/sy_orchestrator.rs:16175-16192`).
- Those unvalidated fields are written into the worker's `hive_yaml`, several of them inside double-quoted YAML scalars (`src/bin/sy_orchestrator.rs:16194-16213`).

Impact:

In normal operation these values are operator-owned, but they are configuration inputs that end up as a remote root command during `add_hive`. A single quote or a control character in a path can break the bootstrap. If someone controls the local `hive.yaml`, they can also turn it into shell injection on the worker. Also, a value with `"`/`\`/newline in the omitted fields can produce a malformed or semantically different remote `hive.yaml`.

Recommendation:

- Do not build the `hive.yaml` with `format!` for structured data: render it with `serde_yaml` or with a builder that escapes scalars.
- Validate every field of `BlobRuntimeConfig` and `DistRuntimeConfig` that goes into shell/YAML: absolute paths, no control chars, and an explicit policy for quote/backslash.
- For remote commands, build the list of paths and apply `shell_single_quote` to each argument, or avoid the shell where feasible.
- Add tests with `blob.path`, `blob.sync.data_dir` and `dist.path` containing `'`, `"`, `\` and newline, verifying rejection before SSH/render.

### SO-2026-07-01-02 - Medium - `run_node` keeps identity/ILK if it fails after the registration

Status: Resolved in the 2026-07-02 remediation

Evidence:

- `ensure_node_identity_registered` registers in `SY.identity` and persists `node_name -> ilk_id/tenant_id` locally before any config or process exists (`src/bin/sy_orchestrator.rs:11395-11425`).
- `run_node_flow` does the identity register/update and only then calls `ensure_node_effective_config_on_spawn` (`src/bin/sy_orchestrator.rs:12815-12890`).
- If the config write fails, the flow returns `CONFIG_WRITE_FAILED` with the `identity` block, but reverts neither the mapping nor the ILK (`src/bin/sy_orchestrator.rs:12869-12890`).
- If `systemd-run` fails, the best-effort rollback deletes only `config_path` (`src/bin/sy_orchestrator.rs:12947-12952`).
- Mapping/ILK removal exists in the explicit teardown/purge paths (`src/bin/sy_orchestrator.rs:13526-13555`, `src/bin/sy_orchestrator.rs:13780-13830`), not as an automatic rollback of the failed spawn.

Impact:

The retry is no longer blocked by `NODE_ALREADY_EXISTS`, which was the main bug of the previous audit. But an ILK/mapping can still be left behind for a node that never started. And if the payload asked for an identity/channels update, that mutation stays applied even though the process does not exist. This pollutes the identity inventory/audit trail and forces a manual cleanup or a retry that reuses the partial state.

Recommendation:

- Model the spawn as a transaction or as an explicit `pending_spawn` state: reserve the identity, write the config, start the unit, and only then mark the identity active.
- If the ILK was created in this attempt, roll back the mapping and request the ILK delete on `CONFIG_WRITE_FAILED` and `SPAWN_FAILED`. If the ILK already existed, do not delete it; mark the case as resume/retry.
- Add tests with an injected failure of `write_json_atomic` and of `execute_on_hive` that verify the cleanup or an observable `pending_spawn` state.

### SO-2026-07-01-03 - Low/Medium - Cleanup of a failed SSH bootstrap depends on `sudo -n`

Status: Resolved in the 2026-07-02 remediation

Evidence:

- Worker and egress can seed the motherbee key over password before configuring sudoers (`src/bin/sy_orchestrator.rs:15956-15967`, `src/bin/sy_orchestrator.rs:16757-16768`).
- If `ensure_remote_orchestrator_sudoers_with_access` fails, the flow returns `SUDO_SETUP_FAILED` (`src/bin/sy_orchestrator.rs:15971-15981`, `src/bin/sy_orchestrator.rs:16770-16777`).
- The admin wrapper adds `ssh_bootstrap_open` on errors by calling `best_effort_revoke_bootstrap` (`src/bin/sy_orchestrator.rs:2047-2056`, `src/bin/sy_orchestrator.rs:2079-2089`).
- That best-effort call invokes `revoke_bootstrap_ssh_access`, which wraps the whole script in `sudo_wrap` (`src/bin/sy_orchestrator.rs:17839-17868`). If the failure was precisely the sudo setup, it probably cannot run the cleanup, even though the key allows a user login.
- The password seeding path edits `~/.ssh/authorized_keys` as a normal user, without sudo (`src/bin/sy_orchestrator.rs:17815-17825`), so a partial cleanup is possible that today is not attempted.

Impact:

The current fix at least reports `ssh_bootstrap_open=true`, so it is not silent. Even so, it leaves a login key open in a class of failure where it could be cleaned up without root privileges. Sudo/root access does not necessarily remain, but SSH access to the bootstrap user does remain until a manual cleanup.

Recommendation:

- Split the cleanup into two phases: remove the `authorized_keys` entry as the remote user (by key and, if there is a password, by password fallback), and then remove sudoers only if `sudo -n` works.
- Report separate flags, for example `ssh_key_removed`, `sudoers_removed`, `ssh_bootstrap_open`.
- Add a test/unit seam for the `SUDO_SETUP_FAILED` post-key-seed case.

## No regressions observed

- `run_node` and `kill_node` now derive the unit from `node_name` and do not accept arbitrary units from the caller (`src/bin/sy_orchestrator.rs:12656-12660`, `src/bin/sy_orchestrator.rs:13376-13390`).
- `handle_system_message` has a local defensive guard that uses the shared `system_policy` policy (`src/bin/sy_orchestrator.rs:2148-2189`; `src/router/system_policy.rs:25-88`).
- There are per-`node_name` locks in local and remote config/lifecycle (`src/bin/sy_orchestrator.rs:12293-12295`, `src/bin/sy_orchestrator.rs:12605-12608`, `src/bin/sy_orchestrator.rs:13222-13235`, `src/bin/sy_orchestrator.rs:13388-13418`, `src/bin/sy_orchestrator.rs:13704-13706`).
- `write_secret_file_0600` re-asserts the permissions after writing, which covers pre-existing files with a broad mode (`src/bin/sy_orchestrator.rs:825-843`).
- The nft `add table` + `flush table` application was verified twice in a throwaway `fluxbee-lab:latest` container with nftables v1.0.9: the second application did not fail. The suspected finding on SO-08 is not kept.

## Verification run in the initial audit

- `cargo test --bin sy_orchestrator`: **109 passed / 0 failed**.
- Isolated check in Docker: `nft -f` applied twice on a test table with `add table` + `flush table`: **OK** (`nftables v1.0.9`).

## Gaps of the initial audit

- No real E2E on Proxmox and no full worker/egress bootstrap were run during this pass.
- Injected IO/sudo/SSH failures were not tested in a harness; the findings come from reading the flow and from static evidence.
- No fuzzing of `hive.yaml` or of `add_hive`/`run_node` payloads was done.

## Original suggested remediation order

1. Close the worker `add_hive` renderer/quoting (`blob/dist` in shell + YAML), because it combines operational impact with a remote injection surface.
2. Define transactional identity semantics for a failed `run_node`: a real rollback or a `pending_spawn` state.
3. Improve the SSH cleanup of a failed add_hive with a user-level removal of `authorized_keys` before depending on sudo.

## Remediation applied and empirical verification - 2026-07-02

Current status: the three original findings were fixed in code, and the real worker `add_hive` flow was validated on Proxmox.

Changes applied:

- **SO-2026-07-01-01:** `blob/dist` validation was added before rendering/provisioning workers (`blob.path`, `blob.sync.data_dir`, `blob.sync.tool`, `dist.path`, `dist.sync.tool`), and the remote paths are quoted with per-argument shell quoting.
- **SO-2026-07-01-02:** `run_node` now keeps `requested_ilk_id`, detects when the ILK was created in the current attempt, and does a best-effort rollback of mapping/ILK on `CONFIG_WRITE_FAILED` or `SPAWN_FAILED`.
- **SO-2026-07-01-03:** the SSH bootstrap cleanup now separates `authorized_keys` from sudoers, tries a user-level removal by key/password fallback, and reports `ssh_key_removed`, `sudoers_removed` and `ssh_bootstrap_open`.

New empirical findings during the remediation:

- **SO-2026-07-02-01 - Resolved - `sudo_wrap` did not elevate whole compound commands.** On Proxmox, `add_hive` failed while distributing the identity HMAC key: `chmod: changing permissions of '/var/lib/fluxbee/identity/keys': Operation not permitted`. Cause: `sudo_wrap("mkdir ... && chmod ...")` ran only the first command under sudo; the `chmod` ran as the bootstrap user. `sudo_bash_wrap` was added and applied to the TLS and identity HMAC mkdir/chmod.
- **SO-2026-07-02-02 - Resolved - `fluxbee-firstboot` could finish without the hive ready.** On a clean VM with the previous package, `vault_put` worked, but `sy-storage` started degraded because it queried vault during a disconnection window, and `sy-orchestrator` kept restarting with `STORAGE_NOT_READY`. `packaging/fluxbee-firstboot` was hardened with `wait_hive_ready`, vault/storage/orchestrator reconnection retries, and a non-zero exit if `/hives` does not reach `status:ok`.

Verification run:

- `cargo test --bin sy_orchestrator`: **114 passed / 0 failed**.
- `bash -n packaging/fluxbee-firstboot`: **OK**.
- Linux Docker build with `fluxbee-lab:latest`: **OK**, package generated `dist/fluxbee_0.1.0-auditfix2_amd64.deb`.
- SHA256 of the `auditfix2` package: `e27305c4eb629cda1883e021c9598c4f6cbe240df118bff0d0e005e4b3c2a03c`.
- Proxmox via `lab/pve.py` on node `sandbox`:
  - VM 211 `fb-audit-mb`: installed `fluxbee 0.1.0-auditfix2`, core services active, `/hives` returns `status:ok`.
  - VM 212 `fb-audit-wk`: empty worker prepared with SSH/PostgreSQL.
  - `POST /hives` for `worker-audit` against `192.168.103.146`: **OK**.
  - `add_hive` response: `wan_connected=true`, `orchestrator_connected=true`, `dist_sync_ready=true`, `syncthing_peer_linked=true`, `ssh_bootstrap_revoked=true`.
  - Inventory: `worker-audit` shows as `alive` with 8 active SY nodes.
  - Worker: `rt-gateway`, `sy-orchestrator`, `sy-identity`, `sy-config-routes`, `sy-opa-rules`, `sy-cognition`, `sy-policy`, `sy-timer`, `sy-wf-rules` and `fluxbee-syncthing` active.
  - Remote HMAC key: `/var/lib/fluxbee/identity/keys` ends up `0700 root:root`; `/var/lib/fluxbee/identity/keys/worker-audit.key` ends up `0600 root:root`.

Remaining gaps:

- A third motherbee was not re-cloned just to test `fluxbee-firstboot` of `auditfix2` from scratch. The race was reproduced with the previous package; then the script was fixed, its syntax was validated, `auditfix2` was installed, and the installed version was verified to contain the fixed helper.
- `run_node` E2E was not run with an injected `systemd-run` failure; the rollback coverage stayed at unit level/flow reading.
