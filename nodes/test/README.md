# Test Nodes Workspace

This directory holds example nodes meant to validate real transport/routing
with the SDK, but written so that they also serve as a reference for external
teams.

It includes two minimal runtimes:

- `IO.test`: connects to the router, optionally gets/provisions a temporary ILK
  and sends a `Resolve` message.
- `AI.test.gov`: acts as a simple frontdesk, receives the message routed by the
  router and answers with an ack.

## Goal

Show, with real and readable nodes:

- how to build `NodeConfig` and connect with `fluxbee_sdk::connect`
- how to emit a `Resolve` message
- how to attach `src_ilk` in `meta.src_ilk`
- how to answer `NODE_STATUS_GET` with the SDK's default handler
- how to observe the auto-routing toward the frontdesk configured in `hive.yaml`

## Structure

```text
nodes/test/
├── ai-test-gov/
└── io-test/
```

## Build

From the repo root:

```bash
cargo check -p io-test
cargo check -p ai-test-gov
```

## Test the temporary ILK auto-routing

In `/etc/fluxbee/hive.yaml` of the local hive:

```yaml
government:
  identity_frontdesk: "AI.test.gov@motherbee"
```

Restart the router if that config changed.

Terminal 1:

```bash
cargo run -p ai-test-gov
```

Terminal 2:

```bash
IO_TEST_ALLOW_PROVISION=1 \
IO_TEST_IDENTITY_TARGET="SY.identity@motherbee" \
cargo run -p io-test
```

Expected behavior:

1. `IO.test` provisions a temporary ILK if it does not get one through env.
2. `IO.test` sends a `Resolve` message with `meta.src_ilk`.
3. The router detects `registration_status=temporary` and forces the destination to
   `AI.test.gov@motherbee` without going through OPA.
4. `AI.test.gov` logs the received message and sends a simple reply back to
   `IO.test`.

## Important note about `src_ilk`

The canonical contract is now `meta.src_ilk`.

## Name contract for managed nodes

For managed nodes spawned by `SY.orchestrator`, the name bootstrap contract
for v1 is:

- `SY.orchestrator` injects `FLUXBEE_NODE_NAME=<node_name@hive>` into the process
- the runtime uses that name as its canonical identity at startup
- the instance's `config.json` is derived from that name with the layout:
  - `/var/lib/fluxbee/nodes/<KIND>/<node_name@hive>/config.json`

This applies both to the normal spawn and to the reboot/reconcile relaunch.

Example:

```bash
FLUXBEE_NODE_NAME="SY.frontdesk.gov@motherbee"
```

For that case, the expected instance path is:

```text
/var/lib/fluxbee/nodes/SY/SY.frontdesk.gov@motherbee/config.json
```

### Why this contract

- it avoids duplicating `NODE_NAME` and `NODE_CONFIG` as two references that could diverge
- it avoids depending on arguments in every `start.sh`
- it concentrates the change in `SY.orchestrator`, using `systemd-run --setenv=...`
- it works the same for singleton and instanced nodes

### SDK helper

The SDK already ships helpers for this contract:

- `fluxbee_sdk::managed_node_name(...)`
  - takes `FLUXBEE_NODE_NAME` as the main source
  - can fall back to the runtime's legacy env vars if compatibility is needed
- `fluxbee_sdk::managed_node_config_path(...)`
  - derives the canonical `config.json` from the `node_name`

Recommended pattern in a runtime:

```rust
let cfg = NodeConfig {
    name: fluxbee_sdk::managed_node_name("AI.test.gov", &["AI_TEST_NODE_NAME"]),
    router_socket: "/var/run/fluxbee/routers".into(),
    uuid_persistence_dir: "/var/lib/fluxbee/state/nodes".into(),
    uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
    config_dir: "/etc/fluxbee".into(),
    version: "0.1.0".to_string(),
};
```

Practical notes:

- if the process starts under the orchestrator, `FLUXBEE_NODE_NAME` should always win
- the legacy env vars (`AI_TEST_NODE_NAME`, `IO_TEST_NODE_NAME`, `GOV_NODE_NAME`) remain only for compatibility or manual use
- `_system.node_name` inside `config.json` is still the persisted source of truth

## Note about node prefixes and lifecycle

The functional prefixes of the name (`AI.*`, `IO.*`, `WF.*`, `SY.*`, `RT.*`) are not
the same as the lifecycle classification (`core` vs managed runtime).

Examples:

- `AI.*`, `IO.*` and `WF.*` usually run as managed runtimes published
  in `dist`
- an `AI.*` node can play a very important role in the architecture and still
  stay in the managed runtime model
- `SY.*` identifies the system's control-plane/core
- `RT.gateway` identifies the hive's base infrastructure

So, for v1, the operational rule is not "AI/IO/WF yes, the rest no".

The real rule is:

- `SY.*` does not come in through managed spawn
- `RT.gateway` does not come in through managed spawn
- `AI.*`, `IO.*`, `WF.*` and `RT.<other>` can come in through the published managed runtime model

That avoids mixing:

- the node's functional family
- the component's origin/lifecycle

## Test the full publish/install with the CLI

There is also an operator-style E2E that uses `fluxbee-publish` end to end
with these two nodes:

```bash
BASE="http://127.0.0.1:8080" \
HIVE_ID="motherbee" \
MOTHER_HIVE_ID="motherbee" \
TENANT_ID="tnt:<uuid-v4>" \
bash scripts/identity_test_nodes_publish_e2e.sh
```

That script:

1. builds `fluxbee-publish`, `ai-test-gov` and `io-test`
2. assembles temporary `full_runtime` packages
3. publishes both with `--deploy`
4. waits for readiness in `/versions`
5. spawns `AI.test.gov` as a managed frontdesk
6. spawns `IO.test` as a one-shot probe
7. checks that the reply comes back with `HANDLED_BY=AI.test.gov@<hive>`

Note:

- this E2E requires `TENANT_ID`, because the frontdesk configured in `hive.yaml`
  is spawned as a managed node and the orchestrator today requires `tenant_id` for
  that identity-aware registration path

It serves to verify the full software cycle:

- package layout
- install in `dist`
- update/sync
- managed spawn
- real execution of the published binary

An E2E for reboot/reconcile of managed custom nodes was also validated:

```bash
BASE="http://127.0.0.1:8080" \
HIVE_ID="motherbee" \
MOTHER_HIVE_ID="motherbee" \
TENANT_ID="tnt:<uuid-v4>" \
bash scripts/custom_node_reboot_reconcile_e2e.sh
```

That script validates:

1. publish of temporary runtimes
2. spawn of the managed frontdesk, using by default the `government.identity_frontdesk` configured in `hive.yaml`
3. `kill_node`
4. restart of `sy-orchestrator`
5. automatic relaunch from `config.json`
6. a real follow-up probe with `IO.test`
