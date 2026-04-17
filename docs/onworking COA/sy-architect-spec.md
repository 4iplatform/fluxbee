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
│  │  • SCMD parsing and execution                                  ││
│  │  • System state aggregation                                     ││
│  │  • IO impersonation engine                                      ││
│  │  • Prompt asset loading (project/binary owned)                  ││
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

Current state in repo:
- `SY.architect` already has a first direct OpenAI path through `fluxbee_ai_sdk`.
- normal chat messages can go through a local `archi` agent when an OpenAI key is configured.
- `SCMD:` remains a separate local/system path and does not invoke the AI provider.
- the local agent can already use read-only socket-backed tools against `SY.admin` for live system state.
- immediate short-horizon memory is now rehydrated through `fluxbee_ai_sdk` using recent interactions, active operations, and conversation summary; see `docs/immediate-conversation-memory-spec.md`.
- chat creation already supports both `operator` and `impersonation` modes.
- impersonation chat launch is now backed by identity SHM options:
  - the user selects an existing `ICH`
  - architect then narrows to the candidate `ILK` list bound to that channel context
- streaming, multi-agent routing, and prompt assets by role are still pending.

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

For v1, this panel should edit only architect-owned runtime settings stored in `config.json`. It should not edit bootstrap/network placement fields from `hive.yaml`.

For network exposure, the bind/listen of the local HTTP server should be declared in `hive.yaml`:

```yaml
architect:
  listen: "127.0.0.1:3000"
```

The intended deployment model is:

- `SY.architect` binds to an internal/local address
- the operator exposes it through an external reverse proxy if desired
- `SY.architect` should not assume direct public exposure
- `listen/bind` remains bootstrap-owned in `hive.yaml` for v1 and is out of scope for the first settings UI

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
- Architect implementation note:
  - the `SY.architect` process is one core system node, but each chat should be treated as a separate logical architect instance for AI/context purposes
  - each chat may carry its own effective ILK/thread context, even though the outer process node is still `SY.architect@motherbee`
  - this is important for future immediate-memory rehydration, per-chat operation tracking, and simulation flows

### 3.4 Chat Area (main panel)

Standard chat interface with:

- Message bubbles (user / architect / system).
- System messages for status updates, command results, errors.
- File upload button (📎) for blob uploads.
- Send button (➤).
- WebSocket connection for real-time streaming of AI responses.

### 3.5 Logical Per-Chat Identity

For AI/runtime behavior, `SY.architect` should treat each chat as a logical node-like instance hosted inside the single architect process.

This means:

- one architect process can host multiple chat instances
- each chat instance may have its own:
  - local session id
  - effective ILK context
  - `thread_id` when the conversation is tied to an IO-defined thread
  - immediate-memory state
  - active operations

Important distinction:

- the outer connected node remains `SY.architect@motherbee`
- the per-chat identity is logical, not a separate L1 router node

Why this matters:

- immediate memory should not bleed across chats
- per-chat operation tracking should remain isolated
- future impersonation/test flows can bind one chat to one impersonated ILK cleanly
- simulations become easier to reason about because the chat behaves like a stable logical actor

Architect does not mint permanent ILKs by itself. It either:

- uses its own system identity as `SY.architect`
- or temporarily operates through an impersonated/effective ILK context for testing/simulation

This keeps the process model simple while allowing each chat to behave like a distinct conversational actor.

### 3.6 Chat Modes

`SY.architect` should support two explicit conversation modes:

- `operator`
  - default mode
  - architect acts as the system operator/control-plane assistant
  - no external channel impersonation is assumed
- `impersonation`
  - explicit debug/simulation mode
  - the chat may run with an effective ILK and/or impersonation target
  - used to test flows before connecting real external IO such as WhatsApp/Instagram/other channels

Important:

- normal chats should default to `operator`
- impersonation chats should be created intentionally, not inferred silently from normal usage
- the current UI already exposes both flows explicitly:
  - `Operator` button for normal control-plane chats
  - `Impersonate` button opening a modal backed by identity SHM options (`ICH` first, then `ILK`)

### 3.7 Immediate Memory Rehydration

For AI turns, `SY.architect` now rebuilds short-horizon conversation context before calling the model.

Current bundle sent through `fluxbee_ai_sdk`:

- `conversation_summary`
  - session goal/focus
  - mode-aware decisions such as `pending_confirm` or `timeout_unknown`
  - confirmed facts including effective `ICH`, effective `ILK`, source channel kind, and `thread_id`
- `recent_interactions`
  - last bounded window of user / architect / system turns
  - recent `SCMD` and admin/tool failures are preserved as explicit interaction summaries, not only as raw UI cards
- `active_operations`
  - non-terminal tracked operations first
  - recent terminal operations when still relevant to the current conversation

This memory is per chat/session and must not bleed across sessions.

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

### 4.3 Config Contract Direction (Pending)

For `SY.architect`, the intended configuration ownership split is:

| Surface | Canonical file | Writer | Purpose |
|---------|----------------|--------|---------|
| bootstrap/system | `/etc/fluxbee/hive.yaml` | human / orchestrator bootstrap | hive identity, role, WAN/system config, `architect.listen`, system-wide fallback values |
| architect runtime | `/var/lib/fluxbee/nodes/SY/SY.architect@<hive>/config.json` | `SY.orchestrator` via `set_node_config` | mutable per-node settings owned by architect (`ai_providers`, future UI-managed settings, `_system` metadata) |

Tentative rules:

- `SY.architect` never writes its own `config.json` directly.
- `SY.architect` reads `config.json` if present, but must tolerate it being absent during early bootstrap.
- `hive.yaml` is bootstrap/fallback input, not the primary mutable settings store for architect.
- `architect.listen` stays in `hive.yaml` for v1; it is not migrated into `config.json`.
- AI provider settings use precedence: `config.json` first, `hive.yaml` fallback second.
- Prompt assets are not config; they live in project/package assets and change only through rebuild/redeploy.

Bootstrap requirement if this direction is adopted:

- before the future settings UI depends on `set_node_config` targeting self, core bootstrap/orchestrator must ensure that `/var/lib/fluxbee/nodes/SY/SY.architect@<hive>/config.json` exists with a minimal root object and `_system` metadata
- if that file is missing, architect may still run in read-only/fallback mode, but self-settings writes are not considered available yet

Important note:

- this contract is not considered closed yet at core level
- today, some core/system nodes still bootstrap from `hive.yaml`; `SY.storage` ya expone `CONFIG_GET` / `CONFIG_SET` y usa `secrets.json` como fuente preferida para su secreto DB, con `hive.yaml` solo como fallback legacy
- while `hive.yaml` remains the effective startup source for core nodes, `config.json` for `SY.architect` should be treated as a pending direction rather than a fully-settled invariant

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

### 5.3 Prompt Assets Are Project Assets

Prompts for `SY.architect` are part of the project/package, not part of the runtime chat surface.

That means:

- prompts live in source-controlled assets within the project
- `SY.architect` may load them locally at runtime once AI integration exists
- changing them requires editing the project assets and rebuilding/redeploying the binary
- there is no chat command surface for mutating prompts in v1

This keeps `SY.architect` aligned with its role as a core system node: the operator can use the chat to observe and operate the system, but not to rewrite the architect's own behavior live.

### 5.4 Runtime Editing Policy

The chat still supports two execution paths:

- normal conversational messages, which may invoke the AI provider later
- local system commands, parsed and executed directly by `SY.architect` without invoking the AI provider

For v1, the only reserved command prefix is:

- `SCMD:` for system operations translated into `ADMIN_COMMAND` calls over socket

There is intentionally no `FCMD:` prompt-editing surface in runtime.

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

Operationally, the chat should expose one explicit non-AI command prefix:

- `SCMD:` → system/admin passthrough commands

`SCMD:` is parsed locally by `SY.architect` and bypasses the LLM.

### 6.2 SCMD Model (System Passthrough)

`SCMD:` exists so a human operator can use the architect chat as a single operational terminal without opening a separate shell. The syntax should resemble `curl`, but must not execute shell commands or invoke a real `curl` process.

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
SCMD: curl -X GET /hives/motherbee/nodes
SCMD: curl -X GET /hives/motherbee/identity/ilks
SCMD: curl -X DELETE /hives/motherbee/nodes/IO.slack.T123/instance
SCMD: curl -X POST /hives/motherbee/nodes/AI.chat@motherbee/messages -d '{"msg_type":"user","msg":"LLM","payload":{"text":"hola"}}'
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
| Node management | `run_node`, `kill_node`, `remove_node_instance`, `list_nodes`, `get_node_status`, `get_node_config`, `set_node_config` |
| Runtime management | `get_versions`, `update`, `sync_hint` |
| Identity read | `list_ilks`, `get_ilk` |
| Routing | `list_routes`, `add_route`, `list_vpns`, `add_vpn` |
| Config | `get_config_storage`, `set_config_storage` |

Notes:

- `TNT_CREATE`, `ILK_REGISTER`, `ILK_ADD_CHANNEL` are identity system calls, not public admin REST mutations.
- If tenant creation/registration is needed from architect workflows, that path should be implemented explicitly as direct system messaging to `SY.identity`.

### 6.5 Command Confirmation

For destructive operations (`kill_node`, `remove_node_instance`, `remove_hive`), the shell should require explicit confirmation before executing. In v1 this should be enforced in code, not left only to prompt discipline.

`kill_node` may also be used with `purge_instance=true` when the operator wants a stop + persisted-instance cleanup in one step before reinstall/recreate.

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
User clicks 📎 → selects file → file is staged in the composer
User sends a normal operator chat turn
Archi uploads the staged files via HTTP multipart to architect
Architect persists them as blobs and sends BlobRef-backed multimodal input to the model

Architect:
  1. Receives file via HTTP `POST /api/attachments`.
  2. Uses blob toolkit: put() → promote().
  3. Creates BlobRef + safe attachment metadata.
  4. Includes BlobRef in the next AI chat turn context.
```

`POST /api/chat` remains JSON-only. The browser sends:

- `message`
- `session_id`
- `attachments[]` with stable attachment descriptors returned by `/api/attachments`

Current-turn attachments are passed to the model as multimodal `input_file` / `input_image` parts. Previous turns are not replayed as live file parts in v1; they are summarized textually in recent-memory reconstruction.

### 8.2 Use Cases

- Attach documents or images to an operator chat turn so the model can inspect them.
- Review prompt assets, specs, configs, CSV/JSON data, and screenshots directly from the Archi UI.

This capability is for **operator AI chat mode**. It is explicitly out of scope in v1 for:

- `SCMD`
- `CONFIRM` / `CANCEL`
- impersonation/debug dispatch

### 8.3 Storage

Files go through the standard blob pipeline: staging → active in `/var/lib/fluxbee/blob/`. The architect uses `fluxbee_sdk::blob` for all operations.

Session persistence stores only safe metadata:

- filename
- mime
- size
- `blob_ref`

Raw bytes are not stored in chat history rows.

### 8.4 Supported Types And Limits

Supported MIME families in v1:

- `text/plain`
- `text/markdown`
- `text/csv`
- `application/json`
- `application/pdf`
- `application/vnd.openxmlformats-officedocument.wordprocessingml.document`
- `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`
- `image/png`
- `image/jpeg`
- `image/webp`
- `image/gif`

Limits in v1:

- max 8 attachments per upload
- max 10 MB per file
- uploads are browser-staged and removable before send

Blob lifecycle is intentionally simple in v1:

- deleting a chat session does not delete blobs
- blob GC remains the cleanup mechanism

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

SessionProfiles:
  session_id: uuid
  chat_mode: string (`operator`|`impersonation`)
  effective_ich_id: string?
  effective_ilk: string?
  impersonation_target: string?
  thread_id: string?
  source_channel_kind: string?
  debug_enabled: bool

Messages:
  message_id: uuid
  session_id: uuid
  role: string (user|architect|system)
  content: string
  timestamp: timestamp
  metadata: json (command results, blob refs, test mode info)
  embedding: vector (for semantic search)

Operations:
  operation_id: uuid
  session_id: uuid
  scope_id: string
  origin: string
  action: string
  target_hive: string
  params_json: string
  params_hash: string
  preview_command: string
  status: string
  created_at_ms: u64
  updated_at_ms: u64
  dispatched_at_ms: u64
  completed_at_ms: u64
  request_id: string?
  trace_id: string?
  error_summary: string?
```

### 9.2 Why LanceDB

- Semantic search over chat history ("what did I configure last week about billing?").
- Embedded, no external process.
- Local to the architect node.
- Reconstructible (chat history is operational, not business-critical).
- Session mode/context can evolve without forcing a migration of the base `sessions` table.

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
  }
}
```

Config path for `SY.architect`:

- `/var/lib/fluxbee/nodes/SY/SY.architect@<hive>/config.json`

For v1, the effective bind order should be:

1. `JSR_ARCHITECT_LISTEN` environment override
2. `architect.listen` in `hive.yaml`
3. fallback default `127.0.0.1:3000`

For AI provider configuration, the effective precedence should be:

1. `config.json`
2. `hive.yaml`
3. unconfigured mode

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

### 12.1 Current Snapshot (2026-03-23)

Current repo state should be treated as a **partial shell**, not as a blank implementation and not as a finished MVP.

- `src/bin/sy_architect.rs` already exists and currently provides:
  - SDK connection loop to router
  - HTTP server with static HTML UI
  - `GET /api/status`
  - `POST /api/chat`
  - `GET /api/identity/ich-options`
  - local `SCMD:` parser translated to `ADMIN_COMMAND`
  - direct OpenAI chat integration through `fluxbee_ai_sdk`
  - persisted chat sessions/messages in local LanceDB
  - persisted per-session chat profile/context (`operator` vs `impersonation`, effective `ICH` / effective `ILK` / thread context)
  - tracked per-session operations for staged, running, timeout-unknown, and recent terminal mutations
  - immediate-memory rehydration into the AI call using summary + recent interactions + active operations
  - explicit UI flows for both operator chats and SHM-backed impersonation/debug chats
  - persisted AI tool results with compact render of tool name, relevant input, summarized output, and full metadata retained
- current friction:
  - header status chips still depend on `/api/status` and backend inventory freshness
  - today each refresh can still open an ephemeral router/admin client path just to render `Hives` / `Nodes` / `Updated`
  - browser polling is already visibility-aware and less aggressive than the initial version, but the inventory summary itself can still lag behind recent mutations
  - long-running mutating actions can outlive the local caller timeout; architect must track them explicitly instead of treating timeout as a clean failure
  - for these three lightweight chips specifically, the preferred next optimization is local inventory snapshot/SHM read inside `SY.architect`, not a control-plane roundtrip
- The current implementation does **not** yet provide:
  - WebSocket chat/streaming
  - settings panel / `set_node_config`
  - uploads / blobs

Because of that, the backlog should prioritize hardening the current control-plane tool and its AI/session semantics before adding more surface area.

### 12.2 Design Frictions To Resolve Early

These are not optional details; they affect the shape of the implementation:

- **Core packaging boundary:** `SY.architect` is a core `SY.*` node, so frontend/prompt assets cannot assume managed runtime `_system.package_path`.
- **Config ownership split:** today there is a practical split between `hive.yaml` bootstrap/fallback config and future self-managed `config.json`. This is still unresolved for core/system nodes as a class: orchestrator/admin can address `config.json`, but current `SY.*` runtimes still bootstrap from `hive.yaml`.
- **Status source of truth:** the full operational surface should continue to use `ADMIN_COMMAND` over socket. However, the top-bar lightweight chips (`Hives`, `Nodes`, `Updated`) should migrate to local inventory snapshot/SHM reads inside `SY.architect` to avoid repeated control-plane roundtrips for low-value polling data.
- **Persistence scope for v1:** chat sessions/messages should persist in LanceDB locally. Internal layout/flush/index strategy can be optimized for the fact that only this process reads/writes these chats.
- **Non-AI usefulness first:** the architect should become a strong operational chat shell even if AI is disabled or delayed.

### 12.3 Sprint 1 — Recommended Next Slice

This is the recommended first execution slice for the current repo state. It deliberately avoids AI-first work.

- [x] ARCH-S1.1. Implement system status bar backed by `ADMIN_COMMAND` over socket (`SY.admin` target, no HTTP dependency).
- [x] ARCH-S1.2. Improve `SCMD` rendering and validation so command results/errors are usable as an operational shell.
- [x] ARCH-S1.3. Add destructive action confirmation flow for `kill_node`, `remove_hive`, and future deletes.
- [x] ARCH-S1.4. Implement real chat sessions (`create/list/load`) so the left rail stops being placeholder UI.
- [x] ARCH-S1.5. Persist sessions and messages in LanceDB, with a schema tuned for single-process local usage.
- [x] ARCH-S1.6. Restore session content on reload and keep chat navigation stable across refreshes.
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

- [x] ARCH-T1. Consolidar `src/bin/sy_architect.rs` como shell base del nodo:
  - conexión SDK al router
  - HTTP server `axum`
  - surface HTTP/UI estable
  - WebSocket queda pendiente para fase posterior
  - Current state: **done for v1 base shell**
- [ ] ARCH-T2. Implementar lectura de configuración:
  - `config.json` propio
  - fallback a `hive.yaml` para provider keys si aplica
  - Current state: **partial** (`config.json` ya se lee para AI provider settings con fallback a `hive.yaml`, pero el contrato de core todavía no está cerrado mientras `SY.*` siga arrancando desde `hive.yaml`)
- [x] ARCH-T2.1. Leer `architect.listen` desde `hive.yaml` (con override opcional por env) para bind interno HTTP detrás de reverse proxy.
- [ ] ARCH-T3. Implementar bootstrap de estado local:
  - directorio de trabajo
  - storage/chat DB
  - carga de prompts/assets
  - Current state: **partial** (chat DB local ya existe; asset loading formal de prompts todavía no)
- [ ] ARCH-T4. Definir health/status mínimo del nodo (`RUNNING`, readiness HTTP, AI provider ready/not configured).
  - Current state: **partial**
- [ ] ARCH-T5. Servir frontend estático mínimo (chat + status + settings).
  - Current state: **partial** (chat + status scaffold existen; settings no)
- [x] ARCH-T8.1. Mantener una sola pantalla principal de chat; no agregar un editor/pantalla separada de prompts para v1.

### Phase C — Control Plane MVP Without AI

This is the most important product phase for now. The architect should become useful even with AI disabled.

- [x] ARCH-T13. Implementar parser y executor de `SCMD:` con sintaxis tipo `curl`, parseado localmente y traducido a llamadas internas contra `SY.admin`.
- [x] ARCH-T15. Representar resultados de comandos como mensajes de sistema en el chat.
- [x] ARCH-T14. Implementar barra/status de sistema usando:
  - `ADMIN_COMMAND` por socket como camino canónico v1
  - y dejar explícito que los chips livianos del topbar (`Hives`, `Nodes`, `Updated`) pueden pasar a snapshot local/SHM directo como optimización operativa
  - Nota: reemplaza placeholders locales del header; ésta es la próxima pieza estructural de UX.
- [ ] ARCH-T14.1. Mover los chips `Hives` / `Nodes` / `Updated` a lectura local de snapshot/SHM dentro de `SY.architect`:
  - mantener `/api/status` como superficie HTTP para la UI
  - evitar roundtrip `SY.architect -> SY.admin -> SY.orchestrator` en polling frecuente
  - reservar `ADMIN_COMMAND` para vistas operativas ricas y acciones, no para contadores livianos de topbar
- [x] ARCH-T16. Endurecer confirmación de operaciones destructivas (`kill_node`, `remove_hive`, futuros deletes).

### Phase C.1 — Admin Surface Coverage

This is now the highest-value execution track. `SY.architect` should converge toward broad operational coverage of what `SY.admin` already exposes today.

- [x] ARCH-T32. Expandir cobertura read-only de `SY.admin` en `SCMD` y tools del agente:
  - `list_hives`, `get_hive`
  - `list_admin_actions`
  - `get_admin_action_help`
  - `list_versions`, `get_versions`
  - `list_runtimes`, `get_runtime`
  - `list_routes`, `list_vpns`
  - `get_storage`, `get_node_state`
  - `list_deployments`, `get_deployments`
  - `list_drift_alerts`, `get_drift_alerts`
  - `opa_get_policy`, `opa_get_status`, `opa_check`
- [x] ARCH-T33. Exponer acciones mutating de `SY.admin` a través de `SY.architect` con política explícita de confirmación:
  - `run_node`, `kill_node`, `remove_node_instance`
  - `add_hive`, `remove_hive`
  - `add_route`, `delete_route`
  - `add_vpn`, `delete_vpn`
  - `set_node_config`, `set_storage`
  - `send_node_message`
  - `update`, `sync_hint`
  - `opa_compile`, `opa_apply`, `opa_compile_apply`, `opa_rollback`
- [x] ARCH-T34. Diseñar contrato de confirmación para tools de escritura:
  - lectura libre
  - escritura/mutación solo con confirmación explícita del operador
  - no depender solo del prompt para seguridad
- [x] ARCH-T36. Registrar operaciones mutating en `SY.architect` con tracking local persistido:
  - `operation_id`
  - scope v1 = chat/session
  - future direction: scope can evolve to `ILK/operation` when chats map to distinct ILKs
  - statuses mínimos: `pending_confirm`, `dispatched`, `timeout_unknown`, `succeeded`, `failed`, `canceled`
- [x] ARCH-T37. Bloquear reintentos equivalentes mientras exista una operación no terminal en el mismo scope:
  - misma acción
  - mismo target
  - mismo payload normalizado
  - timeout local no debe permitir reenvío ciego
- [ ] ARCH-T38. Extender `SY.admin` con operation tracking nativo para acciones largas:
  - `operation_id` canónico compartido con architect/admin/orchestrator
  - consulta de estado de operación
  - reconciliación explícita después de timeout del caller
- Confirmation flow v1:
  - el agente puede preparar una mutación con `fluxbee_system_write`
  - la acción queda pendiente por sesión
  - el operador debe responder `CONFIRM` para ejecutar o `CANCEL` para descartar
  - una vez confirmada, architect registra y secuencia la operación dentro del chat antes de volver a permitir otra equivalente
- [x] ARCH-T35. Mejorar render/persistencia de tool calls:
  - mostrar qué tool usó `archi`
  - mostrar inputs relevantes y resultado resumido
  - mantener salida completa disponible en metadata

Note:
- `SY.admin` should be the canonical dynamic help surface for action discovery.
- `archi` should prefer `/admin/actions` and `/admin/actions/{action}` over hardcoded prompt knowledge when it is unsure about available operations.
- `SY.admin` help should expose standardized request-contract metadata (`path_params`, body fields, notes, examples) so new actions do not require prompt surgery in `SY.architect`.

### Phase D — Prompt Assets And Build-Time Policy

Prompt behavior is owned by the project/package, not by runtime chat commands.

- [x] ARCH-T16.1. Eliminar la superficie runtime de edición de prompts; no exponer comandos de chat que muten `SY.architect`.
- [ ] ARCH-T16.2. Cargar prompts desde assets del proyecto/core packaging cuando la integración AI quede habilitada.
- [ ] ARCH-T16.3. Definir layout/versionado de assets de prompts en el proyecto, con rebuild/redeploy como único camino de cambio.

### Phase E — Sessions, Persistence, And Left Rail Becoming Real

This phase turns the current visual chat navigator into real product behavior.

- [x] ARCH-T27. Implementar sesiones de chat (create/list/load/delete).
- [x] ARCH-T28. Implementar persistencia local de mensajes.
- [x] ARCH-T30. Implementar búsqueda en historial y títulos de sesión.
- [ ] ARCH-T29. Optimizar LanceDB para uso local single-process:
  - schema de sesiones/mensajes
  - estrategia de flush/compaction
  - metadata suficiente para recuperación rápida y futura semantic search
- [x] ARCH-T31. Refinar UI final (history panel, status refresh, command/result rendering).
  - Status refresh ya quedó más frontend-aware:
    - sin requests solapados
    - pausado/degradado con tab oculta
    - cadencia menor que el polling agresivo inicial
    - chips mantenidos porque siguen siendo útiles como resumen operativo rápido
  - History panel ya quedó más legible:
    - mejor jerarquía de meta por chat
    - preview multilinea
    - badge explícito para la sesión abierta
    - contador visible de resultados / chats locales
    - contexto de impersonación más compacto (`ICH` / `ILK` / `Thread`) con tooltip completo
    - sesión abierta priorizada arriba del listado
  - Command/result rendering ya quedó más compacto:
    - preview corto para `payload`, `error`, `input` y `output`
    - detalle expandible para inspección completa cuando hace falta
    - bloque `Tool used` colapsado por default y ordenado para priorizar el intento relevante del turno
  - Nota:
    - `upload state` real queda deferido a `Phase J` y no bloquea el cierre de esta tarea de UI v1

### Phase F — Settings And Self-Configuration

Only after config ownership is explicit.

- [ ] ARCH-T7. Implementar panel/settings mínimo:
  - API key
  - modelo default
  - listen/http config básico
- [ ] ARCH-T8. Persistir settings vía `set_node_config` targeting self.

### Phase G — AI Provider Integration

This is intentionally later. The node should already be operational without it.

- [x] ARCH-T9. Implementar cliente OpenAI con configuración externa y modo “unconfigured”.
- [ ] ARCH-T11. Implementar carga de prompts por rol (`architect`, `operator`, `tester`).
- [ ] ARCH-T12. Implementar selección de agente:
  - switch explícito por usuario
  - heurística simple por intención
- [x] ARCH-T12.1. Separar pipeline de mensajes normales vs mensajes de control; `SCMD:` no debe invocar al proveedor AI.
- [x] ARCH-T12.2. Exponer tools read-only del sistema al agente local usando `ADMIN_COMMAND` sobre socket.
- [x] ARCH-T12.3. Rehidratar contexto conversacional por sesión en cada turno AI:
  - cargar historial reciente de la sesión desde LanceDB
  - definir windowing/truncation para no crecer sin control
  - incluir memoria inmediata mediante `conversation_summary`, `recent_interactions` y `active_operations`
  - preservar errores/tool results recientes como contexto operativo útil
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

- [x] ARCH-T21. Implementar lectura de ILKs desde architect (`list_ilks`, `get_ilk`) para depuración/UX.
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
| `runtime-packaging-cli-spec.md` | Core packaging should ship architect prompt assets; changing them is a project/package update, not a runtime chat operation |
| `runtime-lifecycle-spec.md` | Architect can trigger publish → deploy → spawn flow via chat |
| `identity-v3-direction.md` | Future: architect may become a government node with institutional role |
