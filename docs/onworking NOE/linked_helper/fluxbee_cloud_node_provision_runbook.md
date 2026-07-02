# Runbook — Fluxbee Cloud para provisionar y levantar `IO.linkedhelper`

## 1. Objetivo

Este documento resume lo que necesita saber el equipo de Fluxbee Cloud para:

- verificar que Fluxbee runtime ya tiene disponible el runtime `io.linkedhelper`;
- publicar el secreto del adapter en Vault;
- spawnear un nodo `IO.linkedhelper` por `managed_instance_id`;
- consultar el estado del nodo después del spawn;
- verificar que el nodo quedó arriba y configurado.

Este documento **no** cubre todavía:

- el envío de tráfico desde el adapter hacia el nodo;
- el contrato E2E completo de `desiredState/report_to`;
- ni el camino final con Edge.

El foco es solo:

```text
Cloud -> Vault -> run_node -> node_status -> nodo arriba
```

### 1.1. Estado del documento

Este archivo debe leerse como una **especificación operativa vigente para el estado actual del repo**.

No es una spec externa separada del código. La fuente de verdad para lo que sigue es el contrato hoy implementado en:

- `src/bin/sy_admin.rs`
- `src/bin/sy_orchestrator.rs`
- `docs/07-operaciones.md`
- `docs/onworking COA/node-status-contract.md`

Por lo tanto, en este documento:

- `POST /hives/{hive}/nodes` se trata como contrato formal vigente de spawn;
- `GET /hives/{hive}/nodes/{node_name}/status` se trata como contrato formal vigente de consulta de estado.

---

## 2. Modelo vigente

### 2.1. Relación lógica

El modelo intermedio vigente es:

```text
1 nodo IO.linkedhelper = 1 managed_instance_id = 1 cuenta Linked Helper aprobada
```

### 2.2. Naming canónico

Para una instancia:

```text
managed_instance_id = lhmi_001
```

el nombre canónico del nodo es:

```text
IO.linkedhelper.lhmi_001@motherbee
```

### 2.3. Runtime canónico

Cloud debe usar:

```text
runtime = io.linkedhelper
runtime_version = current
```

Siempre que Fluxbee ya tenga publicado el runtime en:

```text
/var/lib/fluxbee/dist/runtimes/io.linkedhelper
```

---

## 3. Precondiciones que Cloud debe asumir

Antes de hacer `run_node`, Cloud debe asumir o verificar:

1. El hive destino existe.
2. El runtime `io.linkedhelper` está publicado en Fluxbee.
3. El `managed_instance_id` ya fue creado en Cloud.
4. Existe binding con el adapter:
   - `adapter_id`
   - `local_instance_id` cuando aplique
5. Cloud conoce el `adapter_secret`.
6. Cloud puede escribir en Vault del hive destino.
7. Cloud puede invocar el Admin API de Fluxbee.

Si el runtime no está publicado, `run_node` no es suficiente.

---

## 4. Secreto que Cloud debe publicar en Vault

### 4.1. Key estable

Cloud debe publicar el secreto del adapter en:

```text
linkedhelper:adapters:<adapter_id>
```

Ejemplo:

```text
linkedhelper:adapters:adp_123
```

> ⚠️ **Formato de la key de Vault:** SY.vault valida `^[a-z0-9][a-z0-9:_-]*$`
> (máx 256): solo minúsculas, dígitos, `:`, `_` y `-`; sin mayúsculas ni `/`
> (rechaza con `invalid request: invalid key format`). Fluxbee Cloud usa la
> convención `linkedhelper:adapters:<adapter_id>`. El `adapter.auth.key` del nodo
> debe coincidir exactamente con esta key.

### 4.2. `resource_type`

Debe usarse:

```text
resource_type = linkedhelper_adapter
```

### 4.3. Metadata recomendada

```json
{
  "resource_type": "linkedhelper_adapter",
  "tenant_id": "tnt:00000000-0000-0000-0000-000000000001",
  "adapter_id": "adp_123",
  "adapter_type": "linkedhelper",
  "created_by": "fluxbee_cloud",
  "status": "active"
}
```

### 4.4. Shape del valor

El nodo actual acepta que el valor en Vault sea:

- string plano;
- objeto con `adapter_secret`;
- objeto con `secret`;
- objeto con `token`;
- objeto con `bearer_token`.

Recomendación simple:

```json
{
  "adapter_secret": "secret-del-adapter"
}
```

### 4.5. Ejemplo `vault_put`

```bash
curl -sS -X POST http://127.0.0.1:8080/hives/motherbee/vault/secrets \
  -H 'Content-Type: application/json' \
  -d '{
    "key": "linkedhelper:adapters:adp_123",
    "value": {
      "adapter_secret": "secret-del-adapter"
    },
    "metadata": {
      "resource_type": "linkedhelper_adapter",
      "tenant_id": "tnt:00000000-0000-0000-0000-000000000001",
      "adapter_id": "adp_123",
      "adapter_type": "linkedhelper",
      "created_by": "fluxbee_cloud",
      "status": "active"
    }
  }'
```

---

## 5. Especificación formal de `run_node`

Cloud debe invocar:

```text
POST /hives/{hive}/nodes
```

Payload formal vigente:

```json
{
  "node_name": "IO.linkedhelper.lhmi_001@motherbee",
  "runtime": "io.linkedhelper",
  "runtime_version": "current",
  "tenant_id": "tnt:00000000-0000-0000-0000-000000000001",
  "config": {
    "managed_instance_id": "lhmi_001",
    "tenant_id": "tnt:00000000-0000-0000-0000-000000000001",
    "listen": {
      "address": "0.0.0.0",
      "port": 19091
    },
    "adapter": {
      "adapter_id": "adp_123",
      "local_instance_id": "123456",
      "auth": {
        "type": "vault_ref",
        "resource_type": "linkedhelper_adapter",
        "key": "linkedhelper:adapters:adp_123"
      }
    },
    "mode": "direct_http_intermediate"
  }
}
```

### 5.1. Campos obligatorios

- `node_name`
- `runtime`
- `runtime_version`
- `tenant_id`
- `config.managed_instance_id`
- `config.tenant_id`
- `config.listen.address`
- `config.listen.port`
- `config.adapter.adapter_id`
- `config.adapter.auth.type`
- `config.adapter.auth.resource_type`
- `config.adapter.auth.key`

### 5.2. Campos opcionales

- `config.adapter.local_instance_id`
- `config.adapter.label`
- `config.adapter.dst_node`
- `config.http.max_request_bytes`
- `config.identity.target`
- `config.identity.timeout_ms`
- `config.mode`

### 5.3. Reglas importantes

- `node_name` debe matchear el `managed_instance_id`.
- `runtime` debe ser `io.linkedhelper`.
- `runtime_version` debe ser `current`, salvo rollout explícito por versión fija.
- `tenant_id` top-level debe venir en primer spawn de nodos `IO.*` identity-aware.
- `config.adapter.auth.type` debe ser `vault_ref`.
- `config.adapter.auth.resource_type` debe ser `linkedhelper_adapter`.
- el nodo no acepta secreto inline en config.

### 5.4. Semántica del contrato

- `POST /hives/{hive}/nodes` crea la instancia administrada y dispara el `SPAWN_NODE`.
- si `config.json` del nodo ya existe, el spawn falla con `NODE_ALREADY_EXISTS`.
- el `config` enviado se persiste como config efectiva administrada del nodo.
- el runtime resuelve el secreto de Vault durante boot o cuando reciba `config.set`.

---

## 6. Ejemplo de llamada completa

```bash
curl -sS -X POST http://127.0.0.1:8080/hives/motherbee/nodes \
  -H 'Content-Type: application/json' \
  -d '{
    "node_name": "IO.linkedhelper.lhmi_001@motherbee",
    "runtime": "io.linkedhelper",
    "runtime_version": "current",
    "tenant_id": "tnt:00000000-0000-0000-0000-000000000001",
    "config": {
      "managed_instance_id": "lhmi_001",
      "tenant_id": "tnt:00000000-0000-0000-0000-000000000001",
      "listen": {
        "address": "0.0.0.0",
        "port": 19091
      },
      "adapter": {
        "adapter_id": "adp_123",
        "local_instance_id": "123456",
        "auth": {
          "type": "vault_ref",
          "resource_type": "linkedhelper_adapter",
          "key": "linkedhelper:adapters:adp_123"
        }
      },
      "mode": "direct_http_intermediate"
    }
  }'
```

---

## 7. Response HTTP formal de `POST /hives/{hive}/nodes`

La respuesta HTTP externa de `SY.admin` usa este envelope:

### 7.1. Success envelope

```json
{
  "status": "ok",
  "action": "run_node",
  "payload": {
    "status": "ok",
    "node_name": "IO.linkedhelper.lhmi_001@motherbee",
    "runtime": "io.linkedhelper",
    "version": "current",
    "requested_version": "current",
    "hive": "motherbee",
    "target": "motherbee",
    "unit": "fluxbee-IO.linkedhelper.lhmi_001@motherbee",
    "identity": {
      "requested_hive": "motherbee",
      "identity_primary_hive_id": "motherbee",
      "identity_target": "SY.identity@motherbee",
      "register": {},
      "update": {}
    },
    "config": {
      "...": "config efectiva persistida por orchestrator"
    }
  },
  "error_code": null,
  "error_detail": null
}
```

### 7.2. Success con `already_running`

Algunas respuestas exitosas pueden incluir además:

```json
{
  "status": "ok",
  "action": "run_node",
  "payload": {
    "status": "ok",
    "state": "already_running",
    "node_name": "IO.linkedhelper.lhmi_001@motherbee",
    "runtime": "io.linkedhelper",
    "version": "current",
    "requested_version": "current",
    "hive": "motherbee",
    "target": "motherbee",
    "unit": "fluxbee-IO.linkedhelper.lhmi_001@motherbee",
    "identity": {},
    "config": {}
  },
  "error_code": null,
  "error_detail": null
}
```

### 7.3. Error envelope

```json
{
  "status": "error",
  "action": "run_node",
  "payload": {
    "status": "error",
    "error_code": "NODE_ALREADY_EXISTS",
    "message": "node config already exists: ...",
    "target": "motherbee",
    "node_name": "IO.linkedhelper.lhmi_001@motherbee",
    "unit": "fluxbee-IO.linkedhelper.lhmi_001@motherbee",
    "identity": {},
    "config": {
      "path": "/var/lib/fluxbee/nodes/IO/IO.linkedhelper.lhmi_001@motherbee/config.json"
    }
  },
  "error_code": "NODE_ALREADY_EXISTS",
  "error_detail": "node config already exists: ..."
}
```

### 7.4. Error codes relevantes para Cloud

- `INVALID_REQUEST`
- `NODE_ALREADY_EXISTS`
- `CONFIG_WRITE_FAILED`
- `SPAWN_FAILED`
- `SERVICE_FAILED`
- `IDENTITY_REGISTER_FAILED`
- `IDENTITY_UPDATE_FAILED`

Cloud debería persistir como mínimo:

- HTTP status code;
- `status`;
- `action`;
- `error_code`;
- `error_detail`;
- `payload.node_name`;
- `payload.runtime`;
- `payload.version`;
- `payload.unit`.

---

## 8. Secuencia correcta de aprovisionamiento

Cloud debe respetar este orden:

1. Validar que el runtime `io.linkedhelper` ya existe en el hive.
2. Publicar el secreto del adapter en Vault.
3. Ejecutar `run_node` con `auth.type = vault_ref`.
4. Consultar el estado del nodo.
5. Verificar el listener HTTP del runtime.

El paso de Vault debe ocurrir antes del `run_node`, porque el runtime resuelve el secreto durante el boot o cuando recibe `config.set`.

---

## 9. Endpoint exacto para consultar estado del nodo

Después del spawn, Cloud debe consultar:

```text
GET /hives/{hive}/nodes/{node_name}/status
```

Para el caso de ejemplo:

```text
GET /hives/motherbee/nodes/IO.linkedhelper.lhmi_001@motherbee/status
```

Ejemplo:

```bash
curl -sS \
  http://127.0.0.1:8080/hives/motherbee/nodes/IO.linkedhelper.lhmi_001@motherbee/status
```

Este es el endpoint canónico que Cloud debe usar para verificar el estado post-spawn.

---

## 10. Response HTTP formal de `GET /hives/{hive}/nodes/{node_name}/status`

### 10.1. Success envelope

```json
{
  "status": "ok",
  "action": "get_node_status",
  "payload": {
    "action": "node_status",
    "status": "ok",
    "payload": {
      "node_status": {
        "schema_version": "1",
        "node_name": "IO.linkedhelper.lhmi_001@motherbee",
        "hive_id": "motherbee",
        "observed_at": "2026-03-13T18:00:00Z",
        "lifecycle_state": "RUNNING",
        "health_state": "HEALTHY",
        "health_source": "NODE_REPORTED",
        "status_version": 7,
        "runtime": {},
        "process": {},
        "config": {},
        "identity": {},
        "extensions": {}
      }
    }
  },
  "error_code": null,
  "error_detail": null
}
```

### 10.2. Dónde leer el estado efectivo

Cloud debe leer el estado efectivo en:

```text
payload.payload.node_status
```

### 10.3. Campos canónicos a leer

- `schema_version`
- `node_name`
- `hive_id`
- `observed_at`
- `lifecycle_state`
- `health_state`
- `health_source`
- `status_version`
- `runtime`
- `process`
- `config`
- `identity`
- `extensions`

### 10.4. Criterio operativo mínimo para este caso

Para esta etapa, Cloud puede considerar que el nodo quedó correctamente levantado si ve:

- `payload.payload.node_status.node_name` igual al nodo spawneado;
- `payload.payload.node_status.lifecycle_state = RUNNING`;
- `payload.payload.node_status.health_state` distinto de `ERROR`;
- y luego `GET /schema` responde sobre el listener HTTP del nodo.

### 10.5. Error envelope

```json
{
  "status": "error",
  "action": "get_node_status",
  "payload": {
    "action": "node_status",
    "status": "error",
    "error_code": "NODE_NOT_FOUND",
    "error_detail": "node 'IO.linkedhelper.lhmi_001@motherbee' not found in inventory"
  },
  "error_code": "NODE_NOT_FOUND",
  "error_detail": "node 'IO.linkedhelper.lhmi_001@motherbee' not found in inventory"
}
```

### 10.6. Error codes relevantes

- `NODE_NOT_FOUND`
- `HIVE_NOT_FOUND`
- `STATUS_UNAVAILABLE`
- `TIMEOUT`

---

## 11. Qué verifica Fluxbee al arrancar el nodo

Al aplicar el spawn/config, el runtime actual de `IO.linkedhelper`:

1. valida la estructura de config;
2. deriva el binding HTTP desde `config.listen`;
3. resuelve el secret del adapter desde Vault;
4. cachea el secret en memoria;
5. marca el nodo `FAILED_CONFIG` si:
   - falta config obligatoria;
   - Vault no está disponible;
   - la key no existe;
   - el secret está vacío o inválido.

Consecuencia práctica:

- `run_node` puede crear la instancia;
- pero si el secret no está bien publicado en Vault, el nodo no queda operativo.

Por eso la secuencia correcta es:

```text
vault_put primero
run_node después
```

---

## 12. Verificación post-spawn

### 12.1. Verificar que la instancia exista

Cloud debería confirmar que el nodo fue creado en Fluxbee.

### 12.2. Verificar estado del nodo

Cloud debería consultar el estado del nodo y esperar:

```text
RUNNING
```

Si el runtime del nodo no pudo completar configuración, el status operativo puede reflejar degradación y luego el runtime puede publicar `FAILED_CONFIG` como estado interno de control-plane.

Las causas más probables de fallo son:

- `adapter.auth.key` incorrecta;
- secret inexistente en Vault;
- valor de secret vacío;
- error de `listen.address` / `listen.port`;
- falta de `managed_instance_id`.

### 12.3. Verificar schema HTTP del nodo

El nodo expone:

```text
GET /
GET /schema
POST /v1/poll
```

Verificación útil:

```bash
curl -sS http://127.0.0.1:19091/schema
```

En el camino intermedio, Cloud puede considerar exitoso el provisioning cuando:

- el nodo existe;
- el status API refleja `RUNNING`;
- el listener HTTP quedó arriba;
- y el schema responde.

---

## 13. Qué queda listo cuando este runbook termina bien

Si todo sale bien, del lado Fluxbee queda listo:

- el runtime `io.linkedhelper` ya está publicado;
- el secret del adapter está en Vault;
- existe un nodo `IO.linkedhelper.<managed_instance_id>@motherbee`;
- el nodo quedó spawneado;
- el status API lo puede observar;
- el listener HTTP directo está arriba;
- y Cloud ya puede persistir `node_name`, `runtime`, `hive`, `status` y `report_to`.

Lo que todavía no valida este documento:

- que el adapter efectivamente pueda enviar tráfico;
- que el binding `report_to` ya esté consumido por el adapter;
- ni el flujo final con Edge.

---

## 14. Checklist breve para Cloud

- [ ] Confirmar que `io.linkedhelper` existe en `dist/runtimes`.
- [ ] Tener `managed_instance_id`.
- [ ] Tener `adapter_id`.
- [ ] Tener `local_instance_id` si aplica.
- [ ] Hacer `vault_put` en `linkedhelper:adapters:<adapter_id>`.
- [ ] Ejecutar `run_node` con `runtime=io.linkedhelper`.
- [ ] Leer y persistir la response HTTP de `POST /hives/{hive}/nodes`.
- [ ] Consultar `GET /hives/{hive}/nodes/{node_name}/status`.
- [ ] Verificar que el status refleje `RUNNING`.
- [ ] Verificar que `GET /schema` responde.
- [ ] Persistir `node_name`, `runtime`, `hive`, `report_to`, estado y error si corresponde.
