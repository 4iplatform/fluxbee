# IO Nodes - Specification v1.16 (Fluxbee)
> Adaptation note for `json-router`:
> This spec is imported and kept as functional baseline.
> If a section conflicts with router v1.16+ docs in this repo, follow `docs/io/README.md` precedence.

## 1. Role of IO Nodes

IO nodes are **edge adapters** between external communication channels and the Fluxbee message fabric.

They are intentionally:
- Stateless (from a business perspective)
- Non-authoritative
- Simple

---

## 2. Identity Model

IO nodes:
- Resolve identity via SHM
- On miss/error, can call `ILK_PROVISION` via `SY.identity` (shared `io-common` flow)
- Never block provider ACK waiting for identity

If provision fails/timeout, IO forwards with `meta.src_ilk = null` and Router/OPA handles onboarding.

---

## 3. Identity Strategies

### Strategy: Router-Driven Onboarding

- IO runs `lookup -> provision_on_miss -> forward`
- Normal path is non-null `meta.src_ilk` when lookup/provision succeeds
- Degraded fallback forwards with `meta.src_ilk = null`
- Router + OPA redirect to onboarding workflow
- Message is re-injected once identity exists

This strategy provides:
- Durability
- Stateless IO nodes
- Maximum Router alignment

---

## 4. Context Model

IO nodes:
- Produce `meta.ich` as the **local/system-owned channel** operated by that concrete IO instance: `<channel>://<entrypoint id>`, from `meta.context.io` (`build_user_message`, `nodes/io/common/src/router_message.rs`)
- Produce `meta.thread_id`, the communication (`thread:sha256:<hex>`), computed from the channel's own material with `compute_thread_id` (`nodes/io/common/src/io_context.rs`)
- Never persist or forward history externally

Important:
- `ich` is **not** the remote handle of the external interlocutor
- `ich` is **not** the external user's identity
- remote handles and external identities must be represented by the corresponding identity/contact mechanisms, not by `ich`

IO nodes keep no conversational history. The router copies every turn into its stream: SY.storage
writes it to the message log (`turns`), and when an `AI.*` node sends or receives it, SY.cognition
keeps the memory of that communication, `(thread_id, the agent)`. The router attaches that memory
to what it delivers to the agent (`meta.memory_package`). See [storage](../reference/storage.md)
and [cognition](../reference/cognition.md).

---

## 5. Protocol Alignment

IO nodes **MUST follow the protocol** exactly ([protocol](../reference/protocol.md)).

Notably:
- `meta.thread_id` is the carrier of conversational continuity (the message carries no sequence)
- `meta.ctx`, `meta.ctx_seq` and `meta.ctx_window` no longer exist: no SDK has them, and the router drops every key that is not a field of `Meta` ([protocol](../reference/protocol.md) §3.1, §3.6)
- `meta.context` is reserved for OPA input and extra metadata
- `meta.ich` identifies the local/system channel of the IO node, not the remote external handle
- No duplication of context fields

---

## 6. Persistence Model

IO persistence exists only as an **implementation detail**.

Rules:
- Abstract interface required
- In-memory allowed for MVP
- No coupling to Router storage
- No assumptions of durability

---

## 7. Non-Responsibilities

IO nodes explicitly do NOT:
- Store conversation history
- Perform identity orchestration
- Implement retry queues for business messages
- Decide routing or business logic

---


### Clarification - meta.thread_id vs meta.context

- `meta.thread_id` carries the communication (`thread:sha256:<hex>`, computed by the IO node)
- `meta.context` is free JSON for the receiver and the OPA policy; IO nodes put the channel facts in `meta.context.io`

`meta.context` must **NOT** be used to store history, turns or conversational context.
`meta.context.thread_id` is removed from the IO pipeline (`build_user_message` drops it) and must not be emitted.


## Conversational fields

✅ **NORMATIVE**:
- `user` messages carry the identity and the communication: `meta.src_ilk` (or `null`), `meta.ich` (the **local/systemic channel**) and `meta.thread_id`.
- The message carries no context window and no sequence. The order of a communication is the turn number SY.cognition assigns, and an `AI.*` destination receives the memory of its communication in `meta.memory_package`, which only the router sets.


## Lifecycle & configuration (model unified with AI Nodes)

> ✅ **Intent**: do not reinvent the wheel. IO Nodes adopt the **same operational pattern** as AI Nodes:
> `SY.admin` receives commands → `SY.orchestrator` decides spawn/identity → the node starts with a base config and is adjusted through the Control Plane (`CONFIG_SET/GET`, `STATUS/PING`).

### 1) Spawn (high level)

✅ **NORMATIVE**:
- The IO node is created/spawned by `SY.orchestrator` (invoked by `SY.admin`).
- The node is born with a `node_name` and a minimal **base configuration** (it depends on the adapter: Slack/WhatsApp/Email).
- If the base config is incomplete or invalid, the node **must start anyway** and stay in a configuration failure state (see states).

🧩 **TO BE SPECIFIED (once node-spawn is fully standardized for IO)**:
- Detail of the `SPAWN_NODE` message and the exact payload for IO Nodes (align with Fluxbee's global rules).

### 2) States

✅ **NORMATIVE**: states per instance:
- `UNCONFIGURED`: no applicable effective configuration.
- `CONFIGURED`: valid effective configuration to operate.
- `FAILED_CONFIG`: it cannot operate the data-plane, but it serves the Control Plane to recover.

✅ **NORMATIVE**:
- In `FAILED_CONFIG`, the node **MUST** serve: `PING`, `STATUS`, `CONFIG_GET`, `CONFIG_SET`.

### 2.1 Minimal contract regarding the local ICH

✅ **NORMATIVE**:
- An IO node can stay **alive** even when it does not yet have enough material to operate its local `ICH`.
- In that state it must keep serving the control-plane and minimal observability.
- While it does not have enough material to operate its local channel/asset, it **MUST NOT** accept or process real operational traffic of the channel.

Operational interpretation:
- the exact criterion of "enough material" depends on the node;
- it can be a minimal configuration, channel credentials, a resolved local asset or a combination of those factors;
- today that state can be expressed through `UNCONFIGURED` or `FAILED_CONFIG`, without requiring a new `ICH`-specific state yet.

### 3) Hot configuration (Control Plane)

✅ **NORMATIVE**:
- IO Nodes use the same message pattern as AI Nodes:
  - `CONFIG_SET` (replace/merge_patch) to apply changes,
  - `CONFIG_RESPONSE` as the confirmation,
  - `CONFIG_GET` to query the state/config_version,
  - `STATUS`/`PING` for minimal observability.

✅ **NORMATIVE (replace MVP)**:
- For the MVP, `apply_mode="replace"` takes priority, and `merge_patch` (RFC 7396) is allowed later as an improvement.

### 3.1 Explicit boundary with core/orchestrator

✅ **NORMATIVE**:
- `PUT /hives/{hive}/nodes/{node}/config` (core/orchestrator) updates the platform-managed `config.json`.
- That endpoint does **NOT replace** the node-owned IO runtime contract (`CONFIG_SET` / `CONFIG_GET`).
- For an operational hot reload of the IO adapter, the canonical path is `POST .../control/config-set` (`CONFIG_SET`).

### 3.2 IO policy for `CONFIG_CHANGED` (v1)

✅ **NORMATIVE (v1, chosen)**:
- `CONFIG_CHANGED` is treated as an (informational) infrastructure signal in the IO layer.
- It is not the canonical path to apply the adapter's runtime/business config.
- The effective runtime application of IO configuration is done through `CONFIG_SET`.

### 4) Configuration rules: common vs adapter-specific

✅ **NORMATIVE**:
- The common layer defines:
  - versioning (`schema_version`, `config_version`),
  - persistence of the effective config,
  - structural validation,
  - states and errors.
- Each adapter defines its **minimal required config**:
  - Slack Socket Mode: `SLACK_APP_TOKEN` and `SLACK_BOT_TOKEN` (and other fields the adapter declares).
  - WhatsApp/Email: TBD per adapter.

### 5) Secrets: TODAY vs future

✅ **NORMATIVE (TODAY / MVP)**:
- Credentials (tokens, signing secrets, etc.) are allowed to live in **local config** (YAML/env) and/or travel **inline** in `CONFIG_SET`, to allow changes without a restart.
- The node **MUST NOT** persist secrets in clear text in `${STATE_DIR}`.

⚠️ **WARNING**:
- If the secret arrives inline through `CONFIG_SET`, the node can keep it only in memory (and it is lost on restart), unless there is a local source (YAML/env).

🧩 **TO BE SPECIFIED (future)**:
- `*_key_ref` (env/file/vault/kms) + a secret manager (orchestrator/admin or a dedicated service) for secure rotation/persistence.

### 6) Adoption rule for future IO adapters

✅ **NORMATIVE**:
- If an `IO.*` adapter produces textual user messages toward the router, it **MUST** use the canonical `text/v1` contract.
- Those messages **MUST** go out through `fluxbee_sdk::NodeSender::send`.
- The adapter **MUST NOT** reimplement locally the `content` vs `content_ref` decision for `text/v1`.

✅ **CLARIFICATION**:
- The SDK's current auto-offload covers the `text/v1` contract.
- It must not be assumed automatically for arbitrary payloads outside that contract.
- If an adapter needs another, non-`text/v1` payload, that contract must define explicitly how it handles size limits and blobs.


