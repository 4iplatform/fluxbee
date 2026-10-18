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
- Produce `meta.ich` as the **local/system-owned channel** operated by that concrete IO instance
- Consume `ctx_seq` only as legacy-compatible metadata when it appears
- Accept `ctx_window` only as legacy-compatible metadata when it appears
- Never persist or forward history externally

Important:
- `ich` is **not** the remote handle of the external interlocutor
- `ich` is **not** the external user's identity
- remote handles and external identities must be represented by the corresponding identity/contact mechanisms, not by `ich`

The Router is the **sole owner** of conversational history.

---

## 5. Protocol Alignment

IO nodes **MUST follow Router v1.16 protocol** exactly.

Notably:
- `thread_id` is the active canonical carrier for conversational continuity in the current repo (the message carries no sequence)
- `meta.ctx` belongs to historical/legacy context semantics and must not be reintroduced as the primary carrier in new IO paths
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


### Clarification - meta.ctx vs meta.context

According to the Router v1.16 specification:

- `meta.ctx` represents the **Conversational Context (CTX)**
- `meta.context` is reserved exclusively for **additional data evaluated by OPA**
- `meta.thread_id` is the canonical thread carrier (cognition v2)

`meta.context` must **NOT** be used to store history, turns or conversational context.
`meta.context.thread_id` is deprecated/removed in IO and must not be emitted.


## Temporary adaptation (canonical spec vs current core)

✅ **Canonical (Fluxbee protocol)**:
- `user` messages must include identity and canonical conversational material.
- In the current repo, `ich` must be read as the **local/systemic channel** and `thread_id` as the active conversational carrier.

⚠️ **Current state (core/SDK)**:
- `ctx_window` is not guaranteed at runtime today; `ctx_seq` may be missing in some flows.

✅ **NORMATIVE (TODAY)**:
- IO Nodes **MUST** tolerate missing `ctx_window` and `ctx_seq` without failing.
- those fields are no longer part of the repo's active canonical carrier.

🧩 **TO BE SPECIFIED**:
- if they are kept in the protocol, they remain only for historical compatibility; there is no active plan to make them mandatory.


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


