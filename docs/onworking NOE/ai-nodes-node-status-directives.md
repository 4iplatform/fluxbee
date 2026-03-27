# Directivas de implementación — `NODE_STATUS_GET` en runtimes de AI Nodes (MVP)

> Branch referencia: **fluxbee-260302-AINode-260316** (válido “por hoy”).  
> Objetivo: que los AI Nodes reporten salud vía **NODE_REPORTED** (no `ORCHESTRATOR_INFERRED`) en el endpoint de status del hive.

---

## 1) Problema

Cuando se consulta:

```bash
GET /hives/{HIVE_ID}/nodes/{NODE_NAME}@{HIVE_ID}/status
```

`SY.orchestrator` intenta obtener salud real del nodo enviando un mensaje system:

- `meta.msg_type = "system"`
- `meta.msg = "NODE_STATUS_GET"`

Si el nodo **no responde** dentro del timeout (~2s), `SY.orchestrator` aplica fallback y devuelve:

- `health_source = "ORCHESTRATOR_INFERRED"`

Si el nodo **sí responde** con `NODE_STATUS_GET_RESPONSE` incluyendo `health_state`, `SY.orchestrator` devuelve:

- `health_source = "NODE_REPORTED"`

---

## 2) Solución MVP (recomendada): usar helper del SDK

En este branch existe un helper en el SDK:

- archivo: `crates/fluxbee_sdk/src/status.rs`
- función: `fluxbee_sdk::status::try_handle_default_node_status(...)`

Este helper:
- detecta `NODE_STATUS_GET`
- responde automáticamente `NODE_STATUS_GET_RESPONSE`
- incluye un payload mínimo con `health_state` (por default `HEALTHY`)
- devuelve `true` si manejó el mensaje

---

## 3) Qué hay que implementar (directivas)

### 3.1 Interceptar status en el loop principal

En el **loop principal** donde el runtime recibe mensajes del router (antes de cualquier dispatch de negocio):

1) Llamar al helper:

- `handled = try_handle_default_node_status(&sender, &incoming).await?;`

2) Si `handled == true`, **cortar** el procesamiento de ese mensaje (ya se respondió):

- `continue;` (o return en handler).

> Regla: el handler de status debe ejecutarse **antes** de cualquier otro match/route para evitar timeouts.

### 3.2 Configuración por entorno (opcional)

El helper está habilitado por defecto, pero para asegurar el comportamiento en deploy:

- `NODE_STATUS_DEFAULT_HANDLER_ENABLED=true`

Opcional (debug/override):

- `NODE_STATUS_DEFAULT_HEALTH_STATE=HEALTHY|DEGRADED|ERROR|UNKNOWN`

---

## 4) Respuesta esperada

El helper responde:

- `meta.msg = "NODE_STATUS_GET_RESPONSE"`
- payload mínimo:

```json
{
  "status": "ok",
  "health_state": "HEALTHY"
}
```

Esto es suficiente para que `SY.orchestrator` marque:

- `health_source = "NODE_REPORTED"`

---

## 5) Validación (QA)

Ejecutar:

```bash
curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE@$HIVE_ID/status" | jq '.payload.node_status.health_source'
```

Debe devolver:

- `NODE_REPORTED`

y no:

- `ORCHESTRATOR_INFERRED`

---

## 6) Nota para iteraciones futuras (no MVP)

Una vez que el runtime tenga chequeos de dependencias (provider, blobs, lancedb, etc.), se recomienda implementar un handler custom que:

- compute `health_state` real (`DEGRADED` por dependencia externa, etc.)
- incluya `last_error` y `extensions`

pero **para MVP** alcanza con el helper default.

---

## 7) Checklist de implementacion en este repo (AI runner actual)

Nota: el runtime AI actual no usa un loop directo con `NodeSender/NodeReceiver` como en el helper del SDK.
La implementacion equivalente se hace en `nodes/ai/ai-generic/src/bin/ai_node_runner.rs`,
interceptando `NODE_STATUS_GET` en el path de control-plane.

- [x] S1. Agregar manejo explicito de `meta.msg = NODE_STATUS_GET` en control-plane (antes de comandos de negocio).
- [x] S2. Responder `NODE_STATUS_GET_RESPONSE` con payload minimo:
  - `status = "ok"`
  - `health_state` (default `HEALTHY`, configurable por env).
- [x] S3. Implementar flags de entorno equivalentes:
  - `NODE_STATUS_DEFAULT_HANDLER_ENABLED` (default: true)
  - `NODE_STATUS_DEFAULT_HEALTH_STATE` (`HEALTHY|DEGRADED|ERROR|UNKNOWN`, default: `HEALTHY`).
- [x] S4. Asegurar correlacion de respuesta:
  - mismo `trace_id` del request
  - reply al `routing.src` original.
- [x] S5. Agregar tests del runner/SDK para:
  - `NODE_STATUS_GET` respondido correctamente,
  - handler deshabilitado por env,
  - valor invalido de health state cae en default `HEALTHY`.
- [x] S6. Actualizar documentacion operativa (como setear env vars y como validar `health_source=NODE_REPORTED`).

Orden recomendado:
1. S1-S2 (camino minimo funcional)
2. S3 (env contract)
3. S4-S5 (hardening + tests)
4. S6 (docs/runbook)

Nota de validacion local:
- En este entorno, `cargo check` global puede fallar por dependencia preexistente en `crates/fluxbee_sdk/src/identity.rs` (imports `std::os::fd`/`nix`), ajena a este cambio.

Referencia operativa implementada:
- `docs/onworking/node_runtime_install_ops.md` → sección `2.2.1 NODE_STATUS_GET default handler (MVP)`.
