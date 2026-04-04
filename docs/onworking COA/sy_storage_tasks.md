# SY.storage - Contrato canonico de subjects (Fase 1)

> Documento histórico/parcial. Para cognition v2 el camino canónico nuevo es `storage.cognition.*`; `storage.events/items/reactivation` quedan como contrato legado y no como modelo vigente de `SY.cognition`.

Fecha: 2026-02-16

Documento operativo para alinear productores/consumidor (`SY.storage`) en los subjects:
- `storage.turns`
- `storage.events`
- `storage.items`
- `storage.reactivation`

Ownership acordado (2026-02-19, histórico):
- `storage.turns`: `rt-gateway` (activo en codigo).
- `storage.events`: contrato viejo/no canónico para cognition v2.
- `storage.items`: contrato viejo/no canónico para cognition v2.
- `storage.reactivation`: contrato viejo/no canónico para cognition v2.

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

## 7) TODO - Migracion de credenciales DB fuera de `hive.yaml`

Contexto original:
- `SY.storage` hoy arranca leyendo `database.url` desde:
  - `FLUXBEE_DATABASE_URL`
  - `JSR_DATABASE_URL`
  - y en ese momento también tenía fallback `hive.yaml -> database.url`
- Esto está implementado en `src/bin/sy_storage.rs` (`base_database_url(...)` / `database_config(...)`).
- En ese momento `SY.storage` todavía no participaba del control-plane de router como nodo configurable para `CONFIG_GET` / `CONFIG_SET`.

Problema:
- El password de PostgreSQL sigue viviendo en `hive.yaml`, mezclado con bootstrap infra.
- Eso ya no está alineado con la dirección tomada para secrets de nodos (`secrets.json` local + redaction + escritura atómica).

Dirección acordada:
- Sacar el secreto de DB de `hive.yaml`.
- Hacer que la credencial de `SY.storage` entre desde `archi`, pasando por `SY.admin`.
- Persistir el secreto localmente con el mismo helper estándar del SDK (`crates/fluxbee_sdk/src/node_secret.rs`).
- Mantener `hive.yaml` solo como fallback legacy temporal durante migración.

Nota importante de diseño:
- Si `SY.storage` debe quedar alineado con el camino normal de config/secrets del sistema, entonces no alcanza con una acción admin dedicada.
- Primero hay que habilitarle un control-path por socket/control-plane para `CONFIG_GET` / `CONFIG_SET`, análogo al resto de nodos configurables.
- Ese gap original ya quedó resuelto en código.
- Entonces el trabajo correcto queda partido en dos:
  1. habilitar el control-path/socket para `SY.storage`
  2. recién ahí migrar el secreto DB al camino normal de `archi -> SY.admin -> CONFIG_SET`

Dirección ajustada:
- objetivo final:
  - `archi`
  - `SY.admin`
  - `CONFIG_GET` / `CONFIG_SET`
  - persistencia local `secrets.json`
  - restart o reconcile explícito de `SY.storage`
- workaround/transición si hace falta destrabar antes:
  - acción admin dedicada temporal
  - pero no como contrato final deseado

### 7.1 Objetivo v1

- `database.password` o `database_url` secreto ya no debe requerir edición manual en `hive.yaml`.
- `archi` debe poder consultar estado redactado y configurar la credencial operativamente.
- `SY.storage` debe leer el secreto primero desde archivo local privado.
- Logs, respuestas y help no deben mostrar el valor plano.

### 7.2 Contrato mínimo propuesto

Separación recomendada:
- config no secreta:
  - host
  - port
  - user
  - dbname
  - sslmode
- secret:
  - `database.password`
  - o alternativamente `database.url` completa si se decide no separar componentes todavía

Preferencia v1:
- si queremos mínimo cambio, persistir un único storage key secreto:
  - `postgres_url`
- si queremos un diseño mejor, persistir:
  - `postgres_password`
  - y dejar host/user/dbname en config no secreta

Recomendación:
- v1 pragmática: soportar `postgres_url` como secreto canónico para migrar rápido
- v1.1: separar `postgres_password` de config pública si hace falta

### 7.3 Fuente de verdad y precedencia

Para `SY.storage`, la precedencia propuesta debería ser:
1. `secrets.json` local de `SY.storage`
2. env (`FLUXBEE_DATABASE_URL`, `JSR_DATABASE_URL`) como compat/override operacional
3. `hive.yaml -> database.url` solo como fallback legacy temporal

Condición de salida:
- una vez migrado, `hive.yaml` ya no debe contener el secreto en instalaciones nuevas

### 7.4 Transporte/admin

Objetivo final:
- `SY.storage` debe tener un control-path propio por socket/control-plane para:
  - `CONFIG_GET`
  - `CONFIG_SET`

Gap original:
- hoy `SY.storage` está integrado por NATS para metrics/ingesta, pero no por un canal normal de config del nodo
- por lo tanto, antes de hablar de “usar el camino normal”, hay que implementarlo

Estado 2026-03-31:
- `SY.storage` ya entra al router como `SY.storage@motherbee` y responde `CONFIG_GET` / `CONFIG_SET`.
- `SY.admin` ya puede alcanzarlo por el path normal:
  - `POST /hives/motherbee/nodes/SY.storage@motherbee/control/config-get`
  - `POST /hives/motherbee/nodes/SY.storage@motherbee/control/config-set`
- `CONFIG_SET` hoy es `persist-only`:
  - guarda `postgres_url` en `secrets.json`
  - actualiza `config.json` público redactado
  - requiere restart de `sy-storage` para afectar la conexión DB en vivo
- Precedencia efectiva actual para DB:
  1. `secrets.json` local
  2. env compat
- Estado operativo validado:
  - `CONFIG_GET` responde correctamente por `archi -> SY.admin -> SY.storage`
  - `CONFIG_SET` persiste `postgres_url` en `secrets.json` y devuelve redacción correcta
  - restart posterior de `sy-storage` vuelve a levantar usando el secreto local
  - `SY.storage` ya no usa `hive.yaml -> database.url`
  - `SY.identity` ya migró al mismo esquema, así que `database.url` queda fuera del camino canónico de ambos

Fase propuesta:
- Fase A:
  - habilitar socket/control-plane en `SY.storage`
  - definir contrato `CONFIG_GET` / `CONFIG_SET`
  - hacer que `SY.admin` lo pueda alcanzar como nodo configurable
- Fase B:
  - mover secreto DB a `secrets.json`
  - exponer redaction y metadata de secreto
  - reinicio/reconcile post-apply
- Fase T (transición, opcional):
  - acción admin dedicada temporal solo si hace falta destrabar antes de completar Fase A

Resultado:
- no hizo falta una action admin dedicada permanente
- `SY.storage` quedó integrado al camino normal `CONFIG_GET` / `CONFIG_SET`
- el flujo operativo nuevo queda:
  - `CONFIG_SET`
  - persistencia en `secrets.json`
  - restart de `sy-storage`
  - `CONFIG_GET` / health post-restart

### 7.4.1 Contrato canónico propuesto (`ST-DB-2`)

Node identity objetivo:
- `SY.storage@motherbee`

Paths deseados desde `SY.admin` / `archi`:
- `POST /hives/motherbee/nodes/SY.storage@motherbee/control/config-get`
- `POST /hives/motherbee/nodes/SY.storage@motherbee/control/config-set`

Soporte mínimo esperado en `contract`:
- `node_family = "SY"`
- `node_kind = "SY.storage"`
- `supports = ["CONFIG_GET", "CONFIG_SET"]`
- `required_fields`
- `optional_fields`
- `secrets`
- `notes`
- `examples`

#### Campo secreto canónico v1

Se define como secreto canónico v1:
- `config.database.postgres_url`

Storage key recomendado:
- `postgres_url`

Razón:
- minimiza cambio en `sy_storage.rs`, que hoy ya consume una URL completa
- evita introducir en esta fase parse/merge separado de host/user/password/dbname
- deja abierta una migración futura a componentes separados si se necesita

#### Shape recomendado de `CONFIG_GET`

```json
{
  "ok": true,
  "node_name": "SY.storage@motherbee",
  "config_version": 3,
  "state": "configured",
  "config": {
    "database": {
      "mode": "postgres",
      "postgres_url": "***REDACTED***",
      "source": "local_file"
    }
  },
  "contract": {
    "node_family": "SY",
    "node_kind": "SY.storage",
    "supports": ["CONFIG_GET", "CONFIG_SET"],
    "required_fields": [
      "config.database.postgres_url"
    ],
    "optional_fields": [],
    "secrets": [
      {
        "field": "config.database.postgres_url",
        "storage_key": "postgres_url",
        "required": true,
        "configured": true,
        "value_redacted": true,
        "persistence": "local_file"
      }
    ],
    "notes": [
      "SY.storage uses PostgreSQL only on motherbee.",
      "Secret values are persisted in local secrets.json and always returned redacted.",
      "Applying a new DB secret may require restart or controlled reconnect."
    ],
    "examples": [
      {
        "apply_mode": "replace",
        "config": {
          "database": {
            "postgres_url": "postgresql://fluxbee:***@127.0.0.1:5432/fluxbee"
          }
        }
      }
    ]
  }
}
```

Reglas:
- `config.database.postgres_url` nunca debe salir en claro
- `source` puede exponer:
  - `local_file`
  - `env_compat`
  - `missing`
- `configured=true` no implica necesariamente conexión saludable; solo indica que existe secreto local

#### Shape recomendado de `CONFIG_SET`

```json
{
  "schema_version": 1,
  "config_version": 4,
  "apply_mode": "replace",
  "config": {
    "database": {
      "postgres_url": "postgresql://fluxbee:secret@127.0.0.1:5432/fluxbee"
    }
  }
}
```

Reglas:
- `schema_version` requerido
- `config_version` monotónica
- `apply_mode`:
  - v1: `replace`
  - `merge_patch` puede agregarse después si hace falta
- el secreto viaja inline en `payload.config`
- `SY.storage` debe:
  - validar que `postgres_url` no sea vacío
  - persistirlo en `secrets.json`
  - no persistir el valor plano en `config.json`
  - devolver `CONFIG_RESPONSE` redactado

#### Shape recomendado de `CONFIG_RESPONSE`

Éxito:

```json
{
  "ok": true,
  "node_name": "SY.storage@motherbee",
  "config_version": 4,
  "state": "configured",
  "notes": [
    "postgres_url persisted in local secrets.json",
    "service restart may be required to apply connection changes"
  ]
}
```

Error de validación:

```json
{
  "ok": false,
  "node_name": "SY.storage@motherbee",
  "config_version": 4,
  "state": "failed_config",
  "error": {
    "code": "invalid_config",
    "message": "config.database.postgres_url is required"
  }
}
```

#### Reglas operativas

- persistencia:
  - el secreto se guarda solo en `secrets.json`
  - `config.database.postgres_url` no queda en claro en respuestas
- precedencia de lectura:
  1. `secrets.json`
  2. env compat
  3. `hive.yaml` legacy
- aplicación:
  - v1 puede requerir restart explícito del servicio
  - el `CONFIG_SET` debe dejar claro si el apply fue:
    - persist-only
    - persist-and-restart
    - persist-and-reconnect

#### Preguntas cerradas para v1

- secreto canónico:
  - `config.database.postgres_url`
- storage key:
  - `postgres_url`
- transporte final deseado:
  - `CONFIG_GET` / `CONFIG_SET`
- redaction:
  - obligatoria en `CONFIG_GET`, `CONFIG_RESPONSE`, help y logs

### 7.5 Persistencia local

Usar helpers ya existentes del SDK:
- `build_node_secret_record`
- `load_node_secret_record`
- `save_node_secret_record`
- `redacted_node_secret_record`

Node name recomendado:
- `SY.storage@motherbee`

Path esperado:
- `/var/lib/fluxbee/nodes/SY/SY.storage@motherbee/secrets.json`

Reglas:
- `0600`
- escritura atómica
- redaction por defecto

### 7.6 Aplicación del cambio

Pendiente de definición fina:
- `SY.storage` hoy lee su DB config al arranque
- por lo tanto, una actualización del secreto necesita:
  - restart explícito de `sy-storage`
  - o futuro reload controlado si se implementa reconnect seguro

Recomendación v1:
- `set_storage_db_config` persiste secreto
- luego dispara restart controlado del servicio
- `SY.admin` devuelve resultado redactado + estado del restart

### 7.7 Checklist

- [x] ST-DB-0. Habilitar control-path/socket para `SY.storage` y meterlo al mismo plano `CONFIG_GET` / `CONFIG_SET` que otros nodos configurables.
- [x] ST-DB-1. Documentar que `database.url` en `hive.yaml` deja de ser input para `SY.storage`.
- [x] ST-DB-2. Definir contrato canónico para leer/aplicar config secreta de DB:
  - final deseado: `CONFIG_GET` / `CONFIG_SET`
  - transición opcional: action admin dedicada
- [x] ST-DB-3. Implementar persistencia en `secrets.json` usando `fluxbee_sdk::node_secret`.
- [x] ST-DB-4. Cambiar `sy_storage.rs` para leer primero secreto local antes que env/hive.
- [x] ST-DB-5. Agregar redaction en cualquier respuesta/help/status aplicable.
- [x] ST-DB-6. Implementar ciclo operativo:
  - persistir secreto
  - restart de `sy-storage`
  - verificación post-restart
- [x] ST-DB-7. Limpiar docs/examples de mayor visibilidad para que `database.url` en `hive.yaml` deje de figurar como input de `SY.storage`.

### 7.8 Criterio de cierre

- [x] `SY.storage` puede configurarse desde `archi` por su control-path normal sin editar `hive.yaml`.
- [x] El secreto de DB queda persistido en `secrets.json` local, no en config pública.
- [x] `SY.storage` vuelve a levantar correctamente tras restart usando el secreto local.
- [x] `SY.storage` deja de depender de `hive.yaml`; el campo `database.url` queda fuera del camino canónico actual.

Nota:
- validado manualmente el 2026-03-31 con `CONFIG_GET`, `CONFIG_SET`, restart y lectura efectiva posterior.
- mejora futura opcional:
  - separar `postgres_password` del resto de la URL pública en una v1.1
