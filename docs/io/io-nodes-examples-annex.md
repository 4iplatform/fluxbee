# IO Nodes — Anexo de Ejemplos (v1)

> ✅ **Objetivo**: ejemplos claros y completos para implementación/QA del ciclo de vida y configuración de **IO Nodes** (común + Slack).  
> Este anexo asume el patrón unificado con AI Nodes:
> - spawn por orchestrator,
> - estados `UNCONFIGURED/CONFIGURED/FAILED_CONFIG`,
> - Control Plane `CONFIG_SET/GET`, `CONFIG_RESPONSE`, `STATUS/PING`.

---

## 0) Convenciones rápidas

- **Replace**: `apply_mode="replace"` y `payload.config` contiene una config completa.
- **Secrets**: HOY pueden viajar inline (MVP). **No** se persisten en claro en `${STATE_DIR}`.
- **Precedencia de credenciales** (Slack):
  1) override en memoria por `CONFIG_SET` (si existe),
  2) config local (YAML/env).
  Tras restart, el override en memoria se pierde.

---

## 1) Ejemplo YAML por nodo (Slack IO)

> 1 instancia systemd ↔ 1 YAML.  
> Ejemplo: `fluxbee-io-slack@IO.slack.support.service` carga `/etc/fluxbee/io-nodes/IO.slack.support.yaml`.

```yaml
# /etc/fluxbee/io-nodes/IO.slack.support.yaml
# schema_version es opcional en YAML (si falta, se asume 1)

schema_version: 1

node:
  name: "IO.slack.support"
  router_socket: "/var/run/fluxbee/routers"
  config_dir: "/etc/fluxbee"

# runtime es OPCIONAL: si se omite, el runner aplica defaults comunes.
# runtime:
#   handler_timeout_ms: 60000
#   worker_pool_size: 4
#   queue_capacity: 256

adapter:
  kind: "slack_socket_mode"

  # Config funcional (no secreta)
  params:
    # Opcional: restringir a workspace/teams si aplica
    # allowed_team_ids: ["T01234567"]
    # Opcional: canal de soporte default
    # default_channel_id: "C01234567"

    # Qué eventos procesa este nodo
    event_filter:
      # Socket mode: normalmente nos interesa app_mention
      allowed_event_types: ["app_mention"]

# --- Secrets (HOY vs FUTURO) ---
#
# HOY (camino real sin depender de un secret manager):
# - se permite guardar tokens inline en YAML (quedan en disco).
#
# FUTURO (recomendado):
# - usar refs (env/file/vault/kms) y un gestor de secretos.
#
# IMPORTANTE: no usar ambas variantes al mismo tiempo.
#
# (A) HOY: secreto inline en YAML (MVP)
secrets:
  slack:
    app_token: "xapp-REDACTED-IN-YAML"
    bot_token: "xoxb-REDACTED-IN-YAML"

# (B) FUTURO: referencias a secretos (NO usar junto con (A))
# secrets:
#   slack:
#     app_token_ref: "env:SLACK_APP_TOKEN"
#     bot_token_ref: "env:SLACK_BOT_TOKEN"
```

### Comentarios
- En Socket Mode **no aplica** `url_verification/challenge` (eso es Events API por HTTP).
- Si omitís `secrets.slack.*` en YAML, el nodo puede arrancar igual pero quedará `FAILED_CONFIG (missing_secret)` hasta recibir un `CONFIG_SET` con credenciales inline.

---

## 2) Ejemplo: CONFIG_SET (replace) completo para Slack (rotación de credenciales)

> ✅ Este ejemplo muestra un **replace completo** (no merge).  
> Caso típico: se rotó el `bot_token` y queremos actualizarlo sin reiniciar.

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

### Comentarios del ejemplo
- `apply_mode="replace"` implica que `payload.config` trae una config completa para el nodo.
- `config_version` debe ser monotónico. Orchestrator, si no la conoce, consulta primero con `CONFIG_GET`.
- Los secretos inline **no se persisten en claro** en `${STATE_DIR}`. Se retienen en memoria para operar.
- Si el nodo ya tenía tokens en YAML, este `CONFIG_SET` puede actuar como override en memoria (mientras el proceso esté vivo).

---

## 3) Ejemplo: CONFIG_RESPONSE (ok) para Slack

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

## 4) Ejemplo: CONFIG_RESPONSE (error) — config inválida

Caso típico: falta `secrets.slack.app_token` (required para socket mode).

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

### Nota
- Si el nodo estaba `CONFIGURED`, debe **mantener** la config anterior y no degradarse por un CONFIG_SET inválido.
- Si estaba `UNCONFIGURED` o `FAILED_CONFIG`, permanece en ese estado hasta recibir config válida.

---

## 5) Ejemplo: recibir CONFIG_SET cuando el nodo está UNCONFIGURED o FAILED_CONFIG

### 5.1 UNCONFIGURED → CONFIGURED
- El nodo arranca sin tokens (YAML sin secrets).
- Orchestrator manda `CONFIG_SET` replace con tokens inline.
- Responde `CONFIG_RESPONSE status=ok` y pasa a `CONFIGURED`.

### 5.2 FAILED_CONFIG (missing_secret) → CONFIGURED
- El nodo reinicia y pierde override en memoria.
- Si no hay tokens en YAML/env, queda `FAILED_CONFIG (missing_secret)`.
- Orchestrator reenvía `CONFIG_SET` con **la misma** `config_version` (idempotente) incluyendo tokens.
- El nodo vuelve a `CONFIGURED` sin incrementar versión.

Ejemplo mínimo de respuesta esperada:

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
