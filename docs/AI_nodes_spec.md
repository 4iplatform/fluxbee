# AI Nodes Specification — Consolidated Draft Replacement (v1)

> ✅ Este documento consolida las Partes 1–5 en una única especificación “reemplazo” para **AI Nodes** en Fluxbee.  
> Alcance: **AI Nodes** (Control Plane + Data Plane + Behaviors/Providers + Schema/Validation + Operación).  
> Fuera de alcance: *cognitive* (se menciona solo como “a especificar”).

---

## Índice

1. Control Plane (bootstrap, configuración y estado)
2. Data Plane (contrato `text/v1`, blobs/attachments, respuesta)
3. Behaviors & Providers
4. Schema & Validation
5. Operation & Lifecycle

---


## 1. Control Plane

### Seguridad y autorización (capa de policy)

✅ **NORMATIVO**:
- El nodo AI **asume** que mensajes `system/admin` (Control Plane) llegan **ya autorizados** por la capa de policy (OPA/Identity/Routing) de Fluxbee.
- Aun así, el nodo **MUST** validar **estructura y schema** (campos requeridos, tipos, `schema_version`, `config_version`, etc.).
- Si un mensaje `system/admin` es estructuralmente inválido o no valida schema, el nodo **MUST** responder `invalid_config` / `invalid_payload` (según corresponda) y **MUST NOT** aplicar cambios.

🧩 **A ESPECIFICAR (en Fluxbee policy/OPA)**:
- Cómo se autentica/autoriza exactamente quién puede enviar `CONFIG_SET`/`STATUS`/`PING` (fuera del alcance de AI Nodes).



> ✅ **Objetivo**: Este documento es un borrador “reemplazo” de la especificación actual de **AI Nodes**.  
> En esta primera entrega se define el **Control Plane** (bootstrap/config/status), ignorando todo lo relativo a *cognitive* (solo se lo menciona como TBD).

---

## Convenciones de estado del documento

- ✅ **NORMATIVO**: requisito cerrado (MUST/SHOULD/MAY).
- ⚠️ **TENTATIVO**: decisión provisional; debe revisarse.
- 🧩 **TBD**: falta definir (no normativo todavía).
- 🐞 **DESVIACIÓN CÓDIGO**: comportamiento actual que contradice lo normativo.

---

## 1. Control Plane (bootstrap, configuración y estado)

> ✅ **NORMATIVO**: Esta sección define el **plano de control** de un nodo `AI.*`.  
> El nodo **DEBE** poder arrancar y operar en modo `UNCONFIGURED` (sin YAML), recibiendo configuración por mensajes `system/admin`, persistiendo dicha configuración y recuperándola al reiniciar.

### 1.1 Objetivo

- Permitir que un nodo AI:
  - arranque **sin configuración local**;
  - reciba configuración desde el sistema (administrator/orchestrator) vía mensajes `system/admin`;
  - **persista** esa configuración para restarts;
  - exponga `STATUS`/`PING` para operación y debugging;
  - rechace correctamente mensajes de usuario si aún no está configurado.

---

## 2. Estados del nodo

✅ **NORMATIVO**: el nodo AI implementa al menos estos estados operativos:

- `UNCONFIGURED`: no existe configuración efectiva (ni config dinámica del nodo ni fallback de orchestrator).
- `CONFIGURED`: existe configuración efectiva y el nodo puede procesar mensajes de usuario.

✅ **NORMATIVO (operación managed-node actual)**: prioridad de configuración efectiva (precedencia):
1) **Config dinámica persistida por el nodo** (`${STATE_DIR}/ai-nodes/<name>.json`, vía `CONFIG_SET`)
2) **Config de orchestrator en spawn** (`/var/lib/fluxbee/nodes/<TYPE>/<node@hive>/config.json`)
3) `UNCONFIGURED`

⚠️ **Nota de compatibilidad**:
- Si el runner se opera en modo YAML legacy (`--config ...`), ese flujo puede definir su propia precedencia de arranque.
- Para el flujo canónico de nodos AI manejados por orchestrator (sin YAML por instancia), aplica la precedencia 1→2→3 anterior.

---

## 2.1 Lifecycle & gestión de configuración (normativo)

> ✅ **NORMATIVO**: Este bloque define el lifecycle operativo de un nodo `AI.*` respecto a su configuración, independiente del provider.

### Estados

✅ **NORMATIVO**: estados del nodo (por instancia systemd):
- `UNCONFIGURED`: no hay configuración efectiva aplicable.
- `CONFIGURED`: hay configuración efectiva aplicable y el nodo puede procesar mensajes `user`.
- `FAILED_CONFIG`: el nodo NO puede procesar mensajes `user`, pero **sí** atiende Control Plane para permitir recuperación.

✅ **NORMATIVO**: en `FAILED_CONFIG` el nodo **MUST** seguir atendiendo: `PING`, `STATUS`, `CONFIG_GET`, `CONFIG_SET`.

### Configuración efectiva: doble persistencia y responsabilidad

✅ **NORMATIVO**:
- Existen dos persistencias distintas por diseño:
  - `config.json` de orchestrator: infra/bootstrap de la instancia.
  - JSON dinámico del nodo en `${STATE_DIR}/ai-nodes/<name>.json`: configuración runtime/business aplicada por `CONFIG_SET`.
- El nodo AI **MUST** leer/escribir su JSON dinámico con persistencia atómica.
- Si no existe JSON dinámico, el nodo **MAY** bootstrapear desde `config.json` de orchestrator.
- Si no existe ninguna de las dos fuentes, el nodo queda `UNCONFIGURED` y atiende solo Control Plane.

⚠️ **DEPRECATED (no operator-managed)**:
- Los YAML por nodo en `/etc/fluxbee/ai-nodes/*.yaml` dejan de ser “operator-managed config”.
- Si existen, se consideran **plantillas / referencia** (no fuente viva) y **no deben** ser reescritas por el nodo en runtime.

### 2.1.1 Mensajes `user` cuando no está configurado

✅ **NORMATIVO**:
- Si el nodo está `UNCONFIGURED` o `FAILED_CONFIG` y recibe un mensaje `meta.type="user"`:
  - **MUST** responder con error `node_not_configured` (usando `ErrorV1Payload` cuando sea posible, o fallback `text/v1`),
  - **MUST NOT** procesar el mensaje.

### 2.1.2 Aplicación de config: validación y errores

✅ **NORMATIVO**:
- `CONFIG_SET` inválido (schema incompleto, tipos inválidos, `schema_version` no soportado, etc.) **MUST**:
  - responder `invalid_config`,
  - **MUST NOT** persistir cambios,
  - **MUST** mantener la configuración efectiva anterior y el estado previo.

✅ **NORMATIVO**:
- Si el nodo arranca con YAML inválido o config persistida inválida, o no puede cumplir requisitos mínimos de ejecución (ej: falta secret requerido):
  - entra en estado `FAILED_CONFIG`,
  - y solo atiende Control Plane hasta corregir la configuración.

✅ **NORMATIVO (resiliencia runtime)**:
- Errores de configuración o dependencias externas esperables en runtime (por ejemplo `missing_secret`, credenciales ausentes, provider temporalmente no disponible) **MUST NOT** terminar el proceso del nodo.
- Ante estos casos, el nodo **MUST**:
  - responder al mensaje `user` con error estructurado (por ejemplo `ErrorV1Payload`/`text-v1` equivalente),
  - marcar `retryable` según corresponda,
  - continuar operativo para `PING/STATUS/CONFIG_GET/CONFIG_SET` y para mensajes posteriores.
- Los errores fatales deben reservarse para condiciones realmente irrecuperables del proceso (corrupción interna, imposibilidad de inicializar runtime base, etc.).

### 2.1.3 `CONFIG_RESPONSE`: confirmación

✅ **NORMATIVO**:
- Toda respuesta a `CONFIG_SET` **MUST** ser `CONFIG_RESPONSE` e incluir:
  - `ok: boolean`
  - `schema_version`
  - `config_version`
  - `error` (si `ok=false`)
  - `effective_config` (si `ok=true`, sin secretos en claro)

### 2.1.4 Secrets (v1) — `CONFIG_SET` + `secrets.json` local

✅ **NORMATIVO (v1)**:
- Para providers que requieren credenciales (p.ej. OpenAI), `CONFIG_SET` **MAY** incluir el secreto inline dentro de `config.secrets.*` para permitir operación sin intervención humana.
- El campo canónico actual para OpenAI es:
  - `config.secrets.openai.api_key`
- El nodo persiste el secreto en un archivo local:
  - `/var/lib/fluxbee/nodes/<TYPE>/<node@hive>/secrets.json`


#### Precedencia de secretos (HOY vs runtime)

✅ **NORMATIVO (HOY)**:
- **Fuente primaria** (si está presente): secreto en `secrets.json` local del nodo.
- `CONFIG_SET` con `config.secrets.openai.api_key` debe persistir el secreto en `secrets.json`.
- Durante migración, el nodo **MAY** aceptar aliases legacy (`behavior.openai.api_key`, `behavior.api_key`) y migrarlos a `secrets.json`.
- En ningún caso el nodo debe persistir el secreto inline en `${STATE_DIR}` ni dejarlo en el `effective_config` devuelto por `CONFIG_GET`.

✅ **NORMATIVO (en reinicio)**:
- Tras restart, el nodo debe poder volver a `CONFIGURED` usando el secreto guardado en `secrets.json`.
- Si no existe secreto local y no hay otra fuente de compatibilidad temporal, el nodo queda `FAILED_CONFIG (missing_secret)` hasta recibir nuevamente `CONFIG_SET`.

🧩 **A ESPECIFICAR (futuro)**:
- Cuando exista un gestor de secretos y `api_key_ref`, la precedencia recomendada será:
  - `api_key_ref` (gestor) > `secrets.json` local > compat legacy temporal.

⚠️ **ADVERTENCIA (v1)**:
- El nodo **MUST NOT** persistir secretos en claro dentro de `${STATE_DIR}`.
- `CONFIG_GET`, `CONFIG_RESPONSE`, status y logs no deben devolver el valor plano del secreto.
- La metadata del secreto se expone por `contract.secrets[*]`, no por el valor efectivo.

🧩 **A ESPECIFICAR (futuro)**:
- Uso de `api_key_ref` (env/file/vault/kms) y un gestor de secretos (orchestrator/admin o servicio dedicado) para rotación y persistencia segura.
- Actualización de key: se materializa como un nuevo `CONFIG_SET` (con `merge_patch` o `replace`) que actualiza la referencia o el valor.

### 2.1.5 Orden e idempotencia de updates (`config_version`) sin cambiar source-of-truth

✅ **NORMATIVO**:
- **Source of truth** del `config_version` efectivo y de la configuración aplicada es **el nodo AI** (por su persistencia y `CONFIG_GET/STATUS`).
- Orchestrator/admin debe **alinearse** al nodo consultando `CONFIG_GET` cuando no conozca la versión actual.

✅ **NORMATIVO**:
- `config_version` es monotónico (creciente).
- El nodo:
  - **MUST** rechazar `CONFIG_SET` con `config_version` menor a la aplicada (`stale_config_version`).
  - **MAY** tratar `config_version` igual como idempotente (responder ok sin cambios, o re-aplicar sin efectos secundarios).
  - **MUST** aceptar `config_version` mayor (si valida schema) y aplicar/persistir.

✅ **NORMATIVO**: para el primer provision:
- si el nodo está `UNCONFIGURED`, `CONFIG_GET` debe exponer `config_version = 0` (o equivalente), y el primer `CONFIG_SET` usa `config_version = 1`.

✅ **NORMATIVO**: rollback operacional:
- se realiza enviando un nuevo `CONFIG_SET` con `config_version` mayor que restaura valores previos (por `replace` o `merge_patch`).


## 3. Persistencia (coherente con Fluxbee)

✅ **NORMATIVO**: se define un patrón consistente con Fluxbee:

- `CONFIG_DIR` (por defecto): `/etc/fluxbee`
- `STATE_DIR` (por defecto): `/var/lib/fluxbee/state`

### 3.1 Config operator-managed (YAML)

✅ **NORMATIVO**:
- Ubicación recomendada: `${CONFIG_DIR}/ai-nodes/<name>.yaml`
- Ejemplo: `/etc/fluxbee/ai-nodes/support-bot.yaml`

> ✅ **NORMATIVO**: este YAML puede no existir; el nodo debe poder iniciar sin él.

### 3.2 Config dinámica persistida (overlay)

✅ **NORMATIVO**:
- Ubicación recomendada: `${STATE_DIR}/ai-nodes/<name>.json`
- Ejemplo: `/var/lib/fluxbee/state/ai-nodes/support-bot.json`

✅ **NORMATIVO**: el archivo persistido **DEBE** incluir al menos:
- `schema_version` (u32)
- `config_version` (u64; monotónico, para evitar regresiones)
- `node_name` (string)
- `config` (objeto: configuración efectiva o overlay)
- `updated_at` (RFC3339 o epoch; opcional pero recomendado)

⚠️ **TENTATIVO**:
- Si existe una capa central de config distribuida, el nodo podría re-hidratar al arranque sin archivo local.  
  Por ahora, se mantiene **persistencia local** como requisito para resiliencia/offline.

---

## 4. Reglas de procesamiento por tipo de mensaje

✅ **NORMATIVO**: el nodo **DEBE** actuar según `meta.type` (del protocolo Fluxbee):

### 4.1 En estado `UNCONFIGURED`

✅ **NORMATIVO**:
- Si `meta.type ∈ {"system","admin"}` → **MUST** procesar el mensaje (Control Plane).
- Si `meta.type == "user"` → **MUST** responder error `node_not_configured`.
- Para otros valores → **SHOULD** responder `unsupported_msg_type` o ignorar con log (definición final TBD).

### 4.2 En estado `CONFIGURED`

✅ **NORMATIVO**:
- `meta.type == "user"` → **MUST** procesar por Data Plane (comportamiento AI).
- `meta.type ∈ {"system","admin"}` → **MUST** procesar Control Plane (status/config/health).
- Otros tipos → policy TBD (recomendado: error explícito si hay `routing.src`).

---

## 5. Mensajes Control Plane (system/admin)

✅ **NORMATIVO**: Los mensajes de plano de control se identifican por:
- `meta.type ∈ {"system","admin"}`
- `meta.msg` (comando del Control Plane)

✅ **NORMATIVO**: Si `meta.msg` es desconocido, el nodo **MUST** responder `unknown_system_msg`.

### 5.1 PING

✅ **NORMATIVO**
- **Request**:
  - `meta.type`: `"system"` o `"admin"`
  - `meta.msg`: `"PING"`
  - `payload`: puede omitirse o ser vacío
- **Response**:
  - `meta.msg`: `"PONG"`
  - `payload`: opcional (ej. build info mínima)

### 5.2 STATUS

✅ **NORMATIVO**
- **Request**:
  - `meta.type`: `"system"` o `"admin"`
  - `meta.msg`: `"STATUS"`
- **Response** (`meta.msg = "STATUS_RESPONSE"`):
  - `state`: `"UNCONFIGURED" | "CONFIGURED"`
  - `node_name`: string
  - `behavior_kind`: string (si aplica)
  - `config_source`: `"yaml" | "persisted" | "none"`
  - `config_version`: u64 (si aplica)
  - `last_error`: objeto opcional (code/message/at)

✅ **NORMATIVO**: `STATUS` debe funcionar incluso en `UNCONFIGURED`.

🧩 **A ESPECIFICAR (alineación Fluxbee)**: el payload exacto de `STATUS_RESPONSE` debe alinearse con la especificación global de status/health de Fluxbee cuando exista. Hasta entonces, los campos listados son el mínimo recomendado.

### 5.3 CONFIG_GET

✅ **NORMATIVO**
- **Request**:
  - `meta.type`: `"system"` o `"admin"`
  - `meta.msg`: `"CONFIG_GET"`
  - `payload`: opcional (puede incluir `node_name` si el nodo sirve múltiples identidades)
- **Response** (`meta.msg = "CONFIG_RESPONSE"`):
  - `ok`: boolean
  - `config_source`: `"yaml" | "persisted" | "none"`
  - `config_version`: u64 (si aplica)
  - `config`: objeto (si existe)
  - `error`: opcional (code/message)

### 5.4 CONFIG_SET

✅ **NORMATIVO**
- **Request**:
  - `meta.type`: `"system"` o `"admin"`
  - `meta.msg`: `"CONFIG_SET"`
  - `payload` **MUST** incluir:
    - `subsystem`: `"ai_node"`
    - `node_name`: string
    - `config_version`: u64 (monotónico)
    - `config`: objeto (la config a aplicar)
    - `apply`: `"immediate" | "next_reload"` (default `"immediate"`)
- **Validación**:
  - `config_version` **MUST** ser `>=` a la última aplicada/persistida (si no → `stale_config_version`)
  - `config` **MUST** validar contra el schema de configuración del nodo AI
- **Acción**:
  - Persistir config en `${STATE_DIR}/ai-nodes/<name>.json`
  - Aplicar según `apply`
- **Response** (`meta.msg = "CONFIG_RESPONSE"`):
  - `ok`: boolean
  - `config_version`: u64
  - `error`: opcional (code/message)

⚠️ **TENTATIVO**:
- Alternativa de naming para alineación con otras partes del sistema: usar `meta.msg="CONFIG_CHANGED"` con `payload.subsystem="ai_nodes"`.  
  Por ahora se define `CONFIG_SET/GET` para claridad en AI Nodes; revisar con equipo de plataforma.

---

## 6. Errores del Control Plane

🧩 **TBD**: Se define el formato de error final (ver sección “Errores” global).  
Por ahora el Control Plane **DEBE** poder responder al menos con:
- `ErrorV1Payload` (opción B) **o**
- `TextV1Payload` con prefijo estándar `[error:<code>] ...` (opción A)

Códigos recomendados (no exhaustivo):
- `unknown_system_msg`
- `invalid_config`
- `stale_config_version`
- `node_not_configured`
- `unsupported_msg_type`

---

## 7. Política de reply / prevención de loops

✅ **NORMATIVO (por ahora; opción 1)**:
- El nodo **SOLO** puede auto-reply (setear `routing.dst = incoming.routing.src`) para mensajes `meta.type == "user"` (Data Plane).
- Para mensajes `system/admin` (Control Plane), el reply se rige por el contrato del Control Plane:
  - si el request trae `routing.src`, se responde a ese origen;
  - si `routing.src` está vacío (broadcast), se permite no responder o responder por canal configurado (🧩 TBD).

⚠️ **TENTATIVO / A REVISAR**: Casos a ejemplificar y validar con el equipo:
- **Caso A**: IO gateway → AI node (`user`) → reply al gateway (ok).
- **Caso B**: AI node → AI node (`user` mal clasificado) → riesgo de loop de respuestas.
- **Caso C**: Orchestrator/system → AI node (`system/admin`) → respuesta control-plane (ok).
- **Caso D**: Usuario escribe antes de config (`user`, UNCONFIGURED) → error `node_not_configured` (ok).

---

## 8. Desviaciones conocidas del código actual (para backlog de corrección)

🐞 **DESVIACIÓN CÓDIGO (P0)**: el runner actual ignora mensajes donde `meta.type == "system"` y no responde.  
✅ **NORMATIVO**: el nodo **MUST** procesar `system/admin` para Control Plane (CONFIG/STATUS/PING).

---

## 9. Fuera de alcance / referencias

🧩 **TBD**:
- “Cognitive enrichment” y campos derivados (`ctx_window`, `memory_package`, etc.) quedan fuera de alcance por ahora; el AI Node debe tratarlos como *opaque* y no depender de ellos.
- Formato definitivo de errores (ErrorV1Payload) se define en la sección global de “Errores” o en el protocolo.

---

**Siguiente parte prevista:** Data Plane (contrato de input/output, payload text + attachments/blob resolution, behaviors, idempotencia, reintentos, observabilidad).

---

## 2. Data Plane

### Reglas claras para blobs (consumo y creación)

> ✅ **NORMATIVO**: Estas reglas aplican a cualquier nodo `AI.*` cuando el contenido/adjuntos no pueden (o no deben) viajar inline dentro del mensaje JSON.

#### Por qué existen
- Los mensajes Fluxbee viajan como **JSON** y tienen un límite de tamaño (≈ **64KB** para el mensaje completo).
- Para contenido grande (texto largo o adjuntos binarios), el mensaje debe llevar solo un **puntero liviano** (`BlobRef`) y el contenido real vive en el repositorio de blobs.

#### `staging/` vs `active/` (modelo mental)
- `staging/` = **workspace local**: el productor escribe acá mientras “está armando” el archivo.
- `active/` = **consumible**: solo lo que está en `active/` puede ser referenciado por mensajes y consumido por otros nodos.

✅ **NORMATIVO**:
- Un nodo **NO debe consumir** blobs desde `staging/`.
- Un nodo **NO debe referenciar** (en mensajes) blobs que todavía estén en `staging/`.

#### Consumir blobs (cuando el AI node recibe `attachments` o `content_ref`)
✅ **NORMATIVO**:
1) Si el payload trae `content_ref`: el texto principal se lee desde ese blob.
2) Si el payload trae `attachments`: el nodo decide si los soporta (según `mime`/capabilities) y, si los soporta, los lee desde `active/`.
3) Si el blob no existe / no es legible: responder error (usar códigos canónicos del Blob Annex (BLOB_*)).

**Implementación**:
- La spec Fluxbee define helpers en `fluxbee_sdk::blob` (p.ej. `resolve(...)` / `resolve_with_retry(...)`).
- ✅ **NORMATIVO**: AI Nodes deben apoyarse en esos helpers (directo o vía wrapper del AI SDK).

#### Crear blobs (cuando el AI node produce adjuntos o texto grande)
✅ **NORMATIVO**:
1) Escribir el contenido en `staging/` (archivo temporal).
2) Finalizar escritura y **promover** (`promote`) de `staging/` → `active/` (operación atómica).
3) Solo después de promover, incluir el `BlobRef` en:
   - `payload.attachments[]` (adjunto), o
   - `payload.content_ref` (texto largo que no entra inline).

**Cuándo usar `content_ref`**:
- ✅ **NORMATIVO**: si el mensaje JSON completo excedería el límite inline (~64KB), el productor debe mover el contenido grande a blob y referenciarlo con `content_ref` (omitiendo `content`).

**Implementación**:
- La spec Fluxbee define operaciones en `fluxbee_sdk::blob` (p.ej. `put/put_bytes` + `promote`).
- ✅ **NORMATIVO**: el **nodo productor** es responsable de invocar estas operaciones en el momento correcto (no es automático).

#### Dónde vive esta lógica: Fluxbee SDK vs AI SDK
✅ **NORMATIVO**:
- La **fuente de verdad** de blobs (layout, `BlobRef`, `put/promote/resolve`, reglas `staging/active`) es el **SDK de Fluxbee** (Blob Annex).
- El **AI Nodes SDK** que estamos creando **puede** proveer utilidades de conveniencia (por ejemplo: `ai_sdk::blob::read_text(payload)` o `ai_sdk::blob::maybe_offload_text(content)`), pero:
  - no debe redefinir el contrato ni divergir del Blob Annex;
  - debe delegar en `fluxbee_sdk::blob` para mantener consistencia global.



> ✅ **Objetivo**: Esta entrega define el **Data Plane** de un nodo `AI.*`:  
> contrato de entrada/salida para mensajes `meta.type="user"`, parsing del contrato `text/v1`, resolución de blobs/attachments según la **spec de Blob Annex**, construcción de input para el behavior, formato de respuesta y reglas de error.  
> *Cognitive* queda fuera de alcance (solo se lo menciona como “a especificar”).

### Estado de cierre (contrato vs implementación)

✅ **CONTRATO CERRADO (especificación)**:
- `text/v1` con `content`, `attachments[]` y `content_ref`.
- Resolución/creación de blobs delegando en `fluxbee_sdk::blob`.
- Reglas de error para blobs/adjuntos (`BLOB_*`, `unsupported_attachment_mime`, etc.).

🧩 **CONCEPTO AÚN ABIERTO (especificación)**:
- Límites finales por MIME/tamaño están como valores tentativos.
- Tabla final de compatibilidad MIME × behavior × provider.

🔧 **IMPLEMENTACIÓN ACTUAL (runner + SDK AI)**:
- Input `text/v1` completo: `content`, `content_ref`, `attachments[]`.
- Resolución de blobs vía `fluxbee_sdk::blob` desde `fluxbee_ai_sdk::text_payload`.
- Errores canónicos implementados: `BLOB_NOT_FOUND`, `BLOB_IO_ERROR`, `BLOB_TOO_LARGE`, `unsupported_attachment_mime`, `too_many_attachments`.
- Salida `text/v1` con offload automático a `content_ref` cuando excede límite inline.
- Adjuntos de salida generados por behavior quedan para fase posterior (cuando behaviors produzcan artefactos).

---

## Convenciones de estado del documento

- ✅ **NORMATIVO**: requisito cerrado (MUST/SHOULD/MAY).
- ⚠️ **TENTATIVO**: decisión provisional; debe revisarse.
- 🧩 **A ESPECIFICAR**: se documenta la necesidad, pero falta definir/implementar si surge necesidad.
- 🐞 **DESVIACIÓN CÓDIGO**: comportamiento actual que contradice lo normativo.

---

### Campos L3 de contexto (ctx, ctx_seq, ctx_window)

✅ **NORMATIVO (canónico en protocolo)**:
- `ctx` y `ctx_seq` se tratan como campos de contexto L3 del mensaje.
- `ctx_window` se define como historia reciente agregada por router.

⚠️ **TOLERANCIA OPERATIVA (HOY)**:
- En el código core revisado, `ctx_window` no aparece implementado/inyectado por el router (solo está documentado).  
  Por lo tanto, AI Nodes **MUST** tolerar `ctx_window` faltante:
  - loggear advertencia (`missing_ctx_window`),
  - continuar procesando usando `payload.content`/`content_ref` y attachments disponibles.
- Cuando el router lo implemente, `ctx_window` pasará a ser **obligatorio** para el “happy path” (sin cambiar el contrato de AI Nodes).



### `src_ilk` siempre presente (incluye ILK temporal)

✅ **NORMATIVO (alineación core)**:
- Los mensajes `user` deben incluir `src_ilk` como ILK **siempre presente**.
- `src_ilk` puede estar en estado **temporary** (identidad incompleta). En ese caso, nodos como `AI.frontdesk.gov` actúan para completar/actualizar la identidad a través del pipeline de sistema (no “inventan” identity).

> Nota: los detalles del provisioning/upgrade viven en core (SY.admin/SY.orchestrator/SY.identity); el nodo AI solo consume `src_ilk`.


## 1. Alcance del Data Plane

✅ **NORMATIVO**: esta sección aplica a mensajes con:

- `meta.type == "user"` **y**
- el nodo está en estado `CONFIGURED`.

En estado `UNCONFIGURED`, los mensajes `user` se rechazan según Control Plane (Parte 1).

---

## 2. Contrato estándar `text/v1` (Fluxbee)

✅ **NORMATIVO**: los AI Nodes implementan el contrato **`text/v1`** definido por Fluxbee (ver `blob-annex-spec.md`, sección “Contrato de Payload: text/v1”).

Estructura:

```json
{
  "payload": {
    "type": "text",
    "content": "string | vacío",
    "attachments": [ BlobRef, ... ]
  }
}
```

✅ **NORMATIVO**:
- `payload.type` debe ser `"text"` para este contrato.
- `payload.attachments` **debe** existir (array; vacío si no hay adjuntos).
- `payload.content` **debe** existir (string; puede ser vacío si solo hay attachments).

### 2.1 `content_ref` (texto largo promovido a blob)

✅ **NORMATIVO**: el contrato `text/v1` permite `content_ref` (ver `blob-annex-spec.md`):
- cuando `content_ref` está presente, `content` se omite,
- el consumidor lee el texto desde el blob referenciado.

---

## 3. BlobRef y resolución de blobs (Fluxbee Blob Annex)

✅ **NORMATIVO**: `attachments[]` y `content_ref` referencian blobs mediante **`BlobRef` canónico**, definido en `blob-annex-spec.md` (sección “BlobRef — Estructura canónica”).

✅ **NORMATIVO**: el repositorio blob y su layout se definen en `blob-annex-spec.md` (sección “Repositorio Blob / Layout”).

### 3.1 Layout y path de lectura

✅ **NORMATIVO**:
- Los blobs referenciados por mensajes **viven en** `/var/lib/fluxbee/blob/active/…`
- El prefix del subdirectorio se deriva del `blob_name` (2 chars posteriores al último `_`), tal como define el Blob Annex.
- El nodo AI **MUST NOT** leer blobs desde `staging/` (staging es workspace, no consumo).

### 3.2 Política de lectura (errores)

✅ **NORMATIVO**: ante `attachments` o `content_ref`, el nodo AI:

- **MUST** intentar leer cada blob referenciado.
- Si el blob no existe o no es legible:
  - **MUST** responder error `BLOB_NOT_FOUND` o `BLOB_IO_ERROR`.
- Si el blob existe pero excede límites operativos (size/mime):
  - **MUST** responder error `BLOB_TOO_LARGE` o `unsupported_attachment_mime`.

✅ **NORMATIVO (baseline v1)**:
- `text/plain` / `text/markdown` / `application/json`: hasta 10MB por blob (límite operativo vigente del SDK AI).
- `application/pdf`: rechazado por default (`unsupported_attachment_mime`) hasta definir extractor dedicado.
- imágenes (`image/png`, `image/jpeg`, `image/webp`): permitidas solo cuando `multimodal=true`.
- `image/gif`: rechazado por default en baseline v1 (puede revisarse en una fase posterior).

---

## 4. Construcción de “entrada” para el behavior (Model Input Contract)

✅ **NORMATIVO**: el AI Node transforma `text/v1` (texto + blobs) en una representación de entrada compatible con el behavior activo.

### 4.1 Texto principal

✅ **NORMATIVO**:
- `main_text` se obtiene así:
  1) Si existe `payload.content_ref`: leer blob → `main_text`
  2) Caso contrario: usar `payload.content` → `main_text`

### 4.2 Attachments (cómo influyen en el input)

✅ **NORMATIVO (MVP)**: categorías:

1) **Adjuntos textuales** (`mime` in `{"text/plain","text/markdown","application/json"}`):
   - **MUST** leerse y agregarse al input como “contexto adjunto” con separador estable.
2) **Adjuntos no textuales** (PDF, imágenes, audio, etc.):
   - Se rigen por capacidades del behavior/provider:
     - si `multimodal=true`: pasar el blob según adapter del provider,
     - si `multimodal=false`: comportamiento normado en 4.3.

### 4.3 PDFs cuando `multimodal=false`

✅ **NORMATIVO (decisión cerrada)**:
- Si `multimodal=false` y el nodo recibe un attachment con `mime="application/pdf"`:
  - **MUST** responder `unsupported_attachment_mime` (rechazar PDF).

🧩 **A ESPECIFICAR / IMPLEMENTAR SI SURGE NECESIDAD (futuro)**:
- Alternativas posibles si se decide soportar PDFs en modo no multimodal:
  - **Opción B**: permitir `extract_text` si existe extractor instalado y el behavior lo habilita; si no, error.
  - **Opción C**: intentar extracción siempre (dependencia obligatoria; mayor costo y riesgos).

### 4.4 Formato de ensamblado (prompt assembly)

✅ **NORMATIVO (MVP)**: cuando existan adjuntos textuales, se ensamblan así:

```
<main_text>

--- attachments ---
[attachment: <filename_original> | <mime> | <size>]
<contenido>
[attachment_end]
...
--- end attachments ---
```

✅ **NORMATIVO**:
- Ensamblado determinístico.
- Incluye metadata mínima (filename/mime/size).

⚠️ **TENTATIVO**:
- Orden de attachments: el orden del array `payload.attachments`.

---

## 5. Output Contract (respuesta del AI Node)

### 5.1 Payload de respuesta (MVP)

✅ **NORMATIVO**: por defecto, el AI Node responde con `text/v1`:

```json
{
  "payload": {
    "type": "text",
    "content": "respuesta del modelo",
    "attachments": []
  }
}
```

✅ **NORMATIVO**:
- `attachments` debe existir (vacío si no hay adjuntos en salida).
- Si el nodo genera artefactos (PDF/imagen/etc.), debe:
  1) escribirlos al blob storage (staging → promover a active) según Blob Annex,
  2) devolver `BlobRef` en `payload.attachments`.

### 5.2 Routing de respuesta (anti-loop)

🧩 **A ESPECIFICAR (decisión pendiente)**:
- La política exacta de auto-reply y prevención de loops AI↔AI se define más adelante.
- Base acordada (Parte 1): auto-reply solo para `meta.type="user"`.
- Opciones documentadas:
  - allowlist de orígenes (sugerida previamente),
  - denylist por prefijo `AI.*`,
  - no auto-reply (dst siempre explícito).

---

## 6. Meta handling y campos “opaque”

✅ **NORMATIVO**:
- El AI Node **MUST** preservar `meta` recibido (clonar) en su respuesta, salvo campos reservados futuros.
- El AI Node **MUST NOT** depender de enriquecimientos de *cognitive* (`ctx_window`, etc.) para operar (puede ignorarlos sin romperse).

🧩 **A ESPECIFICAR**:
- Consumo/enriquecimiento con outputs de cognitive cuando se rediseñe cognition.

---

## 7. Errores (Data Plane)

### ErrorV1Payload (AI Nodes)

> ✅ **NORMATIVO (AI Nodes)**: los nodos `AI.*` deben responder errores usando un payload explícito de tipo `error` cuando sea posible.  
> 🧩 **A ESPECIFICAR (a nivel sistema Fluxbee)**: este formato es candidato a estándar global; por ahora es normativo solo para AI Nodes.

#### Estructura

```json
{
  "payload": {
    "type": "error",
    "code": "invalid_config",
    "message": "Missing required field: behavior.model",
    "retryable": false,
    "details": {
      "path": "behavior.model",
      "reason": "required"
    }
  }
}
```

#### Campos

✅ **Requeridos**:
- `type`: `"error"`
- `code`: string (catálogo AI + códigos `BLOB_*`)
- `message`: string corto, humano (no debe incluir secretos)

✅ **Opcionales**:
- `retryable`: boolean (default `false`)
- `details`: object (libre; recomendado `path/reason` para validación y `limit/actual` para límites)

#### Reglas

✅ **NORMATIVO (alineación Fluxbee)**:
- En mensajes de **Control Plane** (por ejemplo `meta.msg="CONFIG_RESPONSE"`), el error debe viajar como parte del payload del comando (`status: "error"` + `error: {...}`), y **no** como `payload.type="error"`.

✅ **NORMATIVO**:
- Errores provenientes del Blob Annex deben usar `code` canónico `BLOB_*` (ver sección de errores de blobs).
- En Control Plane (`CONFIG_SET/GET/STATUS/PING`), el nodo debe usar `ErrorV1Payload` para errores estructurales/validación cuando sea posible.
- Si por compatibilidad el consumer no soporta `type="error"`, el nodo **MAY** responder en `text/v1` con prefijo `[error:<code>] ...` (fallback).



🧩 **A ESPECIFICAR (decisión previa)**:
- El formato definitivo de errores se define más adelante.
- Por ahora se documentan **dos opciones**:
  - **Opción A**: `text/v1` con prefijo `[error:<code>] …`
  - **Opción B**: `ErrorV1Payload` (preferida a futuro)

### 7.1 Códigos de error (alineados a Fluxbee)

✅ **NORMATIVO**: Cuando el error proviene del **Blob Annex** (lectura/escritura/validación de `BlobRef`), el AI Node debe usar los códigos canónicos:

- `BLOB_NOT_FOUND`
- `BLOB_IO_ERROR`
- `BLOB_INVALID_NAME`
- `BLOB_TOO_LARGE`
- `BLOB_HASH_MISMATCH` *(si aplica a validaciones de integridad)*

🧩 **A ESPECIFICAR**: Otros errores del AI Node (no definidos en el Blob Annex) quedan pendientes de normalización a nivel plataforma:

- `unsupported_payload_type`
- `invalid_payload`
- `unsupported_attachment_mime`
- `provider_error`
- `rate_limited`
- `timeout`

---

## 8. Idempotencia y reintentos

✅ **NORMATIVO (mínimo)**:
- El AI Node **SHOULD** ser idempotente por `trace_id` cuando sea viable.
- Si no implementa cache idempotente en MVP, **MUST** evitar efectos secundarios innecesarios (p.ej. blobs duplicados).

---

## 9. Observabilidad (mínimo)

✅ **NORMATIVO**:
- Log estructurado por request:
  - `trace_id`, `node_name`, `behavior_kind`, `provider`, `latency_ms`, `status=ok|error`, `error_code` (si aplica)

---

## 10. Desviaciones conocidas del código actual (para backlog)

🔧 **ESTADO CÓDIGO (actual)**:
- El runner consume `text/v1` completo (`content` / `content_ref` / `attachments`) usando SDK AI.
- El contrato de error de blobs está normalizado en SDK AI y propagado como payload de error.

✅ **NORMATIVO**: el AI Node implementa `text/v1` completo según Blob Annex y falla con errores claros cuando no pueda leer blobs/attachments.

---

## 3. Behaviors & Providers

> ✅ **Objetivo**: Esta entrega define el **Behavior Layer** (qué hace un nodo AI) y el **Provider Layer** (cómo integra modelos externos o locales), incluyendo:
> - catálogo de behaviors (MVP + extensibilidad),
> - adapters por provider (`openai`, `anthropic`, `local`),
> - multimodal y manejo de attachments,
> - límites, timeouts y rate limiting,
> - configuración YAML y dinámica (`CONFIG_SET`) asociada a behaviors,
> - tools/handoffs como “a especificar e implementar si surge necesidad”.

---

## Convenciones de estado del documento

- ✅ **NORMATIVO**: requisito cerrado (MUST/SHOULD/MAY).
- ⚠️ **TENTATIVO**: decisión provisional; debe revisarse.
- 🧩 **A ESPECIFICAR**: se documenta la necesidad, pero falta definir/implementar si surge necesidad.
- 🐞 **DESVIACIÓN CÓDIGO**: comportamiento actual que contradice lo normativo.

---

## 1. Qué es un Behavior

✅ **NORMATIVO**: Un **behavior** es la semántica observable del nodo AI:

- consume un input `text/v1` (Parte 2),
- aplica una política de prompting/transformación,
- invoca un **provider** (o no, en behaviors locales),
- produce un output `text/v1` y opcionalmente attachments de salida.

✅ **NORMATIVO**: el behavior activo se selecciona por configuración efectiva del nodo (`behavior.kind`).

### 1.1 Separación conceptual behavior/provider y port del Agents SDK (D3.1)

✅ **NORMATIVO**:
- Se recomienda mantener una separación interna clara (behavior vs provider adapter), para:
  - facilitar agregar providers (`anthropic`/`local`) sin tocar data plane,
  - facilitar portar/adaptar un SDK tipo Agents (loop/tooling) sin comprometer el resto,
  - reducir acoplamientos en el runner.

> Nota: esto no implica tool calling; es modularidad para integrar “cliente/SDK de modelos” como pieza intercambiable.

---

## 2. Catálogo de behaviors

### 2.1 Behaviors mínimos (MVP)

✅ **NORMATIVO**: el sistema soporta al menos:

1) `echo`
- Responde `Echo: <main_text>` (pruebas).
- Provider: ninguno.
- Multimodal: no.
- Attachments: ignora y devuelve vacío.

2) `openai_chat`
- Responde usando provider `openai`.
- Multimodal: depende de modelo configurado/capabilities.

🐞 **DESVIACIÓN CÓDIGO**:
- El runner actual implementa `echo` y `openai_chat`, pero no consume attachments ni aplica errores normativos (Parte 2).

### 2.2 Behaviors opcionales

🧩 **A ESPECIFICAR / IMPLEMENTAR SI SURGE NECESIDAD**:
- `anthropic_chat`
- `local_llm_chat`
- `moderation_gate`
- `summarize`
- `extract_structured`

---

## 3. Provider Layer (adapters)

✅ **NORMATIVO**: Un **provider adapter** traduce:

- `ModelInput` (texto + attachments resueltos) → request del proveedor
- response del proveedor → `text/v1` (y/o blobs de salida)

✅ **NORMATIVO**: Un behavior declara:
- `provider` (o `none`)
- `capabilities.multimodal` (true/false)
- `params` (mapa de parámetros específicos)

### 3.1 Provider: OpenAI

✅ **NORMATIVO**: config mínima:
- `model`
- `api_key_ref` (ver Secrets)
- `timeout_ms`
- `max_output_tokens` (si aplica)
- `temperature` (si aplica)

🧩 **A ESPECIFICAR**:
- Tool calling hosted y mapeo a tools Fluxbee.
- Sesiones/threads nativas del provider.

### 3.2 Provider: Anthropic

🧩 **A ESPECIFICAR / IMPLEMENTAR SI SURGE NECESIDAD**:
- Interfaz equivalente (`model`, `api_key_ref`, `timeout_ms`, etc.).
- Debe respetar el `ModelInput Contract` (Parte 2) y los códigos de error.

### 3.3 Provider: Local

🧩 **A ESPECIFICAR / IMPLEMENTAR SI SURGE NECESIDAD**:
- Runtime local (vLLM/TGI/llama.cpp/etc.) por HTTP o IPC.
- Debe exponer límites y declarar soporte multimodal.

---

## 4. Multimodal y attachments por behavior

### 4.0 Defaults por runtime (decisión cerrada)

✅ **NORMATIVO**:
- `AI.common`: `capabilities.multimodal=true` por default.
- `AI.frontdesk.gov`: `capabilities.multimodal=false` por default (temporal, revisable).
- Ambos runtimes permiten override explícito vía config (`behavior.capabilities.multimodal`).

✅ **NORMATIVO**:
- La implementación multimodal del adapter OpenAI se alinea al modelo operativo vigente de OpenAI (Agents SDK + Responses API), evitando formatos ad-hoc cuando exista forma estándar en OpenAI.

⚠️ **Estado de implementación (2026-04-01)**:
- El camino de adjuntos en AI está avanzado, pero todavía no está cerrado end-to-end para todos los tipos.
- Referencia operativa y comparativa contra Agents SDK: `docs/onworking NOE/ai_attachments_status.md`.
- Pendientes principales: flujo `input_file` (PDF) y render de `payload.type="error"` en `IO.slack`.

✅ **NORMATIVO**:
- El behavior declara `capabilities.multimodal = true|false`.
- Si `multimodal=false`:
  - adjuntos no textuales → error `unsupported_attachment_mime` (y PDFs se rechazan por ahora; ver Parte 2).
- Si `multimodal=true`:
  - el provider adapter envía adjuntos siguiendo el esquema OpenAI vigente para inputs multimodales.

✅ **NORMATIVO (matriz inicial MIME×modo)**:
- `text/plain`, `text/markdown`, `application/json`: permitido en ambos modos (se incorporan al contexto textual).
- `image/png`, `image/jpeg`, `image/webp`: permitido solo con `multimodal=true`.
- `application/pdf`, `image/gif` y resto no textual: `unsupported_attachment_mime` en baseline v1.

---

## 5. Límites, timeouts y rate limiting

✅ **NORMATIVO**: cada behavior soporta al menos:
- `timeout_ms`
- `max_input_bytes`
- `max_output_tokens` (si aplica)
- `max_attachments`
- `max_attachment_bytes`

✅ **NORMATIVO**:
- exceder límites → errores (`input_too_large`, `BLOB_TOO_LARGE`, `too_many_attachments`)
- rate limit → `rate_limited`

---

## 6. Configuración del behavior

✅ **NORMATIVO (vista conceptual)**:

```yaml
node_name: "support-bot"
behavior:
  kind: "openai_chat"
  provider: "openai"
  capabilities:
    multimodal: false
  params:
    model: "gpt-4.1-mini"
    system_prompt: "..."
    timeout_ms: 15000
    max_output_tokens: 512
    temperature: 0.2
limits:
  max_input_bytes: 524288
  max_attachments: 8
  max_attachment_bytes: 10485760
secrets:
  api_key_ref: "env:OPENAI_API_KEY"
```

---

## 7. Config dinámica: `CONFIG_SET` con `merge_patch` (RFC 7396)

✅ **NORMATIVO (decisión cerrada)**:
- `CONFIG_SET.payload` **MUST** incluir:
  - `apply_mode`: `"replace" | "merge_patch"`
- Para `"merge_patch"` se aplica **JSON Merge Patch (RFC 7396)**:
  - objetos: merge por claves,
  - `null`: elimina el campo,
  - arrays: se reemplazan completos.

✅ **NORMATIVO**:
- Si `apply_mode` no está presente → error `invalid_config` (falta campo requerido).
- El nodo **MUST** persistir la config resultante (effective) con `config_version` actualizado (Parte 1).

> Nota: esto habilita updates parciales (p.ej. cambiar solo `behavior.params.model` sin reenviar `system_prompt`).

---

## 8. Secrets (credenciales)

✅ **NORMATIVO**: las credenciales no deben persistirse en claro en `STATE_DIR`.

✅ **NORMATIVO**: se define `api_key_ref` con backends soportados:
- `env:VAR_NAME`
- `file:/path/to/secret`

🧩 **A ESPECIFICAR / IMPLEMENTAR SI SURGE NECESIDAD**:
- `vault:<path>`, `kms:<key>`, etc.

✅ **NORMATIVO**:
- si el secret no está disponible → error `missing_secret`.

---

## 9. Tools / handoffs

🧩 **A ESPECIFICAR E IMPLEMENTAR SI SURGE LA NECESIDAD**:
- Cómo un behavior declara herramientas.
- Cómo se mapea tool calling de provider a tools Fluxbee.
- Contrato de handoff a otros nodos.

✅ **NORMATIVO**: hasta que se defina, el nodo AI debe operar correctamente en modo **sin tools**.

---

## 10. Versionado

✅ **NORMATIVO**:
- La config dinámica persistida incluye `schema_version`.
- Cambios incompatibles requieren bump de `schema_version` + migración o fallback.

---

## 11. Desviaciones conocidas del código actual (para backlog)

🐞 **DESVIACIÓN CÓDIGO (P1)**:
- No hay validación formal de schema/version.
- No hay soporte `apply_mode=merge_patch` en CONFIG_SET.
- No hay `api_key_ref` en config dinámica (solo config directa).

---

## 4. Schema & Validation

> ✅ **Objetivo**: Esta entrega define cómo se **valida** la configuración de AI Nodes (YAML operator-managed y config dinámica por `CONFIG_SET`), cómo se versiona, y cómo se reportan errores de validación.  
> **No** define *cognitive*. Tampoco define aún el formato final de errores (eso queda “a especificificar”, pero se listan códigos).

---

## Convenciones de estado del documento

- ✅ **NORMATIVO**: requisito cerrado (MUST/SHOULD/MAY).
- ⚠️ **TENTATIVO**: decisión provisional; debe revisarse.
- 🧩 **A ESPECIFICAR**: se documenta la necesidad, pero falta definir/implementar si surge necesidad.
- 🐞 **DESVIACIÓN CÓDIGO**: comportamiento actual que contradice lo normativo.

---

## 1. Tipos de configuración y precedencia

✅ **NORMATIVO**: existen dos fuentes de configuración para nodos AI managed:

1) **Config de orchestrator (spawn/bootstrap)**  
   - Archivo en `/var/lib/fluxbee/nodes/<TYPE>/<node@hive>/config.json`.
   - Writer: `SY.orchestrator` (`set_node_config` / `NODE_CONFIG_SET`).

2) **Config dinámica del nodo (Control Plane)**  
   - Se recibe vía mensajes `system/admin` y se persiste en `${STATE_DIR}/ai-nodes/<name>.json`.
   - Ver Parte 1 (Control Plane) y Parte 3 (`merge_patch` RFC 7396).

✅ **NORMATIVO**: precedencia (effective config):
1) config dinámica persistida por el nodo
2) config de orchestrator (`config.json` spawn/bootstrap)
3) `UNCONFIGURED`

---

## 2. Versionado de schema de configuración

### 2.1 `schema_version` y responsabilidad

✅ **NORMATIVO (decisión cerrada)**:
- En **config dinámica persistida** (`STATE_DIR`) `schema_version` **MUST** estar presente.
- En **YAML operator-managed** `schema_version` es **opcional** por compatibilidad:
  - si falta, se asume `schema_version = 1`.

✅ **Responsabilidad (clarificación)**:
- El **owner** del schema es el equipo que mantiene el **AI node runner / fluxbee_ai_sdk**.
- Un cambio incompatible en la estructura de config (renombre, cambio de tipo, semántica breaking) requiere incremento de `schema_version`.
- Operadores/control plane **no “inventan” versiones**: deben usar una versión soportada por el runner.

### 2.2 Cambios incompatibles

✅ **NORMATIVO**:
- Si el nodo recibe `schema_version` que no soporta:
  - **MUST** rechazar con `invalid_config` y detalle `unsupported_schema_version`.

🧩 **A ESPECIFICAR**:
- Migración automática `schema_version N → N+1` (si se implementa).

---

## 3. Validación: cuándo y cómo

### 3.1 Validación al arranque (aislamiento por nodo)

✅ **NORMATIVO (decisión cerrada)**:
- El runner **MUST** validar cada YAML antes de levantar la instancia lógica del nodo.
- Si **un** YAML es inválido:
  - **MUST** omitir solo ese nodo (no interfiere con los demás),
  - **MUST** continuar levantando el resto de nodos configurados,
  - **MUST** loggear el error y exponerlo en `STATUS_RESPONSE` (Parte 1) con un estado tipo `FAILED_CONFIG` para ese `node.name`.

> Nota: esto preserva el principio “un nodo AI no debería interferir en el funcionamiento de otro”.

### 3.2 Validación en `CONFIG_SET`

✅ **NORMATIVO**:
- El nodo **MUST** validar `CONFIG_SET.payload` antes de persistir o aplicar.
- Si `apply_mode = "merge_patch"`:
  - **MUST** validar:
    1) que el patch sea un JSON válido,
    2) que al aplicar el patch resulte una configuración válida,
    3) que el `schema_version` resultante sea soportado.

✅ **NORMATIVO**:
- Si la validación falla:
  - **MUST** responder `invalid_config` y **MUST NOT** persistir cambios.

---

## 4. Esquema de configuración (schema_version = 1) — primera versión

### Campos obligatorios vs opcionales (defaults) y materialización en JSON

✅ **NORMATIVO**:
- La configuración efectiva vive en `${STATE_DIR}/ai-nodes/<node_name>.json`.
- El nodo **MUST** aplicar defaults razonables a campos opcionales faltantes.
- El nodo **MUST** **materializar** (persistir) esos defaults en el JSON efectivo, de modo que el archivo represente la configuración real en uso.

#### Requeridos (mínimo)

✅ **NORMATIVO** (si falta alguno, `invalid_config`):
- `schema_version`
- `config_version`
- `node.name`
- `behavior.kind`

✅ **NORMATIVO** (requeridos condicionales por behavior):
- Si `behavior.kind = openai_chat`:
  - `behavior.params.model`
  - credencial (HOY): `secrets.openai.api_key` vía `CONFIG_SET`, persistida localmente en `secrets.json`, o futura `api_key_ref` (🧩)

#### Opcionales (con defaults)

✅ **NORMATIVO**:
- `runtime.*` es **opcional**. Si falta:
  - el nodo aplica defaults (timeouts, pool, queue, etc.),
  - y los escribe en el JSON efectivo.
- `behavior.params.*` (temperature/top_p/max_output_tokens/timeouts) es **opcional** salvo los requeridos condicionales.
- `capabilities.*` es opcional:
  - en `AI.common`: `multimodal=true` por default,
  - en `AI.frontdesk.gov`: `multimodal=false` por default.

> Nota: materializar defaults evita “config implícita” y facilita operación y debugging.



✅ **NORMATIVO (v1)**: se define un **mínimo** de campos (required/optional) para operar sin leer código.
Campos extra **no** deben romper (ver §7).

> Importante: esta primera versión busca compatibilidad con el YAML de `docs/onworking/ai_node_runner_config.example.yaml`
mencionado en la “Living Spec” de AI Nodes. fileciteturn2file1L129-L137

### 4.1 Campos mínimos requeridos (v1)

✅ **NORMATIVO**: mínimos para una instancia lógica:

- `node.name` (string, requerido, único)
- `node.router_socket` (string, requerido)
- `behavior.kind` (enum, requerido: `echo` | `openai_chat`)
- Si `behavior.kind = openai_chat`:
  - `behavior.model` (string, requerido)
  - credencial (ver §6/§8): `secrets.openai.api_key` (canónico actual) o `secrets.api_key_ref` (futuro)

### 4.2 YAML canónico (v1, ejemplo)

✅ **NORMATIVO (ejemplo compatible)**:

```yaml
schema_version: 1   # opcional en YAML; si falta se asume 1

node:
  name: "AI.support.l1"
  router_socket: "/var/run/fluxbee/routers"
  uuid_persistence_dir: "/var/lib/fluxbee/state/nodes"
  config_dir: "/etc/fluxbee"

runtime:
  read_timeout_ms: 30000
  handler_timeout_ms: 60000
  write_timeout_ms: 10000
  queue_capacity: 128
  worker_pool_size: 4
  retry_max_attempts: 3
  retry_initial_backoff_ms: 200
  retry_max_backoff_ms: 2000
  metrics_log_interval_ms: 30000

behavior:
  kind: "openai_chat"          # echo | openai_chat
  model: "gpt-4.1-mini"
  base_url: "https://api.openai.com/v1/responses"  # opcional
  model_settings:
    temperature: 0.2
    top_p: 1.0
    max_output_tokens: 256
  instructions:
    source: "inline"   # inline | file | env | none
    value: "You are a concise support assistant."
    trim: true

secrets:
  openai:
    api_key_env: "OPENAI_API_KEY"   # compat temporal si no se cargó aún por CONFIG_SET

> Nota operativa v1:
> - el valor real de `secrets.openai.api_key` entra por `CONFIG_SET`
> - luego el nodo lo persiste en `secrets.json`
> - no debe mantenerse en `hive.yaml`
```

⚠️ **TENTATIVO**:
- El set completo de campos opcionales puede crecer en v1 (sin bump) mientras se mantenga “lenient unknown fields”.

---

## 5. Config dinámica `CONFIG_SET`: payload y merge_patch

### 5.1 Alineación con normas generales de Fluxbee

Fluxbee mantiene dos capas:
- capa core/orchestrator (`set_node_config`/`NODE_CONFIG_SET` + señal `CONFIG_CHANGED`)
- capa node control-plane (`CONFIG_GET`/`CONFIG_SET` + `CONFIG_RESPONSE`)

Para AI Nodes, el hot-reload operativo actual se realiza por `CONFIG_SET`.

✅ **NORMATIVO (decisión cerrada)**:
- Para `CONFIG_SET`, se mantiene el mismo naming por consistencia: el contenido a aplicar viaja en `payload.config`.
- Cuando `apply_mode = "merge_patch"`, **`payload.config` contiene el patch** (RFC 7396).  
  (Esto evita introducir un segundo campo `patch` y mantiene consistencia con el patrón Fluxbee “config siempre viaja en `config`”.)

### 5.2 Campos esperados en `CONFIG_SET.payload`

✅ **NORMATIVO**:
- `subsystem: "ai_node"`
- `node_name: string`
- `config_version: u64`
- `apply: "immediate" | "next_reload"`
- `apply_mode: "replace" | "merge_patch"`  (Parte 3)
- `schema_version: u32` (requerido para config dinámica)
- `config: object`
  - si `apply_mode="replace"`: config completa
  - si `apply_mode="merge_patch"`: patch RFC 7396

---

## 6. Errores de validación

🧩 **A ESPECIFICAR**: formato final de error payload (Text prefijado vs ErrorV1Payload).  
Mientras tanto, se definen códigos y detalles esperados.

✅ **NORMATIVO**: códigos recomendados para validación/config:

- `invalid_config`
- `unsupported_schema_version`
- `stale_config_version`
- `missing_required_field`
- `invalid_field_type`
- `invalid_enum_value`

✅ **NORMATIVO**:
- Los errores **deben** incluir `details` con:
  - `path` (ej: `behavior.model_settings.max_output_tokens`)
  - `reason` (ej: “expected integer >= 1”)

---

## 7. Campos desconocidos: política lenient

✅ **NORMATIVO (decisión cerrada)**:
- Campos **desconocidos** en YAML o config dinámica **MUST** ser ignorados (no deben romper ejecución).
- Campos **requeridos** ausentes o con tipo inválido **MUST** producir error (`missing_required_field` / `invalid_field_type`).

Motivo: el nodo (runner) es dueño de su archivo; los typos deberían ser improbables, y priorizamos robustez/forward-compat.

---

## 8. Desviaciones conocidas del código actual (para backlog)

🐞 **DESVIACIÓN CÓDIGO (P1)**:
- El runner actual no valida formalmente la config contra un schema versionado.
- No implementa `apply_mode=merge_patch` (RFC 7396) en `CONFIG_SET`.
- No expone estado “FAILED_CONFIG” por nodo en `STATUS_RESPONSE`.

✅ **NORMATIVO**: incorporar validación + aislamiento por nodo + merge_patch según lo definido.

---

**Siguiente tema (cuando vos quieras):** Operación & Lifecycle (reload, señales, health, status extendido, métricas) alineado con `07-operaciones.md` (paths fijos, filosofía de config). fileciteturn2file10L24-L33

---

## 5. Operation & Lifecycle

> ✅ **Objetivo**: Esta entrega define el **modelo operacional** y el **lifecycle** de un nodo `AI.*` en Fluxbee:
> - cómo se despliega (systemd + 1 YAML por nodo),
> - cómo arranca sin config (UNCONFIGURED),
> - cómo recibe y persiste configuración (Control Plane),
> - cómo se reinicia y rehidrata estado,
> - cómo se apaga ordenadamente (drain).
>
> Health/backpressure/logs/métricas/rollout quedan **a especificar** si surge necesidad o si existen normas globales adicionales.

---

## Convenciones de estado del documento

- ✅ **NORMATIVO**: requisito cerrado (MUST/SHOULD/MAY).
- 🧩 **A ESPECIFICAR**: se documenta la necesidad, pero falta definir/implementar si surge necesidad.
- 🐞 **DESVIACIÓN CÓDIGO**: comportamiento actual que contradice lo normativo.

---

## 1. Modelo de despliegue (systemd)

✅ **NORMATIVO**: Cada nodo AI se despliega como una **instancia independiente** de un servicio systemd template:

- `fluxbee-ai-node@<node>.service`

✅ **NORMATIVO**: relación 1:1:
- **1 nodo AI** ↔ **1 archivo JSON efectivo en `${STATE_DIR}`** ↔ **1 instancia systemd**

⚠️ El YAML por nodo puede existir como **plantilla** (no fuente viva).

✅ **NORMATIVO**: el path del JSON efectivo por instancia es:

- `${STATE_DIR}/ai-nodes/<node>.json`  
  default: `/var/lib/fluxbee/state/ai-nodes/<node>.json`

⚠️ Plantilla opcional (no fuente viva): `/etc/fluxbee/ai-nodes/<node>.yaml`

✅ **NORMATIVO**: no se soporta modo “multi-config” (un proceso leyendo múltiples YAML).  
Cada instancia opera aislada y no debe interferir con otras.

✅ **NORMATIVO (aclaración)**: las opciones de `runtime` en YAML son **opcionales**. Si se omiten, el runner aplica defaults comunes.

---

## 2. Bootstrap y estados

✅ **NORMATIVO**: el nodo AI implementa los estados (ver Parte 1):

- `UNCONFIGURED`: sin configuración efectiva (ni config dinámica persistida ni fallback de orchestrator).
- `CONFIGURED`: con configuración efectiva.

✅ **NORMATIVO**:
- Un nodo puede iniciar en `UNCONFIGURED` y permanecer operativo para Control Plane (STATUS/PING/CONFIG_SET).

---

## 3. Actualización de configuración sin restart

✅ **NORMATIVO**: El nodo AI **MUST** poder actualizar su configuración **sin depender** de que el orchestrator lo reinicie.

✅ **Mecanismo normativo**:
- `CONFIG_SET` (Control Plane) aplica cambios en caliente y persiste la configuración resultante en `${STATE_DIR}/ai-nodes/<name>.json`.
- Ver Parte 1 (mensajes Control Plane) y Parte 3 (merge_patch RFC 7396), Parte 4 (validación).

🧩 **A ESPECIFICAR**:
- Reload de YAML sin restart (ej: SIGHUP) solo si surge necesidad.  
  En v1, el contrato operacional no depende de reload de YAML.

---

## 4. Persistencia y rehidratación en restart
✅ **NORMATIVO (HOY)**:
- El runner del nodo **debe** asegurar que existe un identificador estable (UUID/ILK) para la instancia y persistirlo localmente (p.ej. archivo `<node>.uuid` bajo `/var/lib/fluxbee/nodes/` o el path que defina la operación).

🧩 **A ESPECIFICAR (Fluxbee / node-spawn)**:
- Quién crea inicialmente el archivo `.uuid` (posible futuro: orchestrator crea; nodo mantiene).  
  Mientras no esté normado, se asume que el **nodo** lo crea si no existe.


✅ **NORMATIVO**: al reiniciar una instancia managed:

- Si existe config dinámica persistida en `${STATE_DIR}/ai-nodes/<name>.json`:
  - se utiliza como configuración efectiva (precedencia).
- Si no existe config dinámica persistida:
  - se intenta bootstrap desde `/var/lib/fluxbee/nodes/<TYPE>/<node@hive>/config.json`.
- Si ninguna fuente existe:
  - el nodo queda `UNCONFIGURED`.

✅ **NORMATIVO**: la persistencia y precedencia están definidas en Partes 1 y 4.

---

## 5. Shutdown y “drain”

✅ **NORMATIVO**: Semántica de señales:

- `SIGTERM` → **drain**:
  - el nodo deja de aceptar trabajo nuevo,
  - intenta terminar requests en curso hasta el límite de tiempo configurado,
  - luego finaliza.
- `SIGKILL` → hard stop (sistema operativo/systemd).

✅ **NORMATIVO**: el drain está acotado por:
- timeouts del handler (`handler_timeout_ms`) si existen,
- timeouts de stop de systemd (si aplica).

---

## 6. Aislamiento entre nodos

✅ **NORMATIVO**:
- La falla de un nodo AI (config inválida, crash, provider error) **no debe** afectar a otros nodos AI.
- Esto se garantiza por el modelo de despliegue (una instancia por nodo).

✅ **NORMATIVO**:
- Si el YAML de una instancia es inválido:
  - esa instancia debe quedar en estado `FAILED_CONFIG` (ver Parte 4) y reportarlo en `STATUS_RESPONSE`,
  - otras instancias continúan funcionando normalmente.

---

## 7. Observabilidad y operación (pendiente)

🧩 **A ESPECIFICAR / IMPLEMENTAR SI SURGE NECESIDAD**:
- Health endpoints locales (HTTP) vs health por mensajes system (`PING/STATUS`) vs CLI.
- Backpressure / queueing / overload policy.
- Logging estructurado (formato/rotación) y métricas (Prometheus/exporter).
- Rollout/compatibilidad runtime global.

> Nota: por ahora, el canal normativo de observabilidad mínima es Control Plane (`PING/STATUS`) + logs.

---

## 8. Desviaciones conocidas del código actual (para backlog)

🐞 **DESVIACIÓN CÓDIGO (P1)**:
- El runner actual puede aceptar múltiples `--config` (modo multi-config), pero **en operación** se define 1:1 instancia↔YAML.  
  El binario puede mantener esa capacidad para desarrollo, pero **no** es parte del contrato operacional.

---

**Siguiente paso sugerido:** consolidación final (unificar Partes 1–5 en un solo reemplazo) y un “Compliance Checklist” contra el código actual (qué hay que corregir).

---


---

# Apéndices (material rescatado del spec anterior)

## Apéndice A: Normalización del nombre del nodo con hive_id (normativo)

### 7.3 Destination naming

Node names are normalized by SDK as `<node.name>@<hive_id>` using `/etc/fluxbee/hive.yaml`.

Example:
- `node.name: "AI.chat"`
- `hive_id: "sandbox"`
- registered node name: `AI.chat@sandbox`

## Apéndice B: Repo placement / estructura de crates (no normativo)

## 2) Repo placement

AI node development is split in two layers:

1. `crates/fluxbee_ai_sdk/`
- Rust SDK for `AI.*` nodes.
- Reuses `fluxbee_sdk` socket/protocol primitives.
- Contains: node trait, runtime loop, message helpers, text payload helpers, LLM client abstraction, minimal Agent abstraction.

2. `nodes/ai/ai-generic/src/bin/*.rs` (bootstrap) -> `ai_node_runner` (target)
- Runtime executables layer (currently `nodes/ai/ai-generic/src/bin/`).
- Target state is one generic runner binary (`ai_node_runner`) instantiated with different configs.

Implemented now:
- `ai_node_runner` exists and loads node behavior from YAML config.
- dedicated `ai_echo` binary removed; echo now runs via `behavior.kind: echo` profile.
- `ai_local_probe` exists for direct OpenAI smoke tests without router/IO.
- Startup command contract supports one process with one or many configs:
  - `ai_node_runner --config node-a.yaml`
  - `ai_node_runner --config node-a.yaml --config node-b.yaml`
  - each config => one logical `AI.*` node instance
  - duplicate `node.name` values are rejected at startup
- Bootstrap sample config: `docs/onworking/ai_node_runner_config.example.yaml`.

Rationale:
- Keeps router core and AI runtime concerns separate.
- Allows external repos to consume the same AI SDK crate.
- Lets us iterate on AI features without touching router internals.
- Aligns with Fluxbee operational model: many `AI.*` logical nodes, one runner implementation.

## Apéndice C: Alignment con openai-agents-python (no normativo, completo)

## 4) Alignment with openai-agents-python

We take only the minimal and stable ideas from `openai/openai-agents-python`:
- `Agent` as explicit object
- runtime loop separated from agent declaration
- model provider abstraction (`LlmClient`)
- incremental feature expansion (tools, handoffs later)

We intentionally skip for now:
- guardrails framework
- run state persistence
- multi-turn tool orchestration
- streaming event framework

## Apéndice D: Roadmap incremental (no normativo)

## 5) Incremental roadmap

Phase A (now):
- crate skeleton (`fluxbee_ai_sdk`)
- `AiNode`, `RouterClient`, `NodeRuntime`
- `LlmClient` + OpenAI client
- bootstrap completed with generic runner + echo/openai_chat profiles

Phase B:
- timeouts + worker pool in runtime
- structured logging and metrics hooks
- stricter message validation and error taxonomy

Phase C:
- generic `ai_node_runner` with per-instance config
- startup command contract for multiple node instances
- deprecate dedicated per-node bins

Phase D:
- tool-calling fire-and-forget
- correlation by `trace_id`

Phase E:
- handoffs + advanced orchestration

## Apéndice E: Instalación y gestión de instancias (no normativo)

### 7.4 Install and manage AI node instances

Installation script (does not modify core `install.sh`):

```bash
bash scripts/install-ia.sh
```

Installs:
- `/usr/bin/ai-node-runner`
- `/usr/bin/ai-nodectl`
- systemd template unit `/etc/systemd/system/fluxbee-ai-node@.service`

Per-instance runtime profile:
- `AI.common`: tools/behavior de AI común (sin identidad gov).
- `AI.frontdesk.gov`: tools/behavior de frontdesk gov (incluye identidad, por ejemplo `ilk_register`).
- no se usa `AI_NODE_MODE` para seleccionar capacidades del runtime.

Manage instances with `ai-nodectl`:

```bash
ai-nodectl list
ai-nodectl add ai-chat /tmp/ai_chat.yaml
sudo systemctl enable --now fluxbee-ai-node@ai-chat
ai-nodectl status ai-chat
ai-nodectl logs ai-chat --follow
```


---

## 9. Thread State Store (LanceDB) — estado relevante por thread (MVP)

> ✅ **Objetivo**: permitir que cada nodo `AI.*` guarde **datos duros** relevantes para continuar tareas entre mensajes, sin guardar la conversación completa.
> ⚠️ Este store es **privado por nodo**: un nodo no lee/escribe el store de otro.

### 9.1 Keying principal: `src_ilk` (MVP vigente)

✅ **NORMATIVO (MVP vigente)**:
- El key principal de thread state es `src_ilk`.
- El runner extrae `src_ilk` solo desde `meta.src_ilk` (canónico).
- Si falta `src_ilk`, las tools de thread state deben fallar con error explicito (`missing_src_ilk`).

### 9.2 Compatibilidad legacy con `thread_id`

✅ **NORMATIVO (MVP)**:
- `thread_id` se mantiene solo como compatibilidad legacy/migracion.
- El runner extrae `thread_id` solo desde `meta.thread_id`.
- Si existe estado legacy por `thread_id`, el runtime puede migrarlo al key principal (`src_ilk`) y limpiar la key legacy.

### 9.3 Modelo de datos: 1 JSON por key de estado (`src_ilk`)

✅ **NORMATIVO (MVP)**:
- El nodo mantiene **un unico documento JSON** por `src_ilk`.
- La estructura del JSON es **libre** y se define por prompting/policy según el nodo (frontdesk, soporte, etc.).
- Campos opcionales de sistema:
  - `updated_at` (timestamp de último write)
  - `ttl_seconds` (si el backend lo soporta)

✅ **NORMATIVO**:
- El nodo **decide** qué guardar y cuándo actualizar/borrar (por prompting/policy).
- El nodo **MUST** poder borrar inmediatamente el estado de un thread cuando deje de ser útil.

### 9.4 API (Tool calling) minima

✅ **NORMATIVO (MVP)**: el runtime/SDK debe exponer tools equivalentes a:
- `thread_state_get(thread_id) -> { data?, updated_at? }`
- `thread_state_put(thread_id, data, ttl_seconds?) -> ok`
- `thread_state_delete(thread_id) -> ok`

Nota:
- En modo scoped del runtime, el argumento `thread_id` enviado por el modelo puede ignorarse.
- La ejecucion se scopea al key del contexto actual (`src_ilk`), con fallback legacy opcional.

> Nota: no se requiere query vectorial ni múltiple-key en MVP.

### 9.5 Persistencia y path

✅ **NORMATIVO (MVP)**:
- La base/dataset de LanceDB del nodo vive bajo `${STATE_DIR}/ai-nodes/<node_name>/lancedb/` (o path equivalente que respete patrones Fluxbee).
- El nodo es responsable del mantenimiento: cleanup de threads completados, TTL, compactación si aplica.

🧩 **A ESPECIFICAR**:
- Reglas de backup/restore y migración de schema interno de LanceDB.

### 9.6 Concurrencia e idempotencia

✅ **NORMATIVO (MVP)**:
- Se asume un único proceso writer (la instancia del nodo) para su store.
- El nodo debe serializar accesos concurrentes a un mismo key de estado para evitar corrupcion (mutex/actor queue).

## 10. Immediate Conversation Memory in `ai_node_runner` (v1)

> Objective: add short-horizon conversation continuity in `openai_chat` nodes without mixing it with `thread_state`.

### 10.1 Scope and activation

NORMATIVE (v1):
- Applies to `behavior.kind = openai_chat`.
- Enabled via config:
  - `runtime.immediate_memory.enabled = true`
- If disabled (`false`), runner keeps the classic path without immediate memory.

### 10.2 Keying and isolation

NORMATIVE (v1):
- Canonical key is `src_ilk`.
- `thread_id` is kept as auxiliary metadata.
- Store remains private per node: one node does not read immediate memory from another node.

### 10.3 Relationship with `thread_state`

NORMATIVE (v1):
- `thread_state_*` remains the hard-state tool store.
- `immediate_memory` is short-horizon conversational context managed by the runner.
- Both stores are separate (semantics and path), with no cross-overwrite.

### 10.4 Persistence and path

NORMATIVE (v1):
- Runner persists immediate memory per `src_ilk` under:
  - `${STATE_DIR}/ai-nodes/<node_name>/immediate-memory/threads/`
- Runner applies pruning according to configured limits.

### 10.5 Runtime config (v1)

NORMATIVE (v1): supported `runtime.immediate_memory` fields:
- `enabled` (bool, default `false`)
- `recent_interactions_max` (int, default `10`)
- `active_operations_max` (int, default `8`)
- `summary_max_chars` (int, default `1600`)
- `summary_refresh_every_turns` (int, default `3`)
- `trim_noise_enabled` (bool, default `true`)

### 10.6 Functional status in current v1

NORMATIVE (current v1):
- Runner builds `FunctionRunInput` and uses `run_with_input(...)` when immediate memory is enabled.
- `recent_interactions` is rehydrated/persisted by `src_ilk`.
- `active_operations` is currently empty in runner v1.
- `conversation_summary` refresh remains for a dedicated next phase.
