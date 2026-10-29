# WF Generic

Base runtime for instanceable `WF.*` nodes.

This module holds the workflow engine in Go: it loads one workflow definition (validated by
`go/pkg/wfcel`), runs its instances, and keeps them in SQLite (`wf_instances.db`).

How it runs a workflow (0.1.90, docs/wf-orchestration-v1.md §5.1):

- **Every message a WF sends is a `user` message**, to a node name written in the definition, and
  every message it takes is a `user` message, but a `TIMER_FIRED`: any other `system` message is
  dropped, never answered. A definition with `meta.type`, `target_ref` or `event_match.type` is
  refused.
- **An instance finishes on the definition it was born with.** Each instance records its
  definition's hash; the store keeps every definition a running instance uses and drops it when the
  last one ends. A new instance takes the current definition.
- **At least once.** An inbound event is saved in the store before its transition runs, and the
  transition's commit deletes it, with any earlier event of the instance a failed commit left; a
  creation commits the instance, its internal events and the event in one transaction. After a
  crash the start runs again whatever was left, under the same instance id and trace_id, counting
  each event's attempts first: one already run at 3 starts is not run again, and its instance fails
  (`replay_exhausted`). The store commits with `synchronous=FULL`.
- **Timers.** A fired timer's key is `event.timer_key`. A `TIMER_FIRED` is taken only from SY.timer
  of the node's hive (`sy_timer_l2_name` must name it), only for a timer this node scheduled
  (`owner_l2_name`), and by its `client_ref` alone; a fired timer's index row goes with it, so a
  restart does not fire it again. A restart fires the timers it missed in the order they fire,
  reading each row again first. If SY.timer does not answer at start, the node starts `DEGRADED`
  and reconciles its timers when SY.timer answers; a timer SY.timer never got is scheduled again
  then, and one scheduled after the list was asked is left alone.
- **Who may talk to it.** `WF_HELP`, `WF_GET_INSTANCE`, `WF_LIST_INSTANCES`, `WF_GET_CODE` and
  `WF_CANCEL_INSTANCE` answer only SY.admin@motherbee and SY.wf-rules of the node's hive, in
  snake_case keys. A router notice, a reply (`*_RESPONSE`, `INVALID_INPUT`, `<msg>.error`) and a
  message without a name are never answered, and a reply never starts an instance; nor does a late
  answer, one on the thread of an instance of this node that ended.
- **Robustness.** A `$ref` to a missing field fails the action unless it gives a default
  (`{"$ref": "event.context", "default": {}}`, taken for a missing or `null` field); a `$ref` holds
  only `$ref` and `default`, and its default is a literal. The state holds plain JSON, and a JSON
  integer is an int and any other number a double, live and after a restart, the definition's own
  literals included. CEL compares and equates an int with a double, but fails arithmetic that mixes
  them and `%` on a double: convert first (`double(x) * 1.1`, `int(x) % 2`). A guard is stopped at
  10 ms (false), and a `set_variable` at its cost limit (the action fails). A panic in a transition
  fails that instance (`transition_panic`); more than 100 internal events after one event fail it
  (`internal_event_cap`); an instance whose definition is gone, or lacks its state, fails at the
  start (`definition_unavailable`, `state_not_in_definition`).

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

Example client from the repo (it starts an instance; the instance verbs answer only SY.admin and
SY.wf-rules, see above):

```bash
cd ~/fluxbee
cargo run --example wf_client -- start WF.invoice@motherbee cust-001
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

### 1. The `wf.engine` base runtime

The package seeds `wf.engine` (`packaging/base-nodes.json`) at its own version. Install the package
built from this checkout (`VERSION` defaults to the installed one), and check the runtime:

```bash
cd ~/fluxbee
bash scripts/install.sh [VERSION]
curl -sS "http://127.0.0.1:8080/hives/motherbee/runtimes/wf.engine" | jq .payload
```

Do not publish `wf.engine` by hand on a packaged install: a base runtime published by hand keeps
`current` through every upgrade (FINDINGS A-122), so every WF node would stay on that build.
`scripts/publish-wf-runtime.sh` is for a dev checkout without the package only:

```bash
cd ~/fluxbee
# dev checkout only: makes this build the engine of every WF node (each restarts)
bash scripts/publish-wf-runtime.sh --version 0.1.0 --set-current --sudo
```

### 2. Publish the `wf.invoice` workflow package

The package directory is `go/nodes/wf/examples/packages/wf.invoice`. `fluxbee-publish` installs it
into the local `dist` and makes it `current` (the version comes from its `package.json`, or from
`--version <v>`); `--deploy <hive>` then asks that hive to sync and update
(`docs/reference/operations.md`, "Publish runtimes"). Run it on the motherbee:

```bash
cd ~/fluxbee
cargo build --release --bin fluxbee-publish
sudo ./target/release/fluxbee-publish go/nodes/wf/examples/packages/wf.invoice --deploy motherbee
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
- `sy_timer_l2_name` is derived by default as `SY.timer@<hive>`, and any other value is refused:
  the node takes a `TIMER_FIRED` only from SY.timer of its hive

### 4. Check the state

```bash
curl -sS "http://127.0.0.1:8080/hives/motherbee/nodes/WF.invoice@motherbee/status" | jq
curl -sS "http://127.0.0.1:8080/hives/motherbee/nodes/WF.invoice@motherbee/config" | jq
```

### 5. Test the flow with the example client

```bash
cd ~/fluxbee
cargo run --example wf_client -- start WF.invoice@motherbee cust-001
journalctl -u fluxbee-node-WF.invoice-motherbee | grep 'wf: create'
```

The journal shows the instance the message started (`wf: create accepted instance_id=wfi:...`, then
`wf: create committed ... state=... status=...`). A node spawned by hand, as in step 3, is unknown to
SY.wf-rules: its `GET /hives/motherbee/wf-rules/status?workflow_name=invoice` answers
`WORKFLOW_NOT_FOUND`. A workflow deployed through SY.wf-rules (`docs/wf-rules-quickstart.md`) has a
status there, with its node's running instances (`jq .payload.wf_node`).
