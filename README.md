<div align="center">

<img src="docs/assets/fluxbee-logo.svg" alt="Fluxbee" width="160" />

# Fluxbee

**The OS for autonomous AI agent clusters**

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-fluxbee.ai-blue)](https://fluxbee.ai)
[![Status](https://img.shields.io/badge/status-active%20development-orange)]()

*Describe what you need to Archi. It designs the solution, asks you to confirm, and applies it.
The hive routes every message by identity and policy, keeps the memory of each conversation,
and records what every model call costs.*

</div>

---

## Table of contents

- [What is Fluxbee?](#what-is-fluxbee)
- [Core concepts](#core-concepts)
- [Architecture](#architecture)
- [Current status](#current-status)
- [Getting started](#getting-started)
- [Create a node](#create-a-node)
- [Operate](#operate)
- [Documentation](#documentation)
- [License & contributing](#license--contributing)

---

## What is Fluxbee?

Fluxbee is infrastructure for systems where AI agents, people and workflows work together across
channels. Think of it as a phone system that, instead of phone numbers, connects AI agents (each
with its own role), people (customers and operators), workflows, and channels such as Slack,
WhatsApp or an HTTP API. Every message goes through a router that knows who sent it, where it
should go, and which conversation it belongs to.

Most AI integrations are point to point: one bot on WhatsApp, another on Slack, each with its own
idea of who the user is. Fluxbee puts them on one mesh, with one identity registry, one routing
policy, a memory per conversation, and a record of what each model call costs and who pays for it.

The router, the system services and the AI and IO runtimes are written in Rust. The policy
compiler, the timers and the workflow engine are written in Go.

---

## Core concepts

### Hives

A **hive** is one machine running Fluxbee. Every hive runs a router (`RT.gateway`) and
`SY.orchestrator`, which starts and watches the hive's other nodes. A hive has one of four roles:

| Role | Purpose | Created by |
|---|---|---|
| `motherbee` | The control plane: admin API, Archi, SY.vault, PostgreSQL. One per installation. | The `.deb` package and `sudo fluxbee-firstboot` |
| `worker` | More room for agents, channels and workflows. | `add_hive` from the motherbee |
| `ingress` | The public HTTPS door: `SY.edge` on port 443. | `add_hive` with `"role": "ingress"` |
| `egress` | Outbound NAT: the workers' way to the internet. | `add_hive` with `"role": "egress"` |

### Nodes

A **node** is a process attached to its hive's router through a Unix socket. Its name is
`<PREFIX>.<name>@<hive>`, with five prefixes: `RT` (the router's own nodes, reserved), `SY`
(system services), `AI` (agents), `IO` (channels to the outside world) and `WF` (workflows).
Examples: `SY.identity@motherbee`, `AI.billing@motherbee`, `IO.slack.support@motherbee`,
`WF.onboarding@worker1`.

`AI`, `IO` and `WF` nodes are instances of a **runtime**, a versioned package such as
`ai.generic`. The orchestrator launches them with `run_node`, always for a tenant.

### Identity: ILK and tenant

- **ILK** (`ilk:<uuid>`): the actor. Its type is `human` (a person), `agent` (any node the
  orchestrator launches: AI, IO or WF) or `system` (the `SY.*` services). An ILK can hold several
  channels, such as a Slack user, a WhatsApp number or an email.
- **Tenant** (`tnt:<uuid>`): a **cost center**. It pays for what its ILKs and nodes use. Each ILK
  is charged to exactly one tenant, and charging it to another keeps the same ILK. A tenant is not
  a company: Fluxbee Cloud maps its companies to tenants. Changing an ILK's tenant grants or
  revokes nothing. A tenant that is not active gets no secrets and no new nodes.
- **Root tenant** (`tnt:00000000-0000-0000-0000-000000000001`): the installation's own. Only the
  core runs there (`SY.*`, the router, `IO.cloud`, `IO.blob`, `IO.web`). Every other node runs in a tenant
  you create.

A new person arrives in two steps: an IO node provisions a `temporary` ILK for a sender it does
not know, and `SY.frontdesk.gov` completes the registration. `SY.identity` on the motherbee is the
primary, backed by PostgreSQL; each worker keeps a replica. The full model is in
[docs/ilk-tenant-model-v1.md](docs/ilk-tenant-model-v1.md).

### Agents

An agent is an `AI.*` node on `ai.generic`, the generic AI runtime. Two things define it:

- **Its config:** `behavior.kind` (`ai_chat`, or `echo`, which replies with the input and calls
  no model). The agent runs on its hive's AI provider and model, picked in `hive.yaml`
  (`ai.default_provider`, `ai.providers.<provider>.model`); a fresh install uses OpenAI with
  `gpt-5.5`. An agent that needs its own names them in `behavior.provider` (OpenAI or
  Anthropic) and `behavior.model`, a model of its provider.
- **Its cognitive definition, on its ILK:** `role_hash`, `skill_hashes`, `handbook_hashes` and
  `personality_hash`. Each hash names a JSON asset in `blob://agent-assets/<hash>.json`, and the
  runtime refuses an asset whose content does not match its hash. Archi writes the assets. The
  admin action `set_ilk_definition` puts them on the ILK. The agent rebuilds its prompt when its
  ILK changes, without a restart.

An agent without a definition runs on a fixed prompt that makes it answer only with a
`"status": "unconfigured"` message.

An API key is never node config. An agent reads its key from `SY.vault` on every call: a key it
owns, one assigned to it, or its tenant's general key for its provider.

Every agent has the same tools, declared in the runtime's code: state about the person it talks
to, files it sends with its reply, and a public page. Their catalog, generated from the code, is
[docs/reference/agent-tools.md](docs/reference/agent-tools.md).

### Memory

Memory belongs to a **communication**: the thread between someone and the agent that serves them,
keyed by `(thread_id, agent ILK)`. Two agents in one conversation are two communications, each
with its own memory.

The agent's router copies both directions of its messages to `SY.cognition` on the same hive.
Cognition builds the memory and keeps it in the hive's shared memory, and `SY.storage` stores what
it derives. When the router delivers the agent's next message, it attaches that memory
(`meta.memory_package`). Only `AI.*` nodes receive it. Details: §6 of
[docs/router-stream-v1.md](docs/router-stream-v1.md).

### Routing and policy

A message names its destination: a node (by name or UUID), a broadcast, or nobody (`dst: null`).
When it names nobody, the router decides:

1. A sender with no ILK, or a `temporary` one, goes to `SY.frontdesk.gov`. This rule belongs to
   the **system policy**, compiled from `policy/system.rego` and baked into the router binary. It
   also decides who may send protected system actions. If it fails to load, the router does not
   start.
2. Anyone else goes where the **user policy** says. There is one user policy for the whole
   installation: the motherbee compiles it to WASM and every hive installs it
   ([docs/opa-distribution.md](docs/opa-distribution.md)).

A user policy is package `router` with a `target` rule:

```rego
package router

import rego.v1

# Send what people write to the billing agent.
target := "AI.billing@motherbee" if {
    data.identity[input.meta.src_ilk].ilk_type == "human"
}
```

The input carries the message's `meta`, `routing` (`src`, `src_l2_name`) and `action`.
`data.identity` maps each ILK to `tenant_id`, `ilk_type`, `registration_status`, `handler_node`,
`role_hash`, `skill_hashes`, `handbook_hashes` and `personality_hash`. `tenant_id` is a cost
center: do not use it as a permission. Check a policy with `POST /opa/policy/check` and apply it
with `POST /opa/policy` (body `{"rego": "..."}`).

`SY.config.routes` keeps each hive's routes (forward or drop by name prefix) and VPN rules (VPNs
assigned by name pattern; outside the system nodes, only nodes in the same VPN reach each other).

### Logs and usage

- Each router runs a small stream, `RT.<router>.stream@<hive>`. The router where a message enters
  copies it there once (system and admin messages aside), and `SY.storage` writes it to the
  `turns` table on the motherbee: the message log. The stream is best effort, with a two-minute
  window. It absorbs delays; it is not a store.
- Every model call (agents, cognition, the frontdesk, Archi) leaves a usage record charged to a
  tenant, in the `llm_usage` table. Read it with `GET /hives/motherbee/usage`, filtered by
  `tenant_id`, `from`, `to` and `group_by`. Fluxbee Cloud reads its tenants' records through
  `IO.cloud`.

### SY.admin and Archi

- **SY.admin** is the door for every change. Operators, Archi, Fluxbee Cloud (through `IO.cloud`)
  and `SY.wf-rules` all act through it. Its HTTP API listens on `127.0.0.1:8080` on the motherbee.
- **Archi** (`SY.architect`) is the operator's AI architect: a chat at `http://127.0.0.1:3000` on
  the motherbee. It designs a solution, asks you to confirm, applies it through SY.admin, and
  checks the result. It also publishes packages and writes agent assets.

---

## Architecture

```text
            ┌───────────────────────────────────────────────┐
            │ motherbee                                     │
            │                                               │
            │ RT.gateway              SY.orchestrator       │
            │ SY.admin :8080          Archi :3000           │
            │ SY.identity (primary)   SY.vault              │
            │ SY.storage              SY.cognition  ...     │
            │ PostgreSQL                                    │
            └────────┬───────────────────┬─────────────┬────┘
                     │                   │             │
                     │                   │             │   WAN, port 9000, mutual TLS:
                     │                   │             │   each hive dials the motherbee
                     │                   │             │
      ┌──────────────┴──────────┐ ┌──────┴──────┐ ┌────┴────────┐
      │ worker1                 │ │ ingress     │ │ egress      │
      │                         │ │             │ │             │
      │ RT.gateway              │ │ RT.gateway  │ │ RT.gateway  │
      │ SY.identity (replica)   │ │ SY.edge :443│ │ outbound NAT│
      │ SY.cognition  SY.timer  │ │ public HTTPS│ └─────────────┘
      │ AI, IO and WF nodes     │ └─────────────┘
      └─────────────────────────┘
```

**System nodes** (`system_nodes` in [packaging/hive.yaml.example](packaging/hive.yaml.example);
every hive also runs the router, `RT.gateway`):

| Node | Runs on | What it does |
|---|---|---|
| `SY.orchestrator` | every hive | Launches and watches nodes, joins new hives, rolls out software. |
| `SY.config.routes` | every hive | Routes and VPN rules. |
| `SY.opa.rules` | every hive | Compiles the user policy (motherbee) and installs it (every hive). |
| `SY.identity` | motherbee (primary), workers (replica) | ILKs, tenants and channels. |
| `SY.cognition` | motherbee, workers | Memory per communication. |
| `SY.timer` | motherbee, workers | Persistent timers and time services. |
| `SY.wf-rules` | motherbee, workers | Deploys workflows through SY.admin. |
| `SY.admin` | motherbee | The admin API. |
| `SY.architect` | motherbee | Archi. |
| `SY.storage` | motherbee | Writes the message log, cognition data and usage records to PostgreSQL. |
| `SY.frontdesk.gov` | motherbee | Registers new people. |
| `SY.vault` | motherbee | Secrets, encrypted with AES-256-GCM. |
| `SY.edge` | ingress | The public HTTPS door. |

**Inside a hive**, each node talks to its router over a Unix socket in `/var/run/fluxbee/routers`.
The router reads shared-memory regions that the system nodes keep: its connected nodes
(`jsr-<router uuid>`), routes and VPN rules (`jsr-config-<hive>`), the other hives' topology
(`jsr-lsa-<hive>`), identity (`jsr-identity-<hive>`), the compiled user policy (`jsr-opa-<hive>`)
and memory packages (`jsr-memory-<hive>`).

**Base runtimes**, shipped in the `.deb` ([packaging/base-nodes.json](packaging/base-nodes.json)):

| Runtime | What it is |
|---|---|
| `ai.generic` | The AI agent runtime. |
| `wf.engine` | The workflow engine. WF nodes run workflow packages on it. |
| `io.api` | An HTTP API channel for a tenant, published through SY.edge with its own bearer token. |
| `io.slack` | Slack over Socket Mode, one node per workspace and channel binding. |
| `io.wapp` | WhatsApp, one node per number. Test-grade. |
| `io.linkedhelper` | LinkedHelper. Test-grade. |
| `io.cloud` | The relay for Fluxbee Cloud. Motherbee, root tenant, started at boot. |
| `io.blob` | Public files served through SY.edge. Motherbee, root tenant, started at boot. |

Software lives in `/var/lib/fluxbee/dist/`. Only the motherbee writes it, and Syncthing carries it
to the other hives.

---

## Current status

Fluxbee runs end to end on a multi-hive lab (motherbee, workers, ingress, egress) and is in active
development: APIs and docs still change. This section describes version 0.1.79.

### Working today

- Router: nodes on Unix sockets, shared-memory tables, WAN links between hives with mutual TLS.
- Identity (ILKs, tenants as cost centers, channels), replicated from the motherbee to workers.
- Agents on `ai.generic` with OpenAI or Anthropic, defined by hash-checked assets on their ILK;
  memory per communication, built by `SY.cognition` and delivered by the router.
- The message log and per-tenant LLM usage records, carried by each router's stream.
- Policy: the baked system policy plus one global user policy.
- Install (`.deb` and `fluxbee-firstboot`, then `add_hive`) and packages (scaffold, publish,
  `run_node`).
- Workflows on `wf.engine`, deployed by `SY.wf-rules`; persistent timers in `SY.timer`.
- The public door (`SY.edge`), `IO.cloud` for Fluxbee Cloud, and `IO.blob` for public files.

### Next

- Archi and `io.web` reachable through Fluxbee Cloud, with the edge as reverse proxy
  ([docs/io-web-v2.md](docs/io-web-v2.md): direction agreed, not built).

### Known limits

- The admin API and Archi have no authentication. They listen on loopback only.
- Fluxbee does not manage the host firewall yet: stage 4 of
  [docs/host-posture-and-exposure-spec-v1.md](docs/host-posture-and-exposure-spec-v1.md) is parked.
- Archi writes agent assets to the motherbee's blob store, and they are not synced to workers. An
  agent that needs a definition runs on the motherbee.

---

## Getting started

### 1. Build the package

On an Ubuntu 24.04 build machine with git, Rust (`rustup`), Go, `protoc` and `python3`:

```bash
git clone https://github.com/4iplatform/fluxbee.git
cd fluxbee
packaging/build-deb.sh 0.1.0        # writes dist/fluxbee_0.1.0_amd64.deb
```

### 2. Install the motherbee

On a clean Ubuntu 24.04 machine:

```bash
sudo apt-get install ./fluxbee_0.1.0_amd64.deb   # also installs PostgreSQL
sudoedit /etc/fluxbee/hive.yaml                  # copied from hive.yaml.example: hive_id, wan, ai
sudo fluxbee-firstboot
```

`fluxbee-firstboot` starts PostgreSQL and creates the databases, starts the orchestrator, stores
the database URL in SY.vault, waits for the hive to be ready, and starts `IO.cloud` and `IO.blob`.
Those two run degraded until you configure them.

Then load the AI key the core uses (Archi, cognition, the frontdesk) into the root tenant:

```bash
curl -sS -X POST http://127.0.0.1:8080/hives/motherbee/vault/secrets \
  -H 'Content-Type: application/json' \
  -d '{"key":"openai_root_pool","value":{"api_key":"sk-..."},"metadata":{"tenant_id":"tnt:00000000-0000-0000-0000-000000000001","resource_type":"openai"}}'
```

For Anthropic, use `"resource_type": "anthropic"`, set `ai.default_provider: anthropic` in
`/etc/fluxbee/hive.yaml`, and restart the AI nodes.

### 3. Open Archi

Archi and the admin API listen on loopback, so reach them through an SSH tunnel:

```bash
ssh -L 3000:127.0.0.1:3000 -L 8080:127.0.0.1:8080 <user>@<motherbee>
```

Then open `http://127.0.0.1:3000`.

### 4. Add a worker

A worker starts as a clean Ubuntu 24.04 box with SSH and a user with passwordless sudo. From the
motherbee:

```bash
curl -sS -X POST http://127.0.0.1:8080/hives \
  -H 'Content-Type: application/json' \
  -d '{"hive_id":"worker1","address":"192.0.2.21","ssh_user":"ubuntu","ssh_password":"..."}'
```

- Pass `ssh_key` (an unencrypted PEM private key) instead of `ssh_password` when the box only
  accepts keys. Neither is stored.
- The join runs in the background: the call answers `accepted` (HTTP 202). Poll
  `GET /hives/worker1` until its status is `connected`.
- SSH is used only during the join, which removes the motherbee's key from the box at the end.
- `"role": "ingress"` and `"role": "egress"` add the other roles.
  `GET /admin/actions/add_hive` lists their fields.

---

## Create a node

A package has one of three types:

| Type | What you ship | What it runs on |
|---|---|---|
| `config_only` | Config and assets | A base runtime, such as `ai.generic` |
| `workflow` | A flow definition (`flow/definition.json`) | `wf.engine` |
| `full_runtime` | Your own executable, as `bin/start.sh` | Itself |

This walk-through publishes an agent on `ai.generic` and runs it on the motherbee. You need a
running motherbee, `jq`, and a checkout of this repository with a Rust toolchain. Run the API
calls on the motherbee, or from your machine through the SSH tunnel above.

```bash
ADMIN=http://127.0.0.1:8080
```

### 1. Scaffold the package

```bash
cargo build --release --bin fluxbee-scaffold
./target/release/fluxbee-scaffold new --name ai.billing --type config_only --base ai.generic
```

Runtime names use lowercase letters, digits, dots and hyphens. Versions are `MAJOR.MINOR.PATCH`
(`--version`, default `0.1.0`). The scaffold writes `ai.billing/package.json` (name, version,
type, `runtime_base: "ai.generic"`, `config_template`) and `ai.billing/config/default-config.json`:

```json
{
  "behavior": {
    "kind": "ai_chat"
  }
}
```

### 2. Edit it

- Fill in `description`. Raise `version` for every new publish.
- `behavior.kind` is required. Without `behavior.provider` and `behavior.model`, the agent runs on
  the hive's provider and that provider's model (`gpt-5.5` with the default OpenAI). Name them
  only when the agent needs its own; the model must belong to the agent's provider.
- Leave out `temperature`: the current models reject it.
- Put no keys in the config. `ai.generic` rejects `secrets`, `behavior.api_key` and
  `behavior.openai`.
- The prompt is not config: `ai.generic` rejects `behavior.instructions`. The agent's prompt is
  its ILK's definition (step 6); until it has one, it is the fixed "unconfigured" one.

At spawn, the orchestrator builds the node's config from this file, then the request's `config`
(each top-level key replaces the file's), then a `_system` block it controls.

### 3. Pack it

```bash
./target/release/fluxbee-scaffold pack ./ai.billing    # writes ai.billing-0.1.0.zip
```

`pack` checks the layout (a `full_runtime` needs an executable `bin/start.sh`, a `workflow` needs
`flow/`) and zips the directory next to it.

### 4. Publish it

- **From Archi:** in the chat sidebar, open **Publish Software**, drop the zip, tick
  **Promote To Current**, and click **Publish**. A first version must be current, or
  `"runtime_version": "current"` finds nothing to run.
- **From the admin API**, straight from the files (no zip needed; `set_current` defaults to true):

```bash
jq -n --rawfile pkg ai.billing/package.json --rawfile cfg ai.billing/config/default-config.json \
  '{source: {kind: "inline_package", files: {"package.json": $pkg, "config/default-config.json": $cfg}}}' |
  curl -sS -X POST "$ADMIN/admin/runtime-packages/publish" \
    -H 'Content-Type: application/json' -d @- | jq .
```

The package lands in `/var/lib/fluxbee/dist/runtimes/ai.billing/0.1.0/` on the motherbee.

### 5. Create a tenant and launch the agent

Agents cannot run in the root tenant, and only an active tenant takes new nodes. Create one, store
its general OpenAI key (no owner and no assigned nodes, so every node of the tenant can use it),
and launch the agent:

```bash
TENANT=$(curl -sS -X POST "$ADMIN/hives/motherbee/identity/tenants" \
  -H 'Content-Type: application/json' \
  -d '{"name":"demo","status":"active"}' | jq -r .payload.tenant_id)

curl -sS -X POST "$ADMIN/hives/motherbee/vault/secrets" -H 'Content-Type: application/json' \
  -d '{"key":"demo-openai","value":{"api_key":"sk-..."},"metadata":{"resource_type":"openai","tenant_id":"'"$TENANT"'"}}'

curl -sS -X POST "$ADMIN/hives/motherbee/nodes" -H 'Content-Type: application/json' \
  -d '{"node_name":"AI.billing@motherbee","runtime":"ai.billing","runtime_version":"current","tenant_id":"'"$TENANT"'"}'

curl -sS "$ADMIN/hives/motherbee/nodes/AI.billing@motherbee/status" \
  | jq '.payload.node_status | {lifecycle_state, ilk: .identity.ilk_id}'
```

`lifecycle_state` should read `RUNNING`. The orchestrator registered an agent ILK for the node,
charged to the tenant.

### 6. Give the agent a role

Write a role asset through Archi's API (Archi computes its hash), then put it on the agent's ILK:

```bash
ROLE=$(curl -sS -X POST http://127.0.0.1:3000/api/agent-assets \
  -H 'Content-Type: application/json' \
  -d '{"asset_type":"role","name":"Billing support","description":"Answers questions about invoices, payments and refunds."}' \
  | jq -r .definition_fragment.role_hash)

ILK=$(curl -sS "$ADMIN/hives/motherbee/nodes/AI.billing@motherbee/status" \
  | jq -r .payload.node_status.identity.ilk_id)

curl -sS -X POST "$ADMIN/hives/motherbee/identity/ilks/$ILK/definition" \
  -H 'Content-Type: application/json' \
  -d '{"definition":{"role_hash":"'"$ROLE"'","skill_hashes":[],"handbook_hashes":[],"personality_hash":null}}'
```

Within about ten seconds the agent rebuilds its prompt. Check it:

```bash
curl -sS -X POST "$ADMIN/hives/motherbee/nodes/AI.billing@motherbee/control/config-get" \
  -H 'Content-Type: application/json' -d '{}' | jq .payload.response.definition_state
# "composed"
```

Skills and handbooks are assets too (`"asset_type": "skill"` or `"handbook"`), and Archi's chat
can write all four kinds, personality included.

### 7. Connect a channel

An IO node launched for the same tenant (`io.api`, `io.slack` or `io.wapp`) carries a channel's
messages: its `io.dst_node` setting names the agent, and when it is unset the user policy picks the
target. Each runtime lists its settings in its `CONFIG_GET` contract (`control/config-get`, as
above). What the agent spends shows up under its tenant:

```bash
curl -sS "$ADMIN/hives/motherbee/usage?tenant_id=$TENANT" | jq .
```

### Workflows and custom executables

- **Workflow:** `fluxbee-scaffold new --name wf.onboarding --type workflow --base wf.engine`
  writes the smallest definition `wf.engine` accepts (it waits for a `WF_DONE` message and
  finishes) and `{"gc_retention_days": 7, "gc_interval_seconds": 3600}` as its config. Publish it
  like the agent, then launch `WF.onboarding@<hive>` with runtime `wf.onboarding` and a tenant:
  `WF.*` nodes must run a workflow package. `SY.wf-rules` can publish and launch one in a single
  call (`POST /hives/{hive}/wf-rules`, see
  [docs/wf-rules-quickstart.md](docs/wf-rules-quickstart.md)).
- **Custom executable:** `fluxbee-scaffold new --name ai.custom --type full_runtime` writes a
  placeholder `bin/start.sh` and an empty config. Build your node with an SDK (below), then run
  `fluxbee-scaffold pack ./ai.custom --binary <your executable>`: it copies the executable over
  `bin/start.sh` and zips the package. Publish the zip from Archi.

The package contract (all three types, sources, errors) is in
[docs/history/runtime-packaging-cli-spec.md](docs/history/runtime-packaging-cli-spec.md).

---

## Operate

### The admin API

SY.admin describes itself. List its actions, then ask for one:

```bash
curl -sS http://127.0.0.1:8080/admin/actions | jq -r '.payload.actions[].action'
curl -sS http://127.0.0.1:8080/admin/actions/run_node | jq .payload.entry.request_contract
```

The second call returns the method and path, the required and optional fields, an example
payload, and a ready `curl` line. Every response uses the same envelope: `status`, `action`,
`payload`, `error_code`, `error_detail`. Hive-scoped actions take the hive in the path
(`/hives/{hive}/...`). Secrets, usage records and identity changes live on the motherbee, so those
calls use `/hives/motherbee/...`.

### Ports

| Port | Where | What |
|---|---|---|
| 9000/tcp | motherbee | WAN links between routers. The shipped `hive.yaml` requires mutual TLS (`wan.mtls: required`). |
| 9100/tcp | motherbee | Identity sync to the worker replicas, with a per-hive HMAC (`identity.sync.auth: required`). |
| 9200/tcp | motherbee | IO.web, once the operator starts it: only the ingress' `SY.edge` dials it, over the mesh's mutual TLS, to proxy the web hosts (`archi.<id>`). |
| 22000/tcp | motherbee | Syncthing, for software and blobs. Once a hive switches to the owned options (host posture stage 3), spokes only dial out, and discovery, relays and UDP are off. |
| 443/tcp | ingress | `SY.edge`, the public HTTPS door. |
| 8080/tcp | motherbee, 127.0.0.1 | SY.admin. |
| 3000/tcp | motherbee, 127.0.0.1 | Archi. |
| 8384/tcp | 127.0.0.1 | Syncthing's API. |
| 22/tcp | new spokes | SSH from the motherbee, only while `add_hive` runs. |

### Data on the motherbee

- **PostgreSQL:** `fluxbee_identity` (tenants, ILKs, channels, aliases, tenant history) and
  `fluxbee_storage` (`turns`, the `cognition_*` tables, `llm_usage`). The connection URL
  (credentials and host, no database name) is a SY.vault secret with resource type `postgres` in
  the root tenant. `fluxbee-firstboot` stores it as `storage_postgres_url`, and each service adds
  its own database name.
- **SY.vault:** `/var/lib/fluxbee/vault.db`, values encrypted under
  `/etc/fluxbee/vault.master.key`.
- **Blobs:** `/var/lib/fluxbee/blob/`. `active/` is synced with the workers; `agent-assets/` is
  not.

---

## Documentation

| Topic | Document |
|---|---|
| Identity and tenants (cost centers) | [docs/ilk-tenant-model-v1.md](docs/ilk-tenant-model-v1.md) |
| An agent's cognitive definition | [docs/identity-v2.1-agent-definition-addendum.md](docs/identity-v2.1-agent-definition-addendum.md) |
| Router stream: message log, memory, usage | [docs/router-stream-v1.md](docs/router-stream-v1.md) |
| The user policy on every hive | [docs/opa-distribution.md](docs/opa-distribution.md) |
| IO.cloud, the API for Fluxbee Cloud | [docs/io-cloud-api.md](docs/io-cloud-api.md) |
| SY.edge, the public door | [docs/edge-ingress-spec-v6.md](docs/edge-ingress-spec-v6.md) |
| Host posture and exposure | [docs/host-posture-and-exposure-spec-v1.md](docs/host-posture-and-exposure-spec-v1.md) |
| Secrets | [docs/history/sy-vault-spec.md](docs/history/sy-vault-spec.md) |
| Package format and publishing | [docs/history/runtime-packaging-cli-spec.md](docs/history/runtime-packaging-cli-spec.md) |
| Packaging, build and install (Spanish) | [docs/packaging-and-build.md](docs/packaging-and-build.md) |
| Operations: hive.yaml, add_hive, upgrades (Spanish) | [docs/history/07-operaciones.md](docs/history/07-operaciones.md) |
| Writing a workflow: the handbook's WF chapter, §4.2 | [docs/archi/handbook_fluxbee.md](docs/archi/handbook_fluxbee.md) |
| Deploying and operating workflows with SY.wf-rules | [docs/wf-rules-quickstart.md](docs/wf-rules-quickstart.md) |
| Original architecture, protocol, shared memory and routing specs (Spanish, older) | [docs/history/01-arquitectura.md](docs/history/01-arquitectura.md) to [docs/history/05-conectividad.md](docs/history/05-conectividad.md) |
| Rust SDK | [crates/fluxbee_sdk/README.md](crates/fluxbee_sdk/README.md) |
| AI SDK (Rust) | [crates/fluxbee_ai_sdk/README.md](crates/fluxbee_ai_sdk/README.md) |
| Go SDK | [go/fluxbee-go-sdk/README.md](go/fluxbee-go-sdk/README.md) |

When a document and the code disagree, the code is right.

---

## License & contributing

Fluxbee is licensed under the [MIT License](LICENSE), Copyright (c) 2026 4i Platform Inc.

Contributions are welcome: open an issue or a pull request.

---

<div align="center">

**[fluxbee.ai](https://fluxbee.ai)** &nbsp;·&nbsp; *The OS for autonomous AI agent clusters*

</div>
