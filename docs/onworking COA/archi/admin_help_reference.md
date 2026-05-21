# Fluxbee — Referencia completa de acciones SY.admin

**Fecha extracción:** 2026-05-11  
**Fuente:** `src/bin/sy_admin.rs`  
**Total acciones:** 74
**Propósito:** Documentar exactamente qué información está disponible en el help de admin para los modelos que usan `get_admin_action_help`.

---

## Notas de uso

Archi consulta este help en tiempo de ejecución llamando:
- `GET /admin/actions` — lista todas las acciones disponibles
- `GET /admin/actions/{action}` — detalle de una acción específica

El PlanCompiler tiene la herramienta `get_admin_action_help` como FunctionTool registrado.  
Archi accede al mismo endpoint a través del sistema de lectura genérico (`fluxbee_system_get`).

### Nota sobre herramientas internas de Archi

`fluxbee_start_pipeline` y `fluxbee_pipeline_action` no son acciones SY.admin. Son herramientas internas de `SY.architect`.

Cuando `fluxbee_start_pipeline` encuentra un pipeline bloqueado en la sesión, devuelve una respuesta estructurada con:

```json
{
  "status": "blocked_run_pending",
  "next_tool": "fluxbee_pipeline_action",
  "allowed_actions": ["discard", "restart_from_design", "retry"]
}
```

Semántica:

- `discard` — cierra el pipeline bloqueado y libera la sesión.
- `restart_from_design` — cierra el pipeline bloqueado y rediseña desde cero con la misma tarea.
- `retry` — vuelve al último checkpoint de confirmación cuando existe un plan/checkpoint reintentable.

### Acciones que requieren CONFIRM

`publish_runtime_package`, `remove_hive`, `kill_node`, `remove_node_instance`, `remove_runtime_version`, `set_node_config`, `node_control_config_set`, `set_storage`, `create_tenant`, `update_tenant`, `set_tenant_sponsor`, `vault_put`, `vault_get`, `vault_delete`, `vault_rotate`, `vault_rollback`, `update`, `sync_hint`, `opa_compile`, `opa_compile_apply`, `opa_apply`, `opa_rollback`, `wf_rules_compile`, `wf_rules_compile_apply`, `send_node_message`

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
  - `tenant_id` (string): **tenant id para el primer spawn** (requerido a nivel raíz para nodos `AI.*` / `IO.*`)
  - `unit` (string): override de sufijo de unit systemd
  - `config` (object): config de runtime/nodo pasada durante el spawn
- **Nota:** Usar SOLO para crear/spawnnear una nueva instancia gestionada.
- **Ejemplo:** `POST /hives/motherbee/nodes {"node_name":"AI.support@motherbee","runtime":"ai.generic","runtime_version":"current","tenant_id":"tnt:43d576a3-..."}`

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
- **Nota crítica de versionado:** antes de mutar `AI.*` o `IO.*`, ejecutar `node_control_config_get` y usar `config_version = response.config_version + 1`. Un valor menor falla como `stale_config_version`; un valor igual puede ser idempotente y no aplicar cambios.
- **Nota `ai.generic`:** para OpenAI chat usar `config.behavior.kind=openai_chat`, no `openai`. Los assets cognitivos no van en `CONFIG_SET`; aplicar `role_hash`, `skill_hashes`, `handbook_hashes` y `personality_hash` con `set_ilk_definition` sobre el ILK del agente.
- **Nota WF.* v1:** CONFIG_SET es persist-only, retorna `restart_required`, no hot-aplica CONFIG_CHANGED.
- **Nota AI.*/IO.*:** soporta hot-apply (no requiere restart).

---

## Categoría 5 — Identidad e inventario (7 acciones)

### `list_ilks`
- **Path:** `GET /hives/{hive}/identity/ilks`
- **Read-only:** sí

### `get_ilk`
- **Path:** `GET /hives/{hive}/identity/ilks/{ilk_id}`
- **Read-only:** sí
- **Path param:** `ilk_id` en formato `ilk:<uuid>`

### `set_ilk_definition`
- **Path:** `POST /hives/{hive}/identity/ilks/{ilk_id}/definition`
- **Read-only:** no
- **Path param:** `ilk_id` en formato `ilk:<uuid>`
- **Body:** `definition` con `role_hash`, `skill_hashes`, `handbook_hashes`, `personality_hash` (este último single 64-hex string, no array).
- **Uso:** aplica la definición cognitiva de un agent ILK; no crea assets blob, solo registra los hashes validados en identity/SHM.
- **Nota:** `personality_hash` es opcional. Ausente o `null` ⇒ no se renderiza bloque de personalidad. El router proyecta `personality_hash` cuando está presente; la flat-view de `personality_timezone`/`country_code`/`primary_language` está descopada por ahora (OPA rules deben matchear por hash). Set parcial es válido: enviar sólo `personality_hash` deja role/skill/handbook intactos.

```json
POST /hives/motherbee/identity/ilks/ilk:ai-support/definition
{
  "definition": {
    "role_hash": "a1b2c3d4...",
    "skill_hashes": ["e5f6a7b8...", "c9d0e1f2..."],
    "handbook_hashes": ["3a4b5c6d..."],
    "personality_hash": "9f8e7d6c..."
  }
}
```

### `list_tenants`
- **Path:** `GET /hives/{hive}/identity/tenants`
- **Read-only:** sí
- **Uso:** descubrir tenants root/default, sponsors y tenants cliente antes de crear nodos multi-tenant.

### `get_tenant`
- **Path:** `GET /hives/{hive}/identity/tenants/{tenant_id}`
- **Read-only:** sí
- **Path param:** `tenant_id` en formato `tnt:<uuid>`
- **Uso:** leer un tenant puntual con sponsor resuelto, children y counts.

### `create_tenant`
- **Path:** `POST /hives/{hive}/identity/tenants`
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `name`
- **Campos opcionales:** `domain`, `status`, `settings`, `sponsor_tenant_id`
- **Executor args:** planos en `step.args`; no usar `body`.
- **Uso:** crear tenant admin/company sin sponsor, o tenant cliente con `sponsor_tenant_id` apuntando al tenant admin/company.
- **Nota:** idempotente por `name`/`domain`; si ya existe devuelve `created=false` con el `tenant_id` existente.

### `update_tenant`
- **Path:** `PUT /hives/{hive}/identity/tenants/{tenant_id}`
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos opcionales:** `name`, `domain`, `status`, `settings`, `sponsor_tenant_id`
- **Executor args:** planos en `step.args`; no usar `body`. Ejemplo: `{"hive":"motherbee","tenant_id":"tnt:...","name":"4i Platform Inc."}`.
- **Uso:** mutar campos de tenant existente. `sponsor_tenant_id:null` limpia sponsor.

### `set_tenant_sponsor`
- **Path:** `POST /hives/{hive}/identity/tenants/{tenant_id}/sponsor`
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `sponsor_tenant_id` (`tnt:<uuid>` o `null`)
- **Executor args:** planos en `step.args`; no usar `body`.
- **Uso:** cambio enfocado de relación sponsor sin tocar otros campos.

---

## Categoría 5b — Vault y secretos (7 acciones)

### `vault_list`
- **Path:** `GET /hives/{hive}/vault/secrets`
- **Descripción:** Lista metadata/resúmenes de secrets visibles para el caller; nunca devuelve valores.
- **Read-only:** sí
- **Campos opcionales:** `filter.prefix`, `filter.tenant_id`, `filter.resource_type`, `filter.ilk`, `filter.tags`, `filter.limit`
- **Executor args:** planos: `{"hive":"motherbee","filter":{"prefix":"infra:","resource_type":"openai","limit":100}}`
- **Nota:** callers no-admin solo ven secrets que podrían leer; esto evita enumeración de keys no autorizadas.

### `vault_put`
- **Path:** `POST /hives/{hive}/vault/secrets`
- **Descripción:** Escribe o actualiza un secret en `SY.vault`; el valor se cifra at-rest y se audita localmente.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `key` (string), `value` (object), `metadata` (object)
- **Metadata mínima:** `metadata.resource_type` (canonical lowercase snake_case). `metadata.tenant_id` es opcional y defaulta al root tenant fijo `tnt:00000000-0000-0000-0000-000000000001`; `metadata.owner_node` es opcional para dedicar el secret a un nodo específico; si se omite, el secret queda en el pool del tenant.
- **Executor args:** planos en `step.args`; no usar `body`.
- **Nota crítica:** `value` es sensible y debe quedar redacted en previews, history y logs. PUT con el mismo valor es idempotente y no incrementa versión.
- **Nota Model D':** `metadata.owner_ilk` es legacy y está rechazado en el path admin; usar `owner_node` o pool.
- **Resource types canónicos:** `postgres`, `mysql`, `redis`, `mongodb`, `openai`, `anthropic`, `gemini`, `mistral`, `cohere`, `perplexity`, `google_calendar`, `gmail`, `google_drive`, `google_sheets`, `google_docs`, `google_slides`, `google_cloud`, `microsoft_graph`, `outlook_email`, `outlook_calendar`, `teams`, `sharepoint`, `slack`, `discord`, `hubspot`, `salesforce`, `linked_helper`, `github`, `gitlab`, `jira`, `linear`, `notion`, `stripe`, `twilio`, `sendgrid`, `smtp`, `imap`, `aws`, `azure`, `s3`, `webhook`, `bearer_token`, `api_key`, `oauth_bundle`.
- **Ejemplo:** `POST /hives/motherbee/vault/secrets {"key":"infra:openai-api-key","value":{"api_key":"sk-..."},"metadata":{"resource_type":"openai","owner_node":"SY.architect","description":"OpenAI API key"}}`

### `vault_get_metadata`
- **Path:** `GET /hives/{hive}/vault/secrets/{key}/metadata`
- **Descripción:** Lee metadata de un secret sin devolver el valor.
- **Read-only:** sí
- **Executor args:** planos: `{"hive":"motherbee","key":"infra:openai-api-key"}`
- **Uso recomendado:** inspección por operador/Archi cuando no se necesita plaintext.

### `vault_get`
- **Path:** `GET /hives/{hive}/vault/secrets/{key}`
- **Descripción:** Lee el valor plaintext del secret.
- **Read-only:** sí | **Requiere CONFIRM:** sí
- **Executor args:** planos: `{"hive":"motherbee","key":"infra:openai-api-key"}`
- **Nota crítica:** usar solo si el operador pide explícitamente el valor. Para inspección normal usar `vault_get_metadata`.

### `vault_delete`
- **Path:** `DELETE /hives/{hive}/vault/secrets/{key}`
- **Descripción:** Borra un secret de `SY.vault`.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Executor args:** planos: `{"hive":"motherbee","key":"infra:openai-api-key"}`
- **Nota:** admin/architect only.

### `vault_rotate`
- **Path:** `POST /hives/{hive}/vault/secrets/{key}/rotate`
- **Descripción:** Rota el valor de un secret y conserva la versión previa para rollback.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Campos requeridos:** `key` (string), `value` (object)
- **Executor args:** planos: `{"hive":"motherbee","key":"infra:openai-api-key","value":{"api_key":"sk-..."}}`
- **Nota crítica:** `value` es sensible y debe quedar redacted en previews/history/logs.

### `vault_rollback`
- **Path:** `POST /hives/{hive}/vault/secrets/{key}/rollback`
- **Descripción:** Revierte un secret a su versión inmediatamente anterior.
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Executor args:** planos: `{"hive":"motherbee","key":"infra:openai-api-key"}`
- **Error esperado:** `NO_PREVIOUS_VERSION` si no hay versión anterior disponible.

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
- **Path param:** `runtime` — nombre del runtime, e.g. `ai.generic`

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

## Categoría — SY.architect local control plane

archi tiene su propio control-plane local que se invoca por SCMD desde el chat (operator) o por el admin gateway sobre `SY.architect@<hive>`. En Model D' no persiste secretos locales: consume OpenAI y la DB de mensajes desde `SY.vault` por `resource_type` (`openai`, `postgres`) y refresca runtimes en caliente cuando recibe `VAULT_SECRET_CHANGED`.

### `architect_local_config_get`
- **Path:** `GET /architect/control/config-get` (SCMD) o `POST /hives/{hive}/nodes/SY.architect@{hive}/control/config-get` (admin gateway)
- **Read-only:** sí
- **Descripción:** Devuelve el contrato live de archi: estado de cada recurso de vault (configured / missing_secret), valores redacted y `state` global. Hoy reporta OpenAI API key y messages DB URL.
- **Respuesta clave:** `payload.state` (configured | missing_secret) refleja sólo el recurso OpenAI (requerido). `payload.messages_db_state` reporta la DB de mensajes (opcional). `payload.contract.resources[]` / `payload.resources` enumera los recursos con `resource_type`, `required`, `resolved`, `source`, `vault_key`, `version`.

### `architect_local_config_set`
- **Path:** `POST /architect/control/config-set` (SCMD) o `POST /hives/{hive}/nodes/SY.architect@{hive}/control/config-set` (admin gateway)
- **Read-only:** no | **Requiere CONFIRM:** sí
- **Descripción:** Actualiza sólo configuración no secreta de archi. Los campos secret-bearing son rechazados; cargar/rotar credenciales se hace con `vault_put` / `vault_rotate`.
- **Campos secret-bearing rechazados:** `config.ai_providers.openai.api_key`, `config.ai_providers.openai.api_key_ref`, `config.storage.messages_db_url`, `config.storage.messages_db_url_ref`.
- **Campos opcionales del wrapper admin gateway:** `schema_version` (u32), `config_version` (u64), `apply_mode` (string). Los toma archi pero los ignora; el handler local persiste atómicamente.
- **Nota crítica:** para habilitar OpenAI o la DB de mensajes, usar `vault_put` con `metadata.resource_type="openai"` o `metadata.resource_type="postgres"`. Si el secret es del pool de infraestructura, omitir `metadata.tenant_id` y `owner_node`; si es dedicado a archi, usar `metadata.owner_node="SY.architect"`.
- **Endpoints relacionados que habilita:** `GET /api/messages` (lista paginada), `GET /api/messages/stream` (SSE tail), `GET /api/messages/{dedupe_key}` (detalle).

---

## Casos críticos de diagnóstico

### Crear un nodo nuevo
**Acción correcta:** `run_node`  
**Error frecuente:** usar `node_control_config_set` (que es para nodos que ya existen y corren)

```json
POST /hives/motherbee/nodes
{
  "node_name": "AI.support@motherbee",
  "runtime": "ai.generic",
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
