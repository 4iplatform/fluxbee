# Fluxbee

**Distributed intelligence infrastructure**

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

> *fluxbee.ai*

---

## What is this?

Fluxbee is infrastructure for building systems where AI agents, humans, and automated workflows communicate seamlessly across any channel. It's the nervous system for organizations that want to operate with AI at the center, not at the edges.

Think of it as a phone system, but instead of connecting phone numbers, it connects:
- AI agents with specialized knowledge
- Human operators and customers
- Workflows and integrations
- Any communication channel (WhatsApp, Email, Slack, etc.)

Every message flows through a unified routing layer that knows who's talking, who they're talking to, what they're capable of, and where to send the conversation next.

---

## Why does this exist?

**The problem:** Today's AI integrations are point-to-point. You connect an AI to WhatsApp. Another to your CRM. Another to email. Each one is an hive. They don't share context. They can't hand off conversations. They can't be managed as a coherent system.

**The insight:** What if we treated AI agents the way organizations treat employees? Each one has a role, capabilities, and credentials. They're trained (prompted) for specific jobs. They work together, escalate to each other, and when they can't handle something, they bring in a human.

**The solution:** A routing layer that understands identity (who), capability (what they can do), and conversation flow (where things go next). Built for AI-first but works just as well for humans.

---

## API Quickstart (SY.admin)

Operational API for motherbee control is exposed by `SY.admin` (default `127.0.0.1:8080`).

## Network Ports (Motherbee <-> Workers)

This is the effective port map for current deployments.

| Port | Proto | Scope | Purpose | Default owner/listener |
|---|---|---|---|---|
| `9000` | TCP | inter-hive | WAN gateway channel (`RT.gateway`) | `motherbee` (`wan.listen`) |
| `9100` | TCP | inter-hive | Identity full-sync + delta stream | `SY.identity` primary (`motherbee`) |
| `22000` | TCP | inter-hive | Syncthing data sync (`blob`/`dist`) | peer-to-peer (`motherbee` + workers) |
| `22000` | UDP | inter-hive | Syncthing QUIC/data sync | peer-to-peer (`motherbee` + workers) |
| `21027` | UDP | LAN discovery | Syncthing local discovery | peer-to-peer LAN |
| `8080` | TCP | local | `SY.admin` HTTP API | local host (`127.0.0.1` by default) |
| `8384` | TCP | local | Syncthing API/GUI | local host (`127.0.0.1` by default) |
| `4222` | TCP | local | Embedded NATS | local host (`127.0.0.1` by default) |
| `22` | TCP | ops/bootstrap | SSH bootstrap/maintenance channel | worker host SSH daemon |

Firewall behavior in `SY.orchestrator`:

- Automatic firewall management is currently implemented for Syncthing ports (`22000/tcp`, `22000/udp`, `21027/udp`) when blob/dist sync is enabled.
- Automatic firewall management is also applied for core listener ports:
  - WAN listener from `wan.listen` (for example `9000/tcp`) when configured.
  - Identity sync listener on motherbee (`identity.sync.port`, default `9100/tcp`).
- This runs locally on each hive where `SY.orchestrator` starts and reconciles Syncthing runtime.
- Workers typically do not need `identity.sync.port` inbound because replicas use `identity.sync.upstream` (outbound to primary), but WAN listener firewall rules are applied if `wan.listen` is set on that worker.
- If neither `ufw` nor `firewalld` exists on host, orchestrator logs a warning and port policy remains external.

## PostgreSQL Ownership

`SY.storage` and `SY.identity` use fixed database names defined in code (not in `hive.yaml`):

- `SY.storage` -> `fluxbee_storage`
- `SY.identity` (primary only) -> `fluxbee_identity`

Bootstrap behavior:

- `SY.identity` now exposes `CONFIG_GET` / `CONFIG_SET` for its primary DB bootstrap and uses this precedence:
  - `secrets.json` local via `CONFIG_SET`
  - env overrides
- `SY.storage` ahora usa esta precedencia:
  - `secrets.json` local via `CONFIG_SET`
  - env overrides
  - no usa `database.url` en `hive.yaml`
- On motherbee startup, each service ensures its own database exists (`CREATE DATABASE` if missing) and then ensures its own schema/tables.
- Worker identity replicas remain read-only for DB writes and continue syncing from primary through identity sync.

### Base URL

```bash
BASE="http://127.0.0.1:8080"
```

### Health and local status

```bash
curl -sS "$BASE/health"
curl -sS "$BASE/hive/status"
curl -sS "$BASE/hives"
```

### Add and remove a worker hive

```bash
HIVE_ID="worker-220"
HIVE_ADDR="192.168.8.220"

# bootstrap worker + connect WAN
curl -sS -X POST "$BASE/hives" \
  -H "Content-Type: application/json" \
  -d "{\"hive_id\":\"$HIVE_ID\",\"address\":\"$HIVE_ADDR\"}"

# inspect hive metadata
curl -sS "$BASE/hives/$HIVE_ID"

# deprovision worker services + remove hive metadata
curl -sS -X DELETE "$BASE/hives/$HIVE_ID"
```

### Publish a custom node runtime on Fluxbee (recommended)

The normal operator flow is:

1. Build your node binary.
2. Package it as a Fluxbee runtime.
3. Publish it with `fluxbee-publish` on motherbee.
4. Let `--deploy` propagate `dist` and trigger `SYSTEM_UPDATE` on the target hive.
5. Spawn the node through `SY.admin`.

This is the path validated by the runtime packaging E2E suite (`PUB-T22` through `PUB-T26`).

#### Package types

- `full_runtime`: your package ships its own `bin/start.sh` and binaries. This is the normal choice for a custom node.
- `config_only`: your package ships config/assets and uses the `bin/start.sh` of an existing base runtime.
- `workflow`: your package ships flow definitions/assets and uses the `bin/start.sh` of an existing workflow engine runtime.

If you are publishing a brand new custom node, start with `full_runtime`.

#### Minimal `full_runtime` package

```text
my-node/
├── package.json
├── bin/
│   ├── start.sh
│   └── ai-my-node
└── config/
    └── default-config.json
```

`package.json`:

```json
{
  "name": "ai.my.node",
  "version": "1.0.0",
  "type": "full_runtime",
  "description": "Custom Fluxbee node",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
```

`bin/start.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
exec "$(dirname "${BASH_SOURCE[0]}")/ai-my-node"
```

#### End-to-end example (`full_runtime`)

```bash
MOTHER_HIVE="motherbee"
TARGET_HIVE="worker-220"
RUNTIME_NAME="ai.my.node"
RUNTIME_VERSION="1.0.0"
PKG_DIR="$PWD/package"

# prerequisite for current workspace builds: `protoc` in PATH
# Debian/Ubuntu: sudo apt-get install -y protobuf-compiler

# 1) build your node binary
cargo build --release --bin ai-my-node

# 2) assemble package
mkdir -p "$PKG_DIR/bin" "$PKG_DIR/config"
cp target/release/ai-my-node "$PKG_DIR/bin/ai-my-node"
chmod 0755 "$PKG_DIR/bin/ai-my-node"
cat >"$PKG_DIR/bin/start.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exec "$(dirname "${BASH_SOURCE[0]}")/ai-my-node"
EOF
chmod 0755 "$PKG_DIR/bin/start.sh"
cat >"$PKG_DIR/package.json" <<EOF
{
  "name": "$RUNTIME_NAME",
  "version": "$RUNTIME_VERSION",
  "type": "full_runtime",
  "description": "Custom Fluxbee node",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
EOF
cat >"$PKG_DIR/config/default-config.json" <<'EOF'
{
  "model": "gpt-5",
  "temperature": 0.2
}
EOF

# 3) publish on motherbee and deploy to target hive
FLUXBEE_PUBLISH_BASE="$BASE" \
FLUXBEE_PUBLISH_MOTHER_HIVE_ID="$MOTHER_HIVE" \
target/release/fluxbee-publish "$PKG_DIR" \
  --version "$RUNTIME_VERSION" \
  --deploy "$TARGET_HIVE"
```

`fluxbee-publish` does the following:

- validates the package layout
- installs it under `/var/lib/fluxbee/dist/runtimes/<name>/<version>`
- updates `/var/lib/fluxbee/dist/runtimes/manifest.json`
- when `--deploy` is present, sends `sync-hint + update` to the target hive

Important:

- `update_status=sync_pending` is not a failure by itself
- the correct post-publish check is runtime readiness in `/hives/<hive>/versions`
- for runtime-only validation/materialization you can scope update with `runtime` + `runtime_version`
- targeted runtime update may return `global_runtime_health` as context, but unrelated global drift should not block the targeted deploy

#### Check readiness on the target hive

```bash
curl -sS "$BASE/hives/$TARGET_HIVE/versions" | jq .
```

For one runtime with explicit materialization details:

```bash
curl -sS "$BASE/hives/$TARGET_HIVE/runtimes/$RUNTIME_NAME" | jq .
```

Expected for `full_runtime`:

- `runtime_present = true`
- `start_sh_executable = true`

Expected for `config_only` / `workflow`:

- `runtime_present = true`
- `start_sh_executable = true`
- `base_runtime_ready = true`

#### Spawn the node

```bash
TENANT_ID="tnt:12345678-1234-1234-1234-123456789abc"
NODE_NAME="AI.my.node.1"

curl -sS -X POST "$BASE/hives/$TARGET_HIVE/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"$RUNTIME_NAME\",
    \"runtime_version\":\"current\",
    \"tenant_id\":\"$TENANT_ID\",
    \"config\":{
      \"model\":\"gpt-5\",
      \"temperature\":0.7
    }
  }"
```

At spawn time, the orchestrator builds the final config as:

1. package template defaults
2. request `config` overrides
3. forced `_system` block

For `config_only` and `workflow`, `_system` also includes:

- `runtime_base`
- `package_path`

This lets the node load assets directly from the published package in `dist`.

#### Verify status and effective config

```bash
curl -sS "$BASE/hives/$TARGET_HIVE/nodes/$NODE_NAME/status" | jq .
curl -sS "$BASE/hives/$TARGET_HIVE/nodes/$NODE_NAME/config" | jq .
```

Useful checks:

- `payload.node_status.lifecycle_state`
- `payload.node_status.runtime.name`
- `payload.node_status.runtime.resolved_version`
- `payload.config._system.runtime`
- `payload.config._system.runtime_version`
- `payload.config._system.runtime_base` for `config_only` / `workflow`
- `payload.config._system.package_path` for `config_only` / `workflow`

#### When to use `config_only` or `workflow`

- Use `config_only` when you already have a reusable base runtime and only want to ship config, prompts, or assets.
- Use `workflow` when the package is a flow definition that runs on top of a workflow engine runtime.

In both cases the package must declare `runtime_base` in `package.json`, and the base runtime must already be published and ready on the target hive.

For the full packaging contract and examples of all three package types, see:
- [`docs/runtime-packaging-cli-spec.md`](docs/runtime-packaging-cli-spec.md)

#### Low-level fallback: manual `dist` / manifest editing

Manual editing of `/var/lib/fluxbee/dist/runtimes` and `manifest.json` is still possible for debugging, but it is no longer the recommended operator path. Prefer `fluxbee-publish` unless you are diagnosing a broken rollout or reproducing a low-level runtime lifecycle issue.

For a low-level operational rollout (manual `dist` + `sync-hint` + `update` + run node), see:
- `docs/14-runtime-rollout-motherbee.md`
- `scripts/orchestrator_system_update_api_e2e.sh`

### Trigger/confirm Syncthing convergence (`SYSTEM_SYNC_HINT`, v2.x)

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/sync-hint" \
  -H "Content-Type: application/json" \
  -d '{"channel":"blob","folder_id":"fluxbee-blob","wait_for_idle":true,"timeout_ms":30000}'; echo
```

### Query remote worker state (from motherbee API)

```bash
curl -sS "$BASE/hives/$HIVE_ID/nodes"
curl -sS "$BASE/hives/$HIVE_ID/versions"
curl -sS "$BASE/hives/$HIVE_ID/deployments?limit=10"
```

### Node status/config quick checks

```bash
NODE_NAME="WF.demo.worker@$HIVE_ID"

# canonical node status snapshot
curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/status" | jq .

# effective node config
curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/config" | jq .

# node runtime state (null if not created yet)
curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/state" | jq .
```

Useful status fields:
- `payload.node_status.lifecycle_state`
- `payload.node_status.health_state`
- `payload.node_status.health_source`
- `payload.node_status.status_version`

Expected for remote hive responses:
- `payload.target` should match requested hive (for example `worker-220`)
- node names should match target hive (for example `SY.config.routes@worker-220`)

### Common config calls

```bash
curl -sS "$BASE/config/storage"

curl -sS -X PUT "$BASE/config/vpns" \
  -H "Content-Type: application/json" \
  -d '{"vpns":[
    {"pattern":"WF.echo","match_kind":"PREFIX","vpn_id":20},
    {"pattern":"WF.listen","match_kind":"PREFIX","vpn_id":20}
  ]}'
```

For a larger command matrix and error smoke checks, see:
- `docs/onworking COA/sy-architect-admin-command-test-matrix.md`
- `scripts/admin_add_hive_matrix.sh`

### Identity inspection calls

Useful when debugging registration and routing state from `SY.admin` without
talking to `SY.identity` directly:

```bash
HIVE_ID="motherbee"
ILK_ID="ilk:xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"

# compact list of current ILKs on the hive
curl -sS "$BASE/hives/$HIVE_ID/identity/ilks" | jq .

# full detail for one ILK
curl -sS "$BASE/hives/$HIVE_ID/identity/ilks/$ILK_ID" | jq .
```

`GET /hives/{hive}/identity/ilks` returns a compact row per ILK with:

- `ilk_id`
- `ilk_type`
- `registration_status`
- `tenant_id`
- `tenant_name`
- `display_name`
- `node_name`
- `channel_count`
- `channels` (`ich_id`, `channel_type`, `address`)
- `deleted_at_ms`

### Debug message to one node

For ad-hoc debugging, `SY.admin` can emit a direct message to a specific node by
name and return the generated `trace_id`.

```bash
HIVE_ID="motherbee"
NODE_NAME="AI.frontdesk.gov@motherbee"

curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/messages" \
  -H "Content-Type: application/json" \
  -d '{
    "msg_type": "user",
    "msg": "LLM",
    "payload": {
      "text": "hola desde admin"
    }
  }' | jq .
```

Optional fields:

- `src_ilk`
- `scope`
- `meta_target`
- `meta_action`
- `priority`
- `context`
- `ttl`

### Endpoint Reference (SY.admin)

Current HTTP surface exposed by `SY.admin`:

Global endpoints:

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Liveness/health probe |
| `GET` | `/admin/actions` | Introspect internal admin action catalog (`list_admin_actions`) |
| `GET` | `/hive/status` | Local hive/orchestrator status |
| `GET` | `/hives` | List managed hives |
| `POST` | `/hives` | Add hive (`hive_id`, `address`) |
| `GET` | `/versions` | Effective versions (local or `?hive=`) |
| `GET` | `/deployments` | Historical deployment entries (`?hive=`, `?category=`, `?limit=`) |
| `GET` | `/drift-alerts` | Historical drift alert entries (`?hive=`, `?category=`, `?limit=`) |
| `GET` | `/routes` | Read global routes |
| `POST` | `/routes` | Add/update route entry |
| `DELETE` | `/routes` | Delete route entry |
| `GET` | `/vpns` | Read global VPN rules |
| `POST` | `/vpns` | Add/update VPN rule |
| `DELETE` | `/vpns` | Delete VPN rule |
| `PUT` | `/config/routes` | Replace routes config |
| `PUT` | `/config/vpns` | Replace VPN config |
| `GET` | `/config/storage` | Read storage config |
| `PUT` | `/config/storage` | Update storage config |
| `GET` | `/config/storage/metrics` | Storage metrics passthrough |
| `POST` | `/opa/policy` | Upload policy bundle |
| `POST` | `/opa/policy/compile` | Compile policy |
| `POST` | `/opa/policy/apply` | Apply compiled policy |
| `POST` | `/opa/policy/rollback` | Roll back policy |
| `POST` | `/opa/policy/check` | Validate policy inputs |
| `GET` | `/opa/policy` | Read current policy state |
| `GET` | `/opa/status` | OPA runtime status |
| `GET` | `/modules` | List modules |
| `GET` | `/modules/{name}` | List versions for module |
| `GET` | `/modules/{name}/{version}` | Get module version payload |
| `POST` | `/modules/{name}/{version}` | Publish/update module version |

Hive-scoped endpoints:

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/hives/{hive}` | Get hive metadata |
| `DELETE` | `/hives/{hive}` | Remove hive |
| `GET` | `/hives/{hive}/routes` | List routes for hive |
| `POST` | `/hives/{hive}/routes` | Add/update route on hive |
| `DELETE` | `/hives/{hive}/routes/{prefix}` | Delete route by prefix |
| `GET` | `/hives/{hive}/vpns` | List VPN rules for hive |
| `POST` | `/hives/{hive}/vpns` | Add/update VPN rule on hive |
| `DELETE` | `/hives/{hive}/vpns/{pattern}` | Delete VPN rule by pattern |
| `GET` | `/hives/{hive}/nodes` | List nodes on hive |
| `POST` | `/hives/{hive}/nodes` | Spawn node on hive |
| `DELETE` | `/hives/{hive}/nodes/{name}` | Kill node on hive |
| `GET` | `/hives/{hive}/nodes/{name}/status` | Canonical node status (lifecycle/health/config/process) |
| `GET` | `/hives/{hive}/nodes/{name}/config` | Read node effective config |
| `PUT` | `/hives/{hive}/nodes/{name}/config` | Update node effective config |
| `GET` | `/hives/{hive}/nodes/{name}/state` | Read node runtime state payload |
| `POST` | `/hives/{hive}/nodes/{name}/messages` | Send a direct debug message to one node |
| `POST` | `/hives/{hive}/nodes/{name}/control/config-get` | Request node-owned control-plane contract/config (`CONFIG_GET`) |
| `POST` | `/hives/{hive}/nodes/{name}/control/config-set` | Apply node-owned control-plane config (`CONFIG_SET`) |
| `GET` | `/hives/{hive}/identity/ilks` | List ILKs on hive (compact identity view) |
| `GET` | `/hives/{hive}/identity/ilks/{ilk_id}` | Read one ILK with resolved tenant/alias detail |
| `POST` | `/hives/{hive}/update` | Send `SYSTEM_UPDATE` to hive orchestrator |
| `POST` | `/hives/{hive}/sync-hint` | Send `SYSTEM_SYNC_HINT` (`blob`/`dist`) to hive orchestrator |
| `GET` | `/hives/{hive}/versions` | Effective versions for hive |
| `GET` | `/hives/{hive}/deployments` | Historical deployment entries targeting hive |
| `GET` | `/hives/{hive}/drift-alerts` | Historical drift alert entries for hive |
| `POST` | `/hives/{hive}/opa/policy` | Upload policy for hive |
| `POST` | `/hives/{hive}/opa/policy/compile` | Compile policy for hive |
| `POST` | `/hives/{hive}/opa/policy/apply` | Apply policy on hive |
| `POST` | `/hives/{hive}/opa/policy/rollback` | Roll back policy on hive |
| `POST` | `/hives/{hive}/opa/policy/check` | Validate policy on hive |
| `GET` | `/hives/{hive}/opa/policy` | Read policy state on hive |
| `GET` | `/hives/{hive}/opa/status` | OPA status on hive |

Note:
- Router operations are managed as node lifecycle (`RT.*`) via `/hives/{hive}/nodes`.
- `GET /deployments` and `GET /hives/{hive}/deployments` are history views, not a list of currently running deployments.
- `GET /drift-alerts` and `GET /hives/{hive}/drift-alerts` are history views, not a live health snapshot.
- History responses may include `target_hives_detail[*].present` and `workers[*].hive_present` so removed hives remain visible as history.
- Drift alert history responses may include `hive_present` and `synthetic` markers when a local `motherbee` current-state snapshot is synthesized for history continuity.
- For `motherbee`, hive-scoped deployment history may include synthesized local-current-state entries when no older history was recorded with `motherbee` as explicit deployment target.

---

## Core Concepts

### Three Layers of Routing

| Layer | What it routes | Example |
|-------|----------------|---------|
| **L1 - Connection** | Raw sockets | Which process on which machine |
| **L2 - Node** | Named services | `AI.support.l1@production` |
| **L3 - Interlocutor** | Identities | Customer "John" or Agent "Support-L1" |

Most systems only have L1. Some have L2. Fluxbee has all three, which means you can route based on *who someone is* and *what they need*, not just where the bytes go.

### Hives

An **hive** is a deployment unit - a cluster of nodes that share memory and communicate via Unix sockets. Fast, local, zero serialization overhead.

Hives connect to each other over the network. A customer in São Paulo talks to an AI agent in the São Paulo hive. If that agent needs to escalate, the message routes to the Buenos Aires hive where the senior agents live. The customer doesn't know. The routing is automatic.

### The Identity System (ILK)

Every participant in the system has an **ILK** (Interlocutor Key) - a unique identifier that follows them everywhere:

```
ilk:550e8400-e29b-41d4-a716-446655440000
```

ILKs have types:
- **Tenant** - An organization (for billing, isolation, contracts)
- **Agent** - An AI with a specific degree (training)
- **Human/Internal** - An operator who can see inside the system
- **Human/External** - A customer who interacts from outside

The routing layer uses ILKs to make decisions: this customer belongs to this tenant, should talk to agents with these capabilities, and if things go wrong, escalate to this human.

### The University Model

AI agents don't just exist - they **graduate**.

1. **Modules** are fragments of knowledge: "You speak Spanish", "You know our product catalog", "You escalate after 3 failed attempts"

2. **Degrees** combine modules into a complete training: "Support-L1-Spanish" = Spanish + Product Knowledge + Basic Troubleshooting + Escalation Rules

3. **Graduation** assigns a degree to an agent with a cryptographic seal. The agent cannot operate without a valid degree. If someone tampers with the training, the hash breaks, and the agent refuses to run.

This means:
- You can audit exactly what an agent knows
- You can version and roll back training
- You can't accidentally deploy an untrained agent
- The AI manages the AI (humans write modules, but compilation and verification is automatic)

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Mother Hive                            │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │ PostgreSQL   │  │ SY.identity  │  │ SY.admin     │          │
│  │ (source of   │  │ (graduates   │  │ (human       │          │
│  │  truth)      │  │  agents)     │  │  interface)  │          │
│  └──────────────┘  └──────────────┘  └──────────────┘          │
│                                                                  │
└──────────────────────────┬──────────────────────────────────────┘
                           │
                           │ WAN (broadcast replication)
                           │
┌──────────────────────────┼──────────────────────────────────────┐
│                          ▼                                       │
│                    Production Hive                             │
│                                                                  │
│  ┌─────────┐    ┌─────────┐    ┌─────────┐    ┌─────────┐      │
│  │ Router  │    │ Router  │    │ Gateway │    │ SY.*    │      │
│  │ (RT)    │    │ (RT)    │    │ (to WAN)│    │ (system)│      │
│  └────┬────┘    └────┬────┘    └─────────┘    └─────────┘      │
│       │              │                                          │
│       │   Unix Sockets (fast, local)                           │
│       │              │                                          │
│  ┌────┴────┐    ┌────┴────┐    ┌─────────┐    ┌─────────┐      │
│  │ AI.     │    │ AI.     │    │ IO.     │    │ WF.     │      │
│  │ support │    │ sales   │    │ whatsapp│    │ crm     │      │
│  │ (agent) │    │ (agent) │    │ (edge)  │    │ (flow)  │      │
│  └─────────┘    └─────────┘    └─────────┘    └─────────┘      │
│                                      │                          │
└──────────────────────────────────────┼──────────────────────────┘
                                       │
                                       │ HTTPS (WhatsApp API)
                                       │
                                  ┌────┴────┐
                                  │ Customer│
                                  │ (phone) │
                                  └─────────┘
```

### Node Types

| Prefix | Purpose | Examples |
|--------|---------|----------|
| **RT** | Router - moves messages | `RT.main@production` |
| **SY** | System - configuration, identity, admin | `SY.identity@mother` |
| **IO** | Edge - connects to external channels | `IO.whatsapp@production` |
| **AI** | Agent - processes conversations | `AI.support.l1@production` |
| **WF** | Workflow - orchestrates processes | `WF.onboarding@production` |

### Shared Memory

Within an hive, nodes communicate through shared memory regions:
- **Node table** - Who's connected right now
- **Config** - Routes and VPNs
- **Identity** - ILKs, degrees, capabilities
- **OPA** - Compiled routing policies

No serialization. No network calls. Just memory reads. This is why it's fast.

### OPA Policies

Routing decisions are made by [OPA](https://www.openpolicyagent.org/) (Open Policy Agent). You write rules like:

```rego
# Route to agent with required capability
target = node {
    required := input.meta.context.required_capability
    some ilk
    data.identity[ilk].type == "agent"
    required in data.identity[ilk].capabilities
    node := data.identity[ilk].handler_node
}

# Only internal humans can see system status
allow {
    input.meta.action == "system_status"
    data.identity[input.meta.src_ilk].human_subtype == "internal"
}
```

Policies are compiled to WASM and distributed to all hives. Changes propagate in seconds.

---

## What can you build with this?

### Multi-channel Customer Support
- Customer writes on WhatsApp → AI agent responds
- Same customer emails later → Same context, same agent knowledge
- Agent can't solve it → Escalates to senior AI → Escalates to human
- Human resolves → AI learns for next time

### AI-Native Sales Team
- Lead comes in → AI qualifier assesses fit
- Qualified → AI sales rep handles objections
- Ready to close → Senior AI or human closer takes over
- All in the same conversation thread, all with full context

### Operations Dashboard
- Internal operators (human/internal) can query system status
- See which agents are handling what
- Monitor escalation rates
- Adjust routing in real-time

### Multi-tenant SaaS
- Each tenant gets isolated agents, routing, and data
- Billing per tenant
- Custom training per tenant
- Shared infrastructure, separated concerns

---

## Design Principles

### 1. AI-Native, Human-Compatible
The system assumes AI is the primary operator. Humans are escalation points, not the main workforce. But when humans are needed, they have full visibility.

### 2. Identity is Everything
Every message carries who sent it, who it's for, and what conversation it belongs to. You can't lose context. You can't have orphan messages.

### 3. Verified Knowledge
Agents can't operate without valid credentials. Training is versioned, hashed, and auditable. No "oops, we deployed the wrong prompt."

### 4. Local Speed, Global Reach
Within an hive: shared memory, microsecond latency.
Between hives: async replication, eventual consistency.
Best of both worlds.

### 5. Policy-Driven Routing
Business rules live in OPA policies, not in code. Change who handles what without deploying code. Audit routing decisions after the fact.

### 6. The System Doesn't Self-Modify
Configuration comes from outside (admins, APIs). The system executes but doesn't decide its own rules. This is intentional. AI managing AI is powerful, but there's always a human-controlled layer at the top.

---

## Current Status

This is a working system with ongoing spec/doc alignment. Core routing, SHM regions, node communication, and identity v2 core flows are implemented and running in multi-hive E2E.

### Implemented
- Core router with FIB and shared memory
- Node library with split sender/receiver model
- Inter-hive gateway communication
- OPA policy compilation and distribution
- Configuration broadcast and replication
- `SY.identity` v2 core:
  - primary/replica sync (full + delta),
  - DB persistence (`identity_*` tables),
  - SHM identity region + alias canonicalization,
  - orchestrator node registration integration (`ILK_REGISTER`/`ILK_UPDATE`)

### Still In Progress
- Module/Degree/Graduation lifecycle service
- Product runtimes outside this repo using identity helpers end-to-end:
  - IO runtimes (channel lookup + `ILK_PROVISION`)
  - `AI.frontdesk` runtime (complete register + merge channel flows)
- Broader AI/workflow runtime catalog

---

## Getting Started

See the [Technical Specification](./docs/) for complete details.

### Development Guide

This README explains the system and concepts. For how to run, build, and develop locally, see `DEVELOPMENT.md`.

### Node Development Template (Rust)

If you want to build a node in another repo, use `fluxbee_sdk`
(`json-router/crates/fluxbee_sdk`) as the canonical base SDK.

For AI-specific runtimes, `json-router/crates/fluxbee_ai_sdk` is available as a separate higher-level SDK on top of `fluxbee_sdk`. It is intended for `SY.architect` and future `AI.*` nodes, not as a replacement for the transport/protocol SDK.

SDK entry points:
- [AI SDK README](./crates/fluxbee_ai_sdk/README.md) - Immediate memory, function calling, summary refresh, and AI runtime guidance.
- [AI immediate memory spec](./docs/immediate-conversation-memory-spec.md) - Contract and design scope for short-horizon conversation context.

For domain nodes that live in this repo (for example `.gov`), see:
- `nodes/gov/README.md`

**What to copy**
```
json-router/crates/fluxbee_sdk/
```

**Suggested structure**
```
my-node/
├── Cargo.toml
├── src/
│   └── main.rs
└── fluxbee_sdk/       # copied from json-router/crates/fluxbee_sdk
```

**Cargo.toml**
```toml
[package]
name = "my-node"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1.37", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
uuid = { version = "1.7", features = ["v4"] }
fluxbee-sdk = { path = "./fluxbee_sdk" }
```

**Minimal node example**
```rust
use fluxbee_sdk::{connect, NodeConfig, NodeUuidMode};
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = NodeConfig {
        name: "WF.test".to_string(),
        router_socket: "/var/run/fluxbee/routers".into(),
        uuid_persistence_dir: "/var/lib/fluxbee/state/nodes".into(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: "/etc/fluxbee".into(),
        version: "1.0".to_string(),
    };

    let (sender, mut receiver) = connect(&config).await?;

    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Broadcast,
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: Some("HELLO".to_string()),
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload: serde_json::json!({"hello":"world"}),
    };
    sender.send(msg).await?;

    loop {
        let msg = receiver.recv().await?;
        println!("received: {:?}", msg);
    }
}
```

Key documents:
- `01-arquitectura.md` - Architecture and concepts
- `02-protocolo.md` - Message protocol and node library
- `03-shm.md` - Shared memory structures
- `04-routing.md` - FIB, VPNs, OPA integration
- `10-identity-v2.md` - Identity system v2 and L3 routing

### SDK Tools (Current)

`fluxbee_sdk` is the canonical base toolset for node development. `fluxbee_ai_sdk` is a separate AI-oriented crate layered on top of it for architect/AI runtimes. Current base toolbox:

| Tool | Path | Purpose |
|---|---|---|
| Node connection | `fluxbee_sdk::{connect, NodeConfig}` | Connect node to router (split sender/receiver) |
| Tunable connection config | `fluxbee_sdk::ClientConfig` + `connect_with_client_config` | Configure retry/backoff/keepalive/timeouts for node-router sessions |
| Protocol types | `fluxbee_sdk::protocol` | Build/route messages (`Message`, `Routing`, `Destination`, `Meta`) |
| Typed payloads | `fluxbee_sdk::payload::TextV1Payload` | Canonical `text/v1` payload (`content`, `content_ref`, `attachments`) |
| NATS client wrappers | `fluxbee_sdk::nats` | Request/reply, publish/subscribe and timeout/reconnect-aware helpers |
| Blob toolkit | `fluxbee_sdk::blob::BlobToolkit` | `put`, `put_bytes`, `promote`, `resolve`, `resolve_with_retry`, GC |
| Blob confirmed publish | `fluxbee_sdk::blob::PublishBlobRequest` | `publish_blob_and_confirm` (`SYSTEM_SYNC_HINT` gate before emitting `blob_ref`) |
| Blob metrics snapshot | `fluxbee_sdk::blob::BlobToolkit::metrics_snapshot` | Operational counters (`put/resolve/retry/errors/bytes`) |
| Identity SHM lookup | `fluxbee_sdk::identity::{resolve_ilk_from_shm_name, resolve_ilk_from_hive_id, resolve_ilk_from_hive_config}` | Resolve `(channel_type,address) -> ilk` locally from identity SHM |
| Identity provision | `fluxbee_sdk::identity::{IlkProvisionRequest, provision_ilk}` | Request `ILK_PROVISION` with automatic `NOT_PRIMARY` fallback target support |
| Identity system calls | `fluxbee_sdk::identity::{IdentitySystemRequest, identity_system_call, identity_system_call_ok}` | Generic helpers for `ILK_REGISTER`, `ILK_ADD_CHANNEL`, `ILK_UPDATE`, tenant actions |
| Node status default handler | `fluxbee_sdk::try_handle_default_node_status` | Respond `NODE_STATUS_GET` with canonical `health_state`; avoids `ORCHESTRATOR_INFERRED` fallback when node is healthy |
| Admin internal gateway | `fluxbee_sdk::{admin_command, admin_command_ok, AdminCommandRequest}` | Execute `ADMIN_COMMAND` over socket/WAN against `SY.admin@<hive>` (same control surface as HTTP) |
| Convenience imports | `fluxbee_sdk::prelude::*` | Common SDK symbols in one import |

`NodeConfig` now supports two UUID modes:
- `NodeUuidMode::Persistent`: default for normal nodes. Reuses/stores L1 UUID in `uuid_persistence_dir`.
- `NodeUuidMode::Ephemeral`: for short-lived relays/diagnostic sessions. Generates a new L1 UUID per process without writing `*.uuid` files.

Both modes still require:
- L1 UUID for connection identity (`HELLO.uuid`, `routing.src`)
- L2 node name for canonical process identity (`HELLO.name`, router registration, routing by name)

Recommended for every new node/scaffold:
- call `try_handle_default_node_status(&sender, &msg).await` inside the receive loop.
- keep it enabled unless the runtime provides a custom status handler with the same contract.
- this makes `GET /hives/{hive}/nodes/{name}/status` report `health_source=NODE_REPORTED` in normal operation.

#### Admin internal gateway from SDK (`ADMIN_COMMAND`)

Use this when a node/workflow needs to call `SY.admin` without HTTP (socket/WAN path):

```rust
use std::time::Duration;
use fluxbee_sdk::{admin_command, AdminCommandRequest};
use serde_json::json;

let out = admin_command(
    &sender,
    &mut receiver,
    AdminCommandRequest {
        admin_target: "SY.admin@motherbee",
        action: "list_admin_actions",
        target: None,                 // use Some("worker-220") for hive-scoped actions
        params: json!({}),
        request_id: None,
        timeout: Duration::from_secs(10),
    },
).await?;

assert_eq!(out.status, "ok");
```

Operational checks for this gateway:
- `scripts/admin_internal_socket_actions_e2e.sh`
- `scripts/admin_http_socket_parity_e2e.sh`
- `scripts/admin_list_actions_e2e.sh`
- `scripts/admin_all_actions_matrix_e2e.sh`

#### Blob: basic flow (`put`/`promote` -> attach)

```rust
use fluxbee_sdk::blob::{BlobConfig, BlobToolkit};
use fluxbee_sdk::payload::TextV1Payload;

let blob = BlobToolkit::new(BlobConfig::default())?;
let blob_ref = blob.put_bytes(b"hello", "note.txt", "text/plain")?;
blob.promote(&blob_ref)?;

let payload = TextV1Payload::new("Adjunto archivo", vec![blob_ref]);
let payload_json = payload.to_value()?;
```

#### Blob: confirmed publish before emit (`publish_blob_and_confirm`)

```rust
use fluxbee_sdk::blob::{BlobConfig, BlobToolkit, PublishBlobRequest};
use fluxbee_sdk::{connect, NodeConfig, NodeUuidMode};

let cfg = NodeConfig {
    name: "WF.blob.publisher".into(),
    router_socket: "/var/run/fluxbee/routers".into(),
    uuid_persistence_dir: "/var/lib/fluxbee/state/nodes".into(),
    uuid_mode: NodeUuidMode::Persistent,
    config_dir: "/etc/fluxbee".into(),
    version: "1.0".into(),
};
let (sender, mut receiver) = connect(&cfg).await?;

let blob = BlobToolkit::new(BlobConfig::default())?;
let published = blob.publish_blob_and_confirm(
    &sender,
    &mut receiver,
    PublishBlobRequest {
        data: b"payload",
        filename_original: "payload.txt",
        mime: "text/plain",
        targets: vec!["worker-220".into()],
        wait_for_idle: true,
        timeout_ms: 30_000,
    },
).await?;

// emit only after confirm
let payload = fluxbee_sdk::payload::TextV1Payload::new("ready", vec![published.blob_ref]);
let payload_json = payload.to_value()?;
```

Operational Blob references:
- `docs/blob-annex-spec.md`
- `scripts/blob_sync_e2e.sh`
- `scripts/blob_sync_multi_hive_e2e.sh`

### Functional Specification (Docs)

The functional specification lives in `docs/`. There is no cross-navigation between files yet, so here is the full index:
- `01-arquitectura.md` - Architecture overview
- `02-protocolo.md` - Protocol and node library behavior
- `03-shm.md` - Shared memory regions and layout
- `04-routing.md` - Routing, FIB, VPNs, OPA integration
- `05-conectividad.md` - WAN connectivity and gateway behavior
- `06-regiones.md` - Config/LSA regions and update flows
- `07-operaciones.md` - Ops, deployment, and admin workflows
- `08-apendices.md` - Appendix and reference notes
- `09-router-status.md` - Router implementation status checklist
- `10-identity-v2.md` - Identity system and L3 routing (current spec)
- `SY_nodes_spec.md` - System nodes specification

---

## License

MIT License - see [LICENSE](LICENSE) for details.

---

## Contributing

Contributions welcome. Please read the technical specifications first.

---

**Fluxbee** — *Where AI agents work together*
