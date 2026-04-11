# SY.timer — Nodo del Sistema

**Estado:** v1.0 implemented / aligned with current runtime
**Fecha:** 2026-04-08
**Audiencia:** Desarrolladores de nodos, autores de workflows, ops/SRE

---

## 1. Resumen y Posicionamiento

`SY.timer` es un nodo del sistema Fluxbee que provee dos capacidades al resto del hive:

1. **Timers persistentes** — one-shot y recurrentes, con disparo por evento L2 hacia un nodo target.
2. **Fuente autoritativa de tiempo y timezone** — `now`, conversiones, parse/format, resolución server-side de timezones IANA.

Forma parte de la familia de nodos del sistema, junto con `SY.storage`, `SY.admin`, `SY.architect`, `SY.orchestrator`, `SY.config.routes`, `SY.identity`, `SY.cognition`, `SY.opa.rules`. Se lanza siempre al iniciar el hive, es mantenido por systemd a través del orchestrator, y no requiere configuración por parte del operador más allá de su existencia.

**Uno por hive.** Existe una instancia en motherbee y una en cada worker. Cada instancia es autónoma, no se comunica con otras instancias de `SY.timer`, y delega la sincronización horaria al sistema operativo (NTP/chrony/systemd-timesyncd). La consistencia temporal entre hives es responsabilidad del operador a nivel de host, no de Fluxbee.

**No es un sistema de tiempo real.** `SY.timer` está diseñado para workflows, cognición, agentes y automatización. La granularidad mínima de un timer es **60 segundos**, la precisión de disparo es *best-effort* con tolerancia del orden de segundos, y la carga esperada es de hasta ~10.000 timers concurrentes por hive.

**Ownership estricto por nombre L2 para mutaciones.** Cada nodo puede crear, cancelar y reschedulear sus propios timers. La lectura (`TIMER_GET`, `TIMER_LIST`) es abierta dentro del hive. La única excepción de mutación administrativa es `SY.orchestrator`, que puede invocar `TIMER_PURGE_OWNER` como parte del teardown de un nodo. La resolución `routing.src (UUID) -> nombre L2` se realiza a través del runtime/SDK Go leyendo la SHM del router local al que el nodo quedó efectivamente conectado según el `ANNOUNCE`, no por heurística local del nodo ni por archivos `.uuid`.

---

## 2. Principios de Diseño

- **Autonomía por hive.** Un `SY.timer` por hive, sin sincronización cruzada. Si un host cae, solo se ven afectados los timers locales a ese hive.
- **Delegación del tiempo al SO.** `SY.timer` no implementa NTP ni sincronización externa. Lee el reloj del SO y confía en que el operador lo mantiene sincronizado.
- **Fuente única de verdad temporal.** Cualquier nodo que necesite `now()` consistente a nivel hive lo pide a `SY.timer`. No hay cache local, no hay offsets locales: roundtrip por cada llamada, con reintento acotado en el SDK.
- **Mutación con ownership estricto.** Crear/cancelar/reschedulear mantiene aislamiento por owner L2. La lectura es deliberadamente abierta para observabilidad del hive.
- **Excepción única y explícita.** `SY.orchestrator` puede purgar todos los timers de un owner al borrar un nodo. Es la única vía por la que un timer puede ser tocado por alguien que no sea su creador.
- **JSON descriptivo y extensible.** Los nombres de campos son auto-explicativos, los enums son strings legibles, los errores devuelven mensajes humanos además de códigos. Todos los requests aceptan un campo `metadata` libre que `SY.timer` persiste sin interpretar, pensado para que clientes LLM puedan anotar contexto extra (razones, correlaciones, recordatorios humanos) sin romper el schema.
- **SDK como frontera de abstracción.** Los autores de nodos cliente nunca arman JSON a mano ni resuelven nombres L2. Llaman funciones tipadas del SDK y reciben tipos nativos (`time.Time`, structs). Si cambia el wire format, cambia el SDK, no los nodos.
- **Determinismo y replayability.** `now()` inyectado vía `SY.timer` permite que nodos como `WF` puedan hacer replay determinístico de su ejecución inyectando respuestas fake en tests. Por eso es crítico que los nodos no usen `time.Now()` del lenguaje para lógica de negocio, sino `SY.timer.Now()`.

---

## 3. Principio del Sistema: Self-Description (`HELP`)

Fluxbee adopta como principio de diseño que **todo nodo debe poder describir sus propias capacidades de forma autodescriptiva**, accesible mediante una operación `HELP` estándar.

El propósito es doble:

1. **Descubrimiento por LLM.** Un agente cognitivo puede preguntarle a un nodo "¿qué sabés hacer?" sin depender de documentación externa.
2. **Introspección humana.** Un desarrollador o operador puede interrogar un nodo en vivo para ver qué operaciones expone, qué formato de request aceptan, y qué errores pueden devolver.

`SY.timer` es el **primer nodo en implementar este principio** de forma puntual, dado que es el primer nodo del sistema con interacción directa desde otros nodos sin pasar por `SY.admin`. La operación se llama `TIMER_HELP` por consistencia con el resto de su familia de verbos, pero cumple el contrato general de `HELP`.

### 1.1 Visibilidad mínima vía `SY.admin` en v1

En v1, `SY.admin` expone solo una superficie **read-only** y de introspección para `SY.timer`:

- `timer_help`
- `timer_get`
- `timer_list`
- `timer_now`
- `timer_now_in`
- `timer_convert`
- `timer_parse`
- `timer_format`

Las operaciones owner-bound de mutación siguen fuera de `SY.admin` y se mantienen como uso directo vía SDK:

- `TIMER_SCHEDULE`
- `TIMER_SCHEDULE_RECURRING`
- `TIMER_CANCEL`
- `TIMER_RESCHEDULE`

`TIMER_GET` y `TIMER_LIST` sí pueden exponerse por `SY.admin` porque la lectura es abierta en v1. La razón para mantener scheduling y mutaciones fuera de admin es arquitectónica: `SY.timer` autoriza esas operaciones por identidad efectiva del requester (`routing.src` resuelto a L2), y `SY.admin` no actúa como proxy transparente de ownership.

Nodos futuros con interacción directa (por ejemplo `WF.*`, `SY.policy`, capabilities invocables) deben implementar su propia operación `HELP` equivalente. La generalización del protocolo, los campos comunes y el naming se formalizarán en un documento aparte cuando aparezca el segundo o tercer caso.

---

## 4. Arquitectura Interna

### 4.1 Stack

- **Lenguaje:** Go
- **Persistencia:** SQLite embebido (`modernc.org/sqlite` o `mattn/go-sqlite3`)
- **Timezone data:** `time/tzdata` embebido en binario, con fallback a `/usr/share/zoneinfo` del SO
- **Cron parser:** `github.com/robfig/cron/v3` (spec estándar, conocida por LLMs)
- **Scheduler interno:** heap binario ordenado por `fire_at_utc`, protegido por mutex
- **Runtime:** SDK Go estándar de Fluxbee (`fluxbee-go-sdk`), mismo loop de lectura de socket que el resto de nodos `SY.*`

### 4.2 Componentes

```
┌─────────────────────────────────────────────────────────┐
│                       SY.timer                         │
│                                                         │
│  ┌───────────────┐   ┌────────────────────────────┐   │
│  │ L2 Dispatcher │   │      Timer Scheduler       │   │
│  │               │   │                            │   │
│  │ - recibe msgs │   │  ┌──────────────────────┐ │   │
│  │ - routea por  │   │  │ Heap in-memory       │ │   │
│  │   meta.msg    │◄──┤  │ ordenado por fire_at │ │   │
│  │ - invoca      │   │  └──────────────────────┘ │   │
│  │   handler     │   │                            │   │
│  └───────┬───────┘   │  ┌──────────────────────┐ │   │
│          │           │  │ Tick loop            │ │   │
│          │           │  │ - sleep hasta next   │ │   │
│          │           │  │ - fire → publish L2  │ │   │
│          │           │  │ - lock por timer     │ │   │
│          │           │  └──────────────────────┘ │   │
│          │           └────────────┬───────────────┘   │
│          │                        │                    │
│          ▼                        ▼                    │
│  ┌─────────────────────────────────────────────────┐  │
│  │              SQLite (timers.db)                 │  │
│  │  tabla: timers                                  │  │
│  │  índices: owner, fire_at_utc, status            │  │
│  └─────────────────────────────────────────────────┘  │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

### 4.3 Loop de scheduling

```
1. Al bootear:
   - Abrir SQLite en /var/lib/fluxbee/nodes/SY/SY.timer@<hive>/timers.db
   - Leer TODOS los timers con status = 'pending'
   - Para cada uno:
       * Si fire_at_utc < now():
           - Aplicar missed_policy (fire | drop | fire_if_within)
       * Si fire_at_utc >= now():
           - Insertar en heap in-memory
   - Recomputar próxima instancia de timers recurrentes desde now()

2. Loop de tick:
   loop {
       next = heap.peek()
       if next == nil:
           wait for new timer or shutdown signal
           continue
       sleep_until(next.fire_at_utc)
       for timer in heap.pop_all_due(now()):
           with lock(timer.uuid):
               if timer.status != 'pending':
                   continue  // cancelado o reprogramado
               publish TIMER_FIRED to timer.target
               if timer.is_recurring:
                   compute next fire_at from cron_spec
                   update db, push back to heap
               else:
                   update db: status = 'fired'
   }
```

### 4.4 Concurrencia

- Un mutex global por la estructura del heap.
- Un mutex por timer (sharded map de mutexes por uuid) para serializar el corner case de "reschedule llegando mientras el disparo está en vuelo".
- Operaciones de SQLite serializadas por el driver (SQLite en modo WAL permite múltiples readers + un writer).

---

## 5. Modelo de Datos

### 5.1 Path del archivo

```
/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/timers.db
```

Junto con el resto del estado persistente del nodo (`config.json`, `status_version`, etc.), siguiendo la convención canónica de directorios por nodo del sistema.

### 5.2 Schema

```sql
CREATE TABLE timers (
    uuid                TEXT PRIMARY KEY,
    owner_l2_name       TEXT NOT NULL,
    target_l2_name      TEXT NOT NULL,
    client_ref          TEXT,
    kind                TEXT NOT NULL CHECK(kind IN ('oneshot','recurring')),

    fire_at_utc         INTEGER NOT NULL,  -- unix ms, próximo disparo
    cron_spec           TEXT,              -- solo si kind='recurring'
    cron_tz             TEXT,              -- timezone IANA del cron recurrente

    missed_policy       TEXT NOT NULL DEFAULT 'fire'
                        CHECK(missed_policy IN ('fire','drop','fire_if_within')),
    missed_within_ms    INTEGER,           -- solo si missed_policy='fire_if_within'

    payload             TEXT NOT NULL,     -- JSON blob, opaco para SY.timer
    metadata            TEXT,              -- JSON blob libre, opaco

    status              TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending','fired','canceled')),

    created_at_utc      INTEGER NOT NULL,  -- unix ms, del reloj del SO; preservado aun si se reschedulea
    last_fired_at_utc   INTEGER,           -- último disparo (recurrentes)
    fire_count          INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_timers_owner ON timers(owner_l2_name);
CREATE INDEX idx_timers_fire_at ON timers(fire_at_utc) WHERE status='pending';
CREATE INDEX idx_timers_status ON timers(status);
```

**Notas:**
- `created_at_utc` se preserva para siempre, incluso tras reschedules. Sirve para trazabilidad y para decisiones de `fire_if_within` basadas en antigüedad del timer original.
- `cron_tz` se persiste explícitamente para que el próximo disparo recurrente pueda recomputarse sin perder la timezone de origen.
- Los timers `fired` (one-shot completados) pueden ser purgados por un GC periódico tras N días (configurable, default 7 días) para evitar que la base crezca indefinidamente.

---

## 6. Transporte

### 6.1 Envelope

Todos los mensajes siguen el envelope estándar de Fluxbee:

```json
{
  "routing": {
    "src": "<uuid-del-nodo-cliente>",
    "dst": "SY.timer@<hive>",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "msg_type": "system",
    "msg": "TIMER_SCHEDULE"
  },
  "payload": { ... }
}
```

- Destino: siempre L2 unicast a `SY.timer@<hive-local>`. El SDK resuelve el nombre automáticamente.
- `meta.msg_type`: siempre `"system"`.
- `meta.msg`: uno de los verbos definidos en la sección 7.
- Respuesta: siempre `meta.msg = "TIMER_RESPONSE"`, con el mismo `trace_id` del request.

### 6.2 Verbos

| Verbo | Propósito |
|---|---|
| `TIMER_SCHEDULE` | Crear timer one-shot |
| `TIMER_SCHEDULE_RECURRING` | Crear timer recurrente (cron) |
| `TIMER_CANCEL` | Cancelar timer propio |
| `TIMER_RESCHEDULE` | Modificar `fire_at` de timer propio pendiente |
| `TIMER_GET` | Obtener un timer por uuid |
| `TIMER_LIST` | Listar timers del hive, opcionalmente filtrados por owner |
| `TIMER_NOW` | Obtener instante actual en UTC |
| `TIMER_NOW_IN` | Obtener instante actual en timezone especificada |
| `TIMER_CONVERT` | Convertir instante entre timezones |
| `TIMER_PARSE` | Parsear string a instante con timezone |
| `TIMER_FORMAT` | Formatear instante a string con timezone |
| `TIMER_PURGE_OWNER` | **Solo SY.orchestrator.** Borrar todos los timers de un owner |
| `TIMER_HELP` | Introspección de capabilities |
| `TIMER_RESPONSE` | Respuesta genérica a todos los requests |
| `TIMER_FIRED` | Evento emitido por `SY.timer` al target cuando vence un timer |

---

## 7. Operaciones de Timers

### 7.1 `TIMER_SCHEDULE`

Crea un timer one-shot.

**Request:**

```json
{
  "routing": { "dst": "SY.timer@<hive>", "ttl": 16, "trace_id": "..." },
  "meta": { "msg_type": "system", "msg": "TIMER_SCHEDULE" },
  "payload": {
    "fire_at_utc_ms": 1775577600000,
    "target_l2_name": "WF.onboarding.step2@motherbee",
    "client_ref": "escalation_for_ticket_1234",
    "payload": {
      "ticket_id": 1234,
      "reason": "no_reply_timeout"
    },
    "missed_policy": "fire_if_within",
    "missed_within_ms": 600000,
    "metadata": {
      "created_by_reasoning": "user asked to remind in 10 min"
    }
  }
}
```

**Campos:**

| Campo | Obligatorio | Descripción |
|---|---|---|
| `fire_at_utc_ms` | sí* | Instante absoluto UTC en ms. Alternativa a `fire_in_ms`. |
| `fire_in_ms` | sí* | Duración relativa desde `now()`. Alternativa a `fire_at_utc_ms`. |
| `target_l2_name` | no | Nombre L2 del nodo a notificar. Default: el propio requester, resuelto server-side desde `routing.src` mediante el runtime/SDK. |
| `client_ref` | no | Etiqueta amigable definida por el cliente. |
| `payload` | no | JSON arbitrario que se incluirá en `TIMER_FIRED.payload.user_payload`. |
| `missed_policy` | no | `"fire"` (default), `"drop"`, o `"fire_if_within"`. |
| `missed_within_ms` | condicional | Obligatorio si `missed_policy="fire_if_within"`. |
| `metadata` | no | JSON libre, opaco para `SY.timer`. |

*Exactamente uno de `fire_at_utc_ms` / `fire_in_ms`.

**Validación:**

- `fire_in_ms >= 60000` o equivalentemente `fire_at_utc_ms - now() >= 60000`. Si falla: error `TIMER_BELOW_MINIMUM`.
- `target_l2_name` debe ser un nombre L2 sintácticamente válido.
- `cron_spec` no debe aparecer (es `TIMER_SCHEDULE_RECURRING`).

**Response OK:**

```json
{
  "meta": { "msg_type": "system", "msg": "TIMER_RESPONSE" },
  "payload": {
    "ok": true,
    "verb": "TIMER_SCHEDULE",
    "timer_uuid": "9d8c6f4b-2f4d-4ad4-b5f4-8d8d9d9d0001",
    "fire_at_utc_ms": 1775577600000
  }
}
```

### 7.2 `TIMER_SCHEDULE_RECURRING`

Crea un timer recurrente con sintaxis cron estándar.

**Request:**

```json
{
  "meta": { "msg_type": "system", "msg": "TIMER_SCHEDULE_RECURRING" },
  "payload": {
    "cron_spec": "0 9 * * MON",
    "cron_tz": "America/Argentina/Buenos_Aires",
    "target_l2_name": "AI.weekly_reporter@motherbee",
    "client_ref": "weekly_report_monday_9am",
    "payload": { "report_type": "weekly_summary" },
    "missed_policy": "drop",
    "metadata": { "note": "lunes 9am hora de BA" }
  }
}
```

**Campos adicionales:**

| Campo | Obligatorio | Descripción |
|---|---|---|
| `cron_spec` | sí | Expresión cron estándar (5 campos: min hr dom mon dow). |
| `cron_tz` | no | Timezone IANA para interpretar el cron. Default: `UTC`. |

**Validación:**
- `cron_spec` debe parsear correctamente. Error `TIMER_INVALID_CRON`.
- Intervalo entre disparos debe ser `>= 60s`. Error `TIMER_BELOW_MINIMUM`.
- `cron_tz` debe ser una zona IANA válida. Error `TIMER_INVALID_TIMEZONE`.

**Response OK:** igual que `TIMER_SCHEDULE`, con el `fire_at_utc_ms` del **próximo** disparo computado.

### 7.3 `TIMER_CANCEL`

```json
{
  "meta": { "msg": "TIMER_CANCEL" },
  "payload": { "timer_uuid": "9d8c6f4b-..." }
}
```

Validación: el `owner_l2_name` del timer debe coincidir con el nombre L2 resuelto desde `routing.src`. Si no: `TIMER_NOT_OWNER`.

Si el timer ya fue disparado (`status = 'fired'`): `TIMER_ALREADY_FIRED`. Si no existe: `TIMER_NOT_FOUND`.

**Response OK:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_CANCEL",
    "timer_uuid": "9d8c6f4b-...",
    "status": "canceled"
  }
}
```

### 7.4 `TIMER_RESCHEDULE`

Modifica el `fire_at` de un timer one-shot pendiente.

```json
{
  "meta": { "msg": "TIMER_RESCHEDULE" },
  "payload": {
    "timer_uuid": "9d8c6f4b-...",
    "new_fire_at_utc_ms": 1775580000000
  }
}
```

Alternativamente, `new_fire_in_ms` relativo a `now()`.

Validación: mismo ownership check. Solo aplica a timers en `status='pending'`. Para timers recurrentes no se permite reschedule en v1 (cancelar y recrear). Error `TIMER_RECURRING_NOT_RESCHEDULABLE`.

El mínimo de 60s se aplica al `new_fire_at` también.

### 7.5 `TIMER_GET`

```json
{
  "meta": { "msg": "TIMER_GET" },
  "payload": { "timer_uuid": "9d8c6f4b-..." }
}
```

**Response OK:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_GET",
    "timer": {
      "uuid": "9d8c6f4b-...",
      "owner_l2_name": "WF.onboarding@motherbee",
      "target_l2_name": "WF.onboarding@motherbee",
      "client_ref": "escalation_for_ticket_1234",
      "kind": "oneshot",
      "fire_at_utc_ms": 1775577600000,
      "cron_spec": null,
      "cron_tz": null,
      "missed_policy": "fire_if_within",
      "missed_within_ms": 600000,
      "status": "pending",
      "created_at_utc_ms": 1775574000000,
      "fire_count": 0,
      "last_fired_at_utc_ms": null,
      "payload": { "ticket_id": 1234 },
      "metadata": { "created_by_reasoning": "..." }
    }
  }
}
```

Lectura abierta dentro del hive. No requiere ownership del requester.

### 7.6 `TIMER_LIST`

Lista timers del hive local. Sin parámetros = todos los `pending`. Puede filtrarse por `owner_l2_name`.

```json
{
  "meta": { "msg": "TIMER_LIST" },
  "payload": {
    "owner_l2_name": "WF.onboarding@motherbee",
    "status_filter": "pending",
    "limit": 100
  }
}
```

| Campo | Descripción |
|---|---|
| `owner_l2_name` | Filtro opcional por owner canónico L2. |
| `status_filter` | `"pending"`, `"fired"`, `"canceled"`, o `"all"` (default `"pending"`). |
| `limit` | Máximo de resultados. Default 100, máximo 1000. |

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_LIST",
    "count": 3,
    "timers": [ { ... }, { ... }, { ... } ]
  }
}
```

`owner_l2_name` es opcional. Si se omite, `SY.timer` lista el conjunto completo del hive sujeto a `status_filter` y `limit`.

### 7.7 `TIMER_PURGE_OWNER` (uso exclusivo de `SY.orchestrator`)

Borra **todos los timers** de un `owner_l2_name` dado.

```json
{
  "meta": { "msg": "TIMER_PURGE_OWNER" },
  "payload": {
    "owner_l2_name": "WF.onboarding.step2@motherbee"
  }
}
```

**Autorización:** `SY.timer` valida que `routing.src` corresponda al nombre L2 `SY.orchestrator@<hive-local>`. Cualquier otro origen: error `TIMER_FORBIDDEN`.

**Semántica:**
- Borrado idempotente: si no hay timers, responde `ok` con `deleted_count: 0`.
- Borra tanto `pending` como históricos (`fired`, `canceled`) — consistente con el espíritu de "el nodo ya no existe".
- Silenciosa: no emite `TIMER_FIRED` para los timers pendientes borrados, se consideran cancelados.

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_PURGE_OWNER",
    "owner_l2_name": "WF.onboarding.step2@motherbee",
    "deleted_count": 7
  }
}
```

### 7.8 Evento `TIMER_FIRED`

Emitido por `SY.timer` hacia el `target_l2_name` cuando un timer vence.

```json
{
  "routing": {
    "src": "SY.timer@<hive>",
    "dst": "WF.onboarding.step2@motherbee",
    "ttl": 16,
    "trace_id": "<nuevo-uuid>"
  },
  "meta": {
    "msg_type": "system",
    "msg": "TIMER_FIRED"
  },
  "payload": {
    "timer_uuid": "9d8c6f4b-...",
    "owner_l2_name": "WF.onboarding.step2@motherbee",
    "client_ref": "escalation_for_ticket_1234",
    "kind": "oneshot",
    "scheduled_fire_at_utc_ms": 1775577600000,
    "actual_fire_at_utc_ms": 1775577600123,
    "fire_count": 1,
    "is_last_fire": true,
    "user_payload": {
      "ticket_id": 1234,
      "reason": "no_reply_timeout"
    },
    "metadata": {
      "created_by_reasoning": "..."
    }
  }
}
```

**Notas de diseño:**
- `trace_id` es nuevo, no reutiliza el del `TIMER_SCHEDULE` original. Esto permite trazar el disparo como un evento independiente.
- `scheduled_fire_at_utc_ms` vs `actual_fire_at_utc_ms` permite al consumidor medir el jitter real.
- En v1, `actual_fire_at_utc_ms` se captura inmediatamente antes del intento de envío del evento, no después de confirmar una escritura exitosa del socket.
- `is_last_fire` para recurrentes que tienen algún criterio de terminación (v1 siempre `false` para recurrentes; reservado para v2).
- `user_payload` es exactamente lo que el cliente pasó en el `TIMER_SCHEDULE.payload.payload`, sin modificar.
- `metadata` también se devuelve tal cual, para que el consumidor pueda tener el contexto que el creador dejó.
- El target NO debe responder con `TIMER_RESPONSE`. `TIMER_FIRED` es fire-and-forget; si el target está caído, el evento se pierde. Si hace falta retry, es responsabilidad del creador del timer diseñar esa semántica en su lógica de aplicación.
- `SY.timer` considera el disparo intentado cuando entrega el mensaje al router local; no espera ack aplicativo ni confirmación del target.
- En v1 no existe dead-letter queue ni retry automático de `TIMER_FIRED`. La durabilidad sigue viviendo en la fila del timer y en los logs del nodo.

---

## 8. Operaciones de Tiempo

### 8.1 `TIMER_NOW`

```json
{ "meta": { "msg": "TIMER_NOW" }, "payload": {} }
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_NOW",
    "now_utc_ms": 1775577600123,
    "now_utc_iso": "2026-04-08T15:20:00.123Z"
  }
}
```

### 8.2 `TIMER_NOW_IN`

```json
{
  "meta": { "msg": "TIMER_NOW_IN" },
  "payload": { "tz": "America/Argentina/Buenos_Aires" }
}
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_NOW_IN",
    "now_utc_ms": 1775577600123,
    "tz": "America/Argentina/Buenos_Aires",
    "now_local_iso": "2026-04-08T12:20:00.123-03:00",
    "tz_offset_seconds": -10800,
    "tz_abbrev": "-03"
  }
}
```

### 8.3 `TIMER_CONVERT`

Convierte un instante a la representación local de un timezone.

```json
{
  "meta": { "msg": "TIMER_CONVERT" },
  "payload": {
    "instant_utc_ms": 1775577600000,
    "to_tz": "Europe/Madrid"
  }
}
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_CONVERT",
    "instant_utc_ms": 1775577600000,
    "to_tz": "Europe/Madrid",
    "local_iso": "2026-04-08T17:20:00+02:00",
    "tz_offset_seconds": 7200
  }
}
```

### 8.4 `TIMER_PARSE`

```json
{
  "meta": { "msg": "TIMER_PARSE" },
  "payload": {
    "input": "2026-04-08 17:20:00",
    "layout": "2006-01-02 15:04:05",
    "tz": "Europe/Madrid"
  }
}
```

Usa el formato de layout de Go (`2006-01-02 15:04:05`). El SDK puede proveer helpers con layouts conocidos (ISO8601, RFC3339).

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_PARSE",
    "instant_utc_ms": 1775575200000,
    "resolved_iso_utc": "2026-04-08T15:20:00Z"
  }
}
```

### 8.5 `TIMER_FORMAT`

```json
{
  "meta": { "msg": "TIMER_FORMAT" },
  "payload": {
    "instant_utc_ms": 1775577600000,
    "layout": "Mon 02 Jan 2006 15:04 MST",
    "tz": "America/Argentina/Buenos_Aires"
  }
}
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_FORMAT",
    "formatted": "Wed 08 Apr 2026 12:20 -03"
  }
}
```

### 8.6 Nota sobre costo de roundtrip

Todas las operaciones de tiempo hacen roundtrip L2 a `SY.timer`. El SDK implementa reintento con backoff (3 intentos, 100ms/300ms/1s) y falla ruidosamente si no obtiene respuesta. **No hay cache local ni sincronización de offset.** Si un nodo necesita medir duraciones finas, debe usar el reloj monotónico local del proceso, no `TIMER_NOW`. La regla: *tiempo del mundo real autoritativo vía `SY.timer`, mediciones monotónicas locales con el reloj del proceso*.

---

## 9. `TIMER_HELP`

Devuelve un descriptor completo de las capabilities del nodo, pensado para consumo tanto humano como LLM.

```json
{ "meta": { "msg": "TIMER_HELP" }, "payload": {} }
```

**Response:**
```json
{
  "payload": {
    "ok": true,
    "verb": "TIMER_HELP",
    "node_family": "SY",
    "node_kind": "SY.timer",
    "version": "1.0",
    "description": "System node providing persistent timers and authoritative time/timezone services.",
    "operations": [
      {
        "verb": "TIMER_SCHEDULE",
        "description": "Create a one-shot timer that fires at a specific time.",
        "required_fields": ["fire_at_utc_ms OR fire_in_ms"],
        "optional_fields": ["target_l2_name", "client_ref", "payload", "missed_policy", "missed_within_ms", "metadata"],
        "constraints": ["minimum duration: 60000 ms"],
        "example_request": { "...": "..." },
        "example_response": { "...": "..." }
      },
      { "...": "..." }
    ],
    "error_codes": [
      { "code": "TIMER_BELOW_MINIMUM", "description": "Requested duration is below the minimum of 60 seconds." },
      { "...": "..." }
    ]
  }
}
```

El contenido completo del `TIMER_HELP` se genera a partir de los descriptores internos del nodo y se mantiene sincronizado con la implementación real (no hay riesgo de documentación desfasada).

---

## 10. Semántica de Missed Policies

Cuando `SY.timer` arranca tras un downtime, evalúa cada timer pendiente contra `now()`:

| Política | Timer vencido | Timer vigente |
|---|---|---|
| `fire` | Se dispara inmediatamente en orden. | Se reprograma con `fire_at` original. |
| `drop` | Se descarta, loggeo a nivel `warn`. | Se reprograma normalmente. |
| `fire_if_within` | Si `now() - fire_at_utc <= missed_within_ms`, se dispara. Si no, se descarta. | Se reprograma normalmente. |

Para **timers recurrentes**, el comportamiento al restart es:
- Se computa la **próxima instancia** desde `now()` hacia adelante según `cron_spec`.
- Las instancias que deberían haberse disparado durante el downtime **no se disparan retroactivamente**, independientemente de la `missed_policy`. Esto evita tormentas de disparos al volver de downtimes largos.
- La `missed_policy` para recurrentes se interpreta solo sobre la **instancia actual** (la que estaba próxima a dispararse al momento del bajón).

---

## 11. Semántica de Restart — Detalle

Al arranque del nodo:

1. Abrir `timers.db`. Si no existe, crear schema.
2. `now = time.Now().UTC().UnixMilli()`.
3. `SELECT * FROM timers WHERE status='pending' ORDER BY fire_at_utc ASC`.
4. Para cada timer:
   - Si `fire_at_utc <= now`:
     - Aplicar `missed_policy`.
     - Si se dispara: publicar `TIMER_FIRED`, actualizar `status` y `fire_count` en DB, y si es recurrente computar próxima instancia.
     - Si se descarta: loggear, actualizar `status='canceled'` con razón `missed_dropped` en logs (no es un campo del schema).
   - Si `fire_at_utc > now`:
     - Insertar en heap in-memory con el mismo `fire_at_utc`. **No se modifica el `fire_at_utc`** — ya es el instante absoluto objetivo, y el heap lo dispara cuando el reloj del SO llegue a ese valor.
5. Para timers recurrentes, recomputar `fire_at_utc` desde `now()` con `cron_spec` y `cron_tz`, update DB, push a heap.
6. Ejecutar GC opcional: `DELETE FROM timers WHERE status IN ('fired','canceled') AND last_fired_at_utc < now - retention_ms`.
7. Iniciar loop de tick.

**Garantía:** un timer creado con `fire_at_utc = T` disparará a tiempo `T` del reloj del SO, sujeto a disponibilidad del nodo y a su `missed_policy`. No hay drift acumulado.

---

## 12. Autorización y Ownership

### 12.1 Regla general

Para `TIMER_CANCEL`, `TIMER_RESCHEDULE`:
```
if timer.owner_l2_name != sender_l2_name(routing.src):
    return error TIMER_NOT_OWNER
```

Para `TIMER_GET`:
```
read timer by uuid without ownership restriction
```

Para `TIMER_LIST`:
```
query: optional WHERE owner_l2_name = <owner_l2_name>
```

### 12.2 Excepción única

Para `TIMER_PURGE_OWNER`:
```
if sender_l2_name(routing.src) != "SY.orchestrator@<local-hive>":
    return error TIMER_FORBIDDEN
```

La validación se hace por nombre L2 del sender, resolviendo `routing.src` (UUID L1) a través del runtime/SDK Go y confiando en la capa L2 del router para la integridad de esa identidad. Esto es consistente con el modelo de autorización del resto de nodos del sistema.

En v1 actual, esa resolución en Go toma el `router_name` recibido en el `ANNOUNCE`, lee `state/<router_l2_name>/identity.yaml` para ubicar el `shm.name`, abre esa router SHM en modo read-only y resuelve `uuid -> L2` desde la tabla viva de nodos conectados a ese router.

### 12.3 Flujo de borrado de nodo

```
SY.admin (HTTP request)
    │
    ├─► SY.orchestrator (RPC/L2)
    │       │
    │       ├─► SY.timer: TIMER_PURGE_OWNER { owner_l2_name: "WF.foo@hive" }
    │       │       └─► TIMER_RESPONSE { ok: true, deleted_count: N }
    │       │
    │       ├─► kill process del nodo
    │       ├─► cleanup de /var/lib/fluxbee/nodes/.../
    │       └─► unregister de identity
    │
    └─◄ respuesta HTTP
```

El orchestrator debe llamar a `TIMER_PURGE_OWNER` **antes** de matar el proceso del nodo, para evitar la condición de carrera donde el nodo recibe un `TIMER_FIRED` justo mientras está siendo terminado. Si `SY.timer` está caído en ese momento, el orchestrator debe proceder con el borrado igualmente y loggear el fallo; los timers huérfanos serán descartados naturalmente cuando sus eventos `TIMER_FIRED` no encuentren target (y eventualmente podrán ser limpiados manualmente).

---

## 13. Sintaxis Cron Soportada

`SY.timer` usa cron estándar de 5 campos: `minuto hora dia-del-mes mes dia-de-semana`.

| Campo | Rango | Especiales |
|---|---|---|
| minuto | 0-59 | `*`, `,`, `-`, `/` |
| hora | 0-23 | idem |
| dia del mes | 1-31 | idem, `?` |
| mes | 1-12 o JAN-DEC | idem |
| dia de semana | 0-6 o SUN-SAT | idem |

**Ejemplos comunes:**

| Cron | Significado |
|---|---|
| `*/5 * * * *` | Cada 5 minutos |
| `0 * * * *` | Cada hora en punto |
| `0 9 * * MON` | Lunes a las 9:00 |
| `0 0 1 * *` | Primer día del mes a medianoche |
| `30 14 * * 1-5` | Lunes a viernes a las 14:30 |

**No soportado en v1:**
- Campos de segundos (cron de 6 campos)
- Descriptores especiales tipo `@reboot`, `@yearly`
- Expresiones de intervalo tipo `every 5m` (reservado para v2 si aparece demanda)

---

## 14. SDK Go

El SDK Go de Fluxbee expone un módulo `timer` que encapsula toda la interacción con `SY.timer`. El autor del nodo cliente nunca ve JSON.

### 14.1 Patrón recomendado de uso directo

Para nodos `WF.*`, `AI.*`, `IO.*` y futuros nodos Go, el patrón recomendado es:

1. conectarse al router con `fluxbee-go-sdk`
2. construir un `TimerClient` sobre el `NodeSender`
3. usar métodos tipados del cliente para tiempo, scheduling y lectura
4. procesar `TIMER_FIRED` como mensaje `system` normal en el handler del nodo

En v1 este es el camino primario de integración. `SY.admin` aporta visibilidad read-only y utilidades de tiempo, pero la semántica owner-bound de scheduling y mutaciones sigue siendo de uso directo por SDK.

Ejemplo mínimo:

```go
sender, receiver, err := sdk.Connect(sdk.NodeConfig{
    Name:               "WF.demo",
    RouterSocket:       "/var/run/fluxbee/routers",
    UUIDPersistenceDir: "/var/lib/fluxbee/state/nodes",
    UUIDMode:           sdk.NodeUuidPersistent,
    ConfigDir:          "/etc/fluxbee",
    Version:            "1.0",
})
if err != nil {
    return err
}

timerClient, err := sdk.NewTimerClient(sender, "")
if err != nil {
    return err
}

timerID, err := timerClient.ScheduleIn(ctx, 2*time.Minute, sdk.ScheduleOptions{
    ClientRef: "wf-demo-timeout",
    Payload: map[string]any{
        "kind": "timeout",
    },
})
if err != nil {
    return err
}

_ = timerID
_ = receiver
```

### 14.2 Firmas principales

```go
package timer

// Constantes del SDK
const MinDuration = 60 * time.Second

// --- Tiempo ---
func Now(ctx context.Context) (time.Time, error)
func NowIn(ctx context.Context, tz string) (time.Time, error)
func Convert(ctx context.Context, instant time.Time, toTz string) (time.Time, error)
func Parse(ctx context.Context, input, layout, tz string) (time.Time, error)
func Format(ctx context.Context, instant time.Time, layout, tz string) (string, error)

// --- Timers one-shot ---
type ScheduleOptions struct {
    Target       string          // opcional, default: self
    ClientRef    string          // opcional
    Payload      any             // opcional, serializable a JSON
    MissedPolicy MissedPolicy    // default: Fire
    MissedWithin time.Duration   // obligatorio si MissedPolicy == FireIfWithin
    Metadata     map[string]any  // opcional
}

func Schedule(ctx context.Context, fireAt time.Time, opts ScheduleOptions) (TimerID, error)
func ScheduleIn(ctx context.Context, d time.Duration, opts ScheduleOptions) (TimerID, error)

// --- Timers recurrentes ---
type RecurringOptions struct {
    CronSpec     string
    CronTZ       string          // default: "UTC"
    Target       string
    ClientRef    string
    Payload      any
    MissedPolicy MissedPolicy
    MissedWithin time.Duration
    Metadata     map[string]any
}

func ScheduleRecurring(ctx context.Context, opts RecurringOptions) (TimerID, error)

### 14.3 Estado del SDK Rust

En el estado actual del runtime:

- la superficie rica y tipada de `SY.timer` está implementada en `fluxbee-go-sdk`
- `fluxbee_sdk` (Rust) todavía **no** expone un módulo `timer` equivalente

Eso no es un accidente de diseño ni una decisión de largo plazo: es una deuda explícita de paridad de SDK.

El objetivo correcto para el SDK Rust es replicar:

- helpers tipados de tiempo (`now`, `now_in`, `convert`, `parse`, `format`, `help`)
- cliente tipado de scheduling (`schedule`, `schedule_in`, `schedule_recurring`, `get`, `list`, `cancel`, `reschedule`, `purge_owner`)
- parseo de `TIMER_RESPONSE`
- parseo de `TIMER_FIRED`
- errores y validación client-side comparables al SDK Go

El backlog formal de ese trabajo queda en [sdk_tasks.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/onworking%20COA/sdk_tasks.md) bajo la sección `RUST-TIMER-SDK`.

**Decisión de alcance:** esta paridad del SDK Rust no implica que `SY.orchestrator` deba usar `SY.timer` para su funcionamiento base en v1. El trabajo es primero de capacidad del SDK y de soporte a nuevos consumidores Rust, no de re-arquitectura del control-plane.

// --- Gestión ---
func Cancel(ctx context.Context, id TimerID) error
func Reschedule(ctx context.Context, id TimerID, newFireAt time.Time) error
func Get(ctx context.Context, id TimerID) (*TimerInfo, error)
func ListMine(ctx context.Context, filter ListFilter) ([]TimerInfo, error)

// --- Introspección ---
func Help(ctx context.Context) (*HelpDescriptor, error)
```

### 14.3 Validación client-side

```go
func ScheduleIn(ctx context.Context, d time.Duration, opts ScheduleOptions) (TimerID, error) {
    if d < MinDuration {
        return "", ErrBelowMinimum
    }
    // ... resuelve nombre L2 de SY.timer en hive local
    // ... arma TIMER_SCHEDULE
    // ... envía y parsea response
}
```

El mínimo de 60s se valida **client-side** antes de mandar el request, evitando roundtrip. También se valida server-side como defensa en profundidad.

### 14.4 Reintento de operaciones de tiempo

```go
func Now(ctx context.Context) (time.Time, error) {
    for attempt, backoff := range []time.Duration{100*time.Millisecond, 300*time.Millisecond, 1*time.Second} {
        t, err := doNowRequest(ctx)
        if err == nil {
            return t, nil
        }
        if attempt < 2 {
            time.Sleep(backoff)
        }
    }
    return time.Time{}, ErrTimerUnreachable
}
```

### 14.5 Manejo de `TIMER_FIRED` en el nodo cliente

El SDK expone un handler tipado:

```go
type FiredEvent struct {
    TimerUUID    TimerID
    ClientRef    string
    Kind         TimerKind
    ScheduledAt  time.Time
    ActualAt     time.Time
    FireCount    int
    UserPayload  json.RawMessage
    Metadata     map[string]any
}

// En el runtime del nodo cliente:
node.OnSystemMessage("TIMER_FIRED", func(msg Message) error {
    evt, err := timer.ParseFiredEvent(msg)
    if err != nil {
        return err
    }
    // lógica de aplicación
    return nil
})
```

---

## 15. Códigos de Error

| Código | Descripción |
|---|---|
| `TIMER_BELOW_MINIMUM` | Duración solicitada menor a 60 segundos. |
| `TIMER_INVALID_CRON` | `cron_spec` no parsea correctamente. |
| `TIMER_INVALID_TIMEZONE` | `tz` / `cron_tz` no es una zona IANA válida. |
| `TIMER_INVALID_PAYLOAD` | `payload` del request no es JSON válido o faltan campos obligatorios. |
| `TIMER_NOT_FOUND` | `timer_uuid` no existe. |
| `TIMER_NOT_OWNER` | El requester no es dueño del timer para verbos de mutación (`TIMER_CANCEL`, `TIMER_RESCHEDULE`). |
| `TIMER_ALREADY_FIRED` | Timer ya disparado, no se puede cancelar/reprogramar. |
| `TIMER_RECURRING_NOT_RESCHEDULABLE` | No se permite reschedule sobre recurrentes en v1. |
| `TIMER_FORBIDDEN` | Operación restringida (ej. `TIMER_PURGE_OWNER` desde nodo no-orchestrator). |
| `TIMER_STORAGE_ERROR` | Error interno de SQLite. |
| `TIMER_INTERNAL` | Error interno no clasificado. |
| `TIMER_UNREACHABLE` | (Client-side) `SY.timer` no responde tras reintentos. |

Formato de response de error:

```json
{
  "meta": { "msg_type": "system", "msg": "TIMER_RESPONSE" },
  "payload": {
    "ok": false,
    "verb": "TIMER_SCHEDULE",
    "error": {
      "code": "TIMER_BELOW_MINIMUM",
      "message": "minimum timer duration is 60 seconds, got 30000 ms"
    }
  }
}
```

---

## 16. Operación

### 16.1 Systemd

`SY.timer` es un singleton core por hive, lanzado y mantenido por `SY.orchestrator` a través del mismo modelo operativo que el resto de los nodos `SY.*` del sistema. Tiene binario core instalado, copia en `dist/core/bin`, unit file de systemd y supervisión/restart por orchestrator.

Binario: `/usr/bin/sy-timer` (instalado por `install.sh`).
Copia de distribución: `/var/lib/fluxbee/dist/core/bin/sy-timer`.
Unit: `/etc/systemd/system/sy-timer.service`.

### 16.2 Paths de runtime

| Path | Propósito |
|---|---|
| `/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/timers.db` | SQLite con timers persistidos |
| `/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/config.json` | Config managed por orchestrator |
| `/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/state.json` | Estado opcional del nodo |
| `/var/run/fluxbee/routers/` | Socket para comunicación con router local |

### 16.3 Backup

La base `timers.db` debería incluirse en los snapshots de backup del directorio `/var/lib/fluxbee/nodes/`. SQLite soporta backup en caliente con `.backup` o con `VACUUM INTO`. Para v1 no se provee herramienta específica: backup del directorio con `SY.timer` detenido o con SQLite en modo WAL funciona.

### 16.4 Logs

`SY.timer` emite logs estructurados por stdout (capturados por systemd / orchestrator):

- `INFO`: arranque, cantidad de timers cargados, GC, disparos (opcional: verboso).
- `WARN`: timers descartados por `missed_policy`, targets inalcanzables.
- `ERROR`: errores de SQLite, crons inválidos en DB, violaciones de integridad.

### 16.5 Métricas (futuro)

Reservado para v2. Candidatos: `timers_pending_gauge`, `timers_fired_counter`, `timer_fire_latency_histogram`, `timer_requests_counter` por verbo.

---

## 17. Casos de Uso Previstos

### 17.1 WF — Step timeout

Un workflow programa un timer al entrar a un estado que espera respuesta externa:

```go
timerID, _ := timer.ScheduleIn(ctx, 30*time.Minute, timer.ScheduleOptions{
    ClientRef: fmt.Sprintf("wf_%s_step_%s_timeout", wfInstanceID, stepName),
    Payload:   map[string]any{"wf_instance": wfInstanceID, "step": stepName, "reason": "step_timeout"},
    MissedPolicy: timer.Fire,
})
```

Cuando el WF recibe la respuesta esperada, cancela el timer. Si no, recibe `TIMER_FIRED` y transiciona a un estado de escalation.

### 17.2 Cognition — Reconsideración diferida

Un nodo cognitivo puede programar un recordatorio para revisar una conclusión con datos frescos:

```go
timer.ScheduleIn(ctx, 24*time.Hour, timer.ScheduleOptions{
    ClientRef: "reconsider_hypothesis_42",
    Payload:   map[string]any{"hypothesis_id": 42, "action": "reconsider"},
    MissedPolicy: timer.Drop,
    Metadata: map[string]any{
        "reasoning": "low confidence conclusion, may benefit from fresh data",
    },
})
```

### 17.3 Reportes recurrentes

```go
timer.ScheduleRecurring(ctx, timer.RecurringOptions{
    CronSpec:  "0 9 * * MON",
    CronTZ:    "America/Argentina/Buenos_Aires",
    ClientRef: "weekly_summary",
    Payload:   map[string]any{"report_type": "weekly"},
    MissedPolicy: timer.Drop,
})
```

### 17.4 Tiempo autoritativo para AI nodes

```go
now, _ := timer.Now(ctx)
prompt := fmt.Sprintf("Current date and time: %s\nUser query: %s", now.Format(time.RFC3339), userQuery)
```

### 17.5 Formateo de tiempo por timezone de usuario

```go
formatted, _ := timer.Format(ctx, eventTime, "02/01/2006 15:04", userTimezone)
responseToUser := fmt.Sprintf("El evento es el %s", formatted)
```

---

## 18. Decisiones de Diseño

| Decisión | Razón |
|---|---|
| Uno por hive, autónomo | Simplicidad, sin sincronización distribuida, delega al SO |
| Sin sincronización NTP propia | El SO lo resuelve mejor y ya está instalado |
| Tiempo server-side con roundtrip siempre | Sin resincronización local complicada, consistencia garantizada |
| Mínimo 60 segundos | Elimina casos de uso reactivos finos, alinea con granularidad cron |
| SQLite embebido | Consistencia con otros nodos, simple, transacional, backup trivial |
| Ownership estricto por L2 name | Aislamiento entre nodos, sin admin override, sin complicaciones |
| `TIMER_PURGE_OWNER` solo para orchestrator | Excepción única y trazable para teardown de nodos |
| Sintaxis cron estándar | Conocida por LLMs sin aprendizaje adicional |
| Respuesta genérica `TIMER_RESPONSE` | Consistencia con `CONFIG_RESPONSE` y resto del control plane |
| `TIMER_FIRED` no espera ack | Fire-and-forget, simplicidad; retry es responsabilidad del creador |
| `metadata` como campo libre | Extensibilidad sin romper schema, útil para clientes LLM |
| `client_ref` como campo de primer nivel | Uso suficientemente común (debug, display, búsqueda) para promover |
| UUIDs generados por `SY.timer` | Evita colisiones, simplifica cliente, autoritativo |
| Recurrentes: no disparo retroactivo | Evita tormentas al volver de downtime largo |
| `TIMER_HELP` embebido | Cumple principio de self-description del sistema |
| No reschedule de recurrentes en v1 | Caso borde raro, cancelar+recrear funciona |
| `time/tzdata` embebido | Nodo autocontenido, no depende de `tzdata` del SO (más simple) |

---

## 19. Límites del Sistema

| Límite | Valor | Notas |
|---|---|---|
| Mínimo de duración | 60 s | Constante, no configurable |
| Máximo de timers pendientes | ~10.000 | Por hive, soft limit |
| Máximo de `TIMER_LIST` | 1000 resultados | Por request |
| Retención de timers fired/canceled | 7 días | Configurable en v2 |
| Precisión de disparo | ±5 s | Best-effort, sujeta a carga |
| Tamaño máximo de `payload` | 64 KB | Alineado con mensajes L2 inline |
| Tamaño máximo de `metadata` | 16 KB | Recomendado, no enforced en v1 |
| Timeout de reintento `TIMER_NOW` | ~1.4 s total | 3 intentos con backoff |

---

## 20. Pendientes / v2

- Métricas estructuradas (Prometheus-compatible).
- HA del nodo (replicación de `timers.db` a instancia standby).
- Backup/restore administrativo vía capability.
- Reschedule de recurrentes.
- Disparo retroactivo opcional de recurrentes (para casos donde los "perdidos" sí importan).
- Descriptores cron extendidos (`@hourly`, `@daily`, `@weekly`).
- Retención configurable por owner.
- Dead-letter para `TIMER_FIRED` no entregados.

---

## 21. Referencias Cruzadas

| Tema | Documento |
|---|---|
| Arquitectura general Fluxbee | `01-arquitectura.md` |
| Protocolo L2 y mensajes | `02-protocolo.md` |
| Shared memory (para contexto de otros nodos SY) | `03-shm.md` |
| Routing y nombres L2 | `04-routing.md` |
| Conectividad intra/inter-isla | `05-conectividad.md` |
| Config control plane | `node-config-control-plane-spec.md` |
| Runtime rollout de nodos | `14-runtime-rollout-motherbee.md` |
| Glosario y decisiones globales | `08-apendices.md` |
| (Futuro) Nodos WF | `wf-spec.md` — pendiente |
| (Futuro) Self-description protocol general | pendiente |
