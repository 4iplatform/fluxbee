# SY.wf-rules — Quick Start Guide

Guía práctica para definir, desplegar y operar workflows en Fluxbee.

**Prerequisito:** el runtime base `wf.engine` debe estar publicado en el hive.

```bash
# publicar wf.engine (una sola vez por instalación)
bash scripts/publish-wf-runtime.sh --version 0.1.0 --set-current --sudo
```

---

## Variables de entorno

```bash
export BASE="http://127.0.0.1:8080"
export HIVE="motherbee"
export TENANT_ID="tnt:REEMPLAZAR"
```

Todos los endpoints usan el prefijo `$BASE/hives/$HIVE/wf-rules`.

`TENANT_ID` se usa en el **first deploy** cuando `auto_spawn: true` y el nodo `WF.<workflow_name>@<hive>` todavía no existe. No se propaga por variable de entorno del servicio `sy.wf-rules`; debe viajar en el request.

---

## 1. Escribir la definición del workflow

Un workflow es un JSON con estados, transiciones, guardas CEL y acciones. El sistema valida todo al recibirlo — no hace falta compilar localmente.

### Ejemplo completo: `WF.invoice`

```json
{
  "wf_schema_version": "1",
  "workflow_type": "invoice",
  "description": "Issues an invoice, collects missing data if needed, delivers to customer.",
  "input_schema": {
    "type": "object",
    "required": ["customer_id", "items", "originator_channel"],
    "properties": {
      "customer_id": { "type": "string" },
      "items": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["sku", "qty", "unit_price"],
          "properties": {
            "sku": { "type": "string" },
            "qty": { "type": "integer" },
            "unit_price": { "type": "number" }
          }
        }
      },
      "currency": { "type": "string" },
      "originator_channel": { "type": "string" },
      "originator_ilk": { "type": "string" }
    }
  },
  "initial_state": "validating_data",
  "terminal_states": ["completed", "failed", "cancelled"],
  "states": [
    {
      "name": "validating_data",
      "description": "Asking AI.billing-validator if the customer data is complete.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing-validator@motherbee",
          "meta": { "msg": "VALIDATE_INVOICE_DATA" },
          "payload": {
            "customer_id": { "$ref": "input.customer_id" },
            "items": { "$ref": "input.items" },
            "currency": { "$ref": "input.currency" }
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "validation_timeout",
          "fire_in": "10m",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "validation_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA_RESPONSE" },
          "guard": "event.payload.complete == true",
          "target_state": "creating_invoice",
          "actions": [
            { "type": "set_variable", "name": "validated_at", "value": "now()" }
          ]
        },
        {
          "event_match": { "msg": "VALIDATE_INVOICE_DATA_RESPONSE" },
          "guard": "event.payload.complete == false",
          "target_state": "requesting_missing_data",
          "actions": [
            { "type": "set_variable", "name": "missing_fields", "value": "event.payload.missing" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'validation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'validator timeout'" }
          ]
        }
      ]
    },
    {
      "name": "requesting_missing_data",
      "description": "Asking the customer for missing fields.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing-responder@motherbee",
          "meta": { "msg": "REQUEST_MISSING_INVOICE_DATA" },
          "payload": {
            "originator_channel": { "$ref": "input.originator_channel" },
            "originator_ilk": { "$ref": "input.originator_ilk" },
            "missing_fields": { "$ref": "state.missing_fields" }
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "customer_response_timeout",
          "fire_in": "24h",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "customer_response_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "CUSTOMER_RESPONSE" },
          "guard": "has(event.payload.missing_data)",
          "target_state": "validating_data",
          "actions": [
            { "type": "set_variable", "name": "items", "value": "event.payload.missing_data.items" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'customer_response_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'customer did not respond'" }
          ]
        }
      ]
    },
    {
      "name": "creating_invoice",
      "description": "Invoking IO.quickbooks to create the invoice.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "IO.quickbooks@motherbee",
          "meta": { "msg": "INVOICE_CREATE_REQUEST" },
          "payload": {
            "customer_id": { "$ref": "input.customer_id" },
            "items": { "$ref": "input.items" },
            "currency": { "$ref": "input.currency" }
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "qb_creation_timeout",
          "fire_in": "5m",
          "missed_policy": "drop"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "qb_creation_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "INVOICE_CREATE_RESPONSE" },
          "guard": "event.payload.ok == true",
          "target_state": "delivering_to_customer",
          "actions": [
            { "type": "set_variable", "name": "invoice_id", "value": "event.payload.invoice_id" },
            { "type": "set_variable", "name": "invoice_link", "value": "event.payload.link" }
          ]
        },
        {
          "event_match": { "msg": "INVOICE_CREATE_RESPONSE" },
          "guard": "event.payload.ok == false",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'quickbooks error: ' + event.payload.error" }
          ]
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'qb_creation_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'quickbooks timeout'" }
          ]
        }
      ]
    },
    {
      "name": "delivering_to_customer",
      "description": "Asking AI.billing-responder to send the invoice to the customer.",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "AI.billing-responder@motherbee",
          "meta": { "msg": "DELIVER_INVOICE_TO_CUSTOMER" },
          "payload": {
            "originator_channel": { "$ref": "input.originator_channel" },
            "originator_ilk": { "$ref": "input.originator_ilk" },
            "invoice_id": { "$ref": "state.invoice_id" },
            "invoice_link": { "$ref": "state.invoice_link" }
          }
        },
        {
          "type": "schedule_timer",
          "timer_key": "delivery_timeout",
          "fire_in": "5m",
          "missed_policy": "fire"
        }
      ],
      "exit_actions": [
        { "type": "cancel_timer", "timer_key": "delivery_timeout" }
      ],
      "transitions": [
        {
          "event_match": { "msg": "DELIVER_INVOICE_RESPONSE" },
          "guard": "event.payload.delivered == true",
          "target_state": "completed",
          "actions": []
        },
        {
          "event_match": { "msg": "TIMER_FIRED" },
          "guard": "event.user_payload.timer_key == 'delivery_timeout'",
          "target_state": "failed",
          "actions": [
            { "type": "set_variable", "name": "failure_reason", "value": "'delivery timeout'" }
          ]
        }
      ]
    },
    {
      "name": "completed",
      "description": "Invoice issued and delivered.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "failed",
      "description": "Terminal failure. Check variable 'failure_reason'.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "cancelled",
      "description": "Cancelled by operator via WF_CANCEL_INSTANCE.",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    }
  ]
}
```

Guardar como `invoice-workflow.json` para los ejemplos de abajo.

---

## 2. Operaciones

### 2.1 Compile (validar sin desplegar)

Valida la definición, compila los guards CEL, guarda en staged. No despliega nada.

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/compile" \
  -H "Content-Type: application/json" \
  -d "$(jq -n --arg wf invoice --slurpfile def invoice-workflow.json \
    '{workflow_name: $wf, definition: $def[0]}')" | jq .
```

Respuesta esperada:
```json
{
  "ok": true,
  "state": "configured",
  "effective_config": {
    "staged": {
      "version": 1,
      "hash": "sha256:...",
      "guard_count": 8,
      "state_count": 7
    }
  }
}
```

Si hay errores de validación (guard CEL inválido, estado referenciado que no existe, etc.), el sistema devuelve el error con la ubicación exacta:

```json
{
  "ok": false,
  "error": {
    "code": "COMPILE_ERROR",
    "detail": "states[0].transitions[0].guard: undeclared reference to 'event.payload.complette'"
  }
}
```

### 2.2 Apply (promover staged y desplegar)

Promueve la definición validada de staged a current, genera el package, y spawna/reinicia el nodo WF.

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/apply" \
  -H "Content-Type: application/json" \
  -d "{\"workflow_name\":\"invoice\",\"tenant_id\":\"$TENANT_ID\",\"auto_spawn\":true}" | jq .
```

`auto_spawn: true` hace que el nodo `WF.invoice@motherbee` se cree automáticamente si no existe. Si ya existe, se reinicia con la nueva versión. En el primer caso, `tenant_id` es obligatorio.

Respuesta esperada:
```json
{
  "ok": true,
  "effective_config": {
    "current": {
      "version": 1,
      "hash": "sha256:..."
    }
  },
  "wf_node": {
    "node_name": "WF.invoice@motherbee",
    "action": "restarted",
    "status": "ok"
  }
}
```

### 2.3 Compile + Apply en un solo paso

Para iteración rápida: valida y despliega de una sola vez.

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules" \
  -H "Content-Type: application/json" \
  -d "$(jq -n --arg wf invoice --slurpfile def invoice-workflow.json \
    --arg tenant_id "$TENANT_ID" \
    '{workflow_name: $wf, definition: $def[0], auto_spawn: true, tenant_id: $tenant_id}')" | jq .
```

Este es el comando más usado durante desarrollo. Equivale a `compile` + `apply` en secuencia.

### 2.4 Consultar estado

**Ver la definición actual de un workflow:**

```bash
curl -sS "$BASE/hives/$HIVE/wf-rules?workflow_name=invoice" | jq .
```

**Ver el estado del workflow y su nodo:**

```bash
curl -sS "$BASE/hives/$HIVE/wf-rules/status?workflow_name=invoice" | jq .
```

Respuesta:
```json
{
  "status": "ok",
  "workflow_name": "invoice",
  "current_version": 2,
  "current_hash": "sha256:...",
  "wf_node": {
    "node_name": "WF.invoice@motherbee",
    "running": true,
    "active_instances": 3
  }
}
```

**Listar todos los workflows del hive:**

```bash
curl -sS "$BASE/hives/$HIVE/wf-rules" | jq .
```

### 2.5 Rollback

Restaura la versión anterior (backup → current) y reinicia el nodo.

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/rollback" \
  -H "Content-Type: application/json" \
  -d '{"workflow_name": "invoice"}' | jq .
```

El rollback solo tiene un nivel de profundidad (current ↔ backup). Si se necesita volver a una versión más vieja, hay que reenviar la definición con `compile_apply`.

### 2.6 Eliminar un workflow

**Eliminación segura (falla si hay instancias activas):**

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/delete" \
  -H "Content-Type: application/json" \
  -d '{"workflow_name": "invoice", "force": false}' | jq .
```

**Eliminación forzada (mata el nodo y borra todo, incluyendo instancias en vuelo):**

```bash
curl -sS -X POST "$BASE/hives/$HIVE/wf-rules/delete" \
  -H "Content-Type: application/json" \
  -d '{"workflow_name": "invoice", "force": true}' | jq .
```

---

## 3. Referencia rápida de CEL guards

Los guards son expresiones [CEL](https://github.com/google/cel-go) que deciden si una transición se toma. Se evalúan contra tres variables:

| Variable | Contenido |
|---|---|
| `input` | El payload del evento que creó la instancia (inmutable) |
| `state` | Variables del workflow, modificadas por `set_variable` |
| `event` | El mensaje que llegó ahora y disparó la evaluación |

Función built-in: `now()` retorna timestamp UTC en milisegundos (tiempo local del proceso).

### Patrones comunes

**Chequear un campo booleano en la respuesta:**
```
event.payload.ok == true
```

**Chequear que un campo existe:**
```
has(event.payload.missing_data)
```

**Matchear un timer específico:**
```
event.user_payload.timer_key == 'validation_timeout'
```

**Comparar strings:**
```
event.payload.status == 'approved'
```

**Chequear un valor numérico:**
```
state.retry_count < 3
```

**Combinar condiciones:**
```
event.payload.ok == true && state.retry_count < 5
```

**Chequear pertenencia:**
```
event.payload.status in ['approved', 'auto_approved']
```

---

## 4. Referencia rápida de acciones

Las acciones son los efectos que el workflow ejecuta. Vocabulario fijo en v1:

### `send_message` — enviar mensaje a otro nodo

```json
{
  "type": "send_message",
  "target": "IO.quickbooks@motherbee",
  "meta": { "msg": "INVOICE_CREATE_REQUEST" },
  "payload": {
    "customer_id": { "$ref": "input.customer_id" },
    "items": { "$ref": "state.validated_items" },
    "literal_field": "this is a literal string, not a reference"
  }
}
```

Los valores `{"$ref": "..."}` se resuelven al momento de ejecución. Las tres raíces válidas son `input`, `state`, `event`. Todo lo demás es literal.

### `schedule_timer` — programar un timer vía SY.timer

```json
{
  "type": "schedule_timer",
  "timer_key": "customer_timeout",
  "fire_in": "24h",
  "missed_policy": "fire"
}
```

Duración mínima: 60 segundos. Opciones de `missed_policy`: `fire` (dispara al recuperarse), `drop` (ignora), `fire_if_within` (dispara si el retraso está dentro de un margen).

### `cancel_timer` — cancelar un timer pendiente

```json
{ "type": "cancel_timer", "timer_key": "customer_timeout" }
```

### `reschedule_timer` — reprogramar un timer existente

```json
{ "type": "reschedule_timer", "timer_key": "sla_check", "fire_in": "30m" }
```

### `set_variable` — guardar un valor en el estado de la instancia

```json
{ "type": "set_variable", "name": "invoice_id", "value": "event.payload.invoice_id" }
```

El `value` es una expresión CEL. Ejemplos: `"now()"`, `"'pending'"`, `"state.retry_count + 1"`, `"event.payload.result"`.

---

## 5. Estructura de un workflow

```
workflow
├── wf_schema_version    "1"
├── workflow_type         nombre del workflow
├── description           texto libre
├── input_schema          JSON Schema del evento que crea instancias
├── initial_state         nombre del estado donde empiezan las instancias
├── terminal_states       lista de estados que terminan la instancia
└── states[]
    ├── name              nombre único del estado
    ├── description       texto libre
    ├── entry_actions[]   acciones al ENTRAR al estado
    ├── exit_actions[]    acciones al SALIR del estado
    └── transitions[]     reglas de transición
        ├── event_match   { "msg": "NOMBRE_DEL_MENSAJE" }
        ├── guard         expresión CEL → true/false
        ├── target_state  nombre del estado destino
        └── actions[]     acciones al tomar esta transición
```

Orden de ejecución en una transición:
```
exit_actions del estado actual
  → actions de la transición
    → entry_actions del estado destino
```

Las transiciones se evalúan en orden de declaración. La primera cuyo `event_match` coincida y cuyo `guard` devuelva `true` es la que se toma.

---

## 6. Ciclo de desarrollo típico

```
1. Escribir/editar el JSON del workflow

2. Validar:
   curl -X POST .../wf-rules/compile -d '{workflow_name, definition}'
   → corregir errores de CEL o de estructura

3. Desplegar:
   curl -X POST .../wf-rules/apply -d '{workflow_name, tenant_id, auto_spawn: true}'
   → el nodo WF arranca

4. Probar enviando mensajes al nodo WF

5. Iterar: editar JSON → compile_apply (paso 2+3 juntos)
   curl -X POST .../wf-rules -d '{workflow_name, definition, tenant_id, auto_spawn: true}'

6. Si algo sale mal: rollback
   curl -X POST .../wf-rules/rollback -d '{workflow_name}'

7. Cuando termines de testear: delete
   curl -X POST .../wf-rules/delete -d '{workflow_name, force: true}'
```

---

## 7. Endpoints (resumen)

| Método | Path | Acción |
|---|---|---|
| `POST` | `/hives/{hive}/wf-rules/compile` | Validar y stage |
| `POST` | `/hives/{hive}/wf-rules/apply` | Promover staged → current, restart/spawn |
| `POST` | `/hives/{hive}/wf-rules` | Compile + apply en un paso |
| `POST` | `/hives/{hive}/wf-rules/rollback` | Restaurar versión anterior |
| `POST` | `/hives/{hive}/wf-rules/delete` | Eliminar workflow |
| `GET` | `/hives/{hive}/wf-rules?workflow_name=X` | Ver definición actual |
| `GET` | `/hives/{hive}/wf-rules/status?workflow_name=X` | Ver estado + info del nodo |
| `GET` | `/hives/{hive}/wf-rules` | Listar todos los workflows |

---

## 8. Notas operativas

**Instancias en vuelo al actualizar:** cuando se aplica una nueva versión, las instancias que ya estaban corriendo siguen usando la definición con la que nacieron. Solo las instancias nuevas usan la definición actualizada. Esto es transparente — no hay que hacer nada especial.

**Timers:** todos los timers del workflow pasan por `SY.timer`. Duración mínima 60 segundos. Si el nodo WF reinicia, recupera sus timers pendientes automáticamente.

**Idempotencia downstream:** los nodos que reciben mensajes de un WF (AI, IO) deben ser idempotentes respecto a `meta.trace_id`. En caso de crash y recovery del WF, las acciones pueden re-ejecutarse.

**Correlación de respuestas:** cuando el WF envía un `send_message`, pone `meta.thread_id = instance_id`. Los nodos que responden deben copiar ese `thread_id` en su respuesta para que el WF la correlacione con la instancia correcta.

**Cancelar una instancia en vuelo:** se hace directamente al nodo WF, no a sy.wf-rules:
```bash
# enviar WF_CANCEL_INSTANCE al nodo WF directamente vía admin
curl -sS -X POST "$BASE/hives/$HIVE/nodes/WF.invoice@$HIVE/command" \
  -H "Content-Type: application/json" \
  -d '{"action": "cancel_instance", "instance_id": "wfi:..."}'
```

**Ver instancias activas:**
```bash
curl -sS "$BASE/hives/$HIVE/wf-rules/status?workflow_name=invoice" | jq .wf_node
```
