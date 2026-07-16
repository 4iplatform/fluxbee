# IO.cloud — spec v1 (el representante interno de Fluxbee Cloud)

Estado: **borrador alpha**. Fecha: 2026-07-09. Branch: `daily_onworking_coa`.
Alcance: define QUÉ es `IO.cloud`, su rol de provisioning, el seam `IO.cloud → SY.admin`, el
modelo de confianza y de vault, y los pendientes/seguridad a tener presentes. **No** define los
adaptadores por-proveedor (`IO.wapp`, etc.) — esos son "otra vida", con canal y seguridad propios.

Filosofía del proyecto (operador): **primero ver, después hacer, después seguridad**. Alpha: se
expone un subconjunto acotado + se loguea para aprender qué usa Cloud de verdad, y de ahí se cierra.

Todo lo que sigue está anclado al código actual (se cita `archivo :: símbolo`). Donde algo NO existe
todavía, se marca explícito.

---

## 1. Rol y posición

`IO.cloud` es **el representante interno de Fluxbee Cloud dentro de la malla**: un nodo IO que vive
en un worker, recibe lo que el edge le reenvía desde internet, y **provisiona recursos de backend**
(tenant, ilk, secretos, spawn de nodos IO, externalize) en nombre de Cloud, hablándole a `SY.admin`.
Cada nodo IO que provisiona (p.ej. `IO.wapp@<tenant>`) después **tiene su propia vida** — su propio
adaptador, canal y seguridad.

- Es un **singleton baked en el `.deb` core** (binario + `io-cloud.service` con `ExecCondition`
  gateado a `role: motherbee`), a diferencia de los adaptadores per-tenant (`io-api`/`io-slack`) que
  son runtime packages publicados y spawneados por instancia. (`docs/edge-ingress-spec-v6.md` §10)

### 1.1 El EDGE es la puerta, no la autoridad
- Diseño invariante: **el edge es la única puerta de entrada**. Está en un hive `ingress`
  (red física separada), resuelve el **perímetro** (TLS, verificación del token/código, límites,
  filtrado de headers). `IO.cloud` vive en un worker y **nunca sale directo a internet**.
- El edge es un **firewall**, no una autoridad: autentica *que el caller es válido* y transporta,
  pero **no** decide *qué puede hacer* (no conoce tenants ni acciones). La **autorización por-tenant**
  vive downstream (`IO.cloud`/`SY.admin`). Esto **no implica tocar el edge** — ya está endurecido
  (EDGE-01, EDGE-05, authz de externalize; ver §8).

### 1.2 Dos niveles de confianza
- **Tier 1 — Cloud-como-servicio**: tiene el OAuth verificado (Google por ahora) y hace el
  *lifecycle* (tenant, ilk, provisioning). Es confianza **service-to-service** — Cloud es un peer
  semi-confiable del control-plane, **no** un bearer token por usuario.
- **Tier 2 — usuario final vía Cloud**: scopeado siempre a su tenant (usa sus nodos). Autenticado por
  su **sesión** OAuth.
- **Requisito transversal (la pieza que hoy NO existe):** hilar el **tenant/subject verificado**
  desde Cloud hasta `SY.admin`/`SY.identity`/`SY.vault`. Sin eso, cada acción es un *confused deputy*
  (ver §8.1). El modelo: el token autentica a *Cloud-como-servicio*; Cloud **asegura** el tenant por
  request (claim avalado por su OAuth); admin **scopea** a ese tenant confiando en Cloud.

---

## 2. Estado actual (grounded)

Lo que `IO.cloud` hace HOY (`nodes/io/io-cloud/src/main.rs`):

1. **Conecta** a la malla como `IO.cloud` (`main` → `RouterDispatcher::connect_with_retry`, canal
   `incoming`, `post_pending_rule(RouteMatch::Any → Command("incoming"))`).
2. **Self-provisiona su ilk** (`ensure_own_channel`): si hay `FLUXBEE_NODE_ILK_ID` lo usa; si no,
   `strict_provision_ilk` (`ILK_PROVISION` a `SY.identity`, autorizado por prefijo `IO.`).
3. **Registra su ICH** (`ensure_own_ich` en `nodes/io/common/src/provision.rs`): `ILK_ADD_CHANNEL` +
   `set_ich_enabled(true)`. El owner se estampa server-side desde `src_l2_name` → el ICH queda owned
   by `IO.cloud`.
4. **Self-externalize condicional** (solo si `IO_CLOUD_EDGE_NODE` seteado): `send_admin_rpc(
   action:"externalize", ...)` a `SY.admin`, retry hasta que el edge exista
   (`publish_channel_on_edge_with_retry`). Es la **única** llamada admin que hace.
5. **Handler = echo puro** (`run_loop`): lee `meta.ich`, responde en la misma familia con
   `{status:"ok", handled_by, ich, echo:payload}`. **Sin** dispatch por ich, **sin** tenant/subject,
   **sin** provisioning, **sin** cliente saliente.

Lo que **NO** tiene hoy:
- Sin dispatch por `ich` a tenant/conversación (el comentario "a real IO.cloud dispatches by ich" es
  solo eso, un comentario).
- Sin cliente saliente a ningún backend externo (deps: `fluxbee-sdk`, `io-common`, `serde_json`,
  `tokio`, `tracing` — **cero** `reqwest`/`hyper`/`axum`).
- Sin handler de relay de comandos admin (solo es *cliente* de `externalize`).
- Sin hilado de tenant/subject en el path del request.
- Nunca escribe a vault (para shared-secret solo pasa `IO_CLOUD_SECRET` como `params.secret` y admin
  lo guarda).

> El molde de IO.cloud (nodo real + externalize + handler) sirve; falta la lógica de producto.
> Ver también EDGE-09 en `docs/audits/2026-07-06-sy-edge-ingress-audit.md`.

---

## 3. Arquitectura objetivo

```
Fluxbee Cloud (OAuth Google)                INTERNET
      │  token de servicio + TLS
      ▼
  ┌─────────┐   HTTPS      ┌──────────────┐  malla (Unicast user-family)  ┌──────────┐
  │  Cliente│─────────────▶│  SY.edge     │──────────────────────────────▶│ IO.cloud │
  │  Cloud  │◀─────────────│  (ingress)   │◀──────────────────────────────│ (worker) │
  └─────────┘   response   │  = firewall  │        response (mismo trace)  └────┬─────┘
                           └──────────────┘                                     │ ADMIN_COMMAND
                                                                                ▼
                                                                          ┌──────────┐
                                                                          │ SY.admin │──▶ identity/vault/orchestrator
                                                                          └──────────┘
```

- Entrada **siempre** por el edge. `IO.cloud` recibe el request reenviado (canal `incoming`).
- `IO.cloud` traduce el request a uno o más **`ADMIN_COMMAND`** a `SY.admin@<hive>` y **bloquea en la
  response** (correlación por `trace_id`+`request_id`), igual que hoy hace `externalize`.
- La response vuelve por el mismo camino (edge → HTTPS). **Asunción v1: las responses son < 128 KB**
  (límite de frame, ver §7). El provisioning tiene responses chicas (id/URL/status), así que el límite
  no muerde en el día 1.

### 3.1 El seam `IO.cloud → SY.admin` (grounded)
Path node→admin (SDK `crates/fluxbee_sdk/src/rpc.rs :: send_admin_rpc`):
- `IO.cloud` manda `AdminCommandRequest{admin_target, action, target, params, request_id, timeout}`.
- El SDK arma `Message{ meta.msg_type = "admin" (ADMIN_KIND), meta.msg = "ADMIN_COMMAND",
  dst = Unicast(admin_target), payload = {action, params, request_id, target} }`.
- Admin lo rutea a `RPC_CH_INTERNAL_ADMIN` → `handle_internal_admin_command` →
  `dispatch_internal_admin_command(action, target, params, caller_l2_name = msg.routing.src_l2_name)`.
- Response: `ADMIN_COMMAND_RESPONSE` (`send_admin_command_response`), `dst = Unicast(request.src)`,
  mismo `trace_id`, echoando `request_id`.
- **Es literalmente el mismo path que `externalize`; solo cambia el `action` string.**

### 3.2 Catálogo mínimo Tier-1 (día 1)
El subconjunto acotado que `IO.cloud` expone (NO "todo admin"). Cada acción, su ruta real y su destino
(`src/bin/sy_admin.rs :: INTERNAL_ACTION_REGISTRY` salvo donde se indique):

| Acción | Ruta | Destino | Notas |
|---|---|---|---|
| `create_tenant` | Command | `SY.identity` (`TNT_CREATE`) | tenant_id = `tnt:<uuid>` |
| `set_ilk_definition` / `ILK_PROVISION` | Command | `SY.identity` | ilk del nodo IO |
| `vault_put` | Command | `SY.vault` | secret owned-by `IO.<x>@<tenant>` (ver §4) |
| `run_node` (start/restart) | Command | `SY.orchestrator` | spawn del nodo IO; **requiere tenant** |
| `externalize` / `unexternalize` / `list_externalized` | inline | edge | webhooks inbound; **ya con authz I1+I8** |

Fuera del catálogo día-1 (NO exponer al token público): `add_hive` (toma `ssh_user`/`ssh_password`),
`vault_get`/lectura cruda de secretos, `executor_execute_plan` (LLM → acciones = prompt-injection→root),
`set_node_config`, rutas/VPNs. Se agregan solo si el log muestra que Cloud los necesita, y con gate.

---

## 4. Modelo de vault (lector designado ≠ escritor) — grounded

Confirmado en código (`src/bin/sy_vault.rs :: authorize_read`, `resolve_caller`, `handle_put`;
`src/bin/sy_admin.rs :: normalize_vault_put_payload`):

- **Escritura es admin-only.** `handle_put` rechaza cualquier caller que no sea `SY.admin`/`SY.architect`
  (ilks deterministas). ⇒ **`IO.cloud` nunca escribe vault directo**; lo hace **vía `ADMIN_COMMAND
  vault_put`** y admin escribe como `SY.admin`.
- **Dueño designado por ilk.** Admin acepta `metadata.owner_node` (nombre L2 amigable) y lo resuelve a
  `metadata.ilk` (vía identity SHM `find_ilk_by_handler_node_from_hive_config`, else fallback
  determinista). El secreto queda owned por ESE ilk, **no** por el del escritor.
- **Lectura por match de ilk, sin bypass de admin.** `authorize_read`: si el secreto tiene `owner_ilk`
  y `caller.ilk_id == owner_ilk` → OK; **el admin/escritor NO puede releerlo** (doc explícito: "No admin
  bypass"). Tests: `authorize_read_denies_dedicated_secret_to_different_ilk`.
- **Dos modos de scoping:** (a) **dueño-designado** (`metadata.ilk` set → solo ese nodo lee) o
  (b) **pool por tenant** (`metadata.ilk` vacío → cualquier nodo con `caller.tenant_id ==
  metadata.tenant_id` lee).

⇒ El modelo del operador funciona: **admin (dirigido por `IO.cloud`) escribe el token del proveedor con
`owner_node = IO.wapp@<tenant>`; solo `IO.wapp` lo lee; `IO.cloud` no**. Idéntico patrón que el cert TLS
del edge. **Con dos caveats** (ver §8.2, §8.3): el ordering de registro del ilk (Caveat B) y que la
lectura autoriza sobre `meta.src_ilk` *aseverado* (Caveat C).

---

## 5. Vertical de referencia: `IO.wapp` end-to-end

La primera vertical completa (ejercita provisioning + vault-scoped + spawn + externalize + adaptador).
`IO.wapp` es **otra vida** (su canal/seguridad son propios) — acá solo la **secuencia de provisioning**
que `IO.cloud` orquesta:

1. **Tenant** existe (Tier 1, del OAuth de Cloud). Si es nuevo: `create_tenant`.
2. Cloud corre el registro de WhatsApp (externo) → obtiene el credential. **Cloud no se queda con nada.**
3. **Provision del ilk** de `IO.wapp@<tenant>` (`ILK_PROVISION`, tipo `agent`) — **antes** del vault_put
   (Caveat B: el owner→ilk debe resolver al ilk real registrado).
4. **`vault_put`** del credential, `owner_node = IO.wapp@<tenant>`, scopeado → solo `IO.wapp` lo lee.
5. **`run_node`** de `IO.wapp` (runtime `io.wapp`, con `tenant_id`; systemd `fluxbee-node-<name>`).
6. **`externalize`** del canal inbound de `IO.wapp` (URL pública para los **webhooks** de WhatsApp) — el
   flujo ya endurecido esta sesión.
7. `IO.wapp` corriendo: **outbound** (cliente a la API de WhatsApp, lee su token de vault) + **inbound**
   (webhook → edge → `IO.wapp`).

> **Ojo (net-new):** hoy NO existe (a) runtime `io.wapp`, (b) una orquestación "provisionar nodo IO para
> un tenant" que ate create_tenant→ilk→vault→spawn→externalize. Son piezas a construir sobre el plumbing
> genérico de managed-node (§6).

---

## 6. Cómo se crean tenant / ilk / nodo (grounded)

- **Tenant:** `create_tenant` (admin) → `SY.identity :: TNT_CREATE` → `IdentityStore::create_tenant`
  (mint `tenant_id = tnt:<uuid>`, status default `pending`). Autorizado en identity solo para
  `SY.admin@`/`SY.architect@`/`SY.frontdesk.gov@` **same-hive** — **`IO.*` no puede crear tenants
  directo**, solo vía admin. Root tenant = `tnt:00000000-…-0001` (`fluxbee`, guarda secretos de infra).
- **Ilk de nodo IO:** `ILK_PROVISION` (autorizado por prefijo `IO.`, cross-hive) →
  `provision_temporary_ilk`: mint `ilk:<uuid>`, `registration_status = "temporary"`, canal `enabled:false`,
  `owner_l2_name: None`. Solo `human`/`agent` (rechaza `system`). **Completarlo** (owner, enable via
  `ILK_ADD_CHANNEL`/`ICH_SET_ENABLED`) es un paso aparte, no codificado como un flow único.
- **Spawn:** `run_node` (admin) → `SY.orchestrator :: run_node_flow` → `SPAWN_NODE`. Runtime = primeros
  2 segmentos del nombre (`IO.wapp.<x>` → `io.wapp`). **Requiere `tenant_id`** para `IO.`/`AI.`
  (`run_node_args_require_tenant`). Inyecta `FLUXBEE_NODE_ILK_ID`/`FLUXBEE_NODE_TENANT_ID` al unit.
- **Namespacing:** `ilk_id` es `ilk:<uuid>` (random para IO/AI); el `node@hive` va en
  `identification.node_name`/`ChannelRecord.owner_l2_name`. Solo los SY.* tienen ilk determinista
  (SHA256 del nombre).

---

## 7. Frames y respuestas grandes (paréntesis, para después)

- Límite duro: `MAX_FRAME_SIZE = 128*1024` (`crates/fluxbee_sdk/src/socket/connection.rs:5`),
  length-prefixed, sobre **cada** frame (socket local **y** WAN).
- **v1 asume responses < 128 KB** — el provisioning cabe. El límite muerde en los **reads/list** grandes.
- Cuando se expongan reads grandes, dos problemas distintos:
  - **Estructurado grande** (list nodes, inventory) → **paginación** (cursor desde admin, pasado punta a
    punta). Blob NO sirve acá.
  - **Opaco grande** (archivo, contexto LLM) → **blob por hash** (out-of-band). Idea del operador:
    un `IO.blob` que expone `/blob/<hash>` en el edge, scopeado por tenant. Content-addressed ⇒ una sola
    superficie, no canal-por-IO.
  - Complementos: **gzip** del payload (~10x, previsto), y solo si hace falta, chunking multi-frame.

---

## 8. Consideraciones de seguridad (tener presentes)

### 8.1 EDGE-06 — confused deputy del `ADMIN_COMMAND` de la malla (EL grande)
Confirmado en código:
- El path `ADMIN_COMMAND` **NO está router-origin-gated**: el gate de autoridad del router
  (`serialize_for_local_delivery` → `system_policy::authority`) **solo dispara para `msg_type == SYSTEM`**;
  `ADMIN_COMMAND` es `msg_type = "admin"` → **lo bypassea** y llega a admin sin control.
- Dentro de admin, **solo** `externalize`/`unexternalize`/`list_externalized` tienen gate de caller
  (`authorize_channel_command`: I1 caller `IO.*` + I8 `owner == caller`). **Todo el resto**
  (`create_tenant`, `vault_put`, `set_ilk_definition`, `run_node`, `set_node_config`, `add_hive`,
  rutas…) **ignora `caller_l2_name`** → cualquier nodo de la malla que pueda direccionar `SY.admin` las
  ejecuta con la **autoridad prestada de admin** (admin re-estampa `src_l2_name = SY.admin@hive` al
  forwardear al subsistema, que confía por eso). `executor_execute_plan` también es alcanzable ungated.
- **Implicancia para este spec:** `IO.cloud` va a ser exactamente ese relay, ahora con internet adelante.
  **Ponerle un token público adelante weaponiza el confused-deputy.** ⇒ **REQUISITO:** el seam
  `IO.cloud → SY.admin` necesita su **propia authz explícita y auditada** — un allowlist
  `action → permitido` + el **tenant asegurado** en cada request + logging — **no** el `ADMIN_COMMAND`
  ungated. Si no, se construye un bypass de internet de los gates que ya cerramos.
  (Refs: `src/router/mod.rs :: serialize_for_local_delivery`; `src/bin/sy_admin.rs ::
  dispatch_internal_admin_command` fall-through, `authorize_channel_command`, `send_admin_request`.)

### 8.2 Caveat B — ordering del `owner_node → ilk` para nodos dinámicos
El `owner_node → ilk` en `vault_put` resuelve vía identity SHM **al momento de escribir**. Para un nodo
**dinámico** (`IO.wapp`), si el nodo **no está registrado aún**, cae al fallback determinista, que **no**
matchea el ilk random del nodo → `IO.wapp` sería **denegado en la lectura**. (El edge no sufre esto por ser
SYSTEM con ilk determinista.) ⇒ **Provisionar/registrar el ilk de `IO.wapp` ANTES del `vault_put`** (o
garantizar que el write resuelve su ilk real). (`src/bin/sy_admin.rs :: normalize_vault_put_payload`.)

### 8.3 Caveat C — vault autoriza lectura sobre `meta.src_ilk` *aseverado*
`authorize_read` matchea el `owner_ilk` contra **`caller.ilk_id`, que sale de `meta.src_ilk`** — un campo
que el **caller asevera**. El router estampa `src_l2_name` de forma no-forjable, pero para `meta.src_ilk`
**solo canonicaliza (resuelve alias)**; no se encontró un check que lo ate a la conexión autenticada. ⇒ Un
nodo que pueda conectar y setear `meta.src_ilk = <ilk de la víctima>` no está demostrablemente bloqueado de
leer el secreto dedicado de otro nodo. El campo no-forjable (`src_l2_name`) vault lo usa **solo para audit**,
no para authz de lectura. **Riesgo:** el aislamiento de secretos por-ilk descansa en un campo aseverable.
(Verificar/derivar `src_ilk` del handle autenticado sería el cierre — trabajo aparte, fuera de este spec.)

### 8.4 Otros a tener presentes
- **Token = joya de la corona**: autentica a *Cloud-como-servicio*; si se filtra, alguien impersona a Cloud
  y asevera cualquier tenant. Acotarlo al catálogo §3.2 + siempre-con-tenant + logueado hace el daño
  **acotado y auditable**, no root arbitrario. A futuro: tokens cortos/rotables, no un bearer eterno.
- **No egress de secretos**: `IO.cloud` filtra/shapea las responses; nunca deja salir secretos crudos /
  creds por el HTTPS de vuelta. `vault_get` fuera del catálogo público.
- **Aislamiento por-tenant** descansa en (a) Cloud honesto (tiene el OAuth) y (b) la validación del claim
  de tenant en admin. Hoy ese hilado **no existe** (es el requisito transversal de §1.2).
- **Ya cerrado esta sesión (no re-abrir):** EDGE-01 (origin binding cross-hive, `a5a5912`), EDGE-05
  (edge↔IO rechaza SYSTEM, `b1932e1`), authz de externalize (I1+I8, `99d1d85`). El edge no se toca.

---

## 9. Problemas pendientes / futuro (ya conversados)

1. **Hilado de tenant/subject** Cloud→admin (el requisito transversal). Sin esto no hay authz per-tenant.
   Es la pieza de la que cuelga todo. **Bloqueante para producción; no para el "ver primero" logueado.**
2. **Gate del seam `IO.cloud → SY.admin`** (cerrar el confused-deputy §8.1): allowlist action→permitido +
   tenant + logging. Alternativa de fondo: origin-gatear el `ADMIN_COMMAND` de la malla.
3. **`IO.archi` (caso 2, diferido — "ahora no"):** forwarding HTTP per-tenant a `SY.architect` (`:3000`)
   tras login. Temas: aislamiento de tenant, auth de sesión, y **streaming (websocket/SSE)** que el edge
   hoy no hace (es request/response). Charlar aparte.
4. **`IO.blob` + paginación** (§7): reads grandes. Blob para opaco, paginación para estructurado.
5. **Atributo durable `externalized` en `SY.identity`** — prerequisito para EDGE-05 Fase 2 (acotar edge→IO
   a solo owners externalizados). Hoy diferido (`sy_admin.rs`); sin él, la fuente del allowlist sería el
   edge comprometido (auto-certificado).
6. **Net-new de provisioning:** no existe runtime `io.wapp` ni una orquestación "provisionar nodo IO para
   tenant" (create_tenant→ilk→vault→spawn→externalize atados). Construir sobre managed-node.
7. **Inconsistencia a verificar:** `derive_ilk_type_for_node` (orchestrator) devuelve `"system"` para `IO.*`,
   pero `ILK_REGISTER` rechaza `system` → un `ILK_REGISTER` orchestrator-driven para `IO.*` fallaría; los
   nodos IO dependen del self-provision (`ILK_PROVISION`, human/agent). Confirmar antes de diseñar el spawn
   de `IO.wapp`.
8. **Dispatch real de `IO.cloud`:** hoy es echo. El nodo objetivo enruta por `meta.ich` a
   tenant/conversación y (si aplica) habla con el backend de Cloud (cliente saliente, hoy inexistente).

---

## 10. Preguntas abiertas para cerrar antes de codear

- ¿El **claim de tenant** viaja en el payload firmado por Cloud, y admin lo valida cómo (confía en el token
  de servicio de Cloud, o hay una firma per-request)? — De esto cuelga §9.1.
- ¿El **gate del seam** (§9.2) se hace en `IO.cloud` (traduce/valida y solo emite acciones del catálogo) o
  en `SY.admin` (origin-gate del `ADMIN_COMMAND`)? Probablemente ambos (defensa en capas).
- ¿`IO.cloud` **crea el tenant** o lo hace Cloud por otro canal de servicio y `IO.cloud` solo provisiona
  dentro de un tenant ya creado?
