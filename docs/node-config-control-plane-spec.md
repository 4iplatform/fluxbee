# Node Config Control Plane Spec

Status: draft  
Scope: non-`SY` nodes only (`AI.*`, `IO.*`, `WF.*`, future families outside core)

## 1. Goal

Define one common control-plane contract for node runtime configuration without coupling the core platform to each node's private config schema.

This spec does **not** replace the current orchestrator-owned per-node config path:
- `set_node_config`
- `NODE_CONFIG_SET`
- `CONFIG_CHANGED`

That path remains unchanged and is responsible for infrastructure/bootstrap config persistence.

This spec defines a second, node-facing contract for runtime/business configuration:
- `CONFIG_GET`
- `CONFIG_SET`

## 2. Design principles

- The platform defines only the common envelope and metadata.
- Each node defines its own `payload.config` shape.
- Clients that need to configure a node must first call `CONFIG_GET`.
- `CONFIG_GET` must return enough information for a client such as `archi` to build a valid `CONFIG_SET`.
- The config payload remains open/extensible for future node types.
- This contract applies to all non-`SY` nodes.

## 3. Verbs

Only two functional verbs exist:

- `CONFIG_GET`
- `CONFIG_SET`

Replies use:

- `CONFIG_RESPONSE`

`CONFIG_RESPONSE` is not a third business verb. It is the standard response envelope for `CONFIG_GET` and `CONFIG_SET`.

## 4. Transport

Transport is always:

- L2 unicast message
- target = one concrete node
- JSON payload

Routing baseline:

- `routing.dst`: fully-qualified node name, for example `AI.chat@motherbee`
- `routing.ttl`: default `16`
- `routing.trace_id`: caller-assigned trace id

Metadata baseline:

- `meta.msg_type`: `system`
- `meta.msg`: `CONFIG_GET`, `CONFIG_SET`, or `CONFIG_RESPONSE`

The current platform transport that can already carry this is `send_node_message`.

Shared code support now exists in:

- `crates/fluxbee_sdk/src/node_config.rs`
- `crates/fluxbee_sdk/src/node_secret.rs`

That helper module standardizes:

- control-plane message constants
- common request payload structs
- common response parsing
- response building with preserved `trace_id`
- reusable secret metadata descriptor support (`NodeSecretDescriptor`)

It does not standardize the node-owned `payload.config` schema.

## 5. Common request envelope

### 5.1 CONFIG_GET

Minimal request:

```json
{
  "routing": {
    "dst": "AI.chat@motherbee",
    "ttl": 16,
    "trace_id": "9d8c6f4b-2f4d-4ad4-b5f4-8d8d9d9d0001"
  },
  "meta": {
    "msg_type": "system",
    "msg": "CONFIG_GET"
  },
  "payload": {
    "node_name": "AI.chat@motherbee"
  }
}
```

### 5.2 CONFIG_SET

Minimal request:

```json
{
  "routing": {
    "dst": "AI.chat@motherbee",
    "ttl": 16,
    "trace_id": "9d8c6f4b-2f4d-4ad4-b5f4-8d8d9d9d0002"
  },
  "meta": {
    "msg_type": "system",
    "msg": "CONFIG_SET"
  },
  "payload": {
    "node_name": "AI.chat@motherbee",
    "config_version": 7,
    "apply_mode": "replace",
    "config": {}
  }
}
```

## 6. Common payload metadata

The platform/common contract defines only these common payload fields:

- `node_name`: fully-qualified target node
- `config_version`: monotonic version chosen by the caller/node contract
- `apply_mode`: `replace` or a future compatible mode such as `merge_patch`
- `config`: free-form JSON object interpreted by the node

Optional common payload metadata may be included:

- `request_id`
- `schema_version`
- `contract_version`
- `requested_by`

The platform must not interpret node-specific keys inside `payload.config`.

## 7. CONFIG_GET response requirements

`CONFIG_GET` must return not only the current config, but also the contract needed to build a valid `CONFIG_SET`.

Required conceptual response fields:

- `ok`
- `node_name`
- `config_version`
- `state`
- `config`
- `contract`

The `contract` object should be rich enough for `archi` or another client to guide a user.

Recommended `contract` contents:

- `node_family`
- `node_kind`
- `supports`
- `required_fields`
- `optional_fields`
- `secrets`
- `examples`
- `notes`

### 7.1 Minimum `contract.secrets[*]` contract

When a node supports secret-bearing config, each entry inside `contract.secrets` should expose at least:

- `field`
- `storage_key`
- `required`
- `configured`
- `value_redacted`
- `persistence`

Semantics:

- `field`
  - node-owned logical config field inside `payload.config`
- `storage_key`
  - node-owned local key name used in secret persistence
- `required`
  - whether the node needs this secret for a healthy/usable configuration
- `configured`
  - whether the secret is already present locally
- `value_redacted`
  - must be `true` whenever the node returns secret-related current-state information
- `persistence`
  - for v1 should normally be `local_file`

Rules:

- `CONFIG_GET` must never return raw secret values in `contract.secrets[*]`.
- `CONFIG_GET` must never return raw secret values inside `config`.
- `CONFIG_SET` may carry secret values inside `payload.config`; the platform transport does not interpret them.
- The node is responsible for separating regular config and secrets during persistence.

Example:

```json
{
  "ok": true,
  "node_name": "AI.chat@motherbee",
  "config_version": 6,
  "state": "configured",
  "config": {
    "behavior": {
      "kind": "openai_chat",
      "model": "gpt-4.1-mini"
    }
  },
  "contract": {
    "node_family": "AI",
    "node_kind": "AI.common",
    "supports": ["CONFIG_GET", "CONFIG_SET"],
    "required_fields": [
      "behavior.kind",
      "behavior.model"
    ],
    "optional_fields": [
      "behavior.instructions",
      "behavior.api_key_env",
      "runtime.worker_pool_size"
    ],
    "secrets": [
      {
        "field": "behavior.openai.api_key",
        "storage_key": "openai_api_key",
        "required": false,
        "configured": true,
        "value_redacted": true,
        "persistence": "local_file"
      }
    ],
    "examples": [
      {
        "apply_mode": "replace",
        "config": {
          "behavior": {
            "kind": "openai_chat",
            "model": "gpt-4.1-mini"
          }
        }
      }
    ],
    "notes": [
      "Prompt and OpenAI key are node-level concerns.",
      "CONFIG_SET may override in-memory behavior depending on node implementation."
    ]
  }
}
```

The reusable SDK helper for this metadata shape is:

- `fluxbee_sdk::node_secret::NodeSecretDescriptor`

## 8. CONFIG_SET response requirements

`CONFIG_SET` replies with `CONFIG_RESPONSE`.

Required conceptual fields:

- `ok`
- `node_name`
- `config_version`
- `state`

Optional:

- `error`
- `effective_config`
- `applied_at`
- `notes`

Example success:

```json
{
  "ok": true,
  "node_name": "AI.chat@motherbee",
  "config_version": 7,
  "state": "configured"
}
```

Example error:

```json
{
  "ok": false,
  "node_name": "AI.chat@motherbee",
  "config_version": 7,
  "state": "failed_config",
  "error": {
    "code": "invalid_config",
    "message": "behavior.model is required"
  }
}
```

## 9. Responsibilities

### 9.1 Core/admin

Core/admin owns:

- transport to the target node
- the common message envelope
- request validation for common metadata only
- exposing a stable admin surface that `archi` can call

Core/admin does **not** own:

- the meaning of `payload.config`
- per-node business validation
- node-specific config schemas

### 9.2 Node implementation

Each node owns:

- its `payload.config` schema
- validation of that schema
- the semantics of secret handling
- whether config changes are persisted, in-memory only, or both
- the `contract` information returned by `CONFIG_GET`

## 10. Admin surface

The existing generic transport path is:

- `POST /hives/{hive}/nodes/{node_name}/messages`

That path is enough for debug/manual use, but it is not the best public contract for `archi`.

Recommended admin-facing wrappers:

- `POST /hives/{hive}/nodes/{node_name}/control/config-get`
- `POST /hives/{hive}/nodes/{node_name}/control/config-set`

Behavior:

- admin wraps the request in the common L2 envelope
- admin sends it as unicast to the target node
- admin returns the node's `CONFIG_RESPONSE`

This keeps `archi` off the raw transport details while preserving node-defined config schemas.

## 11. Relationship with orchestrator config

This contract is intentionally separate from:

- `PUT /hives/{hive}/nodes/{node_name}/config`
- orchestrator `config.json`
- `CONFIG_CHANGED`

Recommended mental model:

- orchestrator config = bootstrap/infrastructure config required for a node to exist and start
- `CONFIG_SET` = functional/runtime config owned by the node itself

## 12. Onboarding a new node type

A new non-`SY` node becomes configurable by implementing:

- `CONFIG_GET`
- `CONFIG_SET`
- `CONFIG_RESPONSE`

Minimum onboarding requirement:

- `CONFIG_GET` must describe the schema expected by `CONFIG_SET`

This keeps the platform open for new node types without requiring a platform change for each new node schema.
