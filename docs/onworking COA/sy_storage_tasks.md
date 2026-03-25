# SY.storage - Contrato canonico de subjects (Fase 1)

Fecha: 2026-02-16

Documento operativo para alinear productores/consumidor (`SY.storage`) en los subjects:
- `storage.turns`
- `storage.events`
- `storage.items`
- `storage.reactivation`

Ownership acordado (2026-02-19):
- `storage.turns`: `rt-gateway` (activo en codigo).
- `storage.events`: `SY.cognition` (objetivo; pipeline productor pendiente de cierre E2E).
- `storage.items`: `SY.cognition` (objetivo; pipeline productor pendiente de cierre E2E).
- `storage.reactivation`: `SY.cognition` (objetivo; pipeline productor pendiente de cierre E2E).

## 1) storage.turns

Productor actual: router (`rt-gateway`).
Formato esperado (mensaje router completo):

```json
{
  "routing": {
    "src": "AI.soporte@worker-1",
    "trace_id": "9a3a2d22-98a8-4a90-8ef3-ea8f0fca9f8b"
  },
  "meta": {
    "msg_type": "message",
    "ctx": "ctx:abc123",
    "ctx_seq": 42,
    "ich": "ich:support:phone",
    "src_ilk": "ilk:tenant:operator",
    "dst_ilk": "ilk:tenant:user",
    "tags": ["billing", "refund"]
  },
  "payload": {
    "text": "...",
    "tags": ["billing", "refund"]
  }
}
```

Campos requeridos para ingesta:
- `routing.trace_id`
- `meta.src_ilk` o `routing.src`

Campos con fallback:
- `meta.ctx` -> fallback `trace:<trace_id>`
- `meta.ctx_seq` -> fallback hash estable de `trace_id`
- `meta.msg_type` -> fallback `meta.type` -> fallback `unknown`

## 2) storage.events

Productor esperado: `SY.cognition`.

```json
{
  "event": {
    "event_id": 12345,
    "ctx": "ctx:abc123",
    "start_seq": 30,
    "end_seq": 42,
    "boundary_reason": "task_resolved",
    "cues_agg": ["billing", "refund"],
    "outcome_status": "resolved",
    "outcome_duration_ms": 180000,
    "activation_strength": 0.78,
    "context_inhibition": 0.05,
    "use_count": 3,
    "success_count": 2
  }
}
```

Campos requeridos:
- `ctx`
- `start_seq`
- `end_seq`
- `boundary_reason`
- `cues_agg` o `tags` (no vacio)

Reglas:
- `end_seq >= start_seq`

## 3) storage.items

Productor esperado: `SY.cognition`.

```json
{
  "item": {
    "memory_id": "mem-abc-001",
    "event_id": 12345,
    "item_type": "fact",
    "content": {
      "title": "Factura duplicada",
      "details": "..."
    },
    "confidence": 0.91,
    "cues_signature": ["billing", "invoice"],
    "activation_strength": 0.66
  }
}
```

Campos requeridos:
- `item_type`
- `content`
- `confidence`
- `cues_signature` o `tags` (no vacio)

Campos con fallback:
- `memory_id` -> hash estable del payload

## 4) storage.reactivation

Productor esperado: `SY.cognition`.

```json
{
  "outcome": {
    "status": "resolved"
  },
  "reactivated": {
    "events": [
      { "event_id": 12345, "used": true },
      { "event_id": 12001, "used": false }
    ]
  }
}
```

Campos requeridos:
- `reactivated.events` o `events`
- para cada entrada: `event_id`

Campos con fallback:
- `used` -> `false`
- `outcome.status` ausente -> no suma a `success_count`

## 5) Comportamiento de validacion en SY.storage

- Payload invalido:
  - no se persiste,
  - se loguea warning estructurado,
  - se continua procesando siguientes mensajes.
- Payload valido:
  - escritura idempotente en PostgreSQL.

## 6) Nota de rollout

Mientras se termina Fase 1, cualquier productor nuevo de `storage.events/items/reactivation` debe ajustarse a este contrato antes de habilitar trafico en produccion.
