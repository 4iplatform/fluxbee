# IO Nodes — Examples Annex (v1)

> ✅ **Goal**: clear, complete examples for implementation/QA of the lifecycle and configuration of **IO Nodes** (common + Slack).  
> This annex assumes the pattern unified with AI Nodes:
> - spawn by the orchestrator,
> - states `UNCONFIGURED/CONFIGURED/FAILED_CONFIG`,
> - Control Plane `CONFIG_SET/GET`, `CONFIG_RESPONSE`, `STATUS/PING`.

⚠️ **Status (2026-11-07):** these examples predate the IO family's `vault_ref` pattern (July
2026). Today an IO node's tokens live in SY.vault and its config names their key
(`slack.auth: {type: "vault_ref", resource_type: "slack", key: "<vault key>"}`); io.slack refuses
any other `auth.type` and reads no token from YAML, env or `CONFIG_SET`. Its config is the node's
`config.json`, not a YAML file. The secret-bearing YAML and JSON below (§1 to §5) show the old
MVP; today's config is in [io-slack-deploy-runbook](io-slack-deploy-runbook.md) and
[io-nodes-spec](io-nodes-spec.md) §5.

---

## 0) Quick conventions

- **Replace**: `apply_mode="replace"` and `payload.config` contains a full config.
- **Secrets**: they live in SY.vault, and the config names their key (`vault_ref`, since July 2026). They never travel inline, and are **not** persisted in clear text in `${STATE_DIR}`.
- **Credentials** (Slack): read from SY.vault at start, on a `VAULT_SECRET_CHANGED` for the key and
  every 60 s. A `CONFIG_SET` is written to `config.json`, so it survives a restart.

---

## 1) YAML example per node (Slack IO)

> 1 runtime-managed instance ↔ 1 node config.
> Example: `IO.slack.support@motherbee` runs as `fluxbee-node-IO.slack.support-motherbee.service`.

```yaml
# /etc/fluxbee/io-nodes/IO.slack.support.yaml
# schema_version is optional in YAML (if missing, 1 is assumed)

schema_version: 1

node:
  name: "IO.slack.support"
  router_socket: "/var/run/fluxbee/routers"
  config_dir: "/etc/fluxbee"

# runtime is OPTIONAL: if omitted, the runner applies common defaults.
# runtime:
#   handler_timeout_ms: 60000
#   worker_pool_size: 4
#   queue_capacity: 256

adapter:
  kind: "slack_socket_mode"

  # Functional config (not secret)
  params:
    # Optional: restrict to workspace/teams if applicable
    # allowed_team_ids: ["T01234567"]
    # Optional: default support channel
    # default_channel_id: "C01234567"

    # Which events this node processes
    event_filter:
      # Socket mode: normally we care about app_mention
      allowed_event_types: ["app_mention"]

# --- Secrets (the old MVP vs today) ---
#
# OLD MVP, not accepted today: tokens inline in YAML, as in (A).
#
# TODAY (since July 2026): the tokens live in SY.vault and the config names their key, as in (B).
#
# (A) OLD MVP: inline secret in YAML
secrets:
  slack:
    app_token: "xapp-REDACTED-IN-YAML"
    bot_token: "xoxb-REDACTED-IN-YAML"

# (B) TODAY: a vault reference (slack.auth in the node's config.json)
# slack:
#   auth:
#     type: "vault_ref"
#     resource_type: "slack"
#     key: "slack:io-slack-support"
```

### Comments
- In Socket Mode, `url_verification/challenge` does **not apply** (that is the Events API over HTTP).
- Today a node whose vault secret is missing still starts, and runs degraded until the secret
  arrives ([io-slack](io-slack.md) §12); it is not `FAILED_CONFIG`.

---

## 2) Example: full CONFIG_SET (replace) for Slack (credential rotation)

> ✅ This example shows a **full replace** (not a merge).  
> Typical case: the `bot_token` was rotated and we want to update it without restarting.

```json
{
  "routing": {
    "src": "SY.orchestrator@dev",
    "dst": "IO.slack.support@dev",
    "ttl": 16,
    "trace_id": "uuid-trace-io-config-0001"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_SET"
  },
  "payload": {
    "subsystem": "io_node",
    "node_name": "IO.slack.support",
    "schema_version": 1,
    "config_version": 12,
    "apply": "immediate",
    "apply_mode": "replace",

    "config": {
      "adapter": {
        "kind": "slack_socket_mode",
        "params": {
          "event_filter": {
            "allowed_event_types": ["app_mention"]
          }
        }
      },

      "secrets": {
        "slack": {
          "app_token": "xapp-REDACTED-INLINE-MVP",
          "bot_token": "xoxb-REDACTED-INLINE-MVP"
        }
      },

      "runtime": {
        "handler_timeout_ms": 60000,
        "worker_pool_size": 4,
        "queue_capacity": 256
      }
    }
  }
}
```

### Comments on the example
- `apply_mode="replace"` means that `payload.config` carries a full config for the node.
- `config_version` must be monotonic. If the orchestrator does not know it, it asks first with `CONFIG_GET`.
- Inline secrets are **not persisted in clear text** in `${STATE_DIR}`. They are kept in memory to operate.
- If the node already had tokens in YAML, this `CONFIG_SET` can act as an in-memory override (while the process is alive).

---

## 3) Example: CONFIG_RESPONSE (ok) for Slack

```json
{
  "routing": {
    "src": "IO.slack.support@dev",
    "dst": "SY.orchestrator@dev",
    "ttl": 16,
    "trace_id": "uuid-trace-io-config-0001"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_RESPONSE"
  },
  "payload": {
    "subsystem": "io_node",
    "node_name": "IO.slack.support@dev",
    "status": "ok",
    "state": "CONFIGURED",
    "schema_version": 1,
    "config_version": 12,

    "effective_config": {
      "adapter": {
        "kind": "slack_socket_mode",
        "params": {
          "event_filter": { "allowed_event_types": ["app_mention"] }
        }
      },
      "runtime": {
        "handler_timeout_ms": 60000,
        "worker_pool_size": 4,
        "queue_capacity": 256
      },
      "secrets": {
        "slack": {
          "app_token": "***REDACTED***",
          "bot_token": "***REDACTED***"
        }
      }
    }
  }
}
```

---

## 4) Example: CONFIG_RESPONSE (error) — invalid config

Typical case: `secrets.slack.app_token` is missing (required for socket mode).

```json
{
  "routing": {
    "src": "IO.slack.support@dev",
    "dst": "SY.orchestrator@dev",
    "ttl": 16,
    "trace_id": "uuid-trace-io-config-0002"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_RESPONSE"
  },
  "payload": {
    "subsystem": "io_node",
    "node_name": "IO.slack.support@dev",
    "status": "error",
    "state": "FAILED_CONFIG",
    "schema_version": 1,
    "config_version": 13,

    "error": {
      "code": "invalid_config",
      "message": "Missing required field: secrets.slack.app_token",
      "retryable": false,
      "details": { "path": "secrets.slack.app_token", "reason": "required" }
    }
  }
}
```

### Note
- If the node was `CONFIGURED`, it must **keep** the previous config and not degrade because of an invalid CONFIG_SET.
- If it was `UNCONFIGURED` or `FAILED_CONFIG`, it stays in that state until it receives a valid config.

---

## 5) Example: receiving CONFIG_SET when the node is UNCONFIGURED or FAILED_CONFIG

### 5.1 UNCONFIGURED → CONFIGURED
- The node starts without tokens (YAML without secrets).
- The orchestrator sends a `CONFIG_SET` replace with inline tokens.
- It responds `CONFIG_RESPONSE status=ok` and moves to `CONFIGURED`.

### 5.2 FAILED_CONFIG (missing_secret) → CONFIGURED
- The node restarts and loses the in-memory override.
- If there are no tokens in YAML/env, it stays `FAILED_CONFIG (missing_secret)`.
- The orchestrator resends `CONFIG_SET` with **the same** `config_version` (idempotent), including tokens.
- The node goes back to `CONFIGURED` without incrementing the version.

Minimal example of the expected response:

```json
{
  "meta": { "type": "system", "msg": "CONFIG_RESPONSE" },
  "payload": {
    "subsystem": "io_node",
    "status": "ok",
    "state": "CONFIGURED",
    "schema_version": 1,
    "config_version": 12
  }
}
```
