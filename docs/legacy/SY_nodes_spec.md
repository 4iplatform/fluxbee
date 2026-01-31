# JSON Router - Nodos SY (System)

**Estado:** Draft v0.13
**Fecha:** 2025-01-29
**Documento relacionado:** JSON Router Especificación Técnica v1.13

---

## 1. Introducción

Los nodos SY (System) son componentes de infraestructura que proveen servicios esenciales al sistema de mensajería. A diferencia de los nodos AI, WF e IO que procesan lógica de negocio, los nodos SY mantienen la operación del sistema mismo.

### 1.1 Características comunes

- **Lenguaje:** Rust (obligatorio, **excepto SY.opa.rules que es Go**)
- **Distribución:** Parte del paquete `json-router`
- **Privilegios:** Pueden acceder a shared memory directamente
- **Ciclo de vida:** Gestionados por SY.orchestrator
- **Nomenclatura:** `SY.<servicio>@<isla>`
- **Librería:** `json_router::node_client` (Rust) o equivalente Go

### 1.2 Excepción: SY.opa.rules en Go

| Tipo de nodo | Lenguaje | Razón |
|--------------|----------|-------|
| SY.* (excepto opa.rules) | **Rust (obligatorio)** | Infraestructura crítica, mismo crate que router |
| **SY.opa.rules** | **Go (obligatorio)** | Requiere compilador OPA embebido (solo disponible en Go) |
| AI.*, WF.*, IO.* | Rust o Node.js | Lógica de negocio, flexibilidad permitida |

**Razones para SY.opa.rules en Go:**
- El compilador Rego → WASM solo existe en Go (librería OPA oficial)
- No hay alternativa en Rust que compile a WASM
- Regorus (Rust) solo interpreta, no compila a WASM
- Performance crítica requiere WASM, no interpretación

### 1.3 Catálogo de nodos SY

| Nodo | Lenguaje | Instancias | Estado | Descripción |
|------|----------|------------|--------|-------------|
| `SY.admin` | Rust | Único global (mother) | Especificado | Gateway HTTP REST para toda la infraestructura |
| `SY.orchestrator` | Rust | Uno por isla | Especificado | Orquestación de routers y nodos |
| `SY.config.routes` | Rust | Uno por isla | Especificado | Configuración de rutas estáticas y VPNs |
| `SY.opa.rules` | **Go** | Uno por isla | **Especificado v0.13** | Compilación y gestión de policies OPA |
| `SY.time` | Rust | Uno por isla | Por especificar | Sincronización de tiempo |
| `SY.log` | Rust | Uno por isla | Por especificar | Colector de logs |

---

## 2. SY.config.routes

[Contenido sin cambios - ver documento original]

---

## 3. SY.opa.rules

Responsable de la compilación, distribución y gestión de policies OPA para routing dinámico.

### 3.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Lenguaje | **Go** (único nodo SY en Go) |
| Nombre L2 | `SY.opa.rules@<isla>` (uno por isla) |
| Compilador | OPA library embebida (`github.com/open-policy-agent/opa/compile`) |
| Región SHM | `/dev/shm/jsr-opa-<island>` (WASM para routers) |
| Persistencia disco | `/var/lib/json-router/opa/` |
| Estado en RAM | Rego actual para respuesta rápida |

### 3.2 Arquitectura

```
┌─────────────────────────────────────────────────────────────────┐
│                      SY.opa.rules (Go)                          │
│                                                                 │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │ Message     │  │ OPA         │  │ SHM         │             │
│  │ Handler     │  │ Compiler    │  │ Writer      │             │
│  │             │  │ (embebido)  │  │             │             │
│  │ - broadcast │  │ - rego→wasm │  │ - seqlock   │             │
│  │ - unicast   │  │ - validate  │  │ - wasm data │             │
│  │ - query     │  │             │  │             │             │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘             │
│         │                │                │                     │
│         └────────────────┼────────────────┘                     │
│                          │                                      │
│  ┌───────────────────────┴───────────────────────┐             │
│  │                    State                       │             │
│  │                                                │             │
│  │  RAM:                                          │             │
│  │  ├── current_rego: string                     │             │
│  │  ├── current_version: u64                     │             │
│  │  ├── staged_rego: *string                     │             │
│  │  └── status: enum                             │             │
│  │                                                │             │
│  │  DISCO (/var/lib/json-router/opa/):           │             │
│  │  ├── current/{policy.rego, metadata.json}     │             │
│  │  ├── staged/{policy.rego, metadata.json}      │             │
│  │  └── backup/{policy.rego, metadata.json}      │             │
│  │                                                │             │
│  │  SHM (/dev/shm/jsr-opa-<island>):             │             │
│  │  └── Header + WASM bytes                      │             │
│  │                                                │             │
│  └───────────────────────────────────────────────┘             │
└─────────────────────────────────────────────────────────────────┘
```

### 3.3 Storage Model

#### 3.3.1 Disco (Persistencia)

```
/var/lib/json-router/opa/
├── current/
│   ├── policy.rego        # Fuente Rego activo
│   └── metadata.json      # {version, compiled_at, entrypoint, hash}
├── staged/
│   ├── policy.rego        # Fuente compilado OK, pendiente apply
│   └── metadata.json
└── backup/
    ├── policy.rego        # Backup para rollback
    └── metadata.json
```

#### 3.3.2 RAM (Estado interno)

```go
type OpaRulesState struct {
    // Versión activa (en RAM para respuesta rápida)
    CurrentVersion   uint64
    CurrentRego      string    // Fuente completo
    CurrentHash      string    // SHA256 del WASM
    CompiledAt       time.Time
    Entrypoint       string
    
    // Versión staged (si hay)
    StagedVersion    *uint64
    StagedRego       *string
    StagedHash       *string
    
    // Estado
    Status           Status    // OK, ERROR, COMPILING, STAGED
    LastError        *string
    LastErrorAt      *time.Time
    
    // Info
    IslandID         string
}

type Status int
const (
    StatusOK Status = iota
    StatusError
    StatusCompiling
    StatusStaged
)
```

#### 3.3.3 SHM (WASM para routers)

Los routers leen el WASM compilado directamente de shared memory para máxima performance.

```
/dev/shm/jsr-opa-<island>
┌────────────────────────────────────────────────────────────────┐
│  Header (128 bytes):                                           │
│  ├── magic: u32              = 0x4A534F50 ("JSOP")            │
│  ├── version: u32            = 1                               │
│  ├── seqlock: u64            ← Para lectura consistente        │
│  ├── policy_version: u64     ← Versión de la policy            │
│  ├── wasm_size: u32          ← Tamaño del WASM en bytes        │
│  ├── wasm_hash: [u8; 32]     ← SHA256 del WASM                 │
│  ├── updated_at: u64         ← Timestamp epoch ms              │
│  ├── status: u8              ← 0=ok, 1=error, 2=loading        │
│  ├── entrypoint: [u8; 64]    ← "router/target"                 │
│  ├── entrypoint_len: u8                                        │
│  └── _reserved: [u8; N]                                        │
├────────────────────────────────────────────────────────────────┤
│  WASM Data (variable, hasta 4MB):                              │
│  └── wasm_bytes: [u8; wasm_size]                               │
└────────────────────────────────────────────────────────────────┘
```

**Constantes:**

```rust
pub const OPA_MAGIC: u32 = 0x4A534F50;  // "JSOP"
pub const OPA_VERSION: u32 = 1;
pub const OPA_MAX_WASM_SIZE: u32 = 4 * 1024 * 1024;  // 4MB
```

### 3.4 Flujo de Arranque

```
sy-opa-rules (binario Go)
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. Leer configuración                                       │
│    - /etc/json-router/island.yaml → island_id              │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. Verificar instancia única                                │
│    - Lock file: /var/run/json-router/sy-opa-rules.lock     │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Inicializar SHM                                          │
│    - Crear/abrir /dev/shm/jsr-opa-<island>                 │
│    - Si existe policy en disco → cargar y escribir WASM    │
│    - Si no existe → SHM vacío, status = OK, version = 0    │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. Cargar estado en RAM                                     │
│    - Leer current/policy.rego → CurrentRego                │
│    - Leer current/metadata.json → version, hash, etc.      │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. Conectar al router local                                 │
│    - Registrarse como SY.opa.rules@<isla>                  │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. Loop principal                                           │
│    - Procesar mensajes (broadcast + unicast)               │
│    - Actualizar heartbeat en SHM cada 5s                   │
└─────────────────────────────────────────────────────────────┘
```

### 3.5 Mensajes

SY.opa.rules recibe dos tipos de mensajes:
1. **Broadcast** (CONFIG_CHANGED subsystem: opa) - Operaciones en todas/varias islas
2. **Unicast** (directo a SY.opa.rules@isla) - Operaciones en una sola isla

#### 3.5.1 Broadcast: CONFIG_CHANGED (subsystem: opa)

**Versionado:** SY.admin asigna números de versión usando un contador monotónico persistido en `/var/lib/json-router/opa-version.txt`. El contador siempre incrementa, incluso si la operación falla.

**Compilar nuevo código (queda staged):**

```json
{
  "routing": {
    "src": "<uuid-sy-admin>",
    "dst": "broadcast",
    "ttl": 16
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_CHANGED"
  },
  "payload": {
    "subsystem": "opa",
    "action": "compile",
    "version": 43,
    "auto_apply": false,
    "config": {
      "rego": "package router\n\ndefault target = null\n...",
      "entrypoint": "router/target"
    }
  }
}
```

**Recompilar código actual (refresh, sin nuevo rego):**

```json
{
  "payload": {
    "subsystem": "opa",
    "action": "compile",
    "version": 43,
    "auto_apply": false
    // Sin config.rego = usa el rego actual
  }
}
```

**Aplicar staged en todas las islas:**

```json
{
  "payload": {
    "subsystem": "opa",
    "action": "apply",
    "version": 43
  }
}
```

**Compilar Y aplicar en un paso (operación rápida pero sin staging):**

```json
{
  "payload": {
    "subsystem": "opa",
    "action": "compile",
    "version": 43,
    "auto_apply": true,
    "config": {
      "rego": "package router\n...",
      "entrypoint": "router/target"
    }
  }
}
```

**Rollback en todas las islas:**

```json
{
  "payload": {
    "subsystem": "opa",
    "action": "rollback"
  }
}
```

#### 3.5.2 Respuesta a Broadcast

Cada SY.opa.rules responde a SY.admin:

**Respuesta OK:**

```json
{
  "routing": {
    "src": "<uuid-sy-opa-rules>",
    "dst": "<uuid-sy-admin>"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_RESPONSE"
  },
  "payload": {
    "subsystem": "opa",
    "action": "compile",
    "version": 43,
    "status": "ok",
    "island": "staging",
    "compile_time_ms": 1250,
    "wasm_size_bytes": 245000,
    "hash": "sha256:abc123..."
  }
}
```

**Respuesta ERROR:**

```json
{
  "payload": {
    "subsystem": "opa",
    "action": "compile",
    "version": 43,
    "status": "error",
    "island": "dev",
    "error_code": "COMPILE_ERROR",
    "error_detail": "policy.rego:15: rego_parse_error: unexpected token"
  }
}
```

#### 3.5.3 Unicast: Queries

**GET_POLICY - Obtener policy actual:**

```json
// Request
{
  "routing": {
    "src": "<uuid-quien-pregunta>",
    "dst": "SY.opa.rules@staging"
  },
  "meta": {
    "type": "query",
    "action": "get_policy"
  },
  "payload": {}
}

// Response
{
  "meta": {
    "type": "query_response",
    "action": "get_policy"
  },
  "payload": {
    "version": 42,
    "rego": "package router\n...",
    "hash": "sha256:abc123...",
    "compiled_at": "2025-01-26T10:00:00Z",
    "entrypoint": "router/target",
    "status": "ok"
  }
}
```

**GET_STATUS - Estado del nodo:**

```json
// Request
{
  "meta": {
    "type": "query",
    "action": "get_status"
  },
  "payload": {}
}

// Response
{
  "payload": {
    "island": "staging",
    "current_version": 42,
    "current_hash": "sha256:abc123...",
    "staged_version": null,
    "status": "ok",
    "last_error": null,
    "wasm_size_bytes": 245000,
    "uptime_seconds": 3600
  }
}
```

#### 3.5.4 Unicast: Commands

**COMPILE_POLICY - Compilar en esta isla:**

```json
// Request
{
  "routing": {
    "src": "<uuid-sy-admin>",
    "dst": "SY.opa.rules@staging"
  },
  "meta": {
    "type": "command",
    "action": "compile_policy"
  },
  "payload": {
    "rego": "package router\n...",
    "entrypoint": "router/target",
    "version": 43
  }
}

// Response OK
{
  "meta": {
    "type": "command_response",
    "action": "compile_policy"
  },
  "payload": {
    "status": "ok",
    "version": 43,
    "hash": "sha256:xyz789...",
    "compile_time_ms": 1250,
    "wasm_size_bytes": 245000,
    "staged": true
  }
}

// Response ERROR
{
  "payload": {
    "status": "error",
    "error_code": "COMPILE_ERROR",
    "error_detail": "policy.rego:15: rego_parse_error: unexpected token",
    "version": 43
  }
}
```

**APPLY_POLICY - Aplicar staged:**

```json
// Request
{
  "meta": {
    "type": "command",
    "action": "apply_policy"
  },
  "payload": {
    "version": 43
  }
}

// Response
{
  "payload": {
    "status": "ok",
    "version": 43,
    "previous_version": 42
  }
}
```

**ROLLBACK_POLICY - Volver a backup:**

```json
// Request
{
  "meta": {
    "type": "command",
    "action": "rollback_policy"
  },
  "payload": {}
}

// Response
{
  "payload": {
    "status": "ok",
    "version": 41,
    "rolled_back_from": 42
  }
}
```

### 3.6 Flujo de Compilación

```
SY.opa.rules recibe compile (broadcast o unicast)
         │
         ▼
┌─────────────────────────────────────────┐
│ 1. Validar rego con OPA parser          │
│    - Si error sintaxis → responder error│
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ 2. Compilar Rego → WASM                 │
│    - compile.New().WithTarget(Wasm)...  │
│    - Si error → responder error         │
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ 3. Guardar como staged                  │
│    - staged/policy.rego                 │
│    - staged/metadata.json               │
│    - state.StagedRego = rego            │
│    - state.Status = STAGED              │
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ 4. Responder OK                         │
│    - status: "ok", staged: true         │
└─────────────────────────────────────────┘
```

### 3.7 Flujo de Apply

```
SY.opa.rules recibe apply (broadcast o unicast)
         │
         ▼
┌─────────────────────────────────────────┐
│ 1. Verificar que hay staged             │
│    - Si no hay → error "NOTHING_STAGED" │
│    - Verificar version match            │
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ 2. Backup                               │
│    - Mover current/ → backup/           │
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ 3. Activar                              │
│    - Mover staged/ → current/           │
│    - Actualizar RAM state               │
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ 4. Escribir WASM en SHM                 │
│    - seqlock_begin_write()              │
│    - header.policy_version = version    │
│    - header.wasm_size = len(wasm)       │
│    - copiar wasm_bytes                  │
│    - seqlock_end_write()                │
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ 5. Broadcast OPA_RELOAD local (TTL 2)   │
│    - Notifica routers de esta isla      │
└────────────────────┬────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────┐
│ 6. Responder OK                         │
└─────────────────────────────────────────┘
```

### 3.8 Mensaje OPA_RELOAD (local)

Después de apply, SY.opa.rules envía broadcast **local** (TTL 2) para notificar a los routers:

```json
{
  "routing": {
    "src": "<uuid-sy-opa-rules>",
    "dst": "broadcast",
    "ttl": 2
  },
  "meta": {
    "type": "system",
    "msg": "OPA_RELOAD"
  },
  "payload": {
    "version": 43,
    "hash": "sha256:xyz789..."
  }
}
```

**Nota:** TTL 2 significa que no cruza a otras islas. Es solo para routers locales.

Los routers pueden:
1. Recibir OPA_RELOAD y recargar inmediatamente
2. O detectar cambio en SHM por `policy_version` diferente

### 3.9 Cómo los Routers Leen el WASM

```rust
impl Router {
    fn load_opa_policy(&mut self) -> Result<()> {
        let shm = self.map_opa_region()?;  // /dev/shm/jsr-opa-<island>
        
        loop {
            // Seqlock read protocol
            let seq = shm.header.seqlock.load(Ordering::Acquire);
            if seq & 1 != 0 {
                std::hint::spin_loop();
                continue;  // Writer activo
            }
            
            // Leer datos
            let version = shm.header.policy_version;
            let size = shm.header.wasm_size as usize;
            let wasm = shm.wasm_bytes[..size].to_vec();
            
            // Verificar consistencia
            std::sync::atomic::fence(Ordering::Acquire);
            if shm.header.seqlock.load(Ordering::Acquire) != seq {
                continue;  // Cambió durante lectura
            }
            
            // Cargar en Wasmtime si es nueva versión
            if version != self.opa_policy_version {
                let module = Module::new(&self.engine, &wasm)?;
                self.opa_instance = Some(Instance::new(&self.store, &module)?);
                self.opa_policy_version = version;
                log::info!("Loaded OPA policy version {}", version);
            }
            
            return Ok(());
        }
    }
    
    // Llamado periódicamente o al recibir OPA_RELOAD
    fn check_opa_update(&mut self) {
        if let Some(shm) = self.opa_region.as_ref() {
            if shm.header.policy_version != self.opa_policy_version {
                if let Err(e) = self.load_opa_policy() {
                    log::error!("Failed to load OPA policy: {}", e);
                }
            }
        }
    }
}
```

### 3.10 Flujo desde SY.admin (API HTTP)

**Versionado:** SY.admin mantiene un contador monotónico en `/var/lib/json-router/opa-version.txt`. Cada operación que requiere versión incrementa el contador antes de enviar el mensaje.

#### Endpoints HTTP

| Endpoint | Acción mensaje | auto_apply | Descripción |
|----------|----------------|------------|-------------|
| POST /opa/policy | compile + apply | - | Compila, si OK aplica |
| POST /opa/policy/compile | compile | false | Solo compila, queda staged |
| POST /opa/policy/check | compile | false | Alias de /compile (validación) |
| POST /opa/policy/apply | apply | - | Aplica versión staged |
| POST /opa/policy/rollback | rollback | - | Vuelve a backup |

#### Caso A: Broadcast a todas las islas (compile + apply)

```
POST /opa/policy
{
  "rego": "package router\n...",
  "target": "broadcast"
}

SY.admin:
  1. Incrementar version (persistir en archivo)
  2. Broadcast CONFIG_CHANGED {action: compile, auto_apply: false}
  3. Espera CONFIG_RESPONSE de todas las islas (timeout 30s)
  4. Si TODAS OK → Broadcast CONFIG_CHANGED {action: apply}
  5. Si alguna ERROR → NO aplica, retorna error

HTTP Response:
{
  "status": "ok",
  "version": 43,
  "islands": [
    {"island": "produccion", "status": "ok", "compile_time_ms": 1100},
    {"island": "staging", "status": "ok", "compile_time_ms": 1250}
  ]
}
```

#### Caso B: Unicast a una isla específica

```
POST /opa/policy
{
  "rego": "package router\n...",
  "target": "staging"
}

SY.admin:
  1. Unicast COMPILE_POLICY a SY.opa.rules@staging
  2. Espera respuesta
  3. Si OK → Unicast APPLY_POLICY
  4. Retorna resultado

HTTP Response:
{
  "status": "ok",
  "version": 43,
  "island": "staging",
  "compile_time_ms": 1250
}
```

#### Caso C: Solo check (compilar sin apply)

```
POST /opa/policy/check
{
  "rego": "package router\n...",
  "target": "staging"
}

SY.admin:
  1. Unicast COMPILE_POLICY a SY.opa.rules@staging
  2. Espera respuesta (queda staged, no se aplica)

HTTP Response:
{
  "status": "ok",
  "staged": true,
  "version": 43,
  "compile_time_ms": 1250
}
```

### 3.11 Códigos de Error

| Error | Descripción |
|-------|-------------|
| `COMPILE_ERROR` | Rego no compila (error de sintaxis o semántica) |
| `NOTHING_STAGED` | Se pidió apply pero no hay versión staged |
| `VERSION_MISMATCH` | La versión staged no coincide con la solicitada |
| `NO_BACKUP` | Se pidió rollback pero no hay backup |
| `SHM_ERROR` | Error escribiendo en shared memory |
| `TIMEOUT` | Timeout esperando compilación |

### 3.12 Tiempos Esperados de Compilación

| Complejidad | Líneas Rego | Tiempo típico | WASM size |
|-------------|-------------|---------------|-----------|
| Simple | 10-50 | 100-500 ms | 100-200 KB |
| Mediana | 100-500 | 500-1500 ms | 200-500 KB |
| Compleja | 1000+ | 1-3 segundos | 500KB-2MB |

### 3.13 Systemd

```ini
# /etc/systemd/system/sy-opa-rules.service

[Unit]
Description=JSON Router OPA Rules Service (Go)
After=network.target json-router.service

[Service]
Type=simple
ExecStart=/usr/bin/sy-opa-rules
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

### 3.14 Funcionalidad Futura

> **Historial de versiones:** Capacidad de mantener N versiones anteriores en disco (`history/v41/`, `history/v42/`, etc.) con mensajes para consultar/restaurar versiones específicas (`GET_HISTORY`, `RESTORE_VERSION`). Por ahora solo se mantiene `current`, `staged` y `backup`.

---

## 4. SY.admin

[Contenido sin cambios mayores - ver documento original]

**Cambio relevante:** SY.admin NO tiene Regorus embebido. La validación de sintaxis la hace SY.opa.rules como parte del proceso de compilación. SY.admin espera la respuesta de SY.opa.rules para confirmar que el código es válido.

---

## 5. SY.orchestrator

[Contenido sin cambios - ver documento original]

---

## 6. Apéndice A: Estructura del paquete

```
json-router/
├── Cargo.toml
├── src/
│   ├── bin/
│   │   ├── json-router.rs       # Router principal (Rust)
│   │   ├── sy-admin.rs          # Gateway HTTP (Rust)
│   │   ├── sy-orchestrator.rs   # Orquestador (Rust)
│   │   ├── sy-config-routes.rs  # Rutas estáticas (Rust)
│   │   └── shm-watch.rs         # Herramienta de diagnóstico (Rust)
│   └── lib.rs
└── tests/

sy-opa-rules/                    # Proyecto separado en Go
├── go.mod
├── go.sum
├── main.go
├── compiler/
│   └── compiler.go              # OPA compile embebido
├── shm/
│   └── shm.go                   # Escritura de SHM
└── protocol/
    └── messages.go              # Mensajes JSON
```

---

## Apéndice B: Prioridades de implementación

| Prioridad | Nodo | Razón |
|-----------|------|-------|
| 1 | `SY.config.routes` | Necesario para rutas estáticas y VPNs |
| 2 | `SY.opa.rules` | Necesario para routing dinámico con WASM |
| 3 | `SY.orchestrator` | Necesario para gestión de nodos |
| 4 | `SY.admin` | Gateway HTTP para administración |
| 5 | `SY.time` | Útil pero no crítico inicialmente |
| 6 | `SY.log` | Puede usar logging estándar por ahora |

---

## Apéndice C: Resumen de Mensajes OPA

| Mensaje | Tipo | Origen | Destino | Propósito |
|---------|------|--------|---------|-----------|
| CONFIG_CHANGED (compile, auto_apply=false) | broadcast | SY.admin | todas las islas | Compilar, queda staged |
| CONFIG_CHANGED (compile, auto_apply=true) | broadcast | SY.admin | todas las islas | Compilar + aplicar |
| CONFIG_CHANGED (apply) | broadcast | SY.admin | todas las islas | Aplicar staged |
| CONFIG_CHANGED (rollback) | broadcast | SY.admin | todas las islas | Rollback |
| CONFIG_RESPONSE | unicast | SY.opa.rules | SY.admin | Resultado de operación |
| COMPILE_POLICY | unicast | SY.admin | SY.opa.rules@X | Compilar en isla X |
| APPLY_POLICY | unicast | SY.admin | SY.opa.rules@X | Aplicar en isla X |
| ROLLBACK_POLICY | unicast | SY.admin | SY.opa.rules@X | Rollback en isla X |
| GET_POLICY | unicast | cualquiera | SY.opa.rules@X | Obtener rego actual |
| GET_STATUS | unicast | cualquiera | SY.opa.rules@X | Estado del nodo |
| OPA_RELOAD | broadcast local | SY.opa.rules | routers locales | Recargar WASM |

**Nota sobre versionado:** SY.admin mantiene contador monotónico en `/var/lib/json-router/opa-version.txt`.
