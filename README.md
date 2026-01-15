# json-router

Router de mensajes JSON con modelo mental de router de hardware:

- **Puertos** = Unix domain sockets (`SOCK_SEQPACKET`)
- **FIB** = Tabla de forwarding en shared memory
- **Policy** = OPA compilado a WASM, evaluación local
- **Detección de link** = Automática por estado del socket

> Spec completa: `docs/json-router-spec.md`

---

## Estado

**v0.1** — Implementación inicial en Rust.

---

## Arquitectura

```
┌─────────────────────────────────────────────────────────────┐
│                      Shared Memory                          │
│  ┌─────────────────┐  ┌─────────────────┐                   │
│  │  Tabla Ruteo    │  │  Registro Nodos │                   │
│  └─────────────────┘  └─────────────────┘                   │
└─────────────────────────────────────────────────────────────┘
        ▲                       ▲
        │                       │
┌───────┴───────────────────────┴───────┐
│              Router (Rust)            │
│                                       │
│  ┌─────────┐  ┌─────────┐  ┌───────┐  │
│  │ Sockets │  │   OPA   │  │ Timers│  │
│  │SEQPACKET│  │  WASM   │  │       │  │
│  └─────────┘  └─────────┘  └───────┘  │
└───────────────────────────────────────┘
        │
        ▼
┌───────────────────────────────────────┐
│            Unix Sockets               │
│  /var/run/mesh/nodes/*.sock           │
└───────────────────────────────────────┘
        │
        ▼
┌───────┴───────┬───────────┬───────────┐
│   Nodo AI     │  Nodo IO  │  Nodo WF  │
└───────────────┴───────────┴───────────┘
```

---

## Features v0.1

- [x] Shared memory para tabla de ruteo y registro de nodos
- [x] Unix domain sockets `SOCK_SEQPACKET`
- [x] Detección automática de link up/down
- [x] Mensajes JSON con header estándar
- [x] Forwarding loop asíncrono (tokio)
- [x] OPA policy evaluation (wasmtime)
- [ ] Uplink WAN (TCP entre routers)
- [ ] Blob store para payloads grandes

---

## Estructura del Proyecto

```
json-router/
├── Cargo.toml
├── README.md
├── docs/
│   └── json-router-spec.md
├── src/
│   ├── main.rs                 # Entry point, tokio runtime
│   ├── config.rs               # Configuración y timers
│   ├── shm/
│   │   ├── mod.rs              # Shared memory (memmap2 + raw-sync)
│   │   ├── routes.rs           # Tabla de ruteo
│   │   ├── nodes.rs            # Registro de nodos
│   │   └── routers.rs          # Registro de routers
│   ├── socket/
│   │   ├── mod.rs              # SEQPACKET handling (nix + AsyncFd)
│   │   ├── listener.rs         # Detectar nodos nuevos (inotify)
│   │   └── connection.rs       # Conexión individual
│   ├── opa/
│   │   └── mod.rs              # wasmtime + policy evaluation
│   ├── protocol/
│   │   ├── mod.rs              # JSON message types
│   │   ├── routing.rs          # Header de routing
│   │   ├── meta.rs             # Metadata para OPA
│   │   └── system.rs           # System messages (HELLO, LSA, etc)
│   └── router/
│       ├── mod.rs              # Loop principal
│       └── forward.rs          # Lógica de forwarding
├── node-lib/
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs              # Librería común para nodos
└── examples/
    ├── node-ai/
    ├── node-io/
    └── node-wf/
```

---

## Requisitos

- Rust 1.75+
- Linux (para Unix domain sockets y shared memory POSIX)

---

## Dependencias Clave

| Crate | Propósito |
|-------|-----------|
| `tokio` | Runtime asíncrono |
| `memmap2` | Memory mapping para shared memory |
| `raw-sync` | Locks inter-proceso (RwLock en shm) |
| `nix` | Syscalls POSIX (sockets, inotify) |
| `wasmtime` | Runtime WASM para OPA policies |
| `serde` / `serde_json` | Serialización JSON |
| `uuid` | Generación de identificadores |
| `tracing` | Logging estructurado |

---

## Quick Start

### 1) Clonar y compilar

```bash
git clone <repo>
cd json-router
cargo build --release
```

### 2) Crear directorio de sockets

```bash
sudo mkdir -p /var/run/mesh/nodes
sudo chown $USER:$USER /var/run/mesh/nodes
```

### 3) Ejecutar router

```bash
./target/release/json-router --config config.toml
```

### 4) Ejecutar nodo de ejemplo

```bash
./target/release/examples/node-io
```

---

## Configuración

```toml
# config.toml

[router]
id = "router-01"
socket_dir = "/var/run/mesh/nodes"
shm_name = "/json-router-shm"
shm_size = 10485760  # 10MB

[timers]
ttl_default = 16
message_timeout_ms = 30000
hello_interval_ms = 10000
dead_interval_ms = 40000
route_refresh_ms = 300000
connect_backoff_max_ms = 100
time_sync_interval_ms = 60000

[opa]
policy_path = "/etc/json-router/policy.wasm"

[blob_store]
root = "/var/lib/json-router/blobs"
max_inline_bytes = 65536
retention_days = 2
```

---

## Variables de Entorno

| Variable | Default | Descripción |
|----------|---------|-------------|
| `ROUTER_ID` | (requerido) | Identificador único del router |
| `SOCKET_DIR` | `/var/run/mesh/nodes` | Directorio de sockets de nodos |
| `SHM_NAME` | `/json-router-shm` | Nombre de la región de shared memory |
| `CONFIG_PATH` | `./config.toml` | Path al archivo de configuración |
| `LOG_LEVEL` | `info` | Nivel de logging (trace, debug, info, warn, error) |

---

## Tipos de Nodo

| Tipo | Prefijo | Descripción | Nomenclatura |
|------|---------|-------------|--------------|
| AI | `AI.*` | Agentes LLM | `AI.<área>.<cargo>.<nivel>.<especialización>` |
| IO | `IO.*` | Adaptadores de medio | `IO.<medio>.<identificador>` |
| WF | `WF.*` | Workflows estáticos | `WF.<verbo>.<objeto>.<variante>` |
| SY | `SY.*` | Servicios de sistema | `SY.<servicio>.<instancia>` |

---

## Mensajes de Sistema

| Mensaje | Propósito |
|---------|-----------|
| `ANNOUNCE` | Nodo anuncia existencia |
| `WITHDRAW` | Nodo anuncia shutdown |
| `ECHO` / `ECHO_REPLY` | Ping/Pong |
| `UNREACHABLE` | Destino no existe |
| `TTL_EXCEEDED` | TTL llegó a 0 |
| `SOURCE_QUENCH` | Backpressure |
| `HELLO` | Router anuncia existencia |
| `LSA` | Link State Advertisement |
| `TIME_SYNC` | Broadcast de tiempo UTC |

---

## Desarrollo

### Ejecutar tests

```bash
cargo test
```

### Ejecutar con logging detallado

```bash
RUST_LOG=debug cargo run
```

### Formato y linting

```bash
cargo fmt
cargo clippy
```

---

## Roadmap

### v0.1 (actual)
- Shared memory básica
- Sockets SEQPACKET
- Forwarding loop
- OPA integration

### v0.2
- Uplink WAN (TCP entre routers)
- Blob store
- Múltiples routers coordinados

### v0.3
- Balanceo de carga entre nodos del mismo rol
- Métricas y observabilidad
- Admin API

---

## Licencia

MIT

---

## Referencias

- [Spec completa](docs/json-router-spec.md)
- [OSPF Timers](https://datatracker.ietf.org/doc/html/rfc2328)
- [OPA WASM](https://www.openpolicyagent.org/docs/latest/wasm/)
