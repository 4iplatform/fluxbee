# SY.opa.rules

SY.opa.rules manages the user routing policy: one Rego policy for the whole mesh, compiled to
WebAssembly on the motherbee and installed on every hive. Each hive's SY.opa.rules writes the
compiled policy into the hive's OPA shared-memory region, where the routers of that hive load it.

This is the **user** layer only. The **system** policy (who may send protected system actions)
is baked into the router binary from `policy/system.rego` and SY.opa.rules never touches it
(see [rt-gateway](rt-gateway.md)).

Source: `go/sy-opa-rules/main.go` (node, compile, apply), `go/sy-opa-rules/distribution.go`
(publication to other hives). Admin side: `handle_opa_http` in `src/bin/sy_admin.rs`. Router
side: `maybe_refresh_opa_from_shm`, `apply_opa_reload`, `resolve_target_with_identity` in
`src/router/mod.rs`.

| | |
| --- | --- |
| Binary | `/usr/bin/sy-opa-rules` (`packaging/build-deb.sh`, `GO_BINS` entry `sy-opa-rules:go/sy-opa-rules`) |
| Node name | `SY.opa.rules@<hive_id>` |
| Role | every role: motherbee, worker, ingress, egress |
| systemd unit | `sy-opa-rules.service` (`Restart=always`, `RestartSec=5`) |
| Listens on | the local router socket only; no TCP port |
| Writes | POSIX shared memory `/jsr-opa-<hive_id>` (0600) |
| State | `/var/lib/fluxbee/opa/{current,staged,backup}/`; on the motherbee also `/var/lib/fluxbee/dist/policy/opa/` |

## Where it runs

`SY.opa.rules` is in the `system_nodes` list of all four roles in `/etc/fluxbee/hive.yaml`; it
is in the `wait_for` list of motherbee and worker only. It computes its own ILK
deterministically (`DeterministicSystemIlkID`), so it also starts on ingress and egress hives,
which have no SY.identity.

The motherbee's instance is the primary (`isPrimary`: `hive_id == "motherbee"`): only it
compiles, applies and publishes. Every other instance only installs what the motherbee
published; a write sent to it is refused with `NOT_PRIMARY` (`handleOpaAction`).

## How it starts

`main` in `go/sy-opa-rules/main.go`:

1. With arguments `compile-file <rego> <entrypoint> <out.wasm>` it only compiles a file and
   exits. This is how the baked system policy is built (header of `policy/system.rego`).
2. Reads `hive_id` from `/etc/fluxbee/hive.yaml`.
3. Creates `/var/lib/fluxbee/opa/{current,staged,backup}` with mode 0700 (`ensureDirs`).
4. Takes an exclusive `flock` on `/var/run/fluxbee/sy-opa-rules.lock`; a second instance exits.
5. Loads or creates its UUID in `/var/lib/fluxbee/state/nodes/SY.opa.rules.uuid` and opens or
   creates the region `/jsr-opa-<hive_id>`: a 192-byte header plus room for a 4 MiB wasm
   (`openOrCreateOpaRegion`, `opaMaxWasmSize`). A region left 0666 by an older build is set to
   0600.
6. Loads `current/policy.wasm`, if any, into the region (`loadCurrentPolicy`), so every hive
   starts on its last policy whether or not the motherbee is reachable.
7. On the motherbee, publishes the current policy again (`publishPolicy`).
8. Starts the sync loop (`policySyncLoop`), the router connection (`runRouter`) and a 5 s region
   heartbeat (`heartbeatLoop`).

## Policy files

Each slot directory holds `policy.wasm`, `policy.rego` and `metadata.json` (`PolicyMetadata`:
`version`, `hash` = `sha256:` of the wasm, `entrypoint`, `compiled_at`, `wasm_size_bytes`,
`error_detail`). Files are 0600. The default entrypoint is `router/target`
(`defaultEntrypoint`).

- **compile**: Rego is compiled with the OPA Go compiler (`compileRego`), checked (valid wasm,
  `opa_eval` export, at most 4 MiB) and written to `staged/`.
- **check**: compiled and checked the same way (`compileRego` works in a temporary directory it
  removes) and answered. Nothing is written: not `staged/`, not the region, not `last_error`; no
  version is involved.
- **apply**: `current/` is copied to `backup/`, `staged/` becomes `current/`, the wasm goes
  into the region and an `OPA_RELOAD` broadcast is sent (`applyPolicy`). An explicit version
  must match the staged one (`VERSION_MISMATCH`); nothing staged is `NOTHING_STAGED`.
- **rollback**: `backup/` is written over `current/` and loaded (`rollbackPolicy`;
  `NO_BACKUP` if there is none).
- **clear**: `current/`, `staged/` and `backup/` are emptied, the region is set to "no policy"
  and `OPA_RELOAD` is broadcast with version 0 (`clearPolicy`).

## Distribution to the other hives

`distribution.go`:

- After every apply, rollback or clear, the motherbee writes the wasm and a manifest to
  `/var/lib/fluxbee/dist/policy/opa/`: first `policy-<first 16 hex of the hash>.wasm`, then
  `manifest.json` (`version`, `hash`, `entrypoint`, `wasm_file`, `compiled_at`,
  `published_at`; `hash: ""` means no policy). Older wasm files are deleted. Only the wasm
  travels; the Rego stays on the motherbee.
- That folder reaches the other hives by Syncthing (folder `fluxbee-dist-policy`,
  `OPA_POLICY_SYNC_FOLDER` in `src/bin/sy_admin.rs`).
- Every 5 s (`policySyncInterval`), and every 1 s for 30 s after a notice, each hive's
  instance runs `syncOnce`:
  - a non-primary hive installs the published policy when its hash differs from what it runs
    (`syncFromPublishedPolicy`): it checks the wasm's sha256 against the manifest, stages it
    and applies it. A manifest with an empty hash clears the policy. A wasm that has not
    arrived or does not match is retried; after a minute the node logs that it is still
    waiting (`policyStuckAfter`);
  - the motherbee publishes again if the published manifest does not name what it runs
    (`republishIfStale`);
  - every hive repairs its region if it does not carry the installed policy
    (`ensureRegionMatchesCurrent`).

## How a write reaches every hive

The only write path is SY.admin's `/opa/policy*` routes; `CONFIG_SET` on SY.opa.rules is
refused (`UNSUPPORTED_OPERATION`, `handleNodeConfigSet`). `handle_opa_http`:

1. Refuses a write that names a hive other than the motherbee (`INVALID_REQUEST`: "there is one
   user policy for every hive").
2. Picks the version (a check takes none, see below): `next_opa_version` keeps a counter in
   `/var/lib/fluxbee/opa-version.txt`; an explicit version must be greater than the current one.
   Apply requires `version`.
3. Sends `CONFIG_CHANGED` (`subsystem: opa`, the action, `rego`, `entrypoint`, `hive:
   motherbee`) to `SY.opa.rules@motherbee` and waits up to 30 s (`OPA_PRIMARY_WAIT_SECS`) for
   its `CONFIG_RESPONSE`. For compile + apply it sends `compile`, then `apply`.
4. Once the motherbee applied it, asks the motherbee's orchestrator for a `SYSTEM_SYNC_HINT`
   scan of the policy folder (best effort, 5 s), then broadcasts `CONFIG_CHANGED` with action
   `sync` and the hash to `SY.opa.rules@*`, and collects the hives' answers for up to 10 s
   (`OPA_SYNC_WAIT_SECS`). A hive answers once it runs that hash (`handleSyncNotice`,
   `answerPendingSyncs`; a notice expires after 5 minutes).
5. Answers with the hives that run it, the ones still pending, the pending ones it cannot
   reach, and `converged`. A pending hive is not a failure: it installs the policy when the
   file arrives.

A check (`POST /opa/policy/check`, `opa_check`) is not a write, so a check run between a
compile and its apply leaves the staged policy alone. SY.admin takes no version for it (the
counter is not touched) and sends `CONFIG_CHANGED` with action `check`, `version: 0` and
`config: {rego, entrypoint, check_id}`, a fresh id per check. Only the `CONFIG_RESPONSE` that
echoes that `check_id` counts (`send_opa_check`); the answer reports no version, and nothing is
scanned or announced.

## Messages

| Message | Who may send | What it does |
| --- | --- | --- |
| `CONFIG_CHANGED` (`SYSTEM`, `subsystem: opa`) with `compile`, `compile_apply`, `check`, `apply`, `rollback`, `clear` | `SY.admin@motherbee` only | the operations above, on the motherbee; answered with `CONFIG_RESPONSE` (a check's answer echoes its `check_id`) |
| `CONFIG_CHANGED` with action `sync` | `SY.admin@motherbee` only | "a new policy was published": answered when this hive runs that hash |
| `query` `get_policy` | not checked | current version, Rego (empty on non-primary hives), hash, entrypoint |
| `query` `get_status` | not checked | the status view below |
| `CONFIG_GET` | router policy: `SY.admin@motherbee` | current/staged/backup metadata and the routers' load status; `contract.policy_writes` points to `/opa/policy*` |
| `CONFIG_SET` | router policy: `SY.admin@motherbee` | always refused, `UNSUPPORTED_OPERATION` |
| `NODE_STATUS_GET` | router policy | answered `HEALTHY` |

The origin checks compare the router-stamped `routing.src_l2_name` with `PrimaryAdminNode`
(`handleMessage`). A refused `CONFIG_CHANGED` is answered `UNAUTHORIZED`
(except `sync`, which is only logged). `CONFIG_CHANGED`, `CONFIG_GET` and `CONFIG_SET` are also
protected system actions: the router delivers them only from `SY.admin@motherbee` (rule 2 of
`policy/system.rego`).

Error codes in answers: `COMPILE_ERROR`, `NOTHING_STAGED`, `VERSION_MISMATCH`, `NO_BACKUP`,
`SHM_ERROR`, `PUBLISH_FAILED` (applied on the motherbee, not published; retried every 5 s),
`NOT_PRIMARY`, `UNAUTHORIZED`.

### Status view

`get_status` (`statusView`): `current_version`, `current_hash`, `staged_version`,
`published_version`, `published_hash`, `region_hash`, `in_sync` (published hash = installed
hash = region hash), `routers` (each router's loaded policy version and load status, read from
its region `/jsr-<router uuid>`, `listRouterStatuses`), `last_error`, and `waiting` (version,
hash, since, reason) when the hive is behind.

## What the routers do with it

- A router reads `/jsr-opa-<hive>` on its heartbeat (`maybe_refresh_opa_from_shm`) and on an
  `OPA_RELOAD` broadcast, and loads the wasm when the version changed; a region with version 0
  and no wasm unloads the policy.
- The user policy picks the target of messages sent without a destination
  (`Destination::Resolve`): first the identity pre-resolve (the frontdesk rule of the system
  policy), then the user policy (`resolve_target_with_identity`). Without a loaded policy, or
  on an evaluation error, the sender gets `UNREACHABLE` with reason `OPA_ERROR`; a policy that
  yields no target gives `OPA_NO_TARGET`.

## SY.admin HTTP routes

On the motherbee (`127.0.0.1:8080`):

| Route | Action |
| --- | --- |
| `POST /opa/policy` body `{rego, entrypoint?, version?}` | compile and apply (`opa_compile_apply`) |
| `POST /opa/policy/compile` | compile only (`opa_compile`) |
| `POST /opa/policy/check` body `{rego, entrypoint?}` | compile to validate: no version, nothing staged or applied (`opa_check`) |
| `POST /opa/policy/apply` body `{version}` | apply the staged policy (`opa_apply`) |
| `POST /opa/policy/rollback` | back to the previous policy (`opa_rollback`) |
| `POST /opa/policy/clear` | remove the user policy everywhere (`opa_clear`) |
| `GET /hives/{hive}/opa/policy` (or `GET /opa/policy?hive=`) | `get_policy` of that hive |
| `GET /hives/{hive}/opa/status` (or `GET /opa/status?hive=`) | `get_status` of that hive |

## Failure behavior

- Lock held, unreadable `hive.yaml`, or a region that cannot be created: the process exits;
  systemd restarts it every 5 s.
- A current wasm that is not valid at start: the region is marked error and the router keeps no
  user policy (`loadCurrentPolicy`).
- A hive cut off from the motherbee keeps running its last installed policy and installs the
  new one when the file reaches it.
- `fluxbee-factory-reset` clears the user policy on every hive (`POST /opa/policy/clear`) and
  then polls each hive's status for up to 60 s.

## Health

- `systemctl status sy-opa-rules`; the journal logs `applied opa policy version=N`,
  `published opa policy for all hives`, `installed the published opa policy`,
  `opa policy sync failed`, and `still not installable after ...` when a hive is stuck.
- `curl -s 127.0.0.1:8080/hives/<hive>/opa/status` per hive: `in_sync` should be `true` and
  every router's status `ok`. `lab/ops.py opa-status` prints exactly these fields for each hive.

## Links

- [rt-gateway](rt-gateway.md): the system policy and how routers evaluate the user policy.
- [SY.config.routes](sy-config-routes.md): the other per-hive routing input.
- [Operations](../reference/operations.md): dist folders and Syncthing.
