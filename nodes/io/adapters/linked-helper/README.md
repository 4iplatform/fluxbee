# Linked Helper Adapter

This workspace contains the current Rust adapter for Linked Helper.

It exists to validate and implement the `fluxbee_cloud <> adapter` contract while keeping the adapter physically separated from the Cloud runtime source.

Important:

```text
this adapter living in the current repo does not mean the production adapter must live here forever;
the production adapter may still move to a separate repository;
this workspace should be treated as an isolated adapter project, not as part of fluxbee_cloud apps.
```

Current location in this repo:

```text
adapters/linked-helper/
```

Current contents:

```text
adapters/linked-helper/
  README.md
  .gitignore
  adapter-rs/
```

## Rust adapter

Current location:

```text
adapters/linked-helper/adapter-rs/
```

This is the real adapter codebase in Rust for the current service/alive phase.

### Rust commands

Show CLI help:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- --help
```

Recommended first start flow:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- start \
  --cloud http://localhost:3002 \
  --token <ENROLLMENT_TOKEN> \
  --partitions-root "/Users/developmentapx/Documents/test/Partitions"
```

Behavior:

- if no local bootstrap exists yet, `start` enrolls first and then enters the normal service loop;
- if local bootstrap already exists, `start` skips enroll and runs directly;
- if local bootstrap already exists and you still pass first-start arguments such as `--token` or `--cloud`, `start` now fails explicitly instead of silently ignoring them;
- this is the current closest flow to “downloaded from Fluxbee Cloud and started for the first time”.

Enroll against Cloud:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- enroll \
  --cloud http://localhost:3002 \
  --token <ENROLLMENT_TOKEN> \
  --display-name "Linked Helper Adapter A"
```

Notes:

- local state is persisted in `adapters/linked-helper/adapter-rs/.linkedhelper-adapter-state.json`;
- runtime state is also mirrored into a colocated SQLite file derived from the state file name, for example `adapters/linked-helper/adapter-rs/.linkedhelper-adapter-state-runtime.db`;
- when the SQLite runtime store exists, runtime fields such as `desired_bindings` and `last_known_desired_state_version` are hydrated from it on startup/status reads;
- if the state file already exists, use `--force` only when you intentionally want to overwrite the local enrollment state.

Print local state:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- status
```

`status` now returns both:

- the bootstrap/runtime JSON state currently loaded by the adapter;
- the colocated SQLite runtime snapshot, including:
  - `runtimeMeta`
  - `desiredBindings`
  - `instanceRuntimeState`
  - `syncCheckpoints`

Send one alive payload to Cloud:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- alive --partitions-root "C:/LinkedHelper/Partitions"
```

Scan one Linked Helper `Partitions` folder and print the discovery payload:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- scan \
  --partitions-root "C:/LinkedHelper/Partitions"
```

Scan and send discovery to Cloud:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- discover-scan \
  --partitions-root "C:/LinkedHelper/Partitions"
```

Run one service cycle with alive + incremental discovery:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- run --partitions-root "C:/LinkedHelper/Partitions" --once
```

Run the persistent loop:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- run \
  --partitions-root "C:/LinkedHelper/Partitions" \
  --interval-seconds 60
```

Run the unified first-start flow but only one cycle:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- start \
  --cloud http://localhost:3002 \
  --token <ENROLLMENT_TOKEN> \
  --partitions-root "/Users/developmentapx/Documents/test/Partitions" \
  --once
```

Send one manual discovery:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- discover manual \
  --instance-id 123456 \
  --account-display-name "Cuenta ventas"
```

Send discovery from JSON payload:

```bash
cd adapters/linked-helper/adapter-rs
cargo run -- discover payload-file \
  --payload-file ./sample-discovery.json
```

Current scan/service behavior:

- searches folders named `linked-helper-account-<local_instance_id>-main`;
- reads `lh.db` when present;
- extracts `li_accounts.id`, `li_accounts.external_id`, `full_name`, `email`, `avatar`, `last_login_at`, `created_at`, `updated_at`;
- normalizes `external_id` values to string even when SQLite stores them as `INTEGER`;
- extracts `lh_users.id`, `lh_users.external_id` and `lh_users.last_login_at` when the table exists;
- extracts aggregate counts from `chats`, `pending_messages` and `campaigns` when those tables exist;
- reads `preferences.json` and reports `mwState` when available;
- reports `matchesLocalInstanceId` when `external_id` matches the folder-derived `local_instance_id`.
- sends `alive` with version, build, OS, arch, adapter status and basic Linked Helper compatibility data;
- keeps lightweight local runtime state for `last_successful_alive_at`, `last_successful_discovery_at`, `last_scan_at` and `last_discovery_hash`;
- mirrors `runtime_meta`, current `desired_bindings`, and a first `instance_runtime_state` snapshot into the local SQLite runtime store as a first transition away from JSON-only runtime persistence;
- initializes the `sync_checkpoints` table and now includes basic read/write helpers for future per-channel checkpointing;
- stores `instance_runtime_state` both for desired bindings and for discovery-only local instances, using `local_instance_id` as the local consolidation key.
- writes the first real checkpoint channel already used by the current flow: `cloud_discovery`, keyed by `local_instance_id`.

Current path behavior:

- there is not yet automatic OS-specific discovery of `Partitions`;
- use `--partitions-root` or persist `lh_root_path` in the adapter state file.

## Scope limits

This current phase does not implement:

- sync polling against Edge;
- action delivery;
- provisioning downstream;
- automatic OS-specific `Partitions` autodetection;
- hardening of local secret storage;
- final installer/packaging.

Current validated scope:

- adapter enrollment;
- local adapter state persistence;
- alive payload submission to Fluxbee Cloud;
- manual discovery payload submission;
- scan-based discovery from real Linked Helper folder structure;
- persistent `run` loop with alive and incremental discovery.
