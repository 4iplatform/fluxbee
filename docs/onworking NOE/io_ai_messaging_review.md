# Revisión de Mensajería IO/IA (estado actual)

Estado: borrador de trabajo  
Fecha: 2026-03-04

## 1. Alcance y fuentes

Este documento revisa únicamente mensajería entre nodos IO/IA y Router, usando documentación vigente (excluyendo cognition/cognitive por pedido).

Fuentes principales:
- `docs/02-protocolo.md`
- `docs/04-routing.md`
- `docs/10-identity-layer3.md`
- `docs/io/nodos_io_spec.md`
- `docs/io/io-common.md`
- `docs/io/io-slack.md`
- `docs/AI_nodes_spec.md`
- `docs/AI_Nodes_SDK_Spec_v1.md`
- implementación actual en `crates/fluxbee_sdk`, `crates/fluxbee_ai_sdk`, `crates/fluxbee_ai_nodes`, `nodes/io/common`, `nodes/io/io-slack`

## 2. Modelo canónico del protocolo

Base común esperada:
- `routing`: `src`, `dst`, `ttl`, `trace_id`
- `meta`: `type` + metadata de routing/OPA/L3
- `payload`: contrato de aplicación

Semántica de `routing.dst` esperada:
- `UUID` o nombre L2 `TYPE.name@hive`: unicast
- `"broadcast"`: fanout
- `null`: resolución por OPA (`Destination::Resolve`)

## 3. IO -> Router (inbound)

### 3.1 Especificado

En specs IO + protocolo:
- Salida normal desde IO con `meta.type = "user"`.
- `routing.dst = null` por defecto para que Router/OPA resuelva destino (excepto pruebas controladas con destino fijo).
- `meta.src_ilk` se intenta resolver por SHM; en miss debe permitirse `null` (router-driven onboarding).
- `meta.context.io` lleva datos operativos del canal (`entrypoint`, `conversation`, `message.id`, `reply_target`).
- Payload recomendado tipo `text` con `content`.

### 3.2 Implementado hoy

`io-common` / `io-slack` actualmente hacen:
- Construcción de `Message` con:
  - `routing.src = uuid del nodo IO`
  - `routing.dst = Resolve` o `Unicast(dst fijo)` si `IO_SLACK_DST_NODE` está configurado
  - `routing.trace_id` nuevo UUID
  - `meta.type = "user"`
- Identidad:
  - Se intenta resolver `src_ilk` con `IdentityResolver`.
  - Si falla/miss, se envía igual (`null`).
- Contexto:
  - Se manda `meta.context.io` con datos Slack.
  - Además, por limitación actual del `Meta` del SDK, `src_ilk`/`dst_ilk` se inyectan dentro de `meta.context` (workaround temporal).
- Payload actual desde Slack:
  - `{ "type": "text", "content": "...", "raw": { ... } }`

### 3.3 Falta definir/cerrar

- Cierre de contrato L3 en wire format final:
  - mover `src_ilk`/`dst_ilk` a campos top-level de `meta` cuando el SDK/protocolo efectivo lo soporte.
- Definir política canónica de `meta.target` para IO inbound (cuándo usarlo vs `dst` fijo).
- Formalizar si todos los IO deben adjuntar siempre el bloque completo `meta.context.io` o mínimo requerido por canal.

### 3.4 Ejemplos (IO -> Router)

Ejemplo especificado (canónico, `dst = null`, L3 top-level):

```json
{
  "routing": {
    "src": "uuid-io-node",
    "dst": null,
    "ttl": 16,
    "trace_id": "2f5f8c1a-6bb0-4d2a-9a6b-c7a9b0c4f101" // identifica una operación/mensaje (o cadena de ida-vuelta correlacionada). thread_id/contexto conversacional: identifica la conversación.
  },
  "meta": {
    "type": "user",
    "target": "AI.chat@sandbox",
    "src_ilk": "ilk:550e8400-e29b-41d4-a716-446655440000",
    "dst_ilk": null,
    "context": {
      "io": {
        "channel": "slack",
        "entrypoint": { "kind": "slack_workspace", "id": "T123" },
        "conversation": { "kind": "slack_channel", "id": "C123", "thread_id": "171234.567" },
        "message": { "id": "EvABC" },
        "reply_target": {
          "kind": "slack_post",
          "address": "C123",
          "params": { "thread_ts": "171234.567", "workspace_id": "T123" }
        }
      }
    }
  },
  "payload": {
    "type": "text",
    "content": "Hola",
    "raw": { "slack": {} }
  }
}
```

Ejemplo implementado hoy (workaround: `src_ilk`/`dst_ilk` dentro de `meta.context`):

```json
{
  "routing": {
    "src": "uuid-io-node",
    "dst": null,
    "ttl": 16,
    "trace_id": "2f5f8c1a-6bb0-4d2a-9a6b-c7a9b0c4f101"
  },
  "meta": {
    "type": "user",
    "context": {
      "src_ilk": "ilk:550e8400-e29b-41d4-a716-446655440000",
      "dst_ilk": null,
      "io": {
        "channel": "slack",
        "entrypoint": { "kind": "slack_workspace", "id": "T123" },
        "conversation": { "kind": "slack_channel", "id": "C123", "thread_id": "171234.567" },
        "message": { "id": "EvABC" },
        "reply_target": {
          "kind": "slack_post",
          "address": "C123",
          "params": { "thread_ts": "171234.567", "workspace_id": "T123" }
        }
      }
    }
  },
  "payload": {
    "type": "text",
    "content": "Hola",
    "raw": { "slack": {} }
  }
}
```

## 4. Router -> IO (outbound)

### 4.1 Especificado

En specs IO:
- IO consume mensajes del router y responde al canal externo usando `meta.context.io.reply_target`.
- Si hay `ctx_window` u otros metadatos internos del router, no deben reenviarse al canal externo.

### 4.2 Implementado hoy

`io-slack` outbound loop:
- Recibe mensaje del router.
- Solo procesa si existe `meta.context` y puede extraer `reply_target` compatible Slack.
- Extrae texto desde `payload.content` y opcional `payload.blocks`.
- Publica con `chat.postMessage(channel, text, thread_ts, blocks)`.
- Si no hay `reply_target` parseable, ignora el mensaje silenciosamente.

### 4.3 Falta definir/cerrar

- Política explícita para mensajes sin `reply_target`:
  - ignorar, reintentar, o enviar a dead-letter/log de error operativo.
- Contrato de payload outbound para IO:
  - campos obligatorios (`content`) y opcionales (`blocks`, adjuntos futuros).
- Estrategia de compatibilidad cuando un AI responde sin `meta.context.io`.

### 4.4 Ejemplos (Router -> IO)

Ejemplo válido para `io-slack` (con `reply_target`):

```json
{
  "routing": {
    "src": "uuid-ai-node",
    "dst": "uuid-io-node",
    "ttl": 16,
    "trace_id": "2f5f8c1a-6bb0-4d2a-9a6b-c7a9b0c4f101"
  },
  "meta": {
    "type": "user",
    "context": {
      "io": {
        "reply_target": {
          "kind": "slack_post",
          "address": "C123",
          "params": { "thread_ts": "171234.567", "workspace_id": "T123" }
        }
      }
    }
  },
  "payload": {
    "type": "text",
    "content": "Respuesta del agente",
    "blocks": []
  }
}
```

Ejemplo que hoy `io-slack` ignora (sin `reply_target`):

```json
{
  "routing": {
    "src": "uuid-ai-node",
    "dst": "uuid-io-node",
    "ttl": 16,
    "trace_id": "2f5f8c1a-6bb0-4d2a-9a6b-c7a9b0c4f101"
  },
  "meta": {
    "type": "user",
    "context": {}
  },
  "payload": {
    "type": "text",
    "content": "Respuesta del agente"
  }
}
```

## 5. Router -> IA (inbound a AI)

### 5.1 Especificado

En specs AI SDK:
- Runtime AI debe leer del router y pasar `Message` al handler (`AiNode::on_message`).
- Contrato mínimo útil: payload `text` + helpers (`extract_text`).
- Runtime desacoplado de cognition y sin dependencia obligatoria de storage.

### 5.2 Implementado hoy

`fluxbee_ai_sdk::NodeRuntime` + `ai_node_runner`:
- Runtime async con:
  - `read_timeout` (idle no mata proceso),
  - cola acotada,
  - worker pool,
  - retries para errores recuperables,
  - métricas/logs estructurados.
- `ai_node_runner`:
  - ignora mensajes `meta.type = "system"`.
  - intenta `extract_text(payload)`; fallback a string vacío si no hay texto.
  - comportamiento configurable: `echo` u `openai_chat`.

### 5.3 Falta definir/cerrar

- Contrato estricto de inbound AI:
  - qué campos de `meta` son obligatorios para producción (ej. `src_ilk`, `ctx`, `reply_target`).
- Política para payloads no-texto:
  - descartar, transformar o enrutar a nodo especializado.
- Criterio normativo sobre tratamiento de mensajes `system` (hoy se ignoran en runner genérico).

### 5.4 Ejemplos (Router -> IA)

Ejemplo user message procesable por `ai_node_runner`:

```json
{
  "routing": {
    "src": "uuid-io-node",
    "dst": "uuid-ai-node",
    "ttl": 16,
    "trace_id": "2f5f8c1a-6bb0-4d2a-9a6b-c7a9b0c4f101"
  },
  "meta": {
    "type": "user",
    "context": {
      "io": {
        "reply_target": {
          "kind": "slack_post",
          "address": "C123",
          "params": { "thread_ts": "171234.567", "workspace_id": "T123" }
        }
      }
    }
  },
  "payload": {
    "type": "text",
    "content": "¿Me ayudás con mi factura?"
  }
}
```

Ejemplo system message que hoy se ignora:

```json
{
  "routing": {
    "src": "uuid-router",
    "dst": "uuid-ai-node",
    "ttl": 1,
    "trace_id": "4dfce9a7-f319-4a4f-b6e8-f45f7fa7aa55"
  },
  "meta": {
    "type": "system",
    "msg": "ECHO"
  },
  "payload": {}
}
```

## 6. IA -> Router (outbound desde AI)

### 6.1 Especificado

En specs AI SDK:
- Respuesta debe conservar correlación (`trace_id`).
- Respuesta debería volver al origen del mensaje (reply routing).
- Runtime debe encargarse de escritura al router de forma segura/async.

### 6.2 Implementado hoy

`build_reply_message_runtime_src` + runtime:
- Builder de respuesta:
  - invierte dirección: `routing.dst = Unicast(msg.routing.src)`
  - preserva `routing.trace_id`
  - preserva `ttl` (mínimo 1)
  - copia `meta` completo del mensaje original
- Runtime, antes de enviar, fija `routing.src` al UUID real del nodo conectado.

### 6.3 Falta definir/cerrar

- Regla explícita de qué partes de `meta` deben copiarse vs recalcularse en la respuesta AI.
- Definir si AI debe completar `dst_ilk`/otros campos L3 al responder.
- Política formal para respuestas sin `meta.context.io` (impacta delivery en IO).

### 6.4 Ejemplos (IA -> Router)

Ejemplo de respuesta generada hoy por `ai_node_runner` (patrón actual):

```json
{
  "routing": {
    "src": "uuid-ai-node",
    "dst": "uuid-io-node",
    "ttl": 16,
    "trace_id": "2f5f8c1a-6bb0-4d2a-9a6b-c7a9b0c4f101"
  },
  "meta": {
    "type": "user",
    "context": {
      "io": {
        "reply_target": {
          "kind": "slack_post",
          "address": "C123",
          "params": { "thread_ts": "171234.567", "workspace_id": "T123" }
        }
      }
    }
  },
  "payload": {
    "type": "text",
    "content": "Claro, te ayudo con eso."
  }
}
```

Ejemplo problema típico (sin `meta.context.io` en la respuesta):

```json
{
  "routing": {
    "src": "uuid-ai-node",
    "dst": "uuid-io-node",
    "ttl": 16,
    "trace_id": "2f5f8c1a-6bb0-4d2a-9a6b-c7a9b0c4f101"
  },
  "meta": {
    "type": "user"
  },
  "payload": {
    "type": "text",
    "content": "Respuesta sin metadata de retorno"
  }
}
```

## 7. Brechas transversales detectadas

## 7.1 Brecha protocolo documentado vs structs efectivos SDK

Hoy hay desalineación entre docs que describen `meta.src_ilk`/`meta.dst_ilk` top-level y el `Meta` efectivo de `fluxbee_sdk` (que actualmente no los modela explícitamente).

Efecto actual:
- IO usa workaround metiendo `src_ilk`/`dst_ilk` dentro de `meta.context`.
- OPA/L3 queda funcional solo si el router/consumidores aceptan ese convenio temporal.

## 7.2 Dependencia fuerte de `meta.context.io.reply_target`

El retorno a canal externo depende de que ese bloque sobreviva todo el camino IO -> Router -> IA -> Router -> IO.

Efecto:
- Si algún nodo limpia o no propaga `meta.context.io`, el IO no sabe dónde responder.

## 8. Secciones para revisión y cierre

Checklist de trabajo (para completar en revisiones):

- [ ] Definir contrato L3 final en wire format (`meta.src_ilk`/`meta.dst_ilk`)
- [ ] Definir política canónica IO inbound: `dst=null + meta.target` vs `dst fijo`
- [ ] Definir manejo de outbound IO sin `reply_target`
- [ ] Definir contrato obligatorio de inbound AI
- [ ] Definir política de copia/reescritura de `meta` en respuestas AI
- [ ] Definir contrato mínimo de payload outbound para IO (texto, bloques, adjuntos)
- [ ] Confirmar actualización de docs para eliminar contradicciones con implementación
