# IO.cloud — spec v1 (el representante interno de Fluxbee Cloud)

> **Para el equipo de Fluxbee Cloud, la referencia operativa es
> [`io-cloud-api.md`](io-cloud-api.md)** — probada contra producción y con la procedencia de cada
> dato. Este documento es la especificación de diseño interna.

Estado: **alpha implementada para provisioning**. Fecha de actualización: 2026-07-17. Branch:
`daily_onworking_coa`.
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
en **motherbee** (singleton), recibe lo que el edge le reenvía desde internet, y **provisiona recursos de backend**
(tenant, ilk, secretos, spawn de nodos IO, externalize) en nombre de Cloud, hablándole a `SY.admin`.
Cada nodo IO que provisiona (p.ej. `IO.wapp@<tenant>`) después **tiene su propia vida** — su propio
adaptador, canal y seguridad.

- Es un **singleton baked en el `.deb` core** (binario + `io-cloud.service` con `ExecCondition`
  gateado a `role: motherbee`), a diferencia de los adaptadores per-tenant (`io-api`/`io-slack`) que
  son runtime packages publicados y spawneados por instancia. (`docs/edge-ingress-spec-v6.md` §10)

### 1.1 El EDGE es la puerta, no la autoridad
- Diseño invariante: **el edge es la única puerta de entrada**. Está en un hive `ingress`
  (red física separada), resuelve el **perímetro** (TLS, verificación del token/código, límites,
  filtrado de headers). `IO.cloud` vive en **motherbee** —es un singleton con `ExecCondition` de rol, no un nodo instanciable en un worker— y **nunca sale directo a internet**.
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
- **Decisión alpha (2026-07-17):** el bearer de servicio configurado como `IO_CLOUD_SECRET` es la
  autoridad de Fluxbee Cloud. El edge valida ese bearer y Cloud **asegura** el `tenant_id` de cada
  request. `IO.cloud` valida el origen contra `IO_CLOUD_EDGE_NODE` + su ICH propio, acepta solo el catálogo §3.2 e
  inyecta el tenant en las llamadas admin. Una identidad/firma por usuario final queda diferida para
  producción; no se agregó un segundo mecanismo de auth paralelo.

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
4. **Self-externalize condicional y fail-closed**: si `IO_CLOUD_EDGE_NODE` está seteado también exige
   `IO_CLOUD_SECRET`; publica `auth_mode:"shared-secret"`, `methods:["POST"]` y reintenta hasta que el
   edge exista (`publish_channel_on_edge_with_retry`). No registra el bearer ni el payload completo.
5. **Gate local de entrada** (`authorize_cloud_message`): acepta únicamente mensajes con origen
   router-stamped igual a `IO_CLOUD_EDGE_NODE` y `meta.ich` igual al ICH propio de `IO.cloud`.
6. **Relay real y acotado** (`translate_cloud_op` + `dispatch_cloud_op`): traduce
   `create_tenant`→`create_tenant`, `put_token`→`vault_put` y `provision_node`→`run_node`, propaga
   `request_id` y devuelve respuestas shapeadas sin lectura de secretos.
7. **Catch-all sin respuestas espurias**: como `RouteMatch::Any` también recibe notificaciones internas,
   los mensajes cuyo origen router-stamped no sea el edge configurado se ignoran sin responder. Una
   petición que sí llega desde ese edge pero no cumple ICH/método/contrato recibe el rechazo shapeado.

Lo que **NO** tiene hoy:
- Sin dispatch por `ich` a tenant/conversación (el comentario "a real IO.cloud dispatches by ich" es
  solo eso, un comentario).
- Sin cliente saliente a ningún backend externo (deps: `fluxbee-sdk`, `io-common`, `serde_json`,
  `tokio`, `tracing` — **cero** `reqwest`/`hyper`/`axum`).
- Sin identidad/firma de usuario final; en alpha el bearer de servicio es la autoridad y el tenant es
  un claim confiado de Cloud.
- Nunca escribe a vault (para shared-secret solo pasa `IO_CLOUD_SECRET` como `params.secret` y admin
  lo guarda). Para tokens de proveedor escribe siempre vía `SY.admin`, nunca directo.

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
  │  Cloud  │◀─────────────│  (ingress)   │◀──────────────────────────────│(motherbee)│
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
| `put_token` → `vault_put` | Command | `SY.vault` | pool del tenant u owner `IO.*` ya registrado; descarta `ilk`/ownership crudo (ver §4) |
| `provision_node` → `run_node` | Command | `SY.orchestrator` | spawn de un nodo `IO.*`; **requiere tenant** |

`externalize` se usa internamente durante el bootstrap de `IO.cloud`, pero no forma parte del API
público de Cloud. `set_ilk_definition`, `start_node`, lecturas de vault y el resto de admin tampoco se
exponen. `IO_CLOUD_EXPOSED_ACTIONS` en `SY.admin` contiene exactamente las tres acciones de la tabla.

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
  `metadata.ilk` vía identity SHM (`find_ilk_by_handler_node_from_hive_config`). El fallback
  determinista se permite solo para nodos `SY.*`; un owner dinámico `IO.*` no registrado falla de
  forma explícita. El secreto queda owned por ESE ilk, **no** por el del escritor.
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
3. Elegir el orden según el secreto:
   - si el nodo lo necesita en el primer boot, **`put_token` sin `owner_node`** lo guarda en el pool
     del tenant;
   - para un secreto dedicado, ejecutar primero **`provision_node`**, que registra el ilk real, y luego
     `put_token` con `owner_node = IO.wapp@<tenant>`.
4. **`provision_node`** de `IO.wapp` (runtime `io.wapp`, con `tenant_id`; systemd
   `fluxbee-node-<name>`). Los nodos managed se registran como principals `agent`, no `system`.
5. **`put_token` owner-scoped** posterior al registro, cuando se requiera que solo ese nodo lo lea.
6. **`externalize`** del canal inbound de `IO.wapp` (URL pública para los **webhooks** de WhatsApp) — el
   flujo ya endurecido esta sesión.
7. `IO.wapp` corriendo: **outbound** (cliente a la API de WhatsApp, lee su token de vault) + **inbound**
   (webhook → edge → `IO.wapp`).

> **Ojo (net-new):** hoy NO existe el runtime `io.wapp`. El relay genérico de tenant/token/spawn ya
> existe; una transacción compuesta con rollback y externalize del adaptador sigue siendo trabajo de
> producto sobre el plumbing managed-node (§6).

---

## 6. Cómo se crean tenant / ilk / nodo (grounded)

- **Tenant:** `create_tenant` (admin) → `SY.identity :: TNT_CREATE` → `IdentityStore::create_tenant`
  (mint `tenant_id = tnt:<uuid>`). El default genérico es `pending`; `IO.cloud` envía `active` por
  default porque el bearer de Cloud es la autoridad alpha. Autorizado en identity solo para
  `SY.admin@`/`SY.architect@`/`SY.frontdesk.gov@` **same-hive** — **`IO.*` no puede crear tenants
  directo**, solo vía admin. Root tenant = `tnt:00000000-…-0001` (`fluxbee`, guarda secretos de infra).
- **Ilk de nodo IO:** `ILK_PROVISION` (autorizado por prefijo `IO.`, cross-hive) →
  `provision_temporary_ilk`: mint `ilk:<uuid>`, `registration_status = "temporary"`, canal `enabled:false`,
  `owner_l2_name: None`. Solo `human`/`agent` (rechaza `system`). **Completarlo** (owner, enable via
  `ILK_ADD_CHANNEL`/`ICH_SET_ENABLED`) es un paso aparte, no codificado como un flow único.
- **Spawn:** `run_node` (admin) → `SY.orchestrator :: run_node_flow` → `SPAWN_NODE`. Runtime = primeros
  2 segmentos del nombre (`IO.wapp.<x>` → `io.wapp`). **Requiere `tenant_id`** para `IO.`/`AI.`
  (`run_node_args_require_tenant`). Inyecta `FLUXBEE_NODE_ILK_ID`/`FLUXBEE_NODE_TENANT_ID` al unit y
  registra workloads managed (`AI`/`IO`/`WF`/`RT`) como principals `agent`.
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
  - **Opaco grande** (archivo, contexto LLM) → public artifact out-of-band. El productor llama directo
    a `SY.admin` para conservar su identidad; admin deriva tenant y usa `IO.blob` solo como curator.
    La URL es `/public/<capability>`, no contiene el hash. V1 es link-capability; el acceso verificado
    por tenant se agrega luego mediante Cloud. Ver `io-blob-spec-v1.md`.
  - Complementos: **gzip** del payload (~10x, previsto), y solo si hace falta, chunking multi-frame.

---

## 8. Consideraciones de seguridad (tener presentes)

### 8.1 EDGE-06 — gate del relay Cloud (cerrado para el catálogo alpha)
El `ADMIN_COMMAND` general no está router-origin-gated, por lo que el riesgo histórico de
*confused deputy* sigue siendo relevante para acciones fuera de esta vertical. El relay público de
Cloud queda acotado por tres controles coordinados:

- `SY.edge` valida el bearer `IO_CLOUD_SECRET`, acepta solo `POST` y remueve `Authorization` antes de
  reenviar.
- `IO.cloud` exige el origen router-stamped configurado en `IO_CLOUD_EDGE_NODE`, su ICH propio y
  traduce solo tres operaciones;
  un payload no puede elegir un action admin arbitrario.
- `SY.admin::authorize_cloud_relay` permite `create_tenant`, `vault_put` y `run_node` por mesh solo
  desde el singleton `IO.cloud` de su propio hive. `IO_CLOUD_EXPOSED_ACTIONS` es la misma fuente usada
  por el catálogo publicado.

Por lo tanto, internet no obtiene acceso al dispatcher admin general. La autorización de usuario
final continúa diferida: en alpha el bearer de servicio autoriza a Cloud a asegurar cualquier
`tenant_id` canónico.

Antecedente confirmado en código:
- El path `ADMIN_COMMAND` **NO está router-origin-gated**: el gate de autoridad del router
  (`serialize_for_local_delivery` → `system_policy::authority`) **solo dispara para `msg_type == SYSTEM`**;
  `ADMIN_COMMAND` es `msg_type = "admin"` → **lo bypassea** y llega a admin sin control.
- Acciones fuera de `IO_CLOUD_EXPOSED_ACTIONS` conservan sus gates propios o el comportamiento
  general previo; endurecer todo `ADMIN_COMMAND` es una tarea separada para no cambiar circuitos
  internos existentes.

### 8.2 Caveat B — ordering del `owner_node → ilk` para nodos dinámicos
El `owner_node → ilk` en `vault_put` resuelve vía identity SHM **al momento de escribir**. Para un nodo
dinámico no registrado, admin ahora falla de forma explícita; ya no escribe con un ilk determinista
incorrecto. Un token owner-scoped se guarda después de `provision_node`. Si es necesario para el
primer boot se omite `owner_node` y se usa el pool del tenant. Los nodos `SY.*` conservan el fallback
determinista legítimo. (`src/bin/sy_admin.rs :: normalize_vault_put_payload`.)

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
- **Ownership de vault no aseverable:** `put_token` descarta `metadata.ilk`, `owner_ilk`, `owner_l2`
  y `tenant_id`; solo acepta `owner_node` `IO.*` para que admin resuelva el ilk real, o ausencia para
  usar el pool del tenant autenticado.
- **Aislamiento por-tenant alpha** descansa en Cloud honesto: el bearer de servicio le permite asegurar
  un `tenant_id` canónico y `IO.cloud` lo sobreescribe en metadata/config antes del relay. La firma o
  identidad de usuario final es el endurecimiento pendiente para producción.
- **Ya cerrado esta sesión (no re-abrir):** EDGE-01 (origin binding cross-hive, `a5a5912`), EDGE-05
  (edge↔IO rechaza SYSTEM, `b1932e1`), authz de externalize (I1+I8, `99d1d85`). El edge no se toca.

---

## 9. Problemas pendientes / futuro (ya conversados)

1. **Identidad/firma de usuario final Cloud→tenant.** Diferida por decisión alpha; necesaria antes de
   entregar autoridad de provisioning a tokens de usuario o ampliar el catálogo.
2. **Runtime de producto.** No existe `io.wapp`; debe implementarse y publicarse antes de lanzar ese
   adaptador. Los runtimes existentes sí pueden provisionarse con el relay genérico.
3. **Consumo del secreto por el runtime.** `put_token` deja el token disponible en Vault; cada runtime
   específico debe leerlo con los helpers Vault canónicos. `IO.api` es una excepción deliberada:
   su bearer de ingreso no viene de `put_token` ni de config, sino que Admin lo genera durante
   `externalize` y lo dedica al Edge.
4. **Transacción compuesta de provisioning.** Las tres primitivas son funcionales, pero no hay rollback
   atómico create_tenant→token→spawn→externalize ante fallas intermedias.
5. **`IO.archi` (caso 2, diferido — "ahora no"):** forwarding HTTP per-tenant a `SY.architect` (`:3000`)
   tras login. Temas: aislamiento de tenant, auth de sesión, y **streaming (websocket/SSE)** que el edge
   hoy no hace (es request/response). Charlar aparte.
6. **`IO.blob` + paginación** (§7): reads grandes. Blob para opaco, paginación para estructurado.
7. **Atributo durable `externalized` en `SY.identity`** — prerequisito para EDGE-05 Fase 2 (acotar edge→IO
   a solo owners externalizados). Hoy diferido (`sy_admin.rs`); sin él, la fuente del allowlist sería el
   edge comprometido (auto-certificado).
8. **Producto conversacional de `IO.cloud`.** El provisioning está implementado; dispatch a
   conversaciones y un cliente saliente al backend Cloud siguen fuera de este contrato.

---

## 10. Decisiones cerradas para el alpha

- El bearer de servicio de Cloud es la autoridad y el payload transporta el `tenant_id` asegurado por
  Cloud; no hay firma per-request adicional en esta etapa.
- El gate se aplica en ambos extremos del seam: traducción allowlisted en `IO.cloud` y caller gate en
  `SY.admin` sobre el mismo catálogo.
- `IO.cloud` puede crear tenants y los crea `active` por default. También puede guardar tokens y lanzar
  nodos `IO.*` dentro de un tenant existente.
