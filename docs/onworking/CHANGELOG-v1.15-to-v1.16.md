# Fluxbee - CHANGELOG v1.15 → v1.16+

**Para:** Desarrolladores que tienen implementación basada en v1.15  
**Objetivo:** Guía de migración con todos los cambios estructurales

---

## Resumen Ejecutivo

La versión 1.16+ introduce cambios significativos en:

1. **Arquitectura Motherbee/Worker** - Nuevo modelo centralizado
2. **NATS embebido** - Buffer local en cada router
3. **Sistema Cognitivo** - Memoria episódica completa
4. **Persistencia** - SY.storage como único writer a PostgreSQL
5. **Sincronización** - Epoch/RCU reemplaza Seqlock
6. **Runtimes** - Sistema de distribución de binarios

---

## 1. Cambios de Arquitectura

### 1.1 Nuevo Modelo: Motherbee / Worker

**ANTES (v1.15):** Islas federadas, cada una autónoma

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│  Isla Hija  │────►│  Isla Hija  │────►│  Isla Madre │
│  autónoma   │     │  autónoma   │     │  (solo DB)  │
└─────────────┘     └─────────────┘     └─────────────┘
```

**AHORA (v1.16+):** Motherbee centraliza, Workers son stateless

```
┌─────────────┐     ┌─────────────┐
│   Worker    │     │   Worker    │
│  (stateless)│     │  (stateless)│
└──────┬──────┘     └──────┬──────┘
       └─────────┬─────────┘
                 │
        ┌────────▼────────┐
        │    Motherbee    │
        │  PostgreSQL     │
        │  SY.storage     │
        │  SY.orchestrator│
        └─────────────────┘
```

**Acción requerida:**
- Agregar campo `role: motherbee | worker` a hive.yaml
- Ver `07-operaciones.md` sección 3

### 1.2 Distribución de Componentes

| Componente | v1.15 | v1.16 Motherbee | v1.16 Worker |
|------------|-------|-----------------|--------------|
| PostgreSQL | Cualquier isla | ✓ Solo aquí | - |
| SY.storage | No existía | ✓ NUEVO | - |
| SY.orchestrator | Cualquier isla | ✓ Solo aquí | - |
| SY.admin | Cualquier isla | ✓ Solo aquí | - |
| SY.cognition | No existía | ✓ | ✓ |
| LanceDB | No existía | ✓ | ✓ |
| NATS | No existía | ✓ embebido | ✓ embebido |
| Router | ✓ | ✓ | ✓ |

**Documento:** `07-operaciones.md` sección 3.2

---

## 2. NATS Embebido (NUEVO)

### 2.1 Concepto

El router ahora tiene NATS JetStream embebido para buffering local.

**ANTES:** Router escribía directamente a PostgreSQL (bloqueante)

**AHORA:** Router publica a NATS local (~1ms), SY.storage persiste async

```
Router → NATS local (~1ms) → WAN → NATS madre → SY.storage → PostgreSQL
```

### 2.2 Configuración Nueva en hive.yaml

```yaml
# NUEVO en v1.16
nats:
  mode: embedded    # o "client"
  port: 4222
  storage_dir: "/var/lib/fluxbee/nats"
```

### 2.3 Cambios en Router

```rust
// NUEVO: El router tiene NATS client
pub struct Router {
    // ... existente ...
    
    // NUEVO
    nats_client: async_nats::Client,
    nats_server: Option<async_nats::Server>,  // Si embedded
    wan: Option<WanBridge>,                   // Si tiene WAN config
}
```

**Documento:** `13-storage.md` (NUEVO documento completo)

---

## 3. Sistema Cognitivo (NUEVO)

### 3.1 Nuevas Regiones SHM

**ANTES (v1.15):** 4 regiones

```
/dev/shm/
├── jsr-<router-uuid>
├── jsr-config-<hive>
├── jsr-lsa-<hive>
└── jsr-opa-<hive>
```

**AHORA (v1.16):** 6 regiones

```
/dev/shm/
├── jsr-<router-uuid>
├── jsr-config-<hive>
├── jsr-lsa-<hive>
├── jsr-opa-<hive>
├── jsr-identity-<hive>    # NUEVO
└── jsr-memory-<hive>      # NUEVO
```

### 3.2 Nuevo Proceso: SY.cognition

Lee turns de NATS, consolida en episodios, escribe a LanceDB y jsr-memory.

**Documentos:**
- `12-cognition.md` (NUEVO documento completo)
- `SY_nodes_spec.md` sección SY.cognition

### 3.3 Nuevo Proceso: SY.storage

Único proceso que escribe a PostgreSQL. Consume de NATS.

**Documento:** `13-storage.md` sección 5

### 3.4 Nuevas Tablas PostgreSQL

```sql
-- NUEVAS en v1.16
CREATE TABLE events (...);
CREATE TABLE memory_items (...);

-- MODIFICADA: turns tiene nuevos campos
CREATE TABLE turns (
    ...
    tags TEXT[],  -- NUEVO
);
```

**Documento:** `12-cognition.md` sección 2.2

---

## 4. Sincronización: Epoch/RCU (CAMBIO)

### 4.1 Seqlock → Epoch/RCU

**ANTES (v1.15):** Todas las regiones usan seqlock

```rust
// v1.15 - Reader copiaba datos
loop {
    let s1 = header.seq.load(Ordering::Acquire);
    if s1 & 1 != 0 { continue; }
    let data = /* copiar snapshot */;
    let s2 = header.seq.load(Ordering::Acquire);
    if s1 == s2 { break; }
}
```

**AHORA (v1.16):** Todas las regiones usan epoch/RCU

```rust
// v1.16 - Reader lee in-place, sin copiar
loop {
    let epoch = header.epoch.load(Ordering::Acquire);
    if epoch & 1 != 0 { continue; }  // transición
    let result = f(&*self.active);   // lee directo
    if epoch == header.epoch.load(Ordering::Acquire) {
        return result;
    }
}
```

### 4.2 Cambios en ShmHeader

```rust
// ANTES
pub seq: u64,  // Seqlock

// AHORA
pub epoch: u64,  // Epoch/RCU
```

**Documento:** `03-shm.md` sección 5

---

## 5. Protocolo: Nuevos Mensajes

### 5.1 Mensajes de Cognición (NUEVOS)

| Mensaje | Propósito |
|---------|-----------|
| `LSA_MEMORY` | Propagación de activaciones entre islas |
| `MEMORY_FETCH` | Request de EventPackage cross-isla |
| `MEMORY_RESPONSE` | Response con EventPackage |

**Documento:** `02-protocolo.md` sección 7.7

### 5.2 Mensajes de Orchestrator (NUEVOS)

| Mensaje | Propósito |
|---------|-----------|
| `RUNTIME_UPDATE` | Notifica nuevas versiones de runtimes |
| `SPAWN_NODE` | Solicita ejecución de nodo |
| `KILL_NODE` | Solicita terminación de nodo |

**Documento:** `02-protocolo.md` sección 7.8

### 5.3 Nuevo Campo en Meta: memory_package

```rust
// NUEVO en Meta
pub memory_package: Option<MemoryPackage>,
```

El router agrega `memory_package` con antecedentes episódicos relevantes.

**Documento:** `02-protocolo.md` sección 5 (struct Meta)

---

## 6. Orchestrator: Nuevas Responsabilidades

### 6.1 Solo en Motherbee

**ANTES:** SY.orchestrator podía estar en cualquier isla

**AHORA:** SY.orchestrator SOLO corre en Motherbee

### 6.2 Gestión de Runtimes (NUEVO)

```
/var/lib/fluxbee/runtimes/     ← Repo master en Motherbee
├── AI.soporte/
│   ├── 1.2.0/
│   └── 1.3.0/
└── manifest.json
```

El orchestrator:
- Recibe `RUNTIME_UPDATE` via router
- Sincroniza workers via SSH/rsync
- Verifica consistencia cada 5 min

**Documento:** `07-operaciones.md` sección 4.9

### 6.3 Nuevos Paths

```
/var/lib/fluxbee/orchestrator/
└── runtime-manifest.json    # Persistencia local del orchestrator
```

---

## 7. Identity Layer (CAMBIOS)

### 7.1 Nueva Región: jsr-identity

Contiene ILKs, ICHs, degrees, modules en SHM para consulta rápida.

**Documento:** `10-identity-layer3.md`

### 7.2 Contexto y Conversaciones (NUEVO)

- ICH (Interlocutor Channel)
- CTX (Context = hash de ILK + ICH)
- ctx_window (últimos 20 turns)
- ctx_seq (número de secuencia)

**Documento:** `11-context.md` (NUEVO documento)

---

## 8. Constantes Nuevas

```rust
// NUEVAS en v1.16

// Cognición
pub const MEMORY_MAGIC: u32 = 0x4A534D45;
pub const MEMORY_VERSION: u32 = 1;
pub const MAX_TENANTS: usize = 256;
pub const MAX_TAGS_PER_TENANT: usize = 16384;
pub const MAX_EVENTS_PER_TENANT: usize = 65536;
pub const MEMORY_FETCH_TIMEOUT_MS: u64 = 50;
pub const MEMORY_PACKAGE_MAX_BYTES: usize = 32 * 1024;

// Contexto
pub const CTX_WINDOW_SIZE: usize = 20;

// NATS
pub const NATS_DEFAULT_PORT: u16 = 4222;
pub const NATS_STREAM_MAX_AGE_DAYS: u64 = 7;
pub const WAN_BATCH_SIZE: usize = 100;
pub const WAN_BATCH_TIMEOUT_MS: u64 = 100;

// Runtimes
pub const RUNTIME_VERIFY_INTERVAL_SECS: u64 = 300;
```

**Documento:** `08-apendices.md` sección E

---

## 9. Nuevos Documentos

| Documento | Contenido |
|-----------|-----------|
| `11-context.md` | ICH, CTX, conversaciones, ctx_window |
| `12-cognition.md` | SY.cognition, episodios, jsr-memory, LanceDB |
| `13-storage.md` | NATS embebido, SY.storage, persistencia |

---

## 10. Archivos de Configuración

### 10.1 hive.yaml (CAMBIOS)

```yaml
# v1.15
hive_id: produccion
wan:
  listen: "0.0.0.0:9000"
database:
  url: "postgresql://..."

# v1.16 - NUEVOS campos
hive_id: produccion
role: motherbee              # NUEVO: motherbee | worker

wan:
  listen: "0.0.0.0:9000"
  
nats:                        # NUEVO bloque
  mode: embedded
  port: 4222
  
database:
  url: "postgresql://..."
```

### 10.2 Nuevos Archivos de Estado

```
/var/lib/fluxbee/
├── orchestrator/
│   └── runtime-manifest.json    # NUEVO
├── runtimes/                    # NUEVO directorio
│   └── manifest.json
├── nats/                        # NUEVO directorio
└── memory.lance/                # NUEVO (LanceDB)
```

---

## 11. Resumen de Acciones por Componente

### Router

| Acción | Prioridad | Documento |
|--------|-----------|-----------|
| Cambiar seqlock → epoch/RCU | Alta | `03-shm.md` §5 |
| Agregar NATS client embebido | Alta | `13-storage.md` §3 |
| Agregar WAN bridge (si config) | Alta | `13-storage.md` §4 |
| Leer jsr-memory para queries | Media | `12-cognition.md` §5 |
| Agregar memory_package a forwards | Media | `02-protocolo.md` §5 |

### Nuevos Procesos a Implementar

| Proceso | Ubicación | Documento |
|---------|-----------|-----------|
| SY.storage | Solo Motherbee | `13-storage.md` §5 |
| SY.cognition | Motherbee + Workers | `12-cognition.md` §3 |

### SY.orchestrator

| Acción | Prioridad | Documento |
|--------|-----------|-----------|
| Solo correr en Motherbee | Alta | `07-operaciones.md` §4 |
| Agregar gestión de runtimes | Media | `07-operaciones.md` §4.9 |
| Manejar RUNTIME_UPDATE | Media | `02-protocolo.md` §7.8 |
| Manejar SPAWN_NODE/KILL_NODE | Media | `02-protocolo.md` §7.8 |

### PostgreSQL

| Acción | Prioridad | Documento |
|--------|-----------|-----------|
| Crear tabla events | Alta | `12-cognition.md` §2.2 |
| Crear tabla memory_items | Alta | `12-cognition.md` §2.2 |
| Agregar campo tags a turns | Media | `12-cognition.md` §2.2 |

---

## 12. Orden Sugerido de Implementación

### Fase 1: Infraestructura Base
1. Cambiar seqlock → epoch/RCU en todas las regiones
2. Implementar SY.storage (NATS → PostgreSQL)
3. Agregar NATS embebido al router

### Fase 2: Cognición
4. Crear jsr-memory region
5. Implementar SY.cognition
6. Integrar LanceDB

### Fase 3: Integración Router
7. Router consulta jsr-memory
8. Router agrega memory_package
9. Router publica turns a NATS

### Fase 4: Orchestrator
10. Limitar orchestrator a Motherbee
11. Agregar gestión de runtimes
12. Implementar SPAWN_NODE/KILL_NODE

---

## 13. Breaking Changes

| Cambio | Impacto | Mitigación |
|--------|---------|------------|
| seqlock → epoch | Readers/writers incompatibles | Actualizar todo junto |
| Nuevo campo en Meta | Parseo puede fallar | Usar `#[serde(default)]` |
| NATS requerido | Router no arranca sin NATS | Embebido por default |
| role en hive.yaml | Orchestrator no arranca | Agregar campo |

---

## 14. Referencias Cruzadas

| Tema | Documento Viejo | Documento Nuevo |
|------|-----------------|-----------------|
| Arquitectura | `01-arquitectura.md` | Sin cambios mayores |
| Protocolo | `02-protocolo.md` | + secciones 7.7, 7.8, Meta |
| SHM | `03-shm.md` | Sección 5 completa reescrita |
| Routing | `04-routing.md` | Sin cambios mayores |
| Conectividad | `05-conectividad.md` | + WAN bridge para NATS |
| Regiones | `06-regiones.md` | + jsr-identity, jsr-memory |
| Operaciones | `07-operaciones.md` | Secciones 3, 4 reescritas |
| Apéndices | `08-apendices.md` | + glosario, constantes |
| Identity | `10-identity-layer3.md` | Sin cambios mayores |
| Contexto | - | `11-context.md` (NUEVO) |
| Cognición | - | `12-cognition.md` (NUEVO) |
| Storage | - | `13-storage.md` (NUEVO) |
