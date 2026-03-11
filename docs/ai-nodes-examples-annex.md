# AI Nodes — Anexo de Ejemplos (v1)

> ✅ **Objetivo**: ejemplos claros y completos para implementación/QA del lifecycle de configuración de nodos `AI.*`.  
> Este anexo asume el contrato `text/v1` y el Control Plane definidos en la spec consolidada.

---

## 1) Ejemplo de YAML por nodo (1 YAML ↔ 1 instancia)

> Nota: YAML **por nodo**. La instancia systemd `fluxbee-ai-node@AI.soporte.l1.service` carga `/etc/fluxbee/ai-nodes/AI.soporte.l1.yaml`.

```yaml
# /etc/fluxbee/ai-nodes/AI.soporte.l1.yaml
# schema_version es opcional en YAML (si falta, se asume 1)

schema_version: 1

node:
  # Nombre L2 del nodo (sin @hive; el SDK agrega @hive automáticamente al registrarlo)
  name: "AI.soporte.l1"

  # Socket del router (path fijo por isla, normalmente bajo /var/run/fluxbee/routers/)
  router_socket: "/var/run/fluxbee/routers"

  # Directorio donde el runner persiste el UUID del nodo (si aplica al runtime)
  uuid_persistence_dir: "/var/lib/fluxbee/nodes"

  # Directorio base de config (donde vive hive.yaml)
  config_dir: "/etc/fluxbee"

# runtime es OPCIONAL.
# Si se omite, el runner usa defaults razonables comunes a todos los nodos AI.
# Descomentar solo si necesitás tunear este nodo en particular.
# runtime:
#   read_timeout_ms: 30000
#   handler_timeout_ms: 60000
#   write_timeout_ms: 10000
#   queue_capacity: 128
#   worker_pool_size: 4
#   retry_max_attempts: 3
#   retry_initial_backoff_ms: 200
#   retry_max_backoff_ms: 2000
#   metrics_log_interval_ms: 30000

behavior:
  kind: "openai_chat"
  model: "gpt-4.1-mini"

  instructions:
    source: "inline"
    value: "Sos un agente de soporte conciso. Respondé en español y pedí datos mínimos."
    trim: true

  # --- Secrets (HOY vs FUTURO) ---
  #
  # HOY (camino real recomendado en este estado del sistema):
  # - Mantener la API key en el YAML (sí, queda en disco).
  # - Esto evita depender de un gestor de secretos/orchestrator que la inyecte por env.
  #
  # FUTURO (recomendado a mediano plazo):
  # - Reemplazar por un ref (env/file/vault/kms) y un gestor de secretos.
  #
  # IMPORTANTE: no usar ambas variantes al mismo tiempo.
  #
  # (A) HOY: secreto inline en YAML (MVP)
  openai:
    api_key: "sk-REDACTED-IN-YAML-MVP"

  # (B) FUTURO: referencia a secreto (NO usar junto con (A))
  # secrets:
  #   api_key_ref: "env:OPENAI_API_KEY"
```json
{
  "routing": {
    "src": "uuid-sy-orchestrator",
    "dst": "AI.soporte.l1@dev",
    "ttl": 16,
    "trace_id": "uuid-trace-config-0001"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_SET"
  },
  "payload": {
    "subsystem": "ai_node",
    "node_name": "AI.soporte.l1",
    "schema_version": 1,
    "config_version": 1,
    "apply": "immediate",
    "apply_mode": "replace",
    "config": {
      "behavior": {
        "kind": "openai_chat",
        "model": "gpt-4.1-mini",
        "instructions": {
          "source": "inline",
          "value": "Sos un agente de soporte conciso. Respondé en español.",
          "trim": true
        },

        "openai": {
          "api_key": "sk-REDACTED-INLINE-MVP"
        },

        "model_settings": {
          "temperature": 0.2,
          "top_p": 1.0,
          "max_output_tokens": 256
        }
      },

      "runtime": {
        "handler_timeout_ms": 60000,
        "worker_pool_size": 4,
        "queue_capacity": 128
      }
    }
  }
}
```

### Notas del ejemplo (MVP)
- El secreto `openai.api_key` puede venir **inline** por CONFIG_SET o desde YAML.
- **Precedencia** (si ambos existen): CONFIG_SET (memoria) sobrescribe el valor de YAML mientras el proceso esté vivo.
- El nodo **NO** debe persistir el secreto en claro en `${STATE_DIR}`.
- En reinicio, si no hay otra fuente de secreto, el nodo puede quedar `FAILED_CONFIG (missing_secret)` hasta recibir CONFIG_SET nuevamente.



---

## 3) Ejemplo: respuesta CONFIG_RESPONSE cuando CONFIG_SET es válido

```json
{
  "routing": {
    "src": "uuid-ai-node",
    "dst": "uuid-sy-orchestrator",
    "ttl": 16,
    "trace_id": "uuid-trace-config-0001"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_RESPONSE"
  },
  "payload": {
    "subsystem": "ai_node",
    "node_name": "AI.soporte.l1@dev",
    "status": "ok",
    "schema_version": 1,
    "config_version": 1,
    "state": "CONFIGURED",

    "effective_config": {
      "behavior": {
        "kind": "openai_chat",
        "model": "gpt-4.1-mini",
        "instructions": {
          "source": "inline",
          "value": "Sos un agente de soporte conciso. Respondé en español.",
          "trim": true
        },
        "model_settings": {
          "temperature": 0.2,
          "top_p": 1.0,
          "max_output_tokens": 256
        },

        "openai": {
          "api_key": "***REDACTED***"
        }
      },
      "runtime": {
        "handler_timeout_ms": 60000,
        "worker_pool_size": 4,
        "queue_capacity": 128
      }
    }
  }
}
```

### Nota
- El `trace_id` se usa para correlación (debe coincidir con el request).
- `effective_config` nunca debe exponer secretos en claro.



---

## 4) Ejemplo: respuesta CONFIG_RESPONSE cuando CONFIG_SET NO es válido

Caso típico: falta `behavior.model` o el `schema_version` no es soportado.

```json
{
  "routing": {
    "src": "uuid-ai-node",
    "dst": "uuid-sy-orchestrator",
    "ttl": 16,
    "trace_id": "uuid-trace-config-0002"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_RESPONSE"
  },
  "payload": {
    "subsystem": "ai_node",
    "node_name": "AI.soporte.l1@dev",
    "status": "error",
    "schema_version": 1,
    "config_version": 2,
    "state": "UNCONFIGURED",

    "error_code": "invalid_config",
    "error_message": "Missing required field: behavior.model",
    "details": {
      "path": "behavior.model",
      "reason": "required"
    }
  }
}
```

### Notas
- Si el nodo ya estaba `CONFIGURED`, **debe quedarse** en su config anterior.
- `config_version`:
  - si el request venía con `config_version=2` y es inválido, se reporta el intento, pero **no se aplica**.



---

## 5) Ejemplo: CONFIG_SET recibido cuando el nodo está UNCONFIGURED o FAILED_CONFIG

### 5.1 Nodo UNCONFIGURED recibe CONFIG_SET válido → pasa a CONFIGURED

**Respuesta esperada** (similar a §3):

```json
{
  "meta": { "type": "system", "msg": "CONFIG_RESPONSE" },
  "payload": {
    "subsystem": "ai_node",
    "status": "ok",
    "state": "CONFIGURED",
    "schema_version": 1,
    "config_version": 1
  }
}
```

### 5.2 Nodo FAILED_CONFIG (missing_secret) recibe CONFIG_SET con el secreto → recupera

Escenario:
- El nodo tiene config persistida (sin secreto) y quedó `FAILED_CONFIG (missing_secret)` tras reinicio.
- Orchestrator hace `CONFIG_GET` y ve `config_version=7`.
- Reenvía `CONFIG_SET` con **la misma** `config_version=7` pero con `openai.api_key` inline.

**Respuesta esperada:**

```json
{
  "meta": { "type": "system", "msg": "CONFIG_RESPONSE" },
  "payload": {
    "subsystem": "ai_node",
    "status": "ok",
    "state": "CONFIGURED",
    "schema_version": 1,
    "config_version": 7
  }
}
```

### Nota
- Esto permite recuperación sin restart ni intervención humana.

---

## 6) (Opcional) Ejemplo breve: mensaje user recibido sin configuración

Si el nodo está `UNCONFIGURED` o `FAILED_CONFIG` y recibe `meta.type="user"`:

```json
{
  "meta": { "type": "user" },
  "payload": { "type": "text", "content": "hola" }
}
```

Debe responder:

```json
{
  "meta": { "type": "user" },
  "payload": {
    "type": "text",
    "content": "[node_not_configured] El nodo AI todavía no está configurado. Reintentar luego."
  }
}
```

> Nota: el formato exacto de error payload general (no-config) se puede normalizar más adelante; en MVP pedimos “código + texto”.


---

## 7) Ejemplos con ErrorV1Payload (`payload.type="error"`)

> ✅ Recomendado para AI Nodes. Si el receptor no soporta `type="error"`, se puede usar fallback `text/v1` prefijado.

### 7.1 Error: `node_not_configured`

```json
{
  "meta": { "type": "user" },
  "payload": {
    "type": "error",
    "code": "node_not_configured",
    "message": "El nodo AI todavía no está configurado. Reintentar luego.",
    "retryable": true,
    "details": { "state": "UNCONFIGURED" }
  }
}
```

### 7.2 Error: `invalid_config` (validation) — Control Plane style (`CONFIG_RESPONSE`)

> ✅ En Control Plane, el error viaja como parte del payload del comando (status + error), no como `payload.type="error"`.

```json
{
  "meta": { "type": "system", "msg": "CONFIG_RESPONSE" },
  "payload": {
    "subsystem": "ai_node",
    "status": "error",
    "error": {
      "code": "invalid_config",
      "message": "Missing required field: behavior.model",
      "retryable": false,
      "details": { "path": "behavior.model", "reason": "required" }
    }
  }
}
```


### 7.3 Error: Blob (código canónico)

```json
{
  "payload": {
    "type": "error",
    "code": "BLOB_TOO_LARGE",
    "message": "Attachment exceeds BlobConfig.max_blob_bytes",
    "retryable": false,
    "details": { "limit_bytes": 104857600, "actual_bytes": 209715200 }
  }
}
```
