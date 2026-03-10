# SY.orchestrator — Spec de Cambios v2

**Fecha:** 2026-03-02  
**Estado:** Propuesta para implementación  
**Audiencia:** Desarrolladores de sy_orchestrator, sy_admin  
**Dependencias:** 01-arquitectura.md, 02-protocolo.md, 05-conectividad.md, 07-operaciones.md, software-distribution-spec.md

---

## 0. RESUMEN:
Let me compile all the decisions and changes we've discussed:

Orchestrator is LOCAL - always runs on its own machine
add_hive/remove_hive only on motherbee (`add_hive` usa SSH bootstrap; `remove_hive` es socket-first/local-only)
All commands via socket unicast L2 (not NATS, not SSH)
Software sync via Syncthing to /var/lib/fluxbee/dist/
RUNTIME_UPDATE renamed to SYSTEM_UPDATE for broader applicability
Admin needs a new endpoint to handle updates
run_node and kill_node commands now work across any node type
Hash verification required before any installation
SSH access restricted to bootstrap only, then disabled

---

## 1. Principio rector

**El orchestrator es LOCAL. Siempre.**

Cada `SY.orchestrator` opera exclusivamente sobre su propia máquina. No ejecuta comandos remotos. No usa SSH para operaciones post-bootstrap. Toda comunicación entre orchestrators pasa por el canal de mensajería existente (socket → router → WAN).

La única excepción con SSH es `add_hive` cuando el worker todavía no tiene orchestrator operativo.  
`remove_hive` opera socket-first y, si el worker no está alcanzable por socket, hace cleanup local-only en motherbee.

---

## 2. Qué cambia (resumen ejecutivo)

| Aspecto | Antes | Ahora |
|---------|-------|-------|
| Ejecución de comandos en workers | SSH remoto desde motherbee | Cada orchestrator ejecuta local |
| Comunicación admin → orchestrator | Unicast local + SSH para workers | Unicast L2 inter-isla por socket/WAN |
| SSH post-bootstrap | Permanente, keys en disco | Se cierra o restringe tras add_hive |
| Mensajes de update | `RUNTIME_UPDATE` (solo runtimes) | `SYSTEM_UPDATE` (genérico: runtime/core/vendor) |
| Propagación de binarios | rsync/scp por SSH | Syncthing (repo dist) + comando SYSTEM_UPDATE |
| run_node / kill_node | Solo ciertos nodos SY y routers | Cualquier nodo (AI, IO, WF, SY, RT) |
| Orchestrator en worker | No existe | Sí, `role: worker` |

---

## 3. Roles del orchestrator

El binario `sy-orchestrator` es el mismo en motherbee y workers. El comportamiento cambia según `role` en `hive.yaml`.

### 3.1 role: motherbee

```yaml
# hive.yaml
role: motherbee
```

**Funciones activas:**

- `add_hive` — provisioning remoto por SSH (única función remota)
- `remove_hive` — cleanup remoto por socket (`REMOVE_HIVE_CLEANUP`) o cleanup local-only si el worker no está alcanzable
- Gestión del repo maestro de distribución (`/var/lib/fluxbee/dist/`)
- Generación de manifests (core, vendor, runtimes)
- Monitoreo de respuestas de orchestrators remotos
- Toda la operación LOCAL (igual que role worker)
- Persistencia de historial de deployments y drift alerts

**Funciones exclusivas (no existen en worker):**

- `add_hive`
- `remove_hive`
- Generación de manifests
- Historial centralizado de deployments

### 3.2 role: worker

```yaml
# hive.yaml
role: worker
```

**Funciones activas:**

- Recibir y ejecutar comandos por socket (SYSTEM_UPDATE, SPAWN_NODE, KILL_NODE, etc.)
- Levantar servicios locales al arrancar (rt-gateway, sy-config-routes, sy-opa-rules, etc.)
- Instalar binarios desde repo dist local (`/var/lib/fluxbee/dist/`)
- Verificación de integridad (hashes vs manifest)
- Health check de servicios locales
- Reportar estado a motherbee por socket (respuesta a comandos)

**Funciones que NO tiene:**

- `add_hive` / `remove_hive`
- Acceso SSH a ninguna máquina
- Generación de manifests
- Acceso a PostgreSQL directo (eso es SY.storage en motherbee)

### 3.3 Arranque del orchestrator según rol

```
sy-orchestrator arranca:
  1. Lee hive.yaml → determina role
  2. Conecta a router local por socket
  3. Registra nombre: SY.orchestrator@{hive_id}

  Si role == motherbee:
    4. Inicia servicios locales de motherbee (SY.storage, SY.admin, etc.)
    5. Arranca watchdog de servicios
    6. Arranca Syncthing (si blob.sync.enabled)
    7. Queda escuchando comandos admin + system

  Si role == worker:
    4. Inicia servicios locales de worker (sy-config-routes, sy-opa-rules, etc.)
    5. rt-gateway ya está corriendo (lo arrancó systemd directo, ver §6)
    6. Arranca Syncthing (si blob.sync.enabled)
    7. Queda escuchando comandos admin + system
```

---

## 4. Comunicación

### 4.1 Canal: socket L2 (no NATS)

Toda comunicación de control usa el canal de socket existente (nodo ↔ router). El router resuelve nombres L2 y reenvía inter-isla por WAN cuando corresponde.

**No se usa NATS/JetStream para comandos de control.** NATS queda reservado para datos que requieren persistencia (turns de conversación, eventos para SY.storage).

Razones:

- Socket es rápido y directo
- Socket tiene broadcast (necesario para CONFIG_CHANGED, OPA_RELOAD)
- Los comandos de control no necesitan garantía at-least-once
- NATS agregaría complejidad innecesaria a una operación que es básicamente "ejecutá esto y decime cómo salió"

### 4.2 Flujo de comandos (post-bootstrap)

```
Operador → HTTP → SY.admin@motherbee
                      │
                      │ unicast socket L2
                      ▼
              SY.orchestrator@{isla_destino}
                      │
                      │ ejecución LOCAL
                      ▼
              systemctl / cp / hash / etc.
                      │
                      │ unicast socket L2 (respuesta)
                      ▼
              SY.admin@motherbee
                      │
                      ▼
              HTTP response al operador
```

El router de motherbee resuelve `SY.orchestrator@worker-1` por nombre L2, lo envía al gateway, cruza WAN, llega al router del worker, y se entrega al orchestrator local por socket. Toda esta infraestructura ya existe y funciona.

### 4.3 Validación de origen

Sin cambios. El orchestrator mantiene el allowlist existente para mensajes system:

```rust
// Origen permitido para mensajes de control
const ALLOWED_ORIGINS: &[&str] = &["SY.admin", "WF.orch.diag"];
```

Si el mensaje no viene de un origen permitido → `FORBIDDEN`.

---

## 5. Mensajes: cambios al protocolo

### 5.1 SYSTEM_UPDATE (reemplaza RUNTIME_UPDATE)

`RUNTIME_UPDATE` se renombra a `SYSTEM_UPDATE` y se extiende para cubrir todas las categorías de software.

**Request (SY.admin → SY.orchestrator@{isla}):**

```json
{
  "routing": {
    "src": "",
    "dst": "SY.orchestrator@worker-1",
    "ttl": 16,
    "trace_id": ""
  },
  "meta": {
    "type": "system",
    "msg": "SYSTEM_UPDATE"
  },
  "payload": {
    "category": "runtime",
    "manifest_version": 14,
    "manifest_hash": "sha256:a1b2c3..."
  }
}
```

**Campos de payload:**

| Campo | Tipo | Valores | Descripción |
|-------|------|---------|-------------|
| `category` | string | `runtime`, `core`, `vendor` | Qué grupo de software actualizar |
| `manifest_version` | u64 | monotónico | Versión esperada del manifest |
| `manifest_hash` | string | sha256:... | Hash del manifest esperado |

**Response (SY.orchestrator → SY.admin):**

```json
{
  "routing": {
    "src": "",
    "dst": "",
    "ttl": 16,
    "trace_id": ""
  },
  "meta": {
    "type": "system",
    "msg": "SYSTEM_UPDATE_RESPONSE"
  },
  "payload": {
    "status": "ok",
    "category": "runtime",
    "manifest_version": 14,
    "hive": "worker-1",
    "updated": ["AI.soporte/1.3.0", "IO.whatsapp/2.1.0"],
    "unchanged": ["WF.echo/1.0.0"],
    "restarted": ["AI.soporte.l1", "IO.whatsapp.main"]
  }
}
```

**Status posibles:**

| Status | Significado |
|--------|-------------|
| `ok` | Update aplicado, todos los servicios healthy |
| `sync_pending` | Manifest local no coincide con el esperado, Syncthing aún no propagó |
| `partial` | Algunos servicios actualizados, otros fallaron (detalle en `errors`) |
| `error` | Fallo general (detalle en `error_message`) |
| `rollback` | Update falló, se restauró versión anterior |

**Ejemplo de sync_pending:**

```json
{
  "payload": {
    "status": "sync_pending",
    "category": "core",
    "manifest_version": 14,
    "hive": "worker-1",
    "local_manifest_version": 13,
    "local_manifest_hash": "sha256:old123...",
    "message": "Local manifest does not match expected. Syncthing may still be propagating."
  }
}
```

El operador reintenta. En futuras versiones se puede agregar retry automático con backoff.

### 5.2 SPAWN_NODE y KILL_NODE (extendidos)

Hoy estos mensajes soportan routers y ciertos nodos SY. Se extienden para soportar cualquier tipo de nodo.

**SPAWN_NODE request:**

```json
{
  "routing": {
    "src": "",
    "dst": "SY.orchestrator@worker-1",
    "ttl": 16,
    "trace_id": ""
  },
  "meta": {
    "type": "system",
    "msg": "SPAWN_NODE"
  },
  "payload": {
    "node_name": "IO.slack.jdoe@sandbox",
    "runtime": "IO.slack",
    "runtime_version": "2.1.0",
    "config": {
      "api_token_ref": "secret:slack:jdoe",
      "channel": "#soporte"
    }
  }
}
```

**Campos de payload:**

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `node_name` | string | sí | Nombre L2 completo incluyendo @isla |
| `runtime` | string | sí | Tipo de runtime (debe existir en dist/runtimes/) |
| `runtime_version` | string | no | Versión específica. Si omitido, usa latest del manifest |
| `config` | object | no | Config específica del nodo (se pasa como env/args) |

**SPAWN_NODE response:**

```json
{
  "meta": {
    "type": "system",
    "msg": "SPAWN_NODE_RESPONSE"
  },
  "payload": {
    "status": "ok",
    "node_name": "IO.slack.jdoe@sandbox",
    "pid": 48221,
    "hive": "sandbox"
  }
}
```

**Status posibles (SPAWN_NODE):**

| Status | Significado |
|--------|-------------|
| `ok` | Nodo arrancado y registrado |
| `already_running` | El nodo ya está corriendo |
| `runtime_not_found` | Runtime no existe en dist/ local |
| `error` | Fallo (detalle en `error_message`) |

**KILL_NODE request:**

```json
{
  "meta": {
    "type": "system",
    "msg": "KILL_NODE"
  },
  "payload": {
    "node_name": "IO.slack.jdoe@sandbox",
    "force": false
  }
}
```

| Campo | Tipo | Descripción |
|-------|------|-------------|
| `node_name` | string | Nombre L2 del nodo a detener |
| `force` | bool | `false` = SIGTERM + esperar. `true` = SIGKILL inmediato |

**KILL_NODE response:**

```json
{
  "payload": {
    "status": "ok",
    "node_name": "IO.slack.jdoe@sandbox",
    "hive": "sandbox"
  }
}
```

| Status | Significado |
|--------|-------------|
| `ok` | Nodo detenido |
| `not_found` | El nodo no estaba corriendo |
| `error` | Fallo al detener |

### 5.3 EXTENSIÓN PROPUESTA: SYSTEM_SYNC_HINT (blob/dist)

Motivación:
- mantener Syncthing como único mecanismo de sync.
- reducir latencia de convergencia con un trigger por evento (además del watchdog periódico).
- evitar envío de mensajes con `blob_ref` antes de consolidar propagación.

Estado:
- **propuesto para v2.x** (no reemplaza `SYSTEM_UPDATE` ni agrega camino SSH).

Request:

```json
{
  "routing": {
    "src": "",
    "dst": "SY.orchestrator@worker-1",
    "ttl": 16,
    "trace_id": ""
  },
  "meta": {
    "type": "system",
    "msg": "SYSTEM_SYNC_HINT"
  },
  "payload": {
    "channel": "blob",
    "folder_id": "fluxbee-blob",
    "wait_for_idle": true,
    "timeout_ms": 30000
  }
}
```

Campos:

| Campo | Tipo | Valores | Descripción |
|-------|------|---------|-------------|
| `channel` | string | `blob`, `dist` | Canal lógico a acelerar |
| `folder_id` | string | `fluxbee-blob`, `fluxbee-dist` | Folder Syncthing objetivo |
| `wait_for_idle` | bool | `true/false` | Si espera convergencia observada |
| `timeout_ms` | u64 | >0 | Timeout de espera local |

Response:

```json
{
  "meta": {
    "type": "system",
    "msg": "SYSTEM_SYNC_HINT_RESPONSE"
  },
  "payload": {
    "status": "ok",
    "channel": "blob",
    "folder_id": "fluxbee-blob",
    "api_healthy": true,
    "folder_healthy": true,
    "state": "idle",
    "need_total_items": 0,
    "errors": []
  }
}
```

Estados:

| Status | Significado |
|--------|-------------|
| `ok` | hint aplicado y folder saludable |
| `sync_pending` | hint aplicado pero aún no convergió |
| `error` | health/config inválida del folder o timeout |

Notas de implementación:
- Sin cambios de transporte: socket L2 existente.
- Sin fallback legacy: no SSH, no camino paralelo.
- Reutiliza chequeos de salud por folder que ya usa watchdog (`fluxbee-blob`, `fluxbee-dist`).

---

### 5.4 Semántica de publicación recomendada

#### Blob (IO/AI/WF vía SDK)

Flujo recomendado:
1. `put/put_bytes`
2. `promote`
3. `SYSTEM_SYNC_HINT(channel=blob)`
4. emitir mensaje `text/v1` con `blob_ref`

Regla:
- no propagar mensaje con `blob_ref` hasta confirmar convergencia o timeout explícito.

#### Dist (rollout software)

Flujo recomendado:
1. publicar artefactos/manifest en `dist` (motherbee)
2. `SYSTEM_SYNC_HINT(channel=dist)` sobre hives destino
3. `SYSTEM_UPDATE(category=runtime|core|vendor)`

Regla:
- `SYSTEM_UPDATE` se ejecuta después de hint/confirmación de propagación, no antes.
- este flujo está pensado para el pipeline de plataforma/Admin (publish central en motherbee y ejecución coordinada por hive destino).

### 5.5 Mensajes sin cambios

Estos mensajes se mantienen exactamente como están:

- `CONFIG_CHANGED` / `CONFIG_RESPONSE` — broadcast, manejo de rutas/VPN
- `OPA_RELOAD` — broadcast, recarga de policies
- `HELLO` / `WAN_ACCEPT` / `WAN_REJECT` — handshake WAN
- `LSA` — topología inter-isla

---

## 6. Bootstrap: add_hive

### 6.1 Restricción

`add_hive` y `remove_hive` **solo existen en `SY.orchestrator@motherbee`**. Si `SY.admin` recibe un request de add/remove hive, lo manda a `SY.orchestrator@{motherbee_hive}` (su propio hive). Admin valida que el destino sea el hive local.

```rust
// En SY.admin, al recibir add_hive o remove_hive:
fn validate_provisioning_target(&self, action: &str) -> Result {
    let local_hive = self.hive_config.hive_id;
    if self.hive_config.role != Role::Motherbee {
        return Err("Provisioning actions only available on motherbee");
    }
    // Destino siempre es el orchestrator local de motherbee
    let dst = format!("SY.orchestrator@{}", local_hive);
    Ok(())
}
```

### 6.2 Flujo de add_hive

```
Operador: POST /hives  { hive_id: "worker-1", address: "192.168.8.50", ... }
    │
    ▼
SY.admin@motherbee
    │  valida: role == motherbee
    │  unicast a SY.orchestrator@motherbee (local)
    ▼
SY.orchestrator@motherbee
    │
    │  === FASE SSH (única vez) ===
    │
    │  1. Conecta SSH a 192.168.8.50 con password
    │  2. Copia paquete mínimo:
    │     - /var/lib/fluxbee/core/bin/sy-orchestrator
    │     - /var/lib/fluxbee/core/bin/rt-gateway
    │     - /var/lib/fluxbee/core/bin/sy-config-routes
    │     - /var/lib/fluxbee/core/bin/sy-opa-rules
    │     - /var/lib/fluxbee/core/bin/sy-identity (opcional)
    │  3. Genera y copia hive.yaml:
    │     role: worker
    │     hive_id: worker-1
    │     wan.uplinks: [{ address: "{motherbee_wan_ip}:{port}" }]
    │     blob.sync: (según config)
    │  4. Crea systemd units:
    │     - sy-orchestrator.service (arranque automático)
    │  5. systemctl enable + start sy-orchestrator
    │
    │  === FIN SSH ===
    │
    │  6. Espera conexión WAN de worker-1 (timeout 60s)
    │  7. Si conecta → provisioning OK
    │  8. Restringe/cierra SSH en worker (opcional)
    │
    ▼
SY.orchestrator@worker-1 (arranca por systemd):
    │  Lee hive.yaml → role: worker
    │  Crea systemd units para servicios de worker:
    │    - rt-gateway.service
    │    - sy-config-routes.service
    │    - sy-opa-rules.service
    │    - sy-identity.service (si aplica)
    │    - fluxbee-syncthing.service (si blob.sync.enabled)
    │  Arranca servicios en orden
    │  rt-gateway conecta WAN a motherbee
    │  Worker operativo
```

### 6.3 Paquete mínimo vs instalación completa

En bootstrap solo se copian los binarios esenciales para que el orchestrator arranque y la WAN conecte. El resto de los runtimes (AI.*, IO.*, WF.*) se propagarán por Syncthing después del bootstrap, y se instalarán con SYSTEM_UPDATE cuando el operador decida.

### 6.4 Nota: rt-gateway y systemd

`rt-gateway` debe arrancar antes que el orchestrator pueda comunicarse (necesita el socket del router). Opciones:

**A) systemd dependency:** `sy-orchestrator.service` depende de `rt-gateway.service`. Systemd arranca gateway primero. Orchestrator arranca después.

**B) orchestrator arranca gateway:** el orchestrator arranca primero y levanta gateway como primer acto.

Recomendación: **A**, usando systemd dependency. Es más robusto y no requiere lógica especial en el orchestrator.

```ini
# /etc/systemd/system/sy-orchestrator.service
[Unit]
After=rt-gateway.service
Requires=rt-gateway.service
```

---

## 7. Propagación de software (Syncthing)

### 7.1 Repo de distribución

Cada hive tiene un directorio de distribución que Syncthing mantiene sincronizado:

```
/var/lib/fluxbee/dist/
├── core/
│   ├── bin/
│   │   ├── rt-gateway
│   │   ├── sy-orchestrator
│   │   ├── sy-config-routes
│   │   ├── sy-opa-rules
│   │   └── sy-identity
│   └── manifest.json
├── runtimes/
│   ├── AI.soporte/1.3.0/
│   │   └── AI.soporte
│   ├── IO.whatsapp/2.1.0/
│   │   └── IO.whatsapp
│   └── manifest.json
└── vendor/
    ├── syncthing/
    │   └── syncthing
    └── manifest.json
```

**Este directorio es de solo lectura para el orchestrator.** El orchestrator lee de acá pero nunca escribe. Syncthing es el único que escribe.

### 7.2 Manifest

Cada categoría tiene un manifest con versión monotónica y hashes:

```json
{
  "schema_version": 1,
  "category": "core",
  "version": 13,
  "updated_at": "2026-03-01T14:00:00Z",
  "entries": [
    {
      "name": "rt-gateway",
      "path": "core/bin/rt-gateway",
      "sha256": "a1b2c3d4e5f6...",
      "size_bytes": 15234567
    },
    {
      "name": "sy-orchestrator",
      "path": "core/bin/sy-orchestrator",
      "sha256": "f6e5d4c3b2a1...",
      "size_bytes": 12345678
    }
  ]
}
```

### 7.3 Syncthing como canal de distribución

Syncthing se configura con un folder dedicado para distribución, separado del folder de blobs:

```
Folders Syncthing en motherbee:
  - fluxbee-blob:  /var/lib/fluxbee/blob/    (datos de negocio)
  - fluxbee-dist:  /var/lib/fluxbee/dist/    (software)
```

Ambos folders se sincronizan a los workers que correspondan. Son independientes: blob y dist no tienen nada que ver entre sí.

### 7.4 Flujo de update de software completo

```
1. Operador coloca binarios nuevos en motherbee:/var/lib/fluxbee/dist/
   (puede ser build automático, copia manual, CI/CD)

2. Operador actualiza manifest (`manifest_version` + hashes)

3. Syncthing detecta cambio y propaga a workers
   (automático, sin intervención)

4. Operador verifica que Syncthing sincronizó:
   GET /hives/{id}/sync-status  (futuro, por ahora verificar manualmente)

5. Operador dispara update:
   POST /hives/worker-1/update  { "category": "core", "manifest_version": 13, "manifest_hash": "sha256:..." }

6. SY.admin → unicast → SY.orchestrator@worker-1
   Mensaje: SYSTEM_UPDATE { category: core, manifest_version: 13, manifest_hash: ... }

7. SY.orchestrator@worker-1:
   a. Lee /var/lib/fluxbee/dist/core/manifest.json
   b. Verifica manifest_version == 13 y hash coincide
      - Si no coincide → responde sync_pending, operador reintenta
   c. Para cada binario en manifest:
      - sha256sum del instalado vs manifest
      - Si difiere → detener servicio, copiar de dist/, arrancar
   d. Health check de cada servicio reiniciado
   e. Si health falla → rollback (copiar binario anterior de vuelta)
   f. Responde SYSTEM_UPDATE_RESPONSE con resultado

8. SY.admin recibe respuesta, actualiza historial de deployments
```

---

## 8. Extensión de run/kill a cualquier nodo

### 8.1 Antes vs ahora

Antes: `run_node` / `kill_node` estaban pensados para nodos SY y routers con lógica específica hardcodeada.

Ahora: cualquier nodo se puede crear o destruir por comando. El orchestrator sabe arrancar un runtime genérico basado en el nombre L2.

### 8.2 Resolución de runtime

El orchestrator mapea el nombre del nodo al runtime:

```
Nombre L2: IO.slack.jdoe@sandbox
  → tipo: IO
  → runtime: IO.slack
  → buscar en: /var/lib/fluxbee/dist/runtimes/IO.slack/{version}/
```

```
Nombre L2: AI.soporte.l1@worker-1
  → tipo: AI
  → runtime: AI.soporte
  → buscar en: /var/lib/fluxbee/dist/runtimes/AI.soporte/{version}/
```

Regla de parseo: el runtime es `TYPE.campo1` (los primeros dos segmentos del nombre L2, sin el sufijo de instancia ni la isla).

### 8.3 Ejecución local

```rust
fn spawn_node(&self, name: &str, runtime: &str, version: &str, config: &Value) -> Result {
    // 1. Buscar binario en dist/
    let binary = format!("/var/lib/fluxbee/dist/runtimes/{}/{}/{}", runtime, version, runtime);
    if !Path::new(&binary).exists() {
        return Err(SpawnError::RuntimeNotFound(runtime.to_string()));
    }

    // 2. Verificar que no esté corriendo ya
    if self.is_node_running(name) {
        return Err(SpawnError::AlreadyRunning(name.to_string()));
    }

    // 3. Crear unit systemd dinámica o ejecutar directo
    //    (según estrategia: systemd transient unit o proceso hijo)
    let pid = self.start_process(&binary, name, config)?;

    // 4. Esperar registro en router (el nodo hace HELLO al router)
    self.wait_for_node_registration(name, Duration::from_secs(10))?;

    Ok(SpawnResult { name: name.to_string(), pid })
}
```

### 8.4 API HTTP en SY.admin (nuevo)

Endpoints nuevos o extendidos:

```
POST   /hives/{id}/update          → SYSTEM_UPDATE al orchestrator del hive
POST   /hives/{id}/nodes           → SPAWN_NODE al orchestrator del hive
DELETE /hives/{id}/nodes/{name}    → KILL_NODE al orchestrator del hive
GET    /hives/{id}/nodes           → list_nodes (sin cambios, lee LSA/SHM)
```

**POST /hives/{id}/update:**

```json
// Request
{
  "category": "runtime",
  "manifest_version": 14,
  "manifest_hash": "sha256:a1b2c3..."
}

// Response 200
{
  "status": "ok",
  "hive": "worker-1",
  "category": "runtime",
  "manifest_version": 14,
  "updated": ["AI.soporte/1.3.0"],
  "restarted": ["AI.soporte.l1"]
}

// Response 202 (sync pendiente)
{
  "status": "sync_pending",
  "hive": "worker-1",
  "message": "Retry when Syncthing finishes propagation"
}
```

**POST /hives/{id}/nodes:**

```json
// Request
{
  "node_name": "IO.slack.jdoe",
  "runtime": "IO.slack",
  "runtime_version": "2.1.0",
  "config": {
    "api_token_ref": "secret:slack:jdoe"
  }
}

// Response 200
{
  "status": "ok",
  "node_name": "IO.slack.jdoe@worker-1",
  "pid": 48221
}
```

**DELETE /hives/{id}/nodes/{name}:**

```json
// Request (body opcional)
{
  "force": false
}

// Response 200
{
  "status": "ok",
  "node_name": "IO.slack.jdoe@worker-1"
}
```

---

## 9. Eliminación de SSH post-bootstrap

### 9.1 Qué usaba SSH antes y qué lo reemplaza

| Operación | Antes (SSH) | Ahora |
|-----------|-------------|-------|
| `run_node` / `kill_node` | `ssh worker systemctl start` | Socket unicast → orchestrator@worker ejecuta local |
| rsync runtimes | `rsync -e ssh` | Syncthing (fluxbee-dist) |
| rsync core binaries | `rsync -e ssh` | Syncthing (fluxbee-dist) |
| rsync vendor | `rsync -e ssh` | Syncthing (fluxbee-dist) |
| health check | `ssh worker systemctl is-active` | Orchestrator@worker reporta por socket |
| drift check | `ssh worker sha256sum` | Orchestrator@worker verifica local y reporta |
| `add_hive` | SSH (se mantiene) | SSH (se mantiene, única vez) |
| `remove_hive` | SSH cleanup | Socket (`REMOVE_HIVE_CLEANUP`) o cleanup local-only si el worker no está alcanzable |

### 9.2 SSH post-bootstrap: cerrar o restringir

Después de add_hive exitoso, el operador puede:

**Cerrar SSH:**

```bash
# En el worker, como último paso de add_hive:
systemctl disable --now sshd
```

Más seguro. Si se necesita acceso de emergencia, el operador tiene que ir físicamente a la máquina o usar console remota (IPMI/KVM).


Motherbee usa una sola key (no una por worker). Solo se acepta conexión desde la IP de motherbee. Solo se ejecutan comandos aprobados por el gate script. Para emergencias y troubleshooting.


---

## 10. Cambios al código

### 10.1 sy_orchestrator.rs

**Handler de mensajes system — renombrar:**

```rust
// Antes:
Some("RUNTIME_UPDATE") => self.handle_runtime_update(payload, trace_id, src).await,

// Ahora:
Some("SYSTEM_UPDATE") => self.handle_system_update(payload, trace_id, src).await,
```

**Nuevo: handle_system_update:**

```rust
async fn handle_system_update(&mut self, payload: &Value, trace_id: &str, src: &str) -> Result {
    let category = payload["category"].as_str().unwrap_or("runtime");
    let expected_version = payload["manifest_version"].as_u64().unwrap_or(0);
    let expected_hash = payload["manifest_hash"].as_str().unwrap_or("");

    // 1. Leer manifest local
    let manifest_path = format!("/var/lib/fluxbee/dist/{}/manifest.json", category);
    let manifest = self.read_manifest(&manifest_path)?;

    // 2. Verificar sincronización
    if manifest.version != expected_version || manifest.hash() != expected_hash {
        return self.respond_sync_pending(trace_id, src, category, &manifest).await;
    }

    // 3. Aplicar update según categoría
    match category {
        "runtime" => self.apply_runtime_update(&manifest).await,
        "core" => self.apply_core_update(&manifest).await,
        "vendor" => self.apply_vendor_update(&manifest).await,
        _ => Err(anyhow!("Unknown category: {}", category)),
    }
}
```

**Extender execute_on_hive — eliminar SSH remoto:**

```rust
// Antes:
async fn execute_on_hive(&self, hive: &str, command: &str) -> Result {
    if hive == self.local_hive {
        exec_local(command).await
    } else {
        ssh_with_key(hive, command).await  // ELIMINAR
    }
}

// Ahora:
async fn execute_on_hive(&self, hive: &str, command: &str) -> Result {
    // El orchestrator SOLO ejecuta local
    if hive != self.local_hive {
        return Err(anyhow!("Orchestrator only executes locally. Target: {}", hive));
    }
    exec_local(command).await
}
```

**Extender SPAWN_NODE para cualquier nodo:**

```rust
async fn handle_spawn_node(&mut self, payload: &Value) -> Result {
    let node_name = payload["node_name"].as_str().required()?;
    let runtime = payload["runtime"].as_str().required()?;
    let version = payload["runtime_version"].as_str().unwrap_or("latest");

    // Resolver versión si es "latest"
    let version = if version == "latest" {
        self.resolve_latest_version(runtime)?
    } else {
        version.to_string()
    };

    // Buscar binario en dist/
    let binary = format!("/var/lib/fluxbee/dist/runtimes/{}/{}/{}", runtime, version, runtime);
    if !Path::new(&binary).exists() {
        return self.respond_error("runtime_not_found", &format!("{}/{}", runtime, version));
    }

    // Arrancar como systemd transient unit o proceso
    self.spawn_local_node(node_name, &binary, payload.get("config")).await
}
```

### 10.2 sy_admin.rs

**Nuevos endpoints HTTP:**

```rust
// En el dispatch de acciones:
"update" => self.handle_update(hive_id, payload, trace_id).await,
// run_node y kill_node ya existen, extender para aceptar cualquier tipo
```

**handle_update (nuevo):**

```rust
async fn handle_update(&self, hive_id: &str, payload: &Value, trace_id: &str) -> Result {
    let category = payload["category"].as_str().required()?;
    let manifest_version = payload["manifest_version"].as_u64().required()?;
    let manifest_hash = payload["manifest_hash"].as_str().required()?;

    // Validar categoría
    if !["runtime", "core", "vendor"].contains(&category) {
        return Err(bad_request("Invalid category"));
    }

    // Leer manifest local de motherbee para obtener hash
    let manifest = self.read_local_manifest(category)?;
    if manifest.version != manifest_version {
        return Err(not_found(&format!(
            "Version {} not found in local manifest",
            manifest_version
        )));
    }
    if manifest.hash() != manifest_hash {
        return Err(bad_request("manifest_hash mismatch with local manifest"));
    }

    // Enviar SYSTEM_UPDATE al orchestrator del hive destino
    let msg = SystemMessage::new("SYSTEM_UPDATE", json!({
        "category": category,
        "manifest_version": manifest_version,
        "manifest_hash": manifest_hash
    }));

    let dst = format!("SY.orchestrator@{}", hive_id);
    let response = self.send_and_wait(&dst, msg, trace_id, Duration::from_secs(60)).await?;

    Ok(response)
}
```

### 10.3 Archivos de config

**hive.yaml de worker (generado por add_hive):**

```yaml
# Auto-generado por SY.orchestrator@motherbee
# No editar manualmente
hive_id: worker-1
role: worker

wan:
  gateway_name: RT.gateway
  uplinks:
    - address: "192.168.8.1:9000"

nats:
  mode: embedded
  port: 4222

blob:
  enabled: true
  path: "/var/lib/fluxbee/blob"
  sync:
    enabled: true
    tool: syncthing
    api_port: 8384
    data_dir: "/var/lib/fluxbee/syncthing"

dist:
  path: "/var/lib/fluxbee/dist"
  sync:
    enabled: true
    tool: syncthing
```

Nuevo bloque `dist` en hive.yaml para declarar el repo de distribución y su sincronización.

---

## 11. Checklist de implementación

Estado de cierre (2026-03-08): implementado y validado por E2E en `docs/onworking/sy_orchestrator_v2_tasks.md`.

### Fase 1 — Orchestrator local en worker (prioridad máxima)

- [x] Agregar flag `role` al arranque de sy-orchestrator (leer de hive.yaml)
- [x] Condicionar funciones exclusivas de motherbee (add_hive, remove_hive) a `role == motherbee`
- [x] Eliminar SSH remoto de `execute_on_hive` (error si `hive != local`)
- [x] Crear systemd unit para sy-orchestrator en worker (add_hive la genera)
- [x] Testear arranque de orchestrator@worker: levanta servicios locales
- [x] Testear comunicación: admin@mother → unicast L2 → orchestrator@worker

### Fase 2 — SYSTEM_UPDATE

- [x] Renombrar RUNTIME_UPDATE a SYSTEM_UPDATE (sin backward compat)
- [x] Implementar `handle_system_update` con categorías (runtime/core/vendor)
- [x] Verificación de hash de manifest local vs esperado
- [x] Response `sync_pending` cuando manifest no coincide
- [x] Nuevo endpoint en admin: `POST /hives/{id}/update`
- [x] Testear: update de runtime con Syncthing sincronizado
- [x] Testear: update con sync pendiente (responde sync_pending)

### Fase 3 — SPAWN/KILL extendido

- [x] Extender SPAWN_NODE para aceptar cualquier tipo de nodo (AI, IO, WF)
- [x] Resolver runtime desde nombre L2 (parseo TYPE.campo1)
- [x] Buscar binario en dist/runtimes/ local
- [x] Arranque como systemd transient unit o proceso hijo
- [x] Extender KILL_NODE con flag `force` (SIGTERM vs SIGKILL)
- [x] Nuevo/extendido endpoint en admin: `POST /hives/{id}/nodes` (cualquier tipo)
- [x] Testear: spawn IO.slack.jdoe@worker-1 desde admin en motherbee

### Fase 4 — Syncthing como canal de distribución

- [x] Crear folder Syncthing `fluxbee-dist` separado de `fluxbee-blob`
- [x] Configurar en add_hive: agregar device + folder dist al worker
- [x] Bloque `dist` en hive.yaml
- [x] Testear: colocar binario nuevo en motherbee, verificar que aparece en worker
- [x] Testear: flujo completo (sync + SYSTEM_UPDATE + install + health)

### Fase 5 — Cierre de SSH

- [x] Implementar restricción de SSH post-bootstrap (Opción B: key única + IP + gate)
- [x] Copiar `fluxbee-ssh-gate.sh` en add_hive
- [x] Configurar authorized_keys con restricciones
- [x] Documentar proceso de emergencia (cómo acceder si todo falla)

---

## 12. Migración

Para sistemas existentes con workers ya provisionados sin orchestrator local:

```
1. Actualizar sy-orchestrator en motherbee (nuevo código con roles)
2. Por cada worker existente (usando SSH que todavía tienen):
   a. Copiar sy-orchestrator nuevo
   b. Copiar hive.yaml actualizado (agregar role: worker, bloque dist)
   c. Crear sy-orchestrator.service
   d. systemctl enable --now sy-orchestrator
   e. Verificar conexión por socket L2
3. Una vez todos los workers tienen orchestrator local:
   a. Las operaciones pasan por socket automáticamente
   b. Aplicar restricción de SSH (Fase 5)
```

No es necesario re-provisionar workers desde cero.
