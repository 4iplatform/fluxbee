# Nodos IO - Specification v1.16 (Fluxbee)
> Adaptation note for `json-router`:
> This spec is imported and kept as functional baseline.
> If a section conflicts with router v1.16+ docs in this repo, follow `docs/io/README.md` precedence.

## 1. Role of IO Nodes

IO nodes are **edge adapters** between external communication channels and the Fluxbee message fabric.

They are intentionally:
- Stateless (from a business perspective)
- Non-authoritative
- Simple

---

## 2. Identity Model

IO nodes:
- Resolve identity via SHM
- On miss/error, can call `ILK_PROVISION` via `SY.identity` (shared `io-common` flow)
- Never block provider ACK waiting for identity

If provision fails/timeout, IO forwards with `meta.src_ilk = null` and Router/OPA handles onboarding.

---

## 3. Identity Strategies

### Strategy: Router-Driven Onboarding

- IO runs `lookup -> provision_on_miss -> forward`
- Normal path is non-null `meta.src_ilk` when lookup/provision succeeds
- Degraded fallback forwards with `meta.src_ilk = null`
- Router + OPA redirect to onboarding workflow
- Message is re-injected once identity exists

This strategy provides:
- Durability
- Stateless IO nodes
- Maximum Router alignment

---

## 4. Context Model

IO nodes:
- Produce `ich`
- Consume `ctx_seq`
- Accept `ctx_window`
- Never persist or forward history externally

The Router is the **sole owner** of conversational history.

---

## 5. Protocol Alignment

IO nodes **MUST follow Router v1.16 protocol** exactly.

Notably:
- `meta.ctx` is the only context identifier
- `meta.context` is reserved for OPA input
- No duplication of context fields

---

## 6. Persistence Model

IO persistence exists only as an **implementation detail**.

Rules:
- Abstract interface required
- In-memory allowed for MVP
- No coupling to Router storage
- No assumptions of durability

---

## 7. Non-Responsibilities

IO nodes explicitly do NOT:
- Store conversation history
- Perform identity orchestration
- Implement retry queues for business messages
- Decide routing or business logic

---


### Aclaración - meta.ctx vs meta.context

Según la especificación del Router v1.16:

- `meta.ctx` representa el **Contexto Conversacional (CTX)**
- `meta.context` se reserva exclusivamente para **datos adicionales evaluados por OPA**
- `meta.thread_id` es el carrier canónico de thread (cognition v2)

`meta.context` **NO** debe usarse para almacenar historia, turns ni contexto conversacional.
`meta.context.thread_id` está deprecado/removido en IO y no debe emitirse.


## Adaptación temporal (spec canónica vs core actual)

✅ **Canónico (protocolo Fluxbee)**:
- Mensajes `user` deben incluir identidad/contexto L3 (p.ej. `ich/ctx`, `ctx_seq`, y eventualmente `ctx_window`).

⚠️ **Estado actual (core/SDK)**:
- `ctx_window` no está garantizado en runtime hoy; `ctx_seq` puede no venir en algunos flujos.

✅ **NORMATIVO (HOY)**:
- IO Nodes **MUST** tolerar `ctx_window` y `ctx_seq` ausentes sin fallar.
- esos fields ya no forman parte del carrier canónico activo del repo.

🧩 **A ESPECIFICAR**:
- si se mantienen en el protocolo, quedan solo como compatibilidad histórica; no hay plan activo para volverlos obligatorios.


## Lifecycle & configuración (modelo unificado con AI Nodes)

> ✅ **Intención**: no reinventar la rueda. IO Nodes adoptan el **mismo patrón operacional** que AI Nodes:
> `SY.admin` recibe comandos → `SY.orchestrator` decide spawn/identity → el nodo arranca con config base y se ajusta por Control Plane (`CONFIG_SET/GET`, `STATUS/PING`).

### 1) Spawn (alto nivel)

✅ **NORMATIVO**:
- El nodo IO se crea/spawnea por `SY.orchestrator` (invocado por `SY.admin`).
- El nodo nace con `node_name` y con una **configuración base** mínima (depende del adapter: Slack/WhatsApp/Email).
- Si la config base es incompleta o inválida, el nodo **debe arrancar igual** y quedar en estado de fallo de configuración (ver estados).

🧩 **A ESPECIFICAR (cuando node-spawn quede totalmente normado para IO)**:
- Detalle del mensaje `SPAWN_NODE` y payload exacto para IO Nodes (alinear con normas globales de Fluxbee).

### 2) Estados

✅ **NORMATIVO**: estados por instancia:
- `UNCONFIGURED`: sin configuración efectiva aplicable.
- `CONFIGURED`: configuración efectiva válida para operar.
- `FAILED_CONFIG`: no puede operar data-plane, pero atiende Control Plane para recuperarse.

✅ **NORMATIVO**:
- En `FAILED_CONFIG`, el nodo **MUST** atender: `PING`, `STATUS`, `CONFIG_GET`, `CONFIG_SET`.

### 3) Configuración en caliente (Control Plane)

✅ **NORMATIVO**:
- IO Nodes usan el mismo patrón de mensajes que AI Nodes:
  - `CONFIG_SET` (replace/merge_patch) para aplicar cambios,
  - `CONFIG_RESPONSE` como confirmación,
  - `CONFIG_GET` para consultar estado/config_version,
  - `STATUS`/`PING` para observabilidad mínima.

✅ **NORMATIVO (replace MVP)**:
- Para MVP, se prioriza `apply_mode="replace"` y se permite luego `merge_patch` (RFC 7396) como mejora.

### 3.1 Frontera explícita con core/orchestrator

✅ **NORMATIVO**:
- `PUT /hives/{hive}/nodes/{node}/config` (core/orchestrator) actualiza `config.json` administrado por plataforma.
- Ese endpoint **NO reemplaza** el contrato runtime node-owned de IO (`CONFIG_SET` / `CONFIG_GET`).
- Para hot reload operacional del adapter IO, el camino canónico es `POST .../control/config-set` (`CONFIG_SET`).

### 3.2 Política IO para `CONFIG_CHANGED` (v1)

✅ **NORMATIVO (v1, elegido)**:
- `CONFIG_CHANGED` se trata como señal de infraestructura (informativa) en la capa IO.
- No es el camino canónico para aplicar runtime/business config del adapter.
- La aplicación runtime efectiva de configuración IO se realiza por `CONFIG_SET`.

### 4) Reglas de configuración: común vs adapter-specific

✅ **NORMATIVO**:
- La capa común define:
  - versionado (`schema_version`, `config_version`),
  - persistencia de config efectiva,
  - validación estructural,
  - states y errores.
- Cada adapter define su **config mínima requerida**:
  - Slack Socket Mode: `SLACK_APP_TOKEN` y `SLACK_BOT_TOKEN` (y otros campos que el adapter declare).
  - WhatsApp/Email: TBD según adapter.

### 5) Secrets: HOY vs futuro

✅ **NORMATIVO (HOY / MVP)**:
- Se permite que credenciales (tokens, signing secrets, etc.) vivan en **config local** (YAML/env) y/o viajen **inline** en `CONFIG_SET` para permitir cambios sin reinicio.
- El nodo **MUST NOT** persistir secretos en claro en `${STATE_DIR}`.

⚠️ **ADVERTENCIA**:
- Si el secreto llega inline por `CONFIG_SET`, el nodo puede retenerlo solo en memoria (y se pierde en restart), salvo que exista una fuente local (YAML/env).

🧩 **A ESPECIFICAR (futuro)**:
- `*_key_ref` (env/file/vault/kms) + gestor de secretos (orchestrator/admin o servicio dedicado) para rotación/persistencia segura.

### 6) Regla de adopcion para futuros adapters IO

✅ **NORMATIVO**:
- Si un adapter `IO.*` produce mensajes de usuario textuales hacia router, **MUST** usar el contrato canonico `text/v1`.
- Esos mensajes **MUST** salir por `fluxbee_sdk::NodeSender::send`.
- El adapter **MUST NOT** reimplementar localmente la decision `content` vs `content_ref` para `text/v1`.

✅ **ACLARACION**:
- El auto-offload actual del SDK cubre el contrato `text/v1`.
- No debe asumirse automaticamente para payloads arbitrarios fuera de ese contrato.
- Si un adapter necesita otro payload no `text/v1`, ese contrato debe definir explicitamente como resuelve limites de tamano y blobs.




