# Fluxbee — Referencia completa de acciones SY.admin

**Fecha extracción:** 2026-04-24  
**Fuente:** `src/bin/sy_admin.rs`  
**Total acciones:** 66  
**Propósito:** Documentar exactamente qué información está disponible en el help de admin para los modelos que usan `get_admin_action_help`.

---

## Notas de uso

Archi consulta este help en tiempo de ejecución llamando:
- `GET /admin/actions` — lista todas las acciones disponibles
- `GET /admin/actions/{action}` — detalle de una acción específica

El PlanCompiler tiene la herramienta `get_admin_action_help` como FunctionTool registrado.  
Archi accede al mismo endpoint a través del sistema de lectura genérico (`fluxbee_system_read`).

### Acciones que requieren CONFIRM

`publish_runtime_package`, `remove_hive`, `kill_node`, `remove_node_instance`, `remove_runtime_version`, `set_node_config`, `node_control_config_set`, `set_storage`, `update`, `sync_hint`, `opa_compile`, `opa_compile_apply`, `opa_apply`, `opa_rollback`, `wf_rules_compile`, `wf_rules_compile_apply`, `send_node_message`

---

## Categoría 1 — Metadata y catálogo (2 acciones)

### `list_admin_actions`
- **Path:** `GET /admin/actions`
- **Descripción:** Lista el catálogo dinámico de acciones admin con metadata de help.
- **Read-only:** sí

### `get_admin_action_help`
- **Path:** `GET /admin/actions/{action}`
- **Descripción:** Retorna help metadata para una acción admin específica.
- **Read-only:** sí
- **Path param:** `action` — nombre de la acción, e.g. `add_hive`

---

## Categoría 2 — Topología de hives (5 acciones)

### `hive_status`
- **Path:** `GET /hive/status`
- **Descripción:** Lee el resumen de status del hive local.
- **Read-only:** sí

### `list_hives`
- **Path:** `GET /hives`
- **Descripción:** Lista todos los hives conocidos.
- **Read-only:** sí

### `get_hive`
- **Path:** `GET /hives/{hive}`
- **Descripción:** Lee la definición de un hive.
- **Read-only:** sí
- **Path param:** `hive` — id del hive, e.g. `worker-220`

### `add_hive`
- **Path:** `POST /hives`
- **Descripción:** Crea un hive y lo bootstrapea.
- **Read-only:** no | **Requiere CONFIRM:** no
- **Campos requeridos:**
  - `hive_id` (string): id único del hive a crear
  - `address` (string): dirección WAN o bootstrap accesible desde motherbee
- **Campos opcionales:**
  - `harden_ssh` (bool): habilitar hardening SSH tras bootstrap
  - `restrict_ssh` (bool): restringir acceso SSH tras provisioning
  - `require_dist_sync` (bool): requerir dist sync antes de éxito
  - `dist_sync_probe_timeout_secs` (u64): timeout para probe de dist sync
- **Nota:** Solo motherbee
- **Ejemplo:** `POST /hives {"hive_id":"worker-220","address":"192.168.8.220"}`

### `remove_hive`
- **Path:** `DELETE /hives/{hive}`
- **Descripción:** Elimina un hive.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Path param:** `hive`
- **Nota:** Solo motherbee

---

## Categoría 3 — Gestión de nodos (11 acciones)

### `list_nodes`
- **Path:** `GET /hives/{hive}/nodes`
- **Descripción:** Lista nodos de un solo hive. Para visibilidad global usar `/inventory`.
- **Read-only:** sí
- **Path param:** `hive`
- **Nota importante:** Para todos los nodos del sistema usar `GET /inventory` en vez de iterar hives.

### `get_node_status`
- **Path:** `GET /hives/{hive}/nodes/{node_name}/status`
- **Descripción:** Lee el status efectivo de runtime de un nodo.
- **Read-only:** sí
- **Path params:** `hive`, `node_name` (e.g. `SY.admin@motherbee`)

### `get_node_state`
- **Path:** `GET /hives/{hive}/nodes/{node_name}/state`
- **Descripción:** Lee el payload de estado persistido de un nodo.
- **Read-only:** sí

### `get_node_config`
- **Path:** `GET /hives/{hive}/nodes/{node_name}/config`
- **Descripción:** Lee el snapshot de `config.json` almacenado para un nodo gestionado.
- **Read-only:** sí
- **Nota crítica:** Lee la config persistida (snapshot), NO el contrato live del nodo. Para el contrato live usar `node_control_config_get`.

### `run_node` ⭐
- **Path:** `POST /hives/{hive}/nodes`
- **Descripción:** Crea e inicia una nueva instancia de nodo gestionado en un hive.
- **Read-only:** no | **Requiere CONFIRM:** no
- **Path param:** `hive` — hive destino donde se creará el nodo
- **Campos requeridos:**
  - `node_name` (string): nombre completo del nodo a iniciar
- **Campos opcionales:**
  - `runtime` (string): nombre del runtime (opcional si derivable del node_name)
  - `runtime_version` (string): versión del runtime (default: `current`)
  - `tenant_id` (string): **tenant id para el primer spawn** (requerido por algunos runtimes, incluidos los nodos AI multi-tenant)
  - `unit` (string): override de sufijo de unit systemd
  - `config` (object): config de runtime/nodo pasada durante el spawn
- **Nota:** Usar SOLO para crear/spawnnear una nueva instancia gestionada.
- **Ejemplo:** `POST /hives/motherbee/nodes {"node_name":"AI.support@motherbee","runtime":"ai.common","runtime_version":"current","tenant_id":"tnt:43d576a3-..."}`

### `start_node`
- **Path:** `POST /hives/{hive}/nodes/{node_name}/start`
- **Descripción:** Inicia una instancia de nodo gestionada **que ya existe** en el sistema.
- **Read-only:** no | **Requiere CONFIRM:** no
- **Nota crítica:** Usar cuando la instancia ya existe y se quiere reiniciar. Reutiliza `config.json` y metadatos de runtime almacenados. Para crear una nueva instancia usar `run_node`.

### `restart_node`
- **Path:** `POST /hives/{hive}/nodes/{node_name}/restart`
- **Descripción:** Reinicia una instancia de nodo gestionada existente.
- **Read-only:** no | **Requiere CONFIRM:** no

### `kill_node`
- **Path:** `DELETE /hives/{hive}/nodes/{node_name}`
- **Descripción:** Detiene un nodo. Con `purge_instance` también elimina el directorio de instancia persistido.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos opcionales:**
  - `force` (bool): forzar stop
  - `purge_instance` (bool): eliminar directorio de instancia persistido tras stop
- **Ejemplo:** `DELETE /hives/motherbee/nodes/AI.chat@motherbee {"force":false,"purge_instance":true}`

### `remove_node_instance`
- **Path:** `DELETE /hives/{hive}/nodes/{node_name}/instance`
- **Descripción:** Elimina una instancia de nodo del estado en disco.
- **Read-only:** no | **Requiere CONFIRM:** sí

### `send_node_message`
- **Path:** `POST /hives/{hive}/nodes/{node_name}/messages`
- **Descripción:** Envía un mensaje de sistema a un nodo.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `msg_type` (string), `payload` (object)
- **Campos opcionales:** `msg`, `ttl`, `scope`, `priority`, `src_ilk`, `context`

### `set_node_config`
- **Path:** `PUT /hives/{hive}/nodes/{node_name}/config`
- **Descripción:** Persiste cambios de config de un nodo.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `config` (object)
- **Campos opcionales:** `replace` (bool): reemplazar config completa vs patch; `notify` (bool): notificar runtime

---

## Categoría 4 — Control plane config (2 acciones)

### `node_control_config_get`
- **Path:** `POST /hives/{hive}/nodes/{node_name}/control/config-get`
- **Descripción:** Envía CONFIG_GET a un nodo que participa en control-plane live y retorna su CONFIG_RESPONSE.
- **Read-only:** sí | **Requiere CONFIRM:** no
- **Aplica a:** AI.*, IO.*, WF.*, SY.storage, SY.identity, SY.cognition, SY.config.routes
- **Nota crítica:** Usar para el contrato vivo del nodo. Para config persistida usar `get_node_config`. El nodo DEBE estar corriendo para responder.
- **Campos opcionales:** `request_id`, `contract_version`, `requested_by`, `src_ilk`, `scope`, `context`, `ttl`

### `node_control_config_set`
- **Path:** `POST /hives/{hive}/nodes/{node_name}/control/config-set`
- **Descripción:** Envía CONFIG_SET a un nodo live y retorna su CONFIG_RESPONSE.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `schema_version` (u32), `config_version` (u64), `apply_mode` (string), `config` (object)
- **Nota WF.* v1:** CONFIG_SET es persist-only, retorna `restart_required`, no hot-aplica CONFIG_CHANGED.
- **Nota AI.*/IO.*:** soporta hot-apply (no requiere restart).

---

## Categoría 5 — Identidad e inventario (3 acciones)

### `list_ilks`
- **Path:** `GET /hives/{hive}/identity/ilks`
- **Read-only:** sí

### `get_ilk`
- **Path:** `GET /hives/{hive}/identity/ilks/{ilk_id}`
- **Read-only:** sí
- **Path param:** `ilk_id` en formato `ilk:<uuid>`

### `inventory`
- **Path:** `GET /inventory`, `GET /inventory/summary`, `GET /inventory/{hive}`, `GET /hives/{hive}/inventory/summary`
- **Descripción:** Inventario global o por hive incluyendo visibilidad de nodos del sistema completo.
- **Read-only:** sí
- **Uso:** Para todos los nodos del sistema usar `GET /inventory`. Para un hive específico `GET /inventory/{hive}`.

---

## Categoría 6 — Versiones y runtimes (7 acciones)

### `list_versions`
- **Path:** `GET /versions`
- **Read-only:** sí
- **Nota:** Para versiones SY.* mapear node_name a core.components. Para AI.*/IO.* mapear a runtimes.runtimes[runtime].current.

### `get_versions`
- **Path:** `GET /hives/{hive}/versions`
- **Read-only:** sí

### `list_runtimes`
- **Path:** `GET /hives/{hive}/runtimes`
- **Read-only:** sí
- **Uso frecuente:** Verificar qué runtimes están disponibles y materializados antes de crear un nodo.

### `get_runtime`
- **Path:** `GET /hives/{hive}/runtimes/{runtime}`
- **Read-only:** sí
- **Path param:** `runtime` — nombre del runtime, e.g. `ai.common`

### `publish_runtime_package`
- **Path:** `POST /admin/runtime-packages/publish`
- **Descripción:** Publica un runtime package en dist/manifest en motherbee.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:**
  - `source` (object): descriptor del package (`source.kind` = `inline_package` o `bundle_upload`)
- **Campos opcionales:**
  - `set_current` (bool): promover esta versión a current
  - `sync_to` (string[]): hives que deben recibir sync_hint(channel=dist) tras publicar
  - `update_to` (string[]): hives que deben recibir update(category=runtime) para el nuevo runtime/versión
- **Nota:** Solo motherbee. Publica en dist/manifest pero NO spawnea nodos.

### `remove_runtime_version`
- **Path:** `DELETE /hives/{hive}/runtimes/{runtime}/versions/{version}`
- **Read-only:** no | **Requiere CONFIRM:** sí

---

## Categoría 7 — Routing y VPN (5 acciones)

### `list_routes`
- **Path:** `GET /hives/{hive}/routes`
- **Read-only:** sí

### `add_route`
- **Path:** `POST /hives/{hive}/routes`
- **Descripción:** Agrega una regla de routing a un hive.
- **Read-only:** no | **Requiere CONFIRM:** no
- **Campos requeridos:**
  - `prefix` (string): prefijo de ruta, e.g. `AI.chat.` o `tenant.acme`
  - `action` (string): `FORWARD` o `DROP` (case-sensitive)
- **Campos opcionales:**
  - `match_kind` (string): `PREFIX` (default), `EXACT`, `GLOB`
  - `next_hop_hive` (string): hive destino — requerido cuando action=FORWARD
  - `metric` (u32): métrica para tie-breaking (default 0, menor = preferido)
  - `priority` (u16): prioridad (default 100, mayor = primero evaluado)
- **Ejemplo:** `POST /hives/motherbee/routes {"prefix":"AI.chat.","action":"FORWARD","next_hop_hive":"worker-220"}`

### `delete_route`
- **Path:** `DELETE /hives/{hive}/routes/{prefix}`
- **Read-only:** no | **Requiere CONFIRM:** no

### `list_vpns`
- **Path:** `GET /hives/{hive}/vpns`
- **Read-only:** sí

### `add_vpn`
- **Path:** `POST /hives/{hive}/vpns`
- **Read-only:** no | **Requiere CONFIRM:** no
- **Campos requeridos:** `pattern` (string), `vpn_id` (u32)
- **Campos opcionales:** `match_kind`, `priority`

### `delete_vpn`
- **Path:** `DELETE /hives/{hive}/vpns/{pattern}`
- **Read-only:** no | **Requiere CONFIRM:** no

---

## Categoría 8 — Storage y deployments (6 acciones)

### `get_storage`
- **Path:** `GET /config/storage`
- **Read-only:** sí

### `set_storage`
- **Path:** `PUT /config/storage`
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `path` (string): ruta de storage absoluta

### `list_deployments`
- **Path:** `GET /deployments`
- **Read-only:** sí

### `get_deployments`
- **Path:** `GET /hives/{hive}/deployments`
- **Read-only:** sí

### `list_drift_alerts`
- **Path:** `GET /drift-alerts`
- **Read-only:** sí

### `get_drift_alerts`
- **Path:** `GET /hives/{hive}/drift-alerts`
- **Read-only:** sí
- **Nota:** Si retorna `entries: []`, no inferir drift de otros endpoints.

---

## Categoría 9 — OPA Policy (7 acciones)

### `opa_get_policy`
- **Path:** `GET /hives/{hive}/opa/policy`
- **Read-only:** sí

### `opa_get_status`
- **Path:** `GET /hives/{hive}/opa/status`
- **Read-only:** sí

### `opa_check`
- **Path:** `POST /hives/{hive}/opa/policy/check`
- **Descripción:** Valida un texto Rego sin aplicarlo.
- **Read-only:** sí
- **Campos requeridos:** `rego` (string)
- **Campos opcionales:** `entrypoint` (default `router/target`), `version`

### `opa_compile`
- **Path:** `POST /hives/{hive}/opa/policy/compile`
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `rego` (string)

### `opa_compile_apply`
- **Path:** `POST /hives/{hive}/opa/policy`
- **Descripción:** Compila y aplica política OPA.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `rego` (string)

### `opa_apply`
- **Path:** `POST /hives/{hive}/opa/policy/apply`
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `version` (u64): versión compilada a aplicar

### `opa_rollback`
- **Path:** `POST /hives/{hive}/opa/policy/rollback`
- **Read-only:** no | **Requiere CONFIRM:** sí

---

## Categoría 10 — Workflow Rules (8 acciones)

### `wf_rules_list_workflows`
- **Path:** `GET /hives/{hive}/wf-rules`
- **Read-only:** sí
- **Nota:** Sin `workflow_name` lista todos los workflows.

### `wf_rules_get_workflow`
- **Path:** `GET /hives/{hive}/wf-rules?workflow_name=...`
- **Read-only:** sí

### `wf_rules_get_status`
- **Path:** `GET /hives/{hive}/wf-rules/status?workflow_name=...`
- **Read-only:** sí

### `wf_rules_compile`
- **Path:** `POST /hives/{hive}/wf-rules/compile`
- **Descripción:** Compila una definición de workflow sin aplicarla.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `workflow_name` (string), `definition` (object)
- **Nota:** `workflow_name` debe matchear `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$` — sin underscores.

### `wf_rules_compile_apply`
- **Path:** `POST /hives/{hive}/wf-rules`
- **Descripción:** Compila y aplica una definición de workflow.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `workflow_name` (string), `definition` (object)
- **Campos opcionales:**
  - `auto_spawn` (bool): spawnear el nodo WF si no existe tras apply
  - `tenant_id` (string): requerido en primer deploy cuando `auto_spawn=true` y el nodo WF no existe
  - `version` (u64): versión explícita
- **Ejemplo:** `POST /hives/motherbee/wf-rules {"workflow_name":"invoice","definition":{...},"auto_spawn":true,"tenant_id":"tnt:43d576a3-..."}`

### `wf_rules_apply`
- **Path:** `POST /hives/{hive}/wf-rules/apply`
- **Read-only:** no | **Requiere CONFIRM:** no
- **Campos requeridos:** `workflow_name`
- **Campos opcionales:** `auto_spawn`, `tenant_id`, `version`

### `wf_rules_rollback`
- **Path:** `POST /hives/{hive}/wf-rules/rollback`
- **Read-only:** no | **Requiere CONFIRM:** no
- **Campos requeridos:** `workflow_name`

### `wf_rules_delete`
- **Path:** `POST /hives/{hive}/wf-rules/delete`
- **Read-only:** no | **Requiere CONFIRM:** no
- **Campos requeridos:** `workflow_name`
- **Campos opcionales:** `force` (bool)

---

## Categoría 11 — Hive update y sync (2 acciones)

### `update`
- **Path:** `POST /hives/{hive}/update`
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `manifest_hash` (string)
- **Campos opcionales:** `category` (enum runtime|core|vendor), `manifest_version`, `runtime`, `runtime_version`

### `sync_hint`
- **Path:** `POST /hives/{hive}/sync-hint`
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos opcionales:** `channel` (enum blob|dist), `folder_id`, `wait_for_idle`, `timeout_ms`

---

## Categoría 12 — Timer (8 acciones)

### `timer_help`
- **Path:** `GET /hives/{hive}/timer/help`
- **Descripción:** Lee el catálogo de capacidades de SY.timer para un hive.
- **Read-only:** sí

### `timer_get`
- **Path:** `GET /hives/{hive}/timer/timers/{timer_uuid}`
- **Read-only:** sí

### `timer_list`
- **Path:** `GET /hives/{hive}/timer/timers`
- **Read-only:** sí
- **Campos opcionales:** `owner_l2_name`, `status_filter` (pending|fired|canceled|all), `limit`

### `timer_now`
- **Path:** `GET /hives/{hive}/timer/now`
- **Read-only:** sí

### `timer_now_in`
- **Path:** `POST /hives/{hive}/timer/now-in`
- **Read-only:** sí
- **Campos requeridos:** `tz` (string IANA, e.g. `America/Argentina/Buenos_Aires`)

### `timer_convert`
- **Path:** `POST /hives/{hive}/timer/convert`
- **Read-only:** sí
- **Campos requeridos:** `instant_utc_ms` (i64), `to_tz` (string IANA)

### `timer_parse`
- **Path:** `POST /hives/{hive}/timer/parse`
- **Read-only:** sí
- **Campos requeridos:** `input` (string), `layout` (Go time layout), `tz` (IANA)

### `timer_format`
- **Path:** `POST /hives/{hive}/timer/format`
- **Read-only:** sí
- **Campos requeridos:** `instant_utc_ms` (i64), `layout` (Go time layout), `tz` (IANA)

---

## Casos críticos de diagnóstico

### Crear un nodo nuevo
**Acción correcta:** `run_node`  
**Error frecuente:** usar `node_control_config_set` (que es para nodos que ya existen y corren)

```json
POST /hives/motherbee/nodes
{
  "node_name": "AI.support@motherbee",
  "runtime": "ai.common",
  "runtime_version": "current",
  "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
}
```

### Diferencia run_node vs start_node vs restart_node

| Acción | Cuándo usar |
| --- | --- |
| `run_node` | El nodo NO existe todavía — crear nueva instancia |
| `start_node` | El nodo existe pero está detenido — iniciar la instancia existente |
| `restart_node` | El nodo existe y está corriendo — reiniciarlo |

### Diferencia get_node_config vs node_control_config_get

| Acción | Retorna |
| --- | --- |
| `get_node_config` | Snapshot de `config.json` almacenado en disco. Funciona aunque el nodo esté detenido. |
| `node_control_config_get` | Contrato live del nodo (CONFIG_RESPONSE). Requiere que el nodo esté corriendo. |

### tenant_id en nodos AI

`tenant_id` es un campo **opcional** de `run_node` requerido para nodos que usan identidad multi-tenant. Para nodos AI que van a manejar conversaciones de un tenant específico, siempre pasar `tenant_id` en el spawn inicial. No es posible agregarlo después sin recrear el nodo.
