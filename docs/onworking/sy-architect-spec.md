# SY.architect — System Architect Node

**Status:** v1.0
**Date:** 2026-03-14
**Audience:** Developers implementing the architect node, frontend, and AI integration
**Parent specs:** `admin-internal-gateway-spec.md`, `node-spawn-config-spec.md`, `10-identity-v2.md`, `fluxbee_sdk`

---

## 1. Purpose

SY.architect is the human-facing interface of Fluxbee. It provides a web-based chat where an admin/architect can:

- Configure the system from scratch (starting with AI provider keys).
- Design and deploy node configurations through conversation with AI.
- Monitor system health and status.
- Test deployed flows by impersonating IO nodes.
- Manage hives, nodes, runtimes, tenants, and identity.

SY.architect is a system node (SY.*) registered under the default `fluxbee` tenant. It connects to the Fluxbee infrastructure via SDK (socket to router) and to AI providers (OpenAI initially) via direct HTTP.

### 1.1 Current Frictions / Constraints

Before implementation, the following constraints from the current core must be treated as first-class design inputs:

- `SY.*` nodes are outside the managed `run_node` lifecycle for v1. `SY.architect` should be treated as a core/system unit, not as a normal managed runtime spawned through `/hives/{hive}/nodes`.
- Because of that, the spec cannot assume `_system.package_path` from the managed runtime pipeline unless a dedicated core packaging path is defined.
- `SY.admin` currently exposes mostly read/operational surfaces. Identity writes such as `TNT_CREATE` / `ILK_REGISTER` remain system-call responsibilities, not public admin HTTP mutations.
- L3 routing/testing is available, but any impersonation/test flag should travel through fields the protocol already tolerates (`meta.context` or equivalent), not through undocumented ad-hoc message shapes.
- Ownership/authentication for the web UI remains explicitly out of scope for v1. The first deliverable should assume localhost/VPN-only exposure.
- The system default tenant (`fluxbee`, or configured default) is a bootstrap responsibility of `SY.identity`, not of `SY.architect`. Architect must assume that base tenant exists by the time the core is considered ready.

---

## 2. Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        SY.architect                                  │
│                                                                      │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────────────────┐ │
│  │ HTTP Server   │  │ AI Clients   │  │ Fluxbee SDK Connection    │ │
│  │ (axum)        │  │ (OpenAI)     │  │ (socket to router)        │ │
│  │               │  │              │  │                           │ │
│  │ • Serve UI    │  │ • Multiple   │  │ • ADMIN_COMMAND to admin  │ │
│  │ • WebSocket   │  │   prompts    │  │ • Read SHM (inventory,   │ │
│  │ • File upload │  │ • Up to 3    │  │   identity, status)       │ │
│  │ • Settings    │  │   concurrent │  │ • IO impersonation        │ │
│  │               │  │   agents     │  │ • Blob toolkit            │ │
│  └──────┬───────┘  └──────┬───────┘  └────────────┬──────────────┘ │
│         │                 │                        │                 │
│         ▼                 ▼                        ▼                 │
│  ┌─────────────────────────────────────────────────────────────────┐│
│  │                    Core Logic                                    ││
│  │  • Chat management (conversations, history)                     ││
│  │  • FCMD / ACMD parsing and execution                            ││
│  │  • System state aggregation                                     ││
│  │  • IO impersonation engine                                      ││
│  │  • Prompt version management (chat-driven)                      ││
│  └─────────────────────────────────────────────────────────────────┘│
│                            │                                         │
│                            ▼                                         │
│                     ┌─────────────┐                                  │
│                     │  LanceDB    │                                  │
│                     │  (local)    │                                  │
│                     │  chat history│                                  │
│                     └─────────────┘                                  │
└─────────────────────────────────────────────────────────────────────┘
```

### 2.1 Two Faces

| Face | Transport | Purpose |
|------|-----------|---------|
| User-facing | HTTP (axum) + WebSocket | Serve chat UI, handle user interactions, file uploads |
| System-facing | Fluxbee SDK (socket to router) | Execute ADMIN_COMMANDs, read SHM, impersonate IO, handle blobs |

### 2.2 AI Integration

SY.architect connects directly to AI providers (OpenAI initially) via HTTP. It does NOT use internal AI nodes for its own reasoning. It manages up to 3 concurrent AI agent connections with different system prompts. If more than 3 are needed, they should be spawned as separate AI nodes.

---

## 3. UI Layout

```
┌─────────────────────────────────────────────────────────────────────┐
│ ⚙ Settings │        System Status Bar                              │
│             │  Hives: 3 (2 alive, 1 stale) │ Nodes: 47 │ DB: ok   │
├─────────────┼───────────────────────────────────────────────────────┤
│             │                                                       │
│  Chat       │              Chat Area                                │
│  History    │                                                       │
│             │  [System] Welcome. API key not configured.            │
│  • Session 1│  [System] Click ⚙ to set your OpenAI key.            │
│  • Session 2│                                                       │
│  • Session 3│  [User] I want to set up a support system            │
│             │  [Architect] I'll help you set that up. First...      │
│             │                                                       │
│             │                                                       │
│             │                                                       │
│             │                                                       │
│             │  ┌─────────────────────────────────┐  ┌──┐  ┌──┐    │
│             │  │ Type a message...                │  │📎│  │➤ │    │
│             │  └─────────────────────────────────┘  └──┘  └──┘    │
└─────────────┴───────────────────────────────────────────────────────┘
```

### 3.1 Settings Panel (⚙ top-left)

A small panel (modal or slide-out) for essential configuration that the system needs before it can operate:

- **AI Provider Key:** OpenAI API key (required). Without this, AI is non-functional.
- **AI Model:** Model selector (default: gpt-4).
- **System info:** Hive ID, node name, ILK (read-only, informational).

Settings are saved via ADMIN_COMMAND `set_node_config` targeting self. Orchestrator updates config.json, architect rereads it.

For network exposure, the bind/listen of the local HTTP server should be declared in `hive.yaml`:

```yaml
architect:
  listen: "127.0.0.1:3000"
```

The intended deployment model is:

- `SY.architect` binds to an internal/local address
- the operator exposes it through an external reverse proxy if desired
- `SY.architect` should not assume direct public exposure

### 3.2 System Status Bar (top)

Real-time system health, updated periodically (every 5-10 seconds):

- Hive count and health (alive/stale).
- Total node count by status.
- Key service health (DB, NATS, identity sync).
- Last inventory update timestamp.

Data source: for v1, `ADMIN_COMMAND` over the Fluxbee socket/admin surface should be the canonical path (`get_inventory_summary` or equivalent). Direct SHM reads may be added later as an optimization, but should not be the first contract the UI depends on.

### 3.3 Chat History (left panel)

List of conversation sessions, ordered by last activity. Each session is a separate chat thread. Clicking a session loads its history in the chat area.

- Stored in LanceDB locally.
- Searchable (semantic search over chat history).
- Each session has a title (auto-generated from first message or AI summary).

### 3.4 Chat Area (main panel)

Standard chat interface with:

- Message bubbles (user / architect / system).
- System messages for status updates, command results, errors.
- File upload button (📎) for blob uploads.
- Send button (➤).
- WebSocket connection for real-time streaming of AI responses.

---

## 4. AI Provider Configuration

### 4.1 Key Storage

AI provider keys are stored in the node's config.json (managed by orchestrator via `set_node_config`):

```json
{
  "_system": { ... },
  "ai_providers": {
    "openai": {
      "api_key": "sk-...",
      "default_model": "gpt-4",
      "max_tokens": 4096
    }
  }
}
```

The key is also declared in `hive.yaml` for system-wide availability:

```yaml
# hive.yaml
ai_providers:
  openai:
    api_key: "sk-..."

architect:
  listen: "127.0.0.1:3000"
```

At startup, SY.architect reads from config.json first. If no key there, falls back to hive.yaml. If neither has a key, the architect operates in "unconfigured" mode — it can serve the UI and settings panel but cannot process AI conversations.

### 4.2 First-Run Flow

```
1. SY.architect starts, no API key configured.
2. UI loads, system status bar shows "AI: not configured".
3. User clicks ⚙, enters OpenAI API key.
4. Architect sends ADMIN_COMMAND set_node_config (target: self) with api_key.
5. Orchestrator updates config.json.
6. Architect rereads config.json, initializes OpenAI client.
7. System status bar updates to "AI: ready".
8. User can now chat.
```

---

## 5. Multi-Agent Prompts

### 5.1 Concept

SY.architect uses multiple system prompts for different "modes" of operation. Each prompt defines a different AI agent behavior. The architect switches between agents based on what the user is asking.

### 5.2 Prompt Storage

Prompts are files shipped with the architect deployment artifact. For v1, this is an open packaging decision because `SY.architect` is a `SY.*` node and is not expected to use the normal managed `run_node` path.

Two acceptable deployment shapes for v1:

- core-installed assets colocated with the binary (for example under `/usr/local/share/fluxbee/sy-architect/` or similar)
- a dedicated core dist/assets path defined by orchestrator packaging

The implementation should not assume managed-node `_system.package_path` unless that contract is added explicitly for core nodes.

Example target layout:

```
sy.architect/
├── package.json
├── bin/
│   └── start.sh
└── assets/
    └── prompts/
        ├── architect.md          # Main system design agent
        ├── operator.md           # System monitoring and operations agent
        └── tester.md             # IO impersonation and testing agent
```

At runtime, prompt maintenance should happen through the same chat surface used for normal operation. The persisted storage still lives locally on disk, but the human-facing control surface is the chat, not a separate prompt editor screen.

### 5.3 Prompt Management Model (Chat-Only)

Prompt maintenance for `SY.architect` should not require a separate UI area. The same chat supports two execution paths:

- normal conversational messages, which may invoke the AI provider
- local control messages, parsed and executed directly by `SY.architect` without invoking the AI provider

For v1, local control messages use a reserved prefix:

- `FCMD:` for architect-owned local functions

These commands must be parsed locally by `SY.architect`. They are not interpreted by the LLM, and they must keep a structured shape.

Initial prompt operations should be limited to:

- `prompt.get`
- `prompt.update_draft`
- `prompt.publish`
- `prompt.rollback`
- `prompt.history`

Recommended wire shape in chat:

```text
FCMD: {"op":"prompt.update_draft","prompt_id":"architect","content":"..."}
```

Not recommended:

```text
FCMD: cambiale el prompt al arquitecto para que sea mas corto
```

The node may still help the human improve prompt text conversationally, but the actual mutation only happens when a valid `FCMD:` payload is received.

### 5.4 Prompt Versioning Requirements

Prompt maintenance needs at least a minimal lifecycle, even if AI is unavailable:

- one current published version
- one current draft version
- at least one previous published version for rollback

Publishing should be a local state transition:

- validate draft shape
- promote draft to published
- preserve previous published as rollback target

This keeps architect operational even if the external AI provider is down: prompt inspection, prompt updates, history lookup, and rollback remain available because they are local functions.

### 5.5 Agent Roles (Initial)

| Agent | Prompt file | Purpose |
|-------|-------------|---------|
| Architect | `architect.md` | Design system: configure nodes, deploy runtimes, set up tenants, plan architecture |
| Operator | `operator.md` | Monitor and operate: check status, troubleshoot, manage hives, view logs |
| Tester | `tester.md` | Test flows: impersonate IO nodes, simulate conversations, validate deployments |

### 5.6 Agent Selection

The architect node determines which agent to use based on conversation context. For v1, simple keyword/intent detection. The user can also explicitly switch: "let me test the support flow" → switches to tester agent.

All agents share the same OpenAI connection and API key. They differ only in system prompt.

### 5.7 Limit

Maximum 3 concurrent agent connections to AI provider. If more specialization is needed, spawn separate AI nodes for those roles.

---

## 6. System Operations via ADMIN_COMMAND

### 6.1 How It Works

The architect uses the `fluxbee_sdk::admin_command()` helper to send commands to `SY.admin` via socket. Every operation the admin API exposes is available to the architect.

Operationally, the chat should expose two explicit non-AI command prefixes:

- `FCMD:` → architect-local functions
- `ACMD:` → admin passthrough commands

Both are parsed locally by `SY.architect` and bypass the LLM.

### 6.2 ACMD Model (Admin Passthrough)

`ACMD:` exists so a human operator can use the architect chat as a single operational terminal without opening a separate shell. The syntax should resemble `curl`, but must not execute shell commands or invoke a real `curl` process.

The intent is:

- familiar operator syntax
- parsed locally
- translated into internal socket/admin calls
- no shell, no subprocess, no remote host selection

Accepted v1 shape:

- HTTP-like method: `GET`, `POST`, `PUT`, `DELETE`
- relative path only (for example `/hives/motherbee/nodes`)
- optional JSON body via `-d`

Disallowed:

- full URLs
- arbitrary headers
- pipes / redirects
- shell expansion
- file reads via `@file`
- host override

Examples:

```text
ACMD: curl -X GET /hives/motherbee/nodes
ACMD: curl -X GET /hives/motherbee/identity/ilks
ACMD: curl -X DELETE /hives/motherbee/nodes/IO.slack.T123/instance
ACMD: curl -X POST /hives/motherbee/nodes/AI.chat@motherbee/messages -d '{"msg_type":"user","msg":"LLM","payload":{"text":"hola"}}'
```

### 6.3 Command Flow in Chat

```
User: "Deploy the billing support agent on worker-220"

Architect AI reasons:
  1. Need to check if runtime ai.soporte.billing is published → get_versions
  2. Need to check if worker-220 is alive → get_inventory
  3. Need to spawn node → run_node with config

Architect executes:
  → ADMIN_COMMAND get_versions (worker-220)
  → ADMIN_COMMAND get_inventory_hive (worker-220)
  → ADMIN_COMMAND run_node (worker-220, AI.soporte.billing.l1, ...)

Architect responds to user:
  "Done. AI.soporte.billing.l1 is running on worker-220.
   Runtime version 2.1.0, health: HEALTHY."
```

### 6.4 What the AI Can Do

The AI can execute any available `ADMIN_COMMAND` action exposed by `SY.admin`. For flows not exposed via admin HTTP/socket, the node may also need direct system calls over the SDK (for example identity write paths).

The full list is defined in `admin-internal-gateway-spec.md`. Initial actions expected to matter for architect:

| Category | Actions |
|----------|---------|
| Hive management | `add_hive`, `remove_hive`, `list_hives`, `get_inventory` |
| Node management | `run_node`, `kill_node`, `list_nodes`, `get_node_status`, `get_node_config`, `set_node_config` |
| Runtime management | `get_versions`, `update`, `sync_hint` |
| Identity read | `list_ilks`, `get_ilk` |
| Routing | `list_routes`, `add_route`, `list_vpns`, `add_vpn` |
| Config | `get_config_storage`, `set_config_storage` |

Notes:

- `TNT_CREATE`, `ILK_REGISTER`, `ILK_ADD_CHANNEL` are identity system calls, not public admin REST mutations.
- If tenant creation/registration is needed from architect workflows, that path should be implemented explicitly as direct system messaging to `SY.identity`.

### 6.5 Command Confirmation

For destructive operations (`kill_node`, `remove_hive`), the AI should confirm with the user before executing. This is enforced in the system prompt, not in code.

---

## 7. IO Impersonation (Test Mode)

### 7.1 Purpose

The architect can impersonate an IO node to test deployed flows. This allows the admin to simulate being a WhatsApp user, a Slack user, etc., and see how the system responds — without having actual external channels configured.

### 7.2 How It Works

```
User: "Test the support flow as a WhatsApp user"

Architect (tester agent):
  1. Creates a temporary ILK via ILK_PROVISION (impersonated user).
  2. Sends a message through the router with:
     - src: architect's own node UUID (L1)
     - meta.src_ilk: the temporary ILK
     - routing.dst: null (let OPA route)
  3. OPA sees temporary ILK → routes to AI.frontdesk (or support, depending on status).
  4. Response comes back to the architect (because it's the sender node).
  5. Architect displays the response in the chat.

User sees in chat:
  [Test:WhatsApp] "Hola, necesito ayuda con una factura"
  [AI.soporte.billing] "¡Hola! Puedo ayudarte con tu factura. ¿Cuál es el número?"
  [Test:WhatsApp] "FAC-2026-001"
  [AI.soporte.billing] "Encontré la factura FAC-2026-001..."
```

### 7.3 Impersonation Rules

- Only SY.architect can impersonate. This is a privileged capability.
- Impersonated messages carry a flag or tag so the system can distinguish test traffic from real traffic. For v1, this should live in a tolerated extensibility surface such as `meta.context.test_mode = true` rather than introducing a new protocol field ad hoc.
- The temporary ILK created for testing is marked with `registration_status: temporary` and a special tag in identification indicating it's a test entity.
- After testing, the architect can clean up test ILKs or let them expire naturally.

### 7.4 Routing Consideration

The architect sends messages with `routing.src` as its own UUID (the router knows SY.architect as a connected node). The `meta.src_ilk` is the impersonated ILK. OPA routes based on `src_ilk`, so the message enters the normal routing pipeline. Responses come back to the architect's UUID because it was the sending node at L1.

If OPA or config routes by L1 UUID in some paths, the impersonation may not work for those specific paths. This is a known limitation for v1 — document and address if it becomes a blocker.

---

## 8. File Upload (Blobs)

### 8.1 Flow

```
User clicks 📎 → selects file → file uploads via HTTP multipart to architect

Architect:
  1. Receives file via HTTP.
  2. Uses blob toolkit: put() → promote().
  3. Creates BlobRef.
  4. Includes BlobRef in the next AI message context or ADMIN_COMMAND.
```

### 8.2 Use Cases

- Upload a custom prompt file for a node.
- Upload a runtime package (zip) for publishing.
- Upload configuration files.
- Upload test data for IO impersonation scenarios.

### 8.3 Storage

Files go through the standard blob pipeline: staging → active in `/var/lib/fluxbee/blob/`. The architect uses `fluxbee_sdk::blob` for all operations.

---

## 9. Chat Persistence (LanceDB)

### 9.1 Schema

```
Sessions:
  session_id: uuid
  title: string (auto-generated or user-defined)
  created_at: timestamp
  last_activity_at: timestamp
  agent: string (architect|operator|tester)

Messages:
  message_id: uuid
  session_id: uuid
  role: string (user|assistant|system)
  content: string
  timestamp: timestamp
  metadata: json (command results, blob refs, test mode info)
  embedding: vector (for semantic search)
```

### 9.2 Why LanceDB

- Semantic search over chat history ("what did I configure last week about billing?").
- Embedded, no external process.
- Local to the architect node.
- Reconstructible (chat history is operational, not business-critical).

### 9.3 Location

`/var/lib/fluxbee/nodes/SY/SY.architect@<hive>/architect.lance`

---

## 10. Node Lifecycle

### 10.1 Startup

```
1. SY.architect starts.
2. Reads config.json (from orchestrator).
3. Reads hive.yaml for fallback AI provider key.
4. Initializes LanceDB for chat persistence.
5. Connects to router via SDK (gets node UUID, registers as SY.architect@<hive>).
6. Starts axum HTTP server (default: 127.0.0.1:3000).
7. If API key available: initializes AI client → ready.
8. If no API key: serves UI in unconfigured mode → waits for user to set key.
```

### 10.1.1 Bootstrap Ordering Dependency

Before `SY.architect` is considered operational, the core bootstrap sequence must already have ensured:

1. `SY.identity` is running.
2. `SY.identity` has created or loaded the default system tenant (default name: `fluxbee`; canonical ID generated by identity as `tnt:<uuid>`).
3. `SY.orchestrator` can register core/system ILKs against that tenant.

`SY.architect` must not be responsible for creating the base system tenant. It may later help create or resolve additional tenants through explicit identity system calls, but the initial system tenant is part of core bootstrap sequencing.

### 10.2 ILK and Identity

SY.architect is registered in identity under the `fluxbee` default tenant by orchestrator (standard SY node registration flow). Its ILK is of type `system`.

### 10.3 Config File

```json
{
  "_system": {
    "ilk_id": "ilk:...",
    "node_name": "SY.architect@motherbee",
    "hive_id": "motherbee",
    "runtime": "sy.architect",
    "runtime_version": "1.0.0",
    "created_at": "...",
    "created_by": "..."
  },
  "ai_providers": {
    "openai": {
      "api_key": "sk-...",
      "default_model": "gpt-4",
      "max_tokens": 4096
    }
  },
  "http": {
    "listen": "127.0.0.1:3000"
  }
}
```

For v1, the effective bind order should be:

1. `JSR_ARCHITECT_LISTEN` environment override
2. `architect.listen` in `hive.yaml`
3. fallback default `127.0.0.1:3000`

---

## 11. Security

### 11.1 Current State (Pilot)

- HTTP server binds to `127.0.0.1` by default (localhost only).
- No authentication required to access the chat.
- The system is open until an admin registers ownership.

### 11.2 Open Design Question: Ownership and Access Control

The intended model is: the first person to interact with the system becomes the owner/admin. After that, access should be restricted. However, the authentication and access control mechanism is explicitly deferred. The current security model (passwords, tokens, RBAC) is considered inadequate for the vision of Fluxbee.

**This is an open design area.** The following questions remain unresolved:

- How does the owner authenticate after initial registration? (Token? ILK-based? Biometric?)
- How are additional admins/operators authorized?
- How does the system prevent unauthorized access in a network-exposed deployment?
- What is the model for delegated access (an AI architect operating on behalf of a human)?

Until this is resolved, SY.architect should only be exposed on trusted networks (localhost or VPN). Do not expose to public internet without an external authentication proxy.

---

## 12. Implementation Tasks

### 12.1 Current Snapshot (2026-03-21)

Current repo state should be treated as a **partial shell**, not as a blank implementation and not as a finished MVP.

- `src/bin/sy_architect.rs` already exists and currently provides:
  - SDK connection loop to router
  - HTTP server with static HTML UI
  - `GET /api/status`
  - `POST /api/chat`
  - local `FCMD:` parser with prompt storage on disk
  - local `ACMD:` parser translated to `ADMIN_COMMAND`
- The current implementation does **not** yet provide:
  - WebSocket chat/streaming
  - real AI provider integration
  - real system status bar backed by `ADMIN_COMMAND`/inventory over socket
  - persisted chat sessions/messages
  - settings panel / `set_node_config`
  - IO impersonation
  - uploads / blobs

Because of that, the backlog should prioritize turning the current shell into a useful control-plane tool **before** investing in AI or UI polish.

### 12.2 Design Frictions To Resolve Early

These are not optional details; they affect the shape of the implementation:

- **Core packaging boundary:** `SY.architect` is a core `SY.*` node, so frontend/prompt assets cannot assume managed runtime `_system.package_path`.
- **Config ownership split:** today there is a practical split between `hive.yaml` bootstrap/fallback config and future self-managed `config.json`. This must be made explicit before implementing settings writes.
- **Status source of truth:** for v1, the top bar should read through `ADMIN_COMMAND` over socket, not over HTTP. Direct SHM reads are a possible later optimization, not the initial contract.
- **Persistence scope for v1:** chat sessions/messages should persist in LanceDB locally. Internal layout/flush/index strategy can be optimized for the fact that only this process reads/writes these chats.
- **Non-AI usefulness first:** the architect should become a strong operational chat shell even if AI is disabled or delayed.

### 12.3 Sprint 1 — Recommended Next Slice

This is the recommended first execution slice for the current repo state. It deliberately avoids AI-first work.

- [ ] ARCH-S1.1. Implement system status bar backed by `ADMIN_COMMAND` over socket (`SY.admin` target, no HTTP dependency).
- [ ] ARCH-S1.2. Improve `ACMD` rendering and validation so command results/errors are usable as an operational shell.
- [ ] ARCH-S1.3. Add destructive action confirmation flow for `kill_node`, `remove_hive`, and future deletes.
- [ ] ARCH-S1.4. Implement real chat sessions (`create/list/load`) so the left rail stops being placeholder UI.
- [ ] ARCH-S1.5. Persist sessions and messages in LanceDB, with a schema tuned for single-process local usage.
- [ ] ARCH-S1.6. Restore session content on reload and keep chat navigation stable across refreshes.
- [ ] ARCH-S1.7. Freeze the config contract (`hive.yaml` bootstrap/fallback vs `config.json` self-managed state) before adding settings UI.

### Phase A — Contracts And Bootstrap Invariants

This phase is design-first and should close the major ambiguities before the node grows more features.

- [ ] ARCH-T0.1. Definir lifecycle de despliegue de `SY.architect` como nodo core (`SY.*`), fuera del modelo `run_node`.
- [ ] ARCH-T0.2. Definir path canónico de assets/prompts/frontend para core deployment; no asumir `_system.package_path` de managed runtimes.
- [ ] ARCH-T0.3. Definir boundary explícito `SY.admin` vs system calls directos:
  - lectura/operación por `ADMIN_COMMAND`
  - writes de identity por system messaging a `SY.identity`
- [ ] ARCH-T0.4. Definir contrato de impersonación v1:
  - shape de `Resolve`
  - uso de `meta.src_ilk`
  - ubicación de flag de test en `meta.context`
- [ ] ARCH-T0.5. Declarar seguridad v1 como localhost/VPN only; ownership/auth fuera de alcance.
- [ ] ARCH-T0.6. Alinear secuencia de bootstrap con identity:
  - `SY.identity` asegura tenant default del sistema
  - `SY.orchestrator` registra ILKs core después de eso
  - `SY.architect` asume ese baseline ya existente

### Phase B — Harden The Existing Shell Into A Real Node Base

Goal: consolidate what already exists in `sy_architect.rs` so the node has a reliable non-AI base.

- [ ] ARCH-T1. Consolidar `src/bin/sy_architect.rs` como shell base del nodo:
  - conexión SDK al router
  - HTTP server `axum`
  - surface HTTP/UI estable
  - WebSocket queda pendiente para fase posterior
  - Current state: **partial**
- [ ] ARCH-T2. Implementar lectura de configuración:
  - `config.json` propio
  - fallback a `hive.yaml` para provider keys si aplica
  - Current state: **partial** (`hive.yaml` ya se lee; `config.json` todavía no)
- [ ] ARCH-T2.1. Leer `architect.listen` desde `hive.yaml` (con override opcional por env) para bind interno HTTP detrás de reverse proxy.
  - Current state: **mostly done**
- [ ] ARCH-T3. Implementar bootstrap de estado local:
  - directorio de trabajo
  - storage/chat DB
  - carga de prompts/assets
  - Current state: **partial** (prompt store local existe, pero no hay chat DB ni asset loading formal)
- [ ] ARCH-T4. Definir health/status mínimo del nodo (`RUNNING`, readiness HTTP, AI provider ready/not configured).
  - Current state: **partial**
- [ ] ARCH-T5. Servir frontend estático mínimo (chat + status + settings).
  - Current state: **partial** (chat + status scaffold existen; settings no)
- [ ] ARCH-T8.1. Mantener una sola pantalla principal de chat; no agregar un editor/pantalla separada de prompts para v1.
  - Current state: **aligned**

### Phase C — Control Plane MVP Without AI

This is the most important product phase for now. The architect should become useful even with AI disabled.

- [ ] ARCH-T13. Implementar parser y executor de `ACMD:` con sintaxis tipo `curl`, parseado localmente y traducido a llamadas internas contra `SY.admin`.
  - Current state: **partial**
- [ ] ARCH-T15. Representar resultados de comandos como mensajes de sistema en el chat.
  - Current state: **partial**
- [ ] ARCH-T14. Implementar barra/status de sistema usando:
  - `ADMIN_COMMAND` por socket como camino canónico v1
  - SHM directo solo como optimización futura, no como contrato inicial
  - Nota: reemplaza placeholders locales del header; ésta es la próxima pieza estructural de UX.
- [ ] ARCH-T16. Endurecer confirmación de operaciones destructivas (`kill_node`, `remove_hive`, futuros deletes).

### Phase D — Prompt Control Inside Chat

Prompt control is already partly started and should be completed while the node is still non-AI-first.

- [ ] ARCH-T16.1. Implementar parser local de `FCMD:` con payload JSON estructurado.
  - Current state: **mostly done**
- [ ] ARCH-T16.2. Implementar operaciones mínimas:
  - `prompt.get`
  - `prompt.update_draft`
  - `prompt.publish`
  - `prompt.rollback`
  - `prompt.history`
  - Current state: **mostly done**
- [ ] ARCH-T16.3. Mantener storage local con al menos:
  - draft actual
  - published actual
  - previous published
  - Current state: **mostly done**
- [ ] ARCH-T16.4. Validar localmente estructura mínima del prompt antes de publicar.
- [ ] ARCH-T16.5. Representar historial/versiones de prompt como mensajes de sistema en el chat.

### Phase E — Sessions, Persistence, And Left Rail Becoming Real

This phase turns the current visual chat navigator into real product behavior.

- [ ] ARCH-T27. Implementar sesiones de chat (create/list/load/delete).
- [ ] ARCH-T28. Implementar persistencia local de mensajes.
- [ ] ARCH-T30. Implementar búsqueda en historial y títulos de sesión.
- [ ] ARCH-T29. Optimizar LanceDB para uso local single-process:
  - schema de sesiones/mensajes
  - estrategia de flush/compaction
  - metadata suficiente para recuperación rápida y futura semantic search
- [ ] ARCH-T31. Refinar UI final (history panel, status refresh, upload state, command/result rendering).

### Phase F — Settings And Self-Configuration

Only after config ownership is explicit.

- [ ] ARCH-T7. Implementar panel/settings mínimo:
  - API key
  - modelo default
  - listen/http config básico
- [ ] ARCH-T8. Persistir settings vía `set_node_config` targeting self.

### Phase G — AI Provider Integration

This is intentionally later. The node should already be operational without it.

- [ ] ARCH-T9. Implementar cliente OpenAI con configuración externa y modo “unconfigured”.
- [ ] ARCH-T11. Implementar carga de prompts por rol (`architect`, `operator`, `tester`).
- [ ] ARCH-T12. Implementar selección de agente:
  - switch explícito por usuario
  - heurística simple por intención
- [ ] ARCH-T12.1. Separar pipeline de mensajes normales vs mensajes de control; `FCMD:` y `ACMD:` no deben invocar al proveedor AI.
- [ ] ARCH-T6. Implementar endpoint WebSocket de chat bidireccional.
- [ ] ARCH-T10. Implementar streaming token-by-token hacia WebSocket.

### Phase H — Identity / IO Impersonation

This should happen after the control-plane shell is trustworthy, because it is privileged and operationally sensitive.

- [ ] ARCH-T17. Implementar provisión de ILK temporal para test (`ILK_PROVISION`).
- [ ] ARCH-T18. Implementar envío `Resolve` impersonado con:
  - `routing.dst = null`
  - `meta.src_ilk = <ilk>`
  - `meta.context.test_mode = true`
- [ ] ARCH-T19. Implementar recepción y render de respuestas de test en el mismo chat.
- [ ] ARCH-T20. Definir cleanup v1 de ILKs de test:
  - si no existe delete explícito, documentar expiración/retención
  - si se agrega cleanup, especificar system call

### Phase I — Tenant / Identity Assisted Flows

- [ ] ARCH-T21. Implementar lectura de ILKs desde architect (`list_ilks`, `get_ilk`) para depuración/UX.
- [ ] ARCH-T22. Si architect va a crear tenants o completar registros, implementar system-call path directo a `SY.identity` (`TNT_CREATE`, `ILK_REGISTER`, `ILK_ADD_CHANNEL`) fuera de admin REST.
- [ ] ARCH-T23. Definir cómo se representa al usuario el resultado de writes de identity (ok/error/canonical tenant resolved).

### Phase J — File Upload / Blobs

- [ ] ARCH-T24. Implementar upload multipart HTTP.
- [ ] ARCH-T25. Integrar con blob toolkit (`put -> promote -> BlobRef`).
- [ ] ARCH-T26. Permitir adjuntar BlobRefs en conversación o en acciones operativas posteriores.

### Out of Scope for v1

- [ ] AUTH-TODO. Ownership / autenticación / control de acceso del panel web.
- [ ] AUTH-TODO. Exposición pública a internet sin proxy o capa externa de auth.
- [ ] AUTH-TODO. RBAC multi-operador / acting-on-behalf-of.

---

## 13. Package Structure

```
sy.architect/
├── package.json
├── bin/
│   └── start.sh                    # Compiled binary
├── assets/
│   ├── prompts/
│   │   ├── architect.md            # System design agent prompt
│   │   ├── operator.md             # Operations/monitoring agent prompt
│   │   └── tester.md               # IO impersonation agent prompt
│   └── frontend/
│       ├── index.html
│       ├── app.js
│       └── style.css
└── config/
    └── default-config.json          # Default config template
```

```json
{
  "name": "sy.architect",
  "version": "1.0.0",
  "type": "full_runtime",
  "description": "System architect node — chat-based admin interface with AI",
  "config_template": "config/default-config.json"
}
```

---

## 14. Relationship to Other Specs

| Spec | Relationship |
|------|-------------|
| `admin-internal-gateway-spec.md` | Architect uses ADMIN_COMMAND for all system operations |
| `node-spawn-config-spec.md` | Config.json and state.json follow standard node contract |
| `10-identity-v2.md` | Architect registered as SY node under fluxbee tenant |
| `system-inventory-spec.md` | Status bar reads from inventory |
| `node-status-contract.md` | Architect can query node status for monitoring |
| `runtime-packaging-cli-spec.md` | Core packaging may still ship default prompt assets, but runtime prompt maintenance in v1 happens through chat-local control |
| `runtime-lifecycle-spec.md` | Architect can trigger publish → deploy → spawn flow via chat |
| `identity-v3-direction.md` | Future: architect may become a government node with institutional role |
