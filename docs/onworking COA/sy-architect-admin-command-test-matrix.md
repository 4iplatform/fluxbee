# SY.architect Admin Command Test Matrix

Estado: onworking  
Objetivo: validar sistemáticamente la cobertura de comandos existentes de `SY.admin` desde:

- `curl` HTTP-style path
- `SCMD` dentro de `SY.architect`

## 1. Scope

Este documento cubre el catálogo actual de acciones de `SY.admin` que `SY.architect` ya puede traducir/usar por SCMD.

Conclusión actual:

- No encontré huecos funcionales en `SY.architect` respecto del catálogo actual de `SY.admin`.
- La fricción principal hoy no parece ser “falta un comando”, sino:
  - secuencia de descubrimiento de capacidades
  - uso errático del modelo antes de converger al path/body correcto

## 2. Cómo usar este documento

Para cada acción:

- probá primero el `curl` canónico
- después probá el equivalente `SCMD:` en archi
- si es mutación, usá entorno de prueba y validá el efecto con un GET/check posterior

Bases sugeridas:

```bash
# directo al servicio local
BASE="http://127.0.0.1:8080"

# externo por reverse proxy
# requiere que nginx strippee /control
# BASE="https://fluxbee.ai/control"
```

Regla práctica:

- `curl` valida contrato backend
- `SCMD` valida traducción de architect
- lenguaje natural valida comportamiento del modelo, no el contrato
- en `curl`, usá siempre `"$BASE/..."`
- en `SCMD`, no agregues `/control`; el path sigue siendo relativo a `SY.admin`

## 3. Variables sugeridas

Usá estos valores y cambialos cuando haga falta:

```text
LOCAL_HIVE=motherbee
TEST_HIVE=worker-220-other
TEST_ADDR=192.168.8.220
TEST_RUNTIME=AI.chat
TEST_RUNTIME_VERSION=1.2.3
TEST_NODE=SY.admin@motherbee
TEST_MANAGED_NODE=SY.frontdesk.gov@motherbee
TEST_ROUTE_PREFIX=AI.chat.
TEST_VPN_PATTERN=worker-*
TEST_ILK=ilk:550e8400-e29b-41d4-a716-446655440000
TEST_OPA_VERSION=12
TEST_WF_NAME=test-wf-rules-archi
TEST_TENANT_ID=tnt:43d576a3-d712-4d91-9245-5d5463dd693e
```

## 4. Read-Only / Discovery

### 4.1 Admin catalog

[x] `list_admin_actions`

- curl:
```bash
curl -sS "$BASE/admin/actions"
```
- SCMD:
```text
SCMD: curl -X GET /admin/actions
```
- check:
  - devuelve catálogo dinámico de acciones

[x] `get_admin_action_help`

- curl:
```bash
curl -sS "$BASE/admin/actions/add_hive"
```
- SCMD:
```text
SCMD: curl -X GET /admin/actions/add_hive
```
- check:
  - incluye `request_contract`, `example_scmd`, `path_patterns`

### 4.2 Hive / global status

[x] `hive_status`

- curl:
```bash
curl -sS "$BASE/hive/status"
```
- SCMD:
```text
SCMD: curl -X GET /hive/status
```
- check:
  - `status=ok`

[x] `list_hives`

- curl:
```bash
curl -sS "$BASE/hives"
```
- SCMD:
```text
SCMD: curl -X GET /hives
```
- check:
  - lista hives conocidas

[x] `get_hive`

- curl:
```bash
curl -sS "$BASE/hives/$TEST_HIVE"
```
- SCMD:
```text
SCMD: curl -X GET /hives/worker-220-other
```
- check:
  - devuelve definición de una hive

[x] `inventory` summary

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/inventory/summary"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/inventory/summary
```
- check:
  - total de hives/nodes y timestamps

[x] `inventory` hive

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/inventory/hive"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/inventory/hive
```
- check:
  - inventario detallado de esa hive

### 4.3 Versions / runtimes / deployments / drift

[x] `list_versions`

- curl:
```bash
curl -sS "$BASE/versions"
```
- SCMD:
```text
SCMD: curl -X GET /versions
```
- check:
  - vista global de versiones

[x] `get_versions`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/versions"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/versions
```
- check:
  - versiones/readiness de una hive

[x] `list_runtimes`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/runtimes"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/runtimes
```
- check:
  - runtimes presentes

[x] `get_runtime`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/runtimes/$TEST_RUNTIME"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/runtimes/AI.chat
```
- check:
  - detalle de runtime

[x] `list_deployments`

- curl:
```bash
curl -sS "$BASE/deployments"
```
- SCMD:
```text
SCMD: curl -X GET /deployments
```
- check:
  - lista global de deployments

[x] `get_deployments`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/deployments"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/deployments
```
- check:
  - historial de deployments por hive

[x] `list_drift_alerts`

- curl:
```bash
curl -sS "$BASE/drift-alerts"
```
- SCMD:
```text
SCMD: curl -X GET /drift-alerts
```
- check:
  - alertas globales

[x] `get_drift_alerts`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/drift-alerts"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/drift-alerts
```
- check:
  - historial de drift alerts por hive

### 4.4 Nodes / identity / storage / network

[x] `list_nodes`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/nodes"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/nodes
```
- check:
  - lista instancias persistidas/visibles

[x] `get_node_status`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/nodes/$TEST_NODE/status"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/nodes/SY.admin@motherbee/status
```
- check:
  - snapshot canónico de lifecycle/health

[x] `get_node_config`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/nodes/$TEST_MANAGED_NODE/config"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/nodes/SY.frontdesk.gov@motherbee/config
```
- check:
  - config persistida de un nodo gestionado
  - no usar `SY.*` como ejemplo acá: puede no tener `config.json` node-managed

[x] `get_node_state`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/nodes/$TEST_NODE/state"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/nodes/SY.admin@motherbee/state
```
- check:
  - state diagnóstica o `null`

[x] `list_ilks`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/identity/ilks"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/identity/ilks
```
- check:
  - ilks disponibles

[x] `get_ilk`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/identity/ilks/$TEST_ILK"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/identity/ilks/ilk:550e8400-e29b-41d4-a716-446655440000
```
- check:
  - detalle de ILK
  - filtra por `ilk_id` exacto en formato `ilk:<uuid>`

[x] `list_routes`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/routes"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/routes
```
- check:
  - rutas configuradas

[x] `list_vpns`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/vpns"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/vpns
```
- check:
  - patrones VPN configurados

[x] `get_storage`

- curl:
```bash
curl -sS "$BASE/config/storage"
```
- SCMD:
```text
SCMD: curl -X GET /config/storage
```
- check:
  - path de storage actual

### 4.5 OPA read / validate

[x] `opa_get_policy`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/opa/policy"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/opa/policy
```
- check:
  - policy actual

[x] `opa_get_status`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/opa/status"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/opa/status
```
- check:
  - status OPA

[x] `opa_check`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/opa/policy/check" \
  -H 'Content-Type: application/json' \
  -d '{"rego":"package router\n\ndefault target = null\n","entrypoint":"router/target"}'
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/opa/policy/check -d '{"rego":"package router\n\ndefault target = null\n","entrypoint":"router/target"}'
```
- check:
  - valida rego sin aplicar

### 4.6 WF rules read / status

[x] `wf_rules_get_workflow`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/wf-rules?workflow_name=$TEST_WF_NAME"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/wf-rules?workflow_name=test-wf-rules-archi
```
- check:
  - devuelve la definición actual y metadata

[x] `wf_rules_get_status`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/wf-rules/status?workflow_name=$TEST_WF_NAME"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/wf-rules/status?workflow_name=test-wf-rules-archi
```
- check:
  - incluye `current_version`, `published_version`
  - incluye `wf_node.running`, `wf_node.status_reachable`, `wf_node.health_state`

[x] `wf_rules_list_workflows`

- curl:
```bash
curl -sS "$BASE/hives/$LOCAL_HIVE/wf-rules"
```
- SCMD:
```text
SCMD: curl -X GET /hives/motherbee/wf-rules
```
- check:
  - lista workflows manejados por `SY.wf-rules`
  - incluye `wf_node_running`, `wf_node_reachable`, `wf_node_health_state`

## 5. Mutations

Nota:

- En `curl` se ejecutan directamente.
- En `SCMD` dentro de archi, para prueba interactiva conviene:
  - mandar el `SCMD`
  - esperar mensaje `prepared`
  - responder `CONFIRM`

### 5.1 Hive lifecycle

[ ] `add_hive`

- curl:
```bash
curl -sS -X POST "$BASE/hives" \
  -H 'Content-Type: application/json' \
  -d "{\"hive_id\":\"$TEST_HIVE\",\"address\":\"$TEST_ADDR\"}"
```
- SCMD:
```text
SCMD: curl -X POST /hives -d '{"hive_id":"worker-220-other","address":"192.168.8.220"}'
```
- check:
  - luego `GET /hives/worker-220-other`
  - luego `GET /hives`

[ ] `remove_hive`

- curl:
```bash
curl -sS -X DELETE "$BASE/hives/$TEST_HIVE"
```
- SCMD:
```text
SCMD: curl -X DELETE /hives/worker-220-other
```
- check:
  - luego `GET /hives/worker-220-other` debe fallar/no existir

### 5.2 Runtime / rollout

[ ] `update`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/update" \
  -H 'Content-Type: application/json' \
  -d '{"manifest_hash":"sha256:deadbeef","category":"runtime","manifest_version":42}'
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/update -d '{"manifest_hash":"sha256:deadbeef","category":"runtime","manifest_version":42}'
```
- check:
  - respuesta `ok` o `sync_pending` según readiness

[ ] `sync_hint`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/sync-hint" \
  -H 'Content-Type: application/json' \
  -d '{"channel":"blob","wait_for_idle":true,"timeout_ms":30000}'
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/sync-hint -d '{"channel":"blob","wait_for_idle":true,"timeout_ms":30000}'
```
- check:
  - trigger/espera de convergencia

[ ] `remove_runtime_version`

- curl:
```bash
curl -sS -X DELETE "$BASE/hives/$LOCAL_HIVE/runtimes/$TEST_RUNTIME/versions/$TEST_RUNTIME_VERSION"
```
- SCMD:
```text
SCMD: curl -X DELETE /hives/motherbee/runtimes/AI.chat/versions/1.2.3
```
- check:
  - luego `GET /hives/motherbee/runtimes/AI.chat`

### 5.3 Routes / VPNs

[ ] `add_route`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/routes" \
  -H 'Content-Type: application/json' \
  -d "{\"prefix\":\"$TEST_ROUTE_PREFIX\",\"action\":\"next_hop_hive\",\"next_hop_hive\":\"$TEST_HIVE\"}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/routes -d '{"prefix":"AI.chat.","action":"next_hop_hive","next_hop_hive":"worker-220-other"}'
```
- check:
  - luego `GET /hives/motherbee/routes`

[ ] `delete_route`

- curl:
```bash
curl -sS -X DELETE "$BASE/hives/$LOCAL_HIVE/routes/$TEST_ROUTE_PREFIX"
```
- SCMD:
```text
SCMD: curl -X DELETE /hives/motherbee/routes/AI.chat.
```
- check:
  - luego `GET /hives/motherbee/routes`

[ ] `add_vpn`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/vpns" \
  -H 'Content-Type: application/json' \
  -d "{\"pattern\":\"$TEST_VPN_PATTERN\",\"vpn_id\":220}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/vpns -d '{"pattern":"worker-*","vpn_id":220}'
```
- check:
  - luego `GET /hives/motherbee/vpns`

[ ] `delete_vpn`

- curl:
```bash
curl -sS -X DELETE "$BASE/hives/$LOCAL_HIVE/vpns/$TEST_VPN_PATTERN"
```
- SCMD:
```text
SCMD: curl -X DELETE /hives/motherbee/vpns/worker-*
```
- check:
  - luego `GET /hives/motherbee/vpns`

### 5.4 Node lifecycle / debug

[ ] `run_node`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/nodes" \
  -H 'Content-Type: application/json' \
  -d "{\"node_name\":\"$TEST_RUNTIME@$LOCAL_HIVE\",\"runtime_version\":\"current\"}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/nodes -d '{"node_name":"AI.chat@motherbee","runtime_version":"current"}'
```
- check:
  - luego `GET /hives/motherbee/nodes`
  - luego `GET /hives/motherbee/nodes/AI.chat@motherbee/status`

[ ] `kill_node`

- curl:
```bash
curl -sS -X DELETE "$BASE/hives/$LOCAL_HIVE/nodes/$TEST_RUNTIME@$LOCAL_HIVE"
```
- SCMD:
```text
SCMD: curl -X DELETE /hives/motherbee/nodes/AI.chat@motherbee
```
- check:
  - luego `GET /hives/motherbee/nodes/AI.chat@motherbee/status`

[ ] `remove_node_instance`

- curl:
```bash
curl -sS -X DELETE "$BASE/hives/$LOCAL_HIVE/nodes/$TEST_RUNTIME@$LOCAL_HIVE/instance"
```
- SCMD:
```text
SCMD: curl -X DELETE /hives/motherbee/nodes/AI.chat@motherbee/instance
```
- check:
  - la instancia deja de figurar en `GET /hives/motherbee/nodes`

[ ] `set_node_config`

- curl:
```bash
curl -sS -X PUT "$BASE/hives/$LOCAL_HIVE/nodes/$TEST_MANAGED_NODE/config" \
  -H 'Content-Type: application/json' \
  -d '{"openai":{"default_model":"gpt-4.1-mini"}}'
```
- SCMD:
```text
SCMD: curl -X PUT /hives/motherbee/nodes/SY.frontdesk.gov@motherbee/config -d '{"openai":{"default_model":"gpt-4.1-mini"}}'
```
- check:
  - luego `GET /hives/motherbee/nodes/SY.frontdesk.gov@motherbee/config`

[ ] `send_node_message`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/nodes/$TEST_NODE/messages" \
  -H 'Content-Type: application/json' \
  -d '{"msg_type":"PING","payload":{"ping":true}}'
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/nodes/SY.admin@motherbee/messages -d '{"msg_type":"PING","payload":{"ping":true}}'
```
- check:
  - validar respuesta o efecto en logs/estado del nodo

### 5.5 Storage / OPA

[ ] `set_storage`

- curl:
```bash
curl -sS -X PUT "$BASE/config/storage" \
  -H 'Content-Type: application/json' \
  -d '{"path":"/var/lib/fluxbee"}'
```
- SCMD:
```text
SCMD: curl -X PUT /config/storage -d '{"path":"/var/lib/fluxbee"}'
```
- check:
  - luego `GET /config/storage`

[ ] `opa_compile_apply`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/opa/policy" \
  -H 'Content-Type: application/json' \
  -d '{"rego":"package router\n\ndefault target = null\n","entrypoint":"router/target"}'
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/opa/policy -d '{"rego":"package router\n\ndefault target = null\n","entrypoint":"router/target"}'
```
- check:
  - luego `GET /hives/motherbee/opa/status`

[ ] `opa_compile`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/opa/policy/compile" \
  -H 'Content-Type: application/json' \
  -d '{"rego":"package router\n\ndefault target = null\n","entrypoint":"router/target"}'
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/opa/policy/compile -d '{"rego":"package router\n\ndefault target = null\n","entrypoint":"router/target"}'
```
- check:
  - compile ok sin aplicar

[ ] `opa_apply`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/opa/policy/apply" \
  -H 'Content-Type: application/json' \
  -d "{\"version\":$TEST_OPA_VERSION}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/opa/policy/apply -d '{"version":12}'
```
- check:
  - luego `GET /hives/motherbee/opa/status`

[ ] `opa_rollback`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/opa/policy/rollback" \
  -H 'Content-Type: application/json' \
  -d "{\"version\":$((TEST_OPA_VERSION - 1))}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/opa/policy/rollback -d '{"version":11}'
```
- check:
  - luego `GET /hives/motherbee/opa/status`

[ ] `wf_rules_compile`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/wf-rules/compile" \
  -H 'Content-Type: application/json' \
  -d "{\"workflow_name\":\"$TEST_WF_NAME\",\"definition\":{\"wf_schema_version\":\"1\",\"workflow_type\":\"$TEST_WF_NAME\",\"description\":\"archi wf compile test\",\"input_schema\":{\"type\":\"object\",\"properties\":{\"request_id\":{\"type\":\"string\"}}},\"initial_state\":\"pending\",\"terminal_states\":[\"done\"],\"states\":[{\"name\":\"pending\",\"description\":\"pending\",\"entry_actions\":[],\"exit_actions\":[],\"transitions\":[{\"event_match\":{\"msg\":\"OK\"},\"guard\":\"true\",\"target_state\":\"done\",\"actions\":[]}]},{\"name\":\"done\",\"description\":\"done\",\"entry_actions\":[],\"exit_actions\":[],\"transitions\":[]}]}}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/wf-rules/compile -d '{"workflow_name":"test-wf-rules-archi","definition":{"wf_schema_version":"1","workflow_type":"test-wf-rules-archi","description":"archi wf compile test","input_schema":{"type":"object","properties":{"request_id":{"type":"string"}}},"initial_state":"pending","terminal_states":["done"],"states":[{"name":"pending","description":"pending","entry_actions":[],"exit_actions":[],"transitions":[{"event_match":{"msg":"OK"},"guard":"true","target_state":"done","actions":[]}]},{"name":"done","description":"done","entry_actions":[],"exit_actions":[],"transitions":[]}]}}'
```
- check:
  - respuesta `ok`
  - luego `GET /hives/motherbee/wf-rules/status?workflow_name=test-wf-rules-archi`

[ ] `wf_rules_apply`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/wf-rules/apply" \
  -H 'Content-Type: application/json' \
  -d "{\"workflow_name\":\"$TEST_WF_NAME\",\"auto_spawn\":true,\"tenant_id\":\"$TEST_TENANT_ID\"}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/wf-rules/apply -d '{"workflow_name":"test-wf-rules-archi","auto_spawn":true,"tenant_id":"tnt:43d576a3-d712-4d91-9245-5d5463dd693e"}'
```
- check:
  - `wf_node.action = restarted`
  - luego `GET /hives/motherbee/wf-rules/status?workflow_name=test-wf-rules-archi`

[ ] `wf_rules_compile_apply`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/wf-rules" \
  -H 'Content-Type: application/json' \
  -d "{\"workflow_name\":\"$TEST_WF_NAME\",\"definition\":{\"wf_schema_version\":\"1\",\"workflow_type\":\"$TEST_WF_NAME\",\"description\":\"archi wf compile_apply test\",\"input_schema\":{\"type\":\"object\",\"properties\":{\"request_id\":{\"type\":\"string\"}}},\"initial_state\":\"pending\",\"terminal_states\":[\"done\"],\"states\":[{\"name\":\"pending\",\"description\":\"pending\",\"entry_actions\":[],\"exit_actions\":[],\"transitions\":[{\"event_match\":{\"msg\":\"OK\"},\"guard\":\"true\",\"target_state\":\"done\",\"actions\":[]}]},{\"name\":\"done\",\"description\":\"done\",\"entry_actions\":[],\"exit_actions\":[],\"transitions\":[]}]},\"auto_spawn\":true,\"tenant_id\":\"$TEST_TENANT_ID\"}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/wf-rules -d '{"workflow_name":"test-wf-rules-archi","definition":{"wf_schema_version":"1","workflow_type":"test-wf-rules-archi","description":"archi wf compile_apply test","input_schema":{"type":"object","properties":{"request_id":{"type":"string"}}},"initial_state":"pending","terminal_states":["done"],"states":[{"name":"pending","description":"pending","entry_actions":[],"exit_actions":[],"transitions":[{"event_match":{"msg":"OK"},"guard":"true","target_state":"done","actions":[]}]},{"name":"done","description":"done","entry_actions":[],"exit_actions":[],"transitions":[]}]},"auto_spawn":true,"tenant_id":"tnt:43d576a3-d712-4d91-9245-5d5463dd693e"}'
```
- check:
  - compile + apply + restart en una sola operación

[ ] `wf_rules_rollback`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/wf-rules/rollback" \
  -H 'Content-Type: application/json' \
  -d "{\"workflow_name\":\"$TEST_WF_NAME\",\"auto_spawn\":true,\"tenant_id\":\"$TEST_TENANT_ID\"}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/wf-rules/rollback -d '{"workflow_name":"test-wf-rules-archi","auto_spawn":true,"tenant_id":"tnt:43d576a3-d712-4d91-9245-5d5463dd693e"}'
```
- check:
  - vuelve a la definición backup
  - reinicia el `WF.*`

[ ] `wf_rules_delete`

- curl:
```bash
curl -sS -X POST "$BASE/hives/$LOCAL_HIVE/wf-rules/delete" \
  -H 'Content-Type: application/json' \
  -d "{\"workflow_name\":\"$TEST_WF_NAME\",\"force\":false}"
```
- SCMD:
```text
SCMD: curl -X POST /hives/motherbee/wf-rules/delete -d '{"workflow_name":"test-wf-rules-archi","force":false}'
```
- check:
  - elimina el workflow y el managed WF node asociado si corresponde

## 6. Coverage Summary

Acciones actualmente cubiertas por `SY.architect` vía SCMD/traducción:

- `hive_status`
- `list_hives`, `get_hive`, `add_hive`, `remove_hive`
- `list_admin_actions`, `get_admin_action_help`
- `inventory`
- `list_versions`, `get_versions`
- `list_runtimes`, `get_runtime`, `remove_runtime_version`
- `list_routes`, `add_route`, `delete_route`
- `list_vpns`, `add_vpn`, `delete_vpn`
- `list_nodes`, `run_node`, `kill_node`, `remove_node_instance`
- `get_node_config`, `set_node_config`, `get_node_state`, `get_node_status`
- `send_node_message`
- `list_ilks`, `get_ilk`
- `list_deployments`, `get_deployments`
- `list_drift_alerts`, `get_drift_alerts`
- `get_storage`, `set_storage`
- `update`, `sync_hint`
- `opa_compile_apply`, `opa_compile`, `opa_apply`, `opa_rollback`, `opa_check`, `opa_get_policy`, `opa_get_status`
- `wf_rules_compile_apply`, `wf_rules_compile`, `wf_rules_apply`, `wf_rules_rollback`, `wf_rules_delete`
- `wf_rules_get_workflow`, `wf_rules_get_status`, `wf_rules_list_workflows`

## 7. What Is Still Fragile

Esto no es falta de cobertura de comandos, sino fragilidad del flujo AI:

- el modelo a veces intenta un path incorrecto antes de consultar bien capacidades
- a veces descubre solo `/admin/actions` y no va directo a `/admin/actions/{action}`
- el contrato final suele terminar bien, pero con intentos intermedios ruidosos

Entonces, para validar cobertura:

- usar `SCMD` como camino canónico
- usar lenguaje natural como prueba del comportamiento del modelo

## 8. Nota proxy externo

Para probar desde afuera con:

```text
https://fluxbee.ai/control/<command>
```

la configuración de reverse proxy debe remover el prefijo `/control`, por ejemplo:

```nginx
location /control/ {
  proxy_pass http://127.0.0.1:8080/;
}
```

Si `curl "$BASE/..."` devuelve `not_found` solo cuando `BASE` apunta a `https://fluxbee.ai/control`, revisar primero ese strip de prefijo.
