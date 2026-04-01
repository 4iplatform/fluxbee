# AI Nodes — Anexo de Ejemplos (v1)

> ✅ **Objetivo**: ejemplos claros y completos para implementación/QA del lifecycle de configuración de nodos `AI.*`.  
> Este anexo asume el contrato `text/v1` y el Control Plane definidos en la spec consolidada.

> ⚠️ **Alcance de este anexo**: los ejemplos describen el **contrato objetivo**.  
> Estado actual del runner (MVP técnico): `attachments/content_ref` todavía no están implementados de punta a punta; `thread_id` y `src_ilk` ya viajan canónicamente en `meta` top-level.

## Convenciones de identidad y thread

- `src_ilk` se asume **siempre presente** (puede ser ILK temporal) y viaja en `meta.src_ilk`.
- `thread_id` viaja canónicamente en `meta.thread_id`.
- El key efectivo de thread state en MVP vigente es `src_ilk`.

## Modo del runner (default/gov)

- `AI_NODE_MODE=default`: tools comunes.
- `AI_NODE_MODE=gov`: tools comunes + `ilk_register`.

---

## 1) Ejemplo de archivo JSON efectivo por nodo (single source of truth)

> ✅ **Fuente viva**: `${STATE_DIR}/ai-nodes/<node_name>.json`  
> default: `/var/lib/fluxbee/state/ai-nodes/AI.soporte.l1.json`

> ⚠️ Plantilla opcional (no fuente viva): `/etc/fluxbee/ai-nodes/<node>.yaml` (si existe, es solo referencia)

```json
{
  "schema_version": 1,
  "config_version": 1,

  "node": {
    "name": "AI.soporte.l1"
  },

  "behavior": {
    "kind": "openai_chat",
    "provider": "openai",

    "capabilities": { "multimodal": false },

    "params": {
      "model": "gpt-4.1-mini",
      "system_prompt": "Sos un agente de soporte conciso. Respondé en español.",
      "timeout_ms": 15000,
      "max_output_tokens": 256,
      "temperature": 0.2
    }
  },

  "runtime": {
    "handler_timeout_ms": 60000,
    "worker_pool_size": 4,
    "queue_capacity": 128
  },

  "secrets": {
    "openai": {
      "api_key": "***SET_VIA_CONFIG_SET_AND_PERSIST_LOCALLY***",
      "api_key_env": "OPENAI_API_KEY"
    }
  }
}
```

### Comentarios
- Este archivo representa la **config efectiva** aplicada.
- Si el nodo recibe `CONFIG_SET` válido con `apply_mode="replace"`, persiste un JSON equivalente, pero la key real debe vivir en `secrets.json`, no inline en el snapshot operativo.
- Si el archivo no existe, el nodo arranca `UNCONFIGURED` y espera `CONFIG_SET`.
json
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

        "secrets": {
          "openai": {
            "api_key": "sk-REDACTED-INLINE-MVP",
            "api_key_env": "OPENAI_API_KEY"
          }
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
- El campo canónico actual es `config.secrets.openai.api_key`.
- El secreto entra por `CONFIG_SET` y luego se persiste en `secrets.json` local del nodo.
- Si existen aliases legacy (`behavior.openai.api_key`, `behavior.api_key`), se aceptan solo durante migración.
- El nodo **NO** debe persistir el secreto en claro en `${STATE_DIR}`.
- En reinicio, el nodo debe rehidratar desde `secrets.json`; si no existe secreto local, puede quedar `FAILED_CONFIG (missing_secret)` hasta recibir `CONFIG_SET` nuevamente.



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
        }
      },
      "secrets": {
        "openai": {
          "api_key_env": "OPENAI_API_KEY"
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
- Reenvía `CONFIG_SET` con **la misma** `config_version=7` pero con `config.secrets.openai.api_key`.

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


---

## 8) Ejemplo: Thread State Store (LanceDB) - 1 JSON por `src_ilk`

> MVP vigente: un documento JSON por `src_ilk` (estructura libre por prompting), con compatibilidad legacy por `thread_id`.

### Put
```json
{
  "tool": "thread_state_put",
  "args": {
    "thread_id": "legacy-thread-id-ignored-in-scoped-mode",
    "data": {
      "status": "waiting_information",
      "description": "Asked for: name, phone, mail",
      "name": null,
      "phone": "+541112345678",
      "mail": null
    },
    "ttl_seconds": 86400
  }
}
```

### Get
```json
{
  "tool": "thread_state_get",
  "args": {
    "thread_id": "legacy-thread-id-ignored-in-scoped-mode"
  }
}
```

### Delete
```json
{
  "tool": "thread_state_delete",
  "args": {
    "thread_id": "legacy-thread-id-ignored-in-scoped-mode"
  }
}

## 9) Ejemplo: tool gov `ilk_register` (solo `AI_NODE_MODE=gov`)

```json
{
  "tool": "ilk_register",
  "args": {
    "src_ilk": "ilk:550e8400-e29b-41d4-a716-446655440000",
    "thread_id": "slack:T123:C456:thread:1710000000.000",
    "identity_candidate": {
      "name": "Noelia Eguren",
      "email": "neguren@4iplatform.com",
      "tenant_hint": "4iplatform"
    }
  }
}
```

Respuesta esperada (ok):

```json
{
  "status": "ok",
  "registered": true,
  "effective_target": "SY.identity@motherbee",
  "trace_id": "uuid-trace"
}
```
```
