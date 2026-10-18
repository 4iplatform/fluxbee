# WF Generic

Base runtime for instanceable `WF.*` nodes.

This module holds the generic runtime of the workflow engine in Go.
The first cut implemented sets:

- module and entrypoint
- workflow definition types
- strict loading from JSON
- load-time validation
- compilation of CEL guards

The rest of the runtime (`store`, `dispatch`, `recover`, `actions`, etc.) is
completed on top of this base.

## Minimal runbook

Local build:

```bash
cd ~/fluxbee/go/nodes/wf/wf-generic
go build -o wf-generic .
```

Example config using the canonical workflow:

```bash
cat >/tmp/wf.invoice.config.json <<'EOF'
{
  "workflow_definition_path": "/opt/fluxbee/go/nodes/wf/examples/packages/wf.invoice/flow/definition.json",
  "db_path": "/tmp/wf.invoice.db",
  "sy_timer_l2_name": "SY.timer@motherbee",
  "_system": {
    "node_name": "WF.invoice@motherbee"
  }
}
EOF
```

Direct run:

```bash
cd ~/fluxbee/go/nodes/wf/wf-generic
./wf-generic --config /tmp/wf.invoice.config.json
```

Managed-runtime-style run:

```bash
sudo mkdir -p /var/lib/fluxbee/nodes/WF/WF.invoice@motherbee
sudo cp /tmp/wf.invoice.config.json /var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/config.json
FLUXBEE_NODE_NAME=WF.invoice@motherbee /usr/bin/wf-generic
```

Example client from the repo:

```bash
cd ~/fluxbee
cargo run --example wf_client -- start WF.invoice@motherbee cust-001
cargo run --example wf_client -- list WF.invoice@motherbee
cargo run --example wf_client -- get WF.invoice@motherbee <instance_id>
cargo run --example wf_client -- cancel WF.invoice@motherbee <instance_id>
```

## Runbook managed by the orchestrator

The correct platform model is:

- `wf.engine` as the base runtime
- `wf.invoice` as a `workflow` package with `runtime_base = wf.engine`
- `WF.invoice@motherbee` as a managed instance created by `SY.admin` / `SY.orchestrator`

`wf-generic` supports both paths:

- explicit config with `workflow_definition_path` for smoke/local dev
- package-native, using `_system.package_path/flow/definition.json` when it runs as a workflow package

For v1, `WF.*` exposes the standard `CONFIG_GET` / `CONFIG_SET` contract, but the config is still boot-time only:

- `CONFIG_GET` returns the persisted effective config and the node's contract
- `CONFIG_SET` persists the new config and answers `restart_required`
- `CONFIG_CHANGED` live/hot reload is not implemented in v1

### 1. Publish the `wf.engine` base runtime

Install the base binary:

```bash
cd ~/fluxbee
bash scripts/install.sh
```

Publish the `wf.engine` base runtime to dist:

```bash
cd ~/fluxbee
bash scripts/publish-wf-runtime.sh --version 0.1.0 --set-current --sudo
```

### 2. Publish the `wf.invoice` workflow package

```bash
cd ~/fluxbee
bash scripts/publish-wf-invoice-package.sh --version 0.1.0 --deploy motherbee
```

### 3. Managed spawn through `SY.admin`/`SY.orchestrator`

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes" \
  -H "Content-Type: application/json" \
  -d '{
    "node_name": "WF.invoice@motherbee",
    "runtime": "wf.invoice",
    "runtime_version": "current",
    "tenant_id": "tnt:REPLACE_ME",
    "config": {
      "tenant_id": "tnt:REPLACE_ME"
    }
  }' | jq
```

Notes:

- `workflow_definition_path` is no longer needed; the runtime resolves it from `_system.package_path/flow/definition.json`
- `db_path` defaults to the instance dir: `wf_instances.db`
- `sy_timer_l2_name` is derived by default as `SY.timer@<hive>`

### 4. Check the state

```bash
curl -sS "http://127.0.0.1:8080/hives/motherbee/nodes/WF.invoice@motherbee/status" | jq
curl -sS "http://127.0.0.1:8080/hives/motherbee/nodes/WF.invoice@motherbee/config" | jq
```

### 5. Test the flow with the example client

```bash
cd ~/fluxbee
cargo run --example wf_client -- start WF.invoice@motherbee cust-001
cargo run --example wf_client -- list WF.invoice@motherbee
cargo run --example wf_client -- get WF.invoice@motherbee <instance_id>
cargo run --example wf_client -- cancel WF.invoice@motherbee <instance_id>
```
