# Anexo Técnico: Distribución de Software

**Estado:** v1.0
**Fecha:** 2026-02-24
**Audiencia:** Desarrolladores de orchestrator, ops
**Complementa:** `07-operaciones.md` §4.9, `sy_orchestrator_tasks.md` TODO versionado
**Resuelve:** Gap de versionado de binarios core + integración de terceros vendorizados

---

## 1. Principio

Motherbee es el **repo master** de todo el software ejecutable del sistema. Los workers no descargan nada de internet ni de repos externos. Todo lo que un worker necesita para correr lo recibe del orchestrator vía SSH/rsync.

El orchestrator genera la configuración de cada worker según su rol, porque los servicios y configs de motherbee son distintos a los de un worker.

---

## 2. Categorías de software

| Categoría | Qué contiene | Ejemplos | Versionado |
|-----------|--------------|----------|------------|
| **Core** | Binarios de plataforma Fluxbee | `rt-gateway`, `sy-storage`, `sy-orchestrator`, `sy-admin`, `sy-config-routes`, `sy-opa-rules`, `sy-identity` | Por build (commit hash + semver) |
| **Runtimes** | Binarios de nodos de negocio | `AI.soporte`, `IO.whatsapp`, `WF.echo` | Por versión declarada en manifest (ya implementado §4.9) |
| **Vendor** | Binarios de terceros vendorizados | `syncthing` | Por versión upstream pinneada |

Las tres categorías comparten el mismo modelo: repo en motherbee, manifest que describe qué hay, propagación selectiva a workers por el orchestrator.

---

## 3. Layout en Motherbee (repo master)

```
/var/lib/fluxbee/
├── core/                              ← Binarios de plataforma
│   ├── bin/
│   │   ├── rt-gateway
│   │   ├── sy-storage
│   │   ├── sy-orchestrator
│   │   ├── sy-admin
│   │   ├── sy-config-routes
│   │   ├── sy-opa-rules
│   │   └── sy-identity
│   └── manifest.json
│
├── runtimes/                          ← Binarios de nodos (ya existente §4.9)
│   ├── AI.soporte/
│   │   └── 1.3.0/
│   ├── IO.whatsapp/
│   │   └── 2.1.0/
│   └── manifest.json
│
├── vendor/                            ← Binarios de terceros
│   ├── syncthing/
│   │   └── syncthing                  ← Binario estático
│   └── manifest.json
│
└── orchestrator/
    ├── runtime-manifest.json          ← (ya existente)
    ├── core-manifest.json             ← Copia local del core manifest
    └── vendor-manifest.json           ← Copia local del vendor manifest
```

### 3.1 ¿Cómo llegan los binarios al repo master?

**Core:** el proceso de build (CI o manual) compila los binarios y los coloca en `/var/lib/fluxbee/core/bin/`. `scripts/install.sh` ya hace esto hoy — instala en `/usr/bin/` y crea services. La propuesta es que además se mantenga copia en `/var/lib/fluxbee/core/bin/` como repo versionado.

**Runtimes:** ya resuelto. Llegan por `RUNTIME_UPDATE` o por copia manual.

**Vendor:** el binario se incluye en el release de Fluxbee. `scripts/install.sh` lo copia a `/var/lib/fluxbee/vendor/syncthing/`. Alternativa: el operador lo coloca manualmente. El orchestrator no descarga nada de internet.

---

## 4. Manifests

### 4.1 Core manifest

```json
{
  "schema_version": 1,
  "version": 12,
  "updated_at": "2026-02-24T10:00:00Z",
  "build_id": "abc123def",
  "components": {
    "rt-gateway": {
      "hash": "sha256:aabb1122...",
      "size": 15728640
    },
    "sy-storage": {
      "hash": "sha256:ccdd3344...",
      "size": 8388608
    },
    "sy-orchestrator": {
      "hash": "sha256:eeff5566...",
      "size": 10485760
    },
    "sy-admin": {
      "hash": "sha256:11223344...",
      "size": 9437184
    },
    "sy-config-routes": {
      "hash": "sha256:55667788...",
      "size": 6291456
    },
    "sy-opa-rules": {
      "hash": "sha256:99aabbcc...",
      "size": 7340032
    },
    "sy-identity": {
      "hash": "sha256:ddeeff00...",
      "size": 5242880
    }
  }
}
```

`version` es monotónica. `hash` es SHA-256 del binario. Sirve para detección de drift y verificación post-sync.

### 4.2 Vendor manifest

```json
{
  "schema_version": 1,
  "version": 1,
  "updated_at": "2026-02-24T10:00:00Z",
  "components": {
    "syncthing": {
      "upstream_version": "2.0.14",
      "hash": "sha256:fedcba98...",
      "size": 20971520,
      "path": "syncthing/syncthing"
    }
  }
}
```

Los argumentos de arranque vendor se mantienen **hardcodeados en orchestrator** por simplicidad operativa en esta etapa.

### 4.3 Runtime manifest

Ya existe (`runtime-manifest.json`). Sin cambios, se agrega `schema_version` como hardening (TODO §1).

---

## 5. Qué recibe cada worker

Un worker **no recibe todo**. El orchestrator selecciona qué propagar según el rol.

### 5.1 Binarios por rol

| Binario | Motherbee | Worker |
|---------|-----------|--------|
| `rt-gateway` | ✓ | ✓ |
| `sy-storage` | ✓ | ✗ |
| `sy-orchestrator` | ✓ | ✗ |
| `sy-admin` | ✓ | ✗ |
| `sy-config-routes` | ✓ | ✓ |
| `sy-opa-rules` | ✓ | ✓ |
| `sy-identity` | ✓ | ✓ (opcional) |
| `syncthing` | ✓ (si `blob.sync.enabled=true` y `blob.sync.tool=syncthing`) | ✓ (si `blob.sync.enabled=true` y `blob.sync.tool=syncthing`) |
| Runtimes (AI/IO/WF) | ✓ (repo master) | ✓ (los asignados) |

### 5.2 Layout resultante en worker

```
/usr/bin/
├── rt-gateway
├── sy-config-routes
├── sy-opa-rules
└── sy-identity                        ← opcional

/opt/fluxbee/bin/
└── syncthing                          ← si `blob.sync.enabled=true` y `blob.sync.tool=syncthing`

/var/lib/fluxbee/
├── runtimes/                          ← solo los asignados al worker
│   └── AI.soporte/1.3.0/
├── blob/
│   ├── staging/
│   └── active/
└── syncthing/                         ← estado de syncthing (autogenerado)

/etc/fluxbee/
└── hive.yaml                          ← generado por orchestrator
```

---

## 6. Generación de hive.yaml por rol

El orchestrator genera el `hive.yaml` de cada worker durante `add_hive`. El worker **nunca edita su propio hive.yaml** — lo recibe de motherbee.

### 6.1 hive.yaml de motherbee (editado por humano)

```yaml
hive_id: produccion
role: motherbee

wan:
  listen: "0.0.0.0:9000"
  authorized_hives:
    - worker-1
    - worker-2

nats:
  mode: embedded
  port: 4222

database:
  url: "postgresql://..."

blob:
  enabled: true
  path: "/var/lib/fluxbee/blob"
  sync:
    enabled: true
    tool: "syncthing"
    api_port: 8384
    data_dir: "/var/lib/fluxbee/syncthing"
```

### 6.2 hive.yaml de worker (generado por orchestrator)

```yaml
# Auto-generado por SY.orchestrator durante add_hive
# No editar manualmente
hive_id: worker-1
role: worker

wan:
  gateway_name: RT.gateway
  uplinks:
    - address: "{mother_wan_ip}:{mother_wan_port}"

nats:
  mode: embedded
  port: 4222

blob:
  enabled: true
  path: "/var/lib/fluxbee/blob"
  sync:
    enabled: true
    tool: "syncthing"
    api_port: 8384
    data_dir: "/var/lib/fluxbee/syncthing"
```

**Diferencias clave entre motherbee y worker:**

| Campo | Motherbee | Worker |
|-------|-----------|--------|
| `role` | `motherbee` | `worker` |
| `wan.listen` | Sí (acepta conexiones) | No (solo uplinks) |
| `wan.uplinks` | No (es el hub) | Sí (apunta a motherbee) |
| `wan.authorized_hives` | Sí (controla quién conecta) | No |
| `database` | Sí (PostgreSQL) | No |
| `blob.sync` | Igual | Igual (si habilitado) |
| `nats` | Igual | Igual |

El orchestrator toma los datos de la motherbee (IP WAN, puerto, config de blob/nats) y genera el hive.yaml del worker con los valores correctos para su rol.

---

## 7. Flujo de propagación

### 7.1 add_hive (bootstrap de worker nuevo)

Extiende el flujo actual (`07-operaciones.md` §5) con propagación selectiva:

```
PASO 5 (revisado): Copiar binarios según rol worker
  │
  ├── Core: scp rt-gateway, sy-config-routes, sy-opa-rules, sy-identity
  │         desde /var/lib/fluxbee/core/bin/ → worker:/usr/bin/
  │
  ├── Vendor: si `blob.sync.enabled=true` y `blob.sync.tool=syncthing`
  │           scp syncthing desde /var/lib/fluxbee/vendor/syncthing/
  │           → worker:/opt/fluxbee/bin/syncthing
  │
  └── Runtimes: si hay runtimes asignados a este worker
                rsync desde /var/lib/fluxbee/runtimes/<assigned>/
                → worker:/var/lib/fluxbee/runtimes/

PASO 7 (revisado): Generar hive.yaml según rol
  │
  └── Orchestrator genera hive.yaml con:
      - hive_id del worker
      - role: worker
      - wan.uplinks apuntando a motherbee
      - nats config copiada de motherbee
      - blob config copiada de motherbee (si habilitado)
      - SIN database, SIN wan.listen, SIN authorized_hives
```

### 7.2 Actualización de core (post-build)

Cuando se compila una nueva versión de los binarios core:

```
1. Build produce binarios nuevos
2. Operador actualiza /var/lib/fluxbee/core/bin/ y core manifest.json
   (o scripts/install.sh lo hace automáticamente)
3. Orchestrator detecta cambio en manifest (versión monotónica)
4. Para cada worker:
   a. rsync binarios core según rol del worker
   b. Verificar hash post-sync
   c. Restart servicios en orden de dependencia:
      i.   sy-opa-rules (sin dependencias)
      ii.  sy-config-routes (sin dependencias)
      iii. sy-identity (sin dependencias)
      iv.  rt-gateway (depende de sy-config-routes, sy-opa-rules)
      Con health-gate entre pasos (verificar servicio activo antes de siguiente)
   d. Si health-check falla → rollback del componente que falló
5. Log resultado por worker
```

### 7.3 Actualización de vendor

Mismo flujo que core pero más infrecuente. El operador actualiza el binario en `/var/lib/fluxbee/vendor/` y el manifest. El orchestrator propaga.

### 7.4 Actualización de runtimes

Ya implementado en §4.9. Sin cambios.

---

## 8. Detección de drift

El orchestrator ya verifica drift de runtimes cada 5 min (§4.9.5). El mismo mecanismo se extiende a core y vendor:

```
Cada 5 minutos:
  Para cada worker:
    1. SSH: sha256sum de cada binario core instalado
    2. Comparar con core manifest local
    3. Si drift → rsync + restart afectados
    4. SSH: sha256sum de cada binario vendor instalado
    5. Comparar con vendor manifest local
    6. Si drift → rsync + restart afectados
```

Los tres manifests (core, vendor, runtime) se verifican en el mismo loop. Un solo pass por worker.

---

## 9. Orden de restart por dependencia

Cuando se actualizan binarios core en un worker, el restart no puede ser simultáneo. Hay dependencias:

```
sy-opa-rules       ← independiente
sy-config-routes   ← independiente
sy-identity        ← independiente (opcional)
rt-gateway         ← depende de: sy-config-routes, sy-opa-rules cargados en SHM
```

**Regla:** restart de mayor a menor dependencia. Para cada servicio:
1. Detener servicio
2. Reemplazar binario (ya hecho por rsync)
3. Arrancar servicio
4. Esperar health (servicio activo + SHM actualizado si aplica)
5. Si health falla en 30s → rollback: restaurar binario anterior, arrancar, alertar

**Rollback granular:** se mantiene una copia del binario anterior en `/var/lib/fluxbee/core/bin.prev/` (solo el último). Si el nuevo falla, se restaura desde ahí.

---

## 10. Vendor: integración de Syncthing

### 10.1 Origen del binario

El binario de Syncthing se incluye en el release de Fluxbee. En el repo de desarrollo:

```
fluxbee/
└── vendor/
    └── syncthing/
        ├── syncthing-linux-amd64      ← binario estático descargado de releases
        ├── SHA256SUMS                 ← checksum oficial de Syncthing
        └── VERSION                    ← "2.0.14"
```

`scripts/install.sh` copia el binario a `/var/lib/fluxbee/vendor/syncthing/syncthing` durante instalación.

### 10.2 Ciclo de vida en runtime

El orchestrator gestiona Syncthing como cualquier servicio:

```
Bootstrap (motherbee o add_hive):
  1. Copiar binario a /opt/fluxbee/bin/syncthing
  2. Arrancar con: syncthing --no-upgrade -home /var/lib/fluxbee/syncthing/
  3. Primer arranque genera: Device ID, cert TLS, config.xml, database
  4. Orchestrator configura via REST API (folder, devices, discovery off)

Runtime:
  - Health check via GET /rest/noauth/health
  - Restart si no responde
  - Reconfiguración en add_hive/remove_hive

Nota:
- Flags de arranque se mantienen hardcodeadas en orchestrator (no en manifest vendor) en esta fase.
```

### 10.3 Actualización de Syncthing

Infrecuente. El operador:
1. Descarga nuevo binario de Syncthing releases
2. Verifica checksum
3. Reemplaza en `/var/lib/fluxbee/vendor/syncthing/`
4. Actualiza `vendor/manifest.json`
5. Orchestrator propaga a workers en siguiente ciclo de verificación

`--no-upgrade` impide que Syncthing se auto-actualice.

---

## 11. Constantes

```rust
// fluxbee::distribution::constants

/// Intervalo de verificación de drift (compartido core/vendor/runtimes)
pub const DIST_VERIFY_INTERVAL_SECS: u64 = 300;

/// Timeout de health-check post-restart de servicio
pub const DIST_HEALTH_TIMEOUT_SECS: u64 = 30;

/// Timeout de rsync por worker
pub const DIST_SYNC_TIMEOUT_SECS: u64 = 300;

/// Path de binarios core en motherbee
pub const CORE_BIN_PATH: &str = "/var/lib/fluxbee/core/bin";

/// Path de binarios core previos (para rollback)
pub const CORE_BIN_PREV_PATH: &str = "/var/lib/fluxbee/core/bin.prev";

/// Path de binarios vendor en motherbee
pub const VENDOR_PATH: &str = "/var/lib/fluxbee/vendor";

/// Path de instalación de vendor en workers
pub const VENDOR_INSTALL_PATH: &str = "/opt/fluxbee/bin";
```

---

## 12. Relación con TODO existente (estado de diseño)

Este documento **cierra el diseño objetivo**, no la implementación en código, de los ítems abiertos en `sy_orchestrator_tasks.md`:

| TODO | Estado con este doc |
|------|---------------------|
| §1 Contrato versionado runtimes | Diseñado (`schema_version`, monotonicidad, errores). Implementación pendiente |
| §2 Rollout runtimes por worker | Diseñado (verificación post-sync, canary). Implementación pendiente |
| §3 Versionado binarios core | Diseñado (manifest, promoción, restart, rollback). Implementación pendiente |
| §4 API/observabilidad versiones | Diseñado. Implementación pendiente |
| §5 Validación E2E | Diseñado (casos a validar). Implementación pendiente |

Adicionalmente resuelve:
- Integración de vendor (terceros) que no estaba contemplada
- Generación de hive.yaml por rol (worker no edita su config)
- Propagación selectiva de binarios según rol

---

## 13. Plan de compatibilización por colisiones (doc-first, sin cambios de código)

Principio operativo:
- Primero se decide contrato/camino en documentación.
- Recién después se implementa en código.

Decisiones confirmadas (2026-02-25):
1. **C1 - Vendor sin internet**  
   Aprobado: workers no instalan vendor vía internet; fuente única = repo master local (`/var/lib/fluxbee/vendor`).
2. **C2 - Contrato canónico de `hive.yaml` para blob sync**  
   Aprobado: contrato oficial = `blob.sync.enabled/tool/api_port/data_dir`.
3. **C3 - Origen de binarios core**  
   Aprobado: bootstrap/add_hive debe usar `/var/lib/fluxbee/core/bin/*` (no `/usr/bin/*` como origen de distribución).
4. **C4 - Flags vendor**  
   Aprobado: flags permanecen hardcodeadas en orchestrator por ahora.

Resultado esperado de cada colisión:
- decisión explícita (aceptada/rechazada),
- impacto en docs afectados,
- tarea de implementación concreta en `sy_orchestrator_tasks.md`.
