# JSON Router - Nodos SY (System)

**Estado:** Draft v0.3
**Fecha:** 2025-01-18
**Documento relacionado:** JSON Router Especificación Técnica v1.2

---

## 1. Introducción

Los nodos SY (System) son componentes de infraestructura que proveen servicios esenciales al sistema de mensajería. A diferencia de los nodos AI, WF e IO que procesan lógica de negocio, los nodos SY mantienen la operación del sistema mismo.

### 1.1 Características comunes

- **Lenguaje:** Rust (mismo stack que el router)
- **Distribución:** Parte del paquete `json-router`
- **Privilegios:** Pueden acceder a shared memory directamente
- **Ciclo de vida:** Gestionados por systemd junto con los routers
- **Nomenclatura:** `SY.<servicio>.<instancia>`

### 1.2 Catálogo de nodos SY

| Nodo | Estado | Descripción |
|------|--------|-------------|
| `SY.config.routes` | Especificado | Configuración centralizada de rutas estáticas y VPNs |
| `SY.time` | Por especificar | Sincronización de tiempo en la red |
| `SY.monitor` | Por especificar | Métricas y health checks |
| `SY.log` | Por especificar | Colector centralizado de logs |
| `SY.admin` | Por especificar | Consola de administración |

---

## 2. SY.config.routes

Responsable de la configuración centralizada de rutas estáticas y VPNs.

### 2.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.config.routes` (uno por isla, sin instancia) |
| Región SHM | `/jsr-config-<island_id>` |
| Persistencia | `/etc/json-router/sy-config-routes.yaml` |
| Propagación | Broadcast (CONFIG_ANNOUNCE) |

**Restricciones:**
- Solo puede correr UNO por isla
- Si ya hay uno corriendo, el segundo debe salir con error
- Lee island_id del archivo de configuración (no por CLI)

### 2.2 Arquitectura

```
┌─────────────────────────────────────────────────────────────┐
│                      SY.config.routes                       │
│                                                              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ API Handler │  │ SHM Writer  │  │ Persistence │         │
│  │             │  │             │  │             │         │
│  │ - add_route │  │ - seqlock   │  │ - sy-config │         │
│  │ - del_route │  │ - heartbeat │  │   -routes.  │         │
│  │ - list_*    │  │ - version++ │  │   yaml      │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         │                │                │                 │
│         └────────────────┼────────────────┘                 │
│                          │                                  │
│  ┌───────────────────────┴───────────────────────┐         │
│  │              Broadcast Handler                 │         │
│  │                                                │         │
│  │  - Envía CONFIG_ANNOUNCE (broadcast)          │         │
│  │  - Recibe CONFIG_ANNOUNCE de otras islas      │         │
│  │  - Actualiza zona global en shm               │         │
│  └───────────────────────────────────────────────┘         │
└──────────────────────────────────────────────────────────────┘
                           │
                           ▼
              /jsr-config-<island_id>
              ┌─────────────────────┐
              │ LocalRoutes[]       │ ← De archivo local
              │ GlobalRoutes[]      │ ← De broadcasts recibidos
              └─────────────────────┘
```

**Separación de responsabilidades:**
- **Router:** Solo mueve mensajes (data plane). Lee config de shm, no la propaga.
- **SY.config.routes:** Escribe config local, hace broadcast, recibe broadcasts de otras islas.

### 2.3 Línea de comandos

```bash
# Arranque simple - todo por default
sy-config-routes

# Con directorio custom
sy-config-routes --config-dir /otro/path
```

| Parámetro | CLI | Env | Default | Obligatorio |
|-----------|-----|-----|---------|-------------|
| Config directory | `--config-dir` | `JSR_CONFIG_DIR` | `/etc/json-router` | No |
| Log level | `--log-level` | `JSR_LOG_LEVEL` | `info` | No |

**No hay parámetros obligatorios.** Todo se lee del archivo de configuración.

### 2.4 Directorios por Default

| Directorio | Default | Descripción |
|------------|---------|-------------|
| Config | `/etc/json-router/` | Archivos de configuración |
| State | `/var/lib/json-router/` | Estado persistente |
| Sockets | `/var/run/json-router/` | Unix sockets |
| SHM | `/dev/shm/` | Shared memory (automático) |

### 2.5 Archivos

```
/etc/json-router/
├── island.yaml              # Config de la isla (lee island_id de acá)
└── sy-config-routes.yaml    # Rutas estáticas locales (este nodo lo maneja)

/dev/shm/
└── jsr-config-<island>      # Región SHM (este nodo la crea/escribe)
```

**Convención de nombres:** `sy-<servicio>.yaml` para todos los nodos SY.

### 2.6 Flujo de arranque

```
sy-config-routes
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. Leer configuración                                       │
│    - Leer /etc/json-router/island.yaml → island_id         │
│    - Leer /etc/json-router/sy-config-routes.yaml → rutas   │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. Verificar instancia única                                │
│    - Intentar abrir /jsr-config-<island>                   │
│    - Si existe y owner_pid está vivo → EXIT "Ya corriendo" │
│    - Si existe y owner muerto → Reclamar región            │
│    - Si no existe → Crear región                           │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Inicializar shared memory                                │
│    - Escribir header (magic, version, owner_pid, etc.)     │
│    - Escribir rutas locales (de archivo)                   │
│    - Zona global vacía (se llena con broadcasts)           │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. Conectar al router local                                 │
│    - Crear socket en /var/run/json-router/                 │
│    - Enviar HELLO, registrarse como SY.config.routes       │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. Broadcast inicial                                        │
│    - Enviar CONFIG_ANNOUNCE con rutas locales              │
│    - Otras islas reciben y actualizan su zona global       │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. Loop principal                                           │
│    - Actualizar heartbeat en SHM cada 5s                   │
│    - Escuchar mensajes (API, CONFIG_ANNOUNCE de otros)     │
│    - Broadcast periódico de refresh (cada 60s)             │
└─────────────────────────────────────────────────────────────┘
```
┌─────────────────────────────────────────────────────────────┐
│ 2. Verificar región SHM existente                           │
│    - Si existe y owner vivo → ERROR (ya hay primary)        │
│    - Si existe y owner muerto → Reclamar                    │
│    - Si no existe → Crear                                   │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Cargar configuración                                     │
│    - Leer /etc/json-router/routes.yaml                     │
│    - Si no existe → Crear vacío                            │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. Escribir en región SHM                                   │
│    - Inicializar header (magic, version, owner, etc.)      │
│    - Escribir todas las rutas y VPNs                       │
│    - config_version = 1                                    │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. Conectar al router local                                 │
│    - Crear socket en node_socket_dir                       │
│    - Enviar HELLO, recibir asignación de UUID              │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. Loop principal                                           │
│    - Actualizar heartbeat en SHM cada 5s                   │
│    - Escuchar mensajes de API                              │
│    - Procesar requests, actualizar SHM, persistir          │
└─────────────────────────────────────────────────────────────┘
```

### 2.6 API de mensajes

El nodo recibe mensajes JSON via el router (como cualquier nodo).

#### 2.6.1 Listar rutas estáticas

**Request:**
```json
{
  "routing": {
    "src": "<uuid-cliente>",
    "dst": "<uuid-sy-config>",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "admin",
    "action": "list_routes"
  },
  "payload": {}
}
```

**Response:**
```json
{
  "routing": { "src": "<uuid-sy-config>", "dst": "<uuid-cliente>", ... },
  "meta": { "type": "admin", "action": "list_routes", "status": "ok" },
  "payload": {
    "config_version": 42,
    "routes": [
      {
        "prefix": "AI.soporte.*",
        "match_kind": "PREFIX",
        "action": "FORWARD",
        "next_hop_island": "produccion-us",
        "metric": 10,
        "priority": 100,
        "installed_at": 1737110400000
      }
    ]
  }
}
```

#### 2.6.2 Agregar ruta estática

**Request:**
```json
{
  "meta": { "type": "admin", "action": "add_route" },
  "payload": {
    "prefix": "AI.ventas.*",
    "match_kind": "PREFIX",
    "action": "FORWARD",
    "next_hop_island": "produccion-us",
    "metric": 10,
    "priority": 100
  }
}
```

**Response (éxito):**
```json
{
  "meta": { "type": "admin", "action": "add_route", "status": "ok" },
  "payload": {
    "config_version": 43,
    "route": { ... }
  }
}
```

**Response (error):**
```json
{
  "meta": { "type": "admin", "action": "add_route", "status": "error" },
  "payload": {
    "error": "DUPLICATE_PREFIX",
    "message": "Route with prefix 'AI.ventas.*' already exists"
  }
}
```

#### 2.6.3 Eliminar ruta estática

**Request:**
```json
{
  "meta": { "type": "admin", "action": "delete_route" },
  "payload": {
    "prefix": "AI.ventas.*"
  }
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "delete_route", "status": "ok" },
  "payload": {
    "config_version": 44
  }
}
```

#### 2.6.4 Listar VPNs

**Request:**
```json
{
  "meta": { "type": "admin", "action": "list_vpns" },
  "payload": {}
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "list_vpns", "status": "ok" },
  "payload": {
    "vpns": [
      {
        "vpn_id": 1,
        "vpn_name": "vpn-staging",
        "remote_island": "staging",
        "endpoints": ["10.0.1.100:9000", "10.0.1.101:9000"],
        "created_at": 1737110400000
      }
    ]
  }
}
```

#### 2.6.5 Agregar VPN

**Request:**
```json
{
  "meta": { "type": "admin", "action": "add_vpn" },
  "payload": {
    "vpn_name": "vpn-staging",
    "remote_island": "staging",
    "endpoints": ["10.0.1.100:9000", "10.0.1.101:9000"]
  }
}
```

#### 2.6.6 Eliminar VPN

**Request:**
```json
{
  "meta": { "type": "admin", "action": "delete_vpn" },
  "payload": {
    "vpn_id": 1
  }
}
```

### 2.7 Códigos de error

| Error | Descripción |
|-------|-------------|
| `DUPLICATE_PREFIX` | Ya existe una ruta con ese prefix |
| `PREFIX_NOT_FOUND` | No existe ruta con ese prefix |
| `INVALID_PREFIX` | Formato de prefix inválido |
| `INVALID_ACTION` | Acción no reconocida |
| `INVALID_MATCH_KIND` | match_kind debe ser EXACT, PREFIX o GLOB |
| `VPN_NOT_FOUND` | No existe VPN con ese ID |
| `DUPLICATE_VPN_NAME` | Ya existe VPN con ese nombre |
| `MAX_ROUTES_EXCEEDED` | Se alcanzó el límite de 256 rutas |
| `MAX_VPNS_EXCEEDED` | Se alcanzó el límite de 64 VPNs |
| `PERSISTENCE_ERROR` | Error al guardar en disco |

### 2.8 Persistencia: sy-config-routes.yaml

```yaml
# /etc/json-router/sy-config-routes.yaml
# Solo rutas LOCALES de esta isla
# Rutas de otras islas NO se persisten (llegan por broadcast)

version: 1
updated_at: "2025-01-18T10:30:00Z"

routes:
  # Rutas capa 2 (se propagan por broadcast)
  - prefix: "AI.soporte.*"
    match_kind: PREFIX
    action: FORWARD
    metric: 10
    priority: 100
    
  - prefix: "AI.ventas.interno"
    match_kind: EXACT
    action: DROP
    
  # Rutas capa 1 (NO se propagan, solo locales)
  - prefix: "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
    match_kind: EXACT
    action: FORWARD
    metric: 5

vpns:
  - vpn_id: 1
    vpn_name: vpn-staging
    remote_island: staging
    endpoints:
      - "10.0.1.100:9000"
      - "10.0.1.101:9000"
```

**Nota:** Este archivo solo contiene rutas de ESTA isla. Las rutas de otras islas se reciben por broadcast y se guardan solo en shm (no se persisten).

### 2.9 Systemd

```ini
# /etc/systemd/system/sy-config-routes.service

[Unit]
Description=JSON Router Config Service
After=network.target json-router.service

[Service]
Type=simple
ExecStart=/usr/bin/sy-config-routes
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
# Habilitar
systemctl enable sy-config-routes

# Iniciar
systemctl start sy-config-routes

# Ver logs
journalctl -u sy-config-routes -f
```

**Nota:** No hay template `@` porque solo puede haber uno por isla/máquina.

### 2.10 Propagación entre Islas (Broadcast)

La propagación de configuración entre islas usa **broadcast**, no mensajes punto a punto. El router maneja el broadcast de forma transparente.

#### 2.10.1 Modelo Simplificado

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              BROADCAST                                  │
│                                                                         │
│   SY.config.A ─────► CONFIG_ANNOUNCE ─────► todas las islas            │
│   SY.config.B ─────► CONFIG_ANNOUNCE ─────► todas las islas            │
│   SY.config.C ─────► CONFIG_ANNOUNCE ─────► todas las islas            │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘

Cada SY.config.routes:
  - Archivo local: solo rutas de SU isla (persiste)
  - SHM local: rutas propias + zona global (recibida por broadcast)
  - NO persiste rutas de otras islas (se reconstruyen al arrancar)
```

**Principios:**
- El archivo `sy-config-routes.yaml` solo tiene rutas locales
- La zona global en shm se llena con broadcasts recibidos
- Si reinicia, espera broadcasts de otras islas para reconstruir zona global

#### 2.10.2 Mensaje CONFIG_ANNOUNCE

```json
{
  "routing": {
    "src": "<uuid-sy-config>",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_ANNOUNCE",
    "target": "SY.config.*"
  },
  "payload": {
    "island": "produccion",
    "version": 42,
    "timestamp": "2025-01-18T10:00:00Z",
    "routes": [
      {
        "prefix": "AI.soporte.*",
        "match_kind": "PREFIX",
        "action": "FORWARD",
        "metric": 10,
        "priority": 100
      }
    ],
    "vpns": [ ... ]
  }
}
```

**Campos importantes:**
- `dst: "broadcast"` - El router lo envía a todos
- `meta.target: "SY.config.*"` - Filtro: solo nodos SY.config.* lo procesan
- `payload.island` - Quién origina esta configuración
- `payload.version` - Para detectar duplicados/orden

#### 2.10.3 Cuándo hacer Broadcast

| Evento | Acción |
|--------|--------|
| Arranque | Broadcast inmediato de rutas locales |
| Cambio de config local | Broadcast inmediato |
| Timer periódico (60s) | Broadcast de refresh |

El refresh periódico asegura que islas que arrancaron después reciban la config.

#### 2.10.4 Al Recibir CONFIG_ANNOUNCE

```
1. ¿Es de mi propia isla? → Ignorar
2. ¿version > la que tengo de esa isla? 
   → Sí: Actualizar zona global en shm
   → No: Ignorar (ya tengo igual o más nuevo)
```

**No se persiste.** Solo se guarda en shm. Si reinicia, espera nuevos broadcasts.

#### 2.10.5 Estructura de la Shared Memory

```
/jsr-config-<island>
├── Header
│   ├── magic, version
│   ├── owner_pid, heartbeat
│   └── local_version, global_versions[]
│
├── LocalRoutes[]           ← De sy-config-routes.yaml
│   └── Rutas de ESTA isla
│
└── GlobalRoutes[]          ← De CONFIG_ANNOUNCE recibidos
    ├── [isla-a]: version, timestamp, routes[]
    ├── [isla-b]: version, timestamp, routes[]
    └── ...
```

#### 2.10.6 Rutas que NO se propagan

Solo se propagan rutas **capa 2** (por nombre). Rutas capa 1 (por UUID) son locales.

```yaml
# sy-config-routes.yaml

routes:
  # Esta se propaga (capa 2, nombre)
  - prefix: "AI.ventas.*"
    match_kind: PREFIX
    action: FORWARD
    
  # Esta NO se propaga (capa 1, UUID)
  - prefix: "a1b2c3d4-e5f6-..."
    match_kind: EXACT
    action: DROP
```

**Detección automática:** Si prefix es un UUID válido → no propagar.

#### 2.10.7 Consistencia Eventual

- Sin coordinación distribuida (no hay Paxos/Raft)
- Puede haber ventanas donde islas tienen versiones distintas
- El refresh periódico converge eventualmente
- Para operaciones críticas: verificar version antes de actuar

---

## 3. SY.time

Sincronización de tiempo en la red.

### 3.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.time.<instancia>` |
| Función | Broadcast periódico de tiempo UTC |
| Dependencias | Ninguna (puede usar NTP del sistema) |

### 3.2 Funcionalidad

- Broadcast periódico (cada 1s o configurable) con timestamp UTC
- Los nodos pueden usar este tiempo para sincronizar operaciones
- No requiere región SHM propia (usa mensajes)

### 3.3 Mensaje TIME_SYNC

```json
{
  "routing": {
    "src": "<uuid-sy-time>",
    "dst": null,
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "TIME_SYNC"
  },
  "payload": {
    "timestamp_utc": "2025-01-17T10:30:00.123Z",
    "epoch_ms": 1737110400123,
    "seq": 12345
  }
}
```

**dst: null** indica broadcast local. TTL=1 evita propagación WAN.

### 3.4 Por especificar

- Mecanismo de broadcast (OPA policy, lista de suscriptores, o multicast address)
- Precisión requerida
- Manejo de drift

---

## 4. SY.monitor

Métricas y health checks.

### 4.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.monitor.<instancia>` |
| Función | Recolectar métricas, exponer health checks |
| Dependencias | Lee SHM de routers y config |

### 4.2 Funcionalidad

- Lee todas las regiones SHM para reportar estado
- Expone endpoint HTTP para health checks (Kubernetes, load balancers)
- Puede enviar métricas a sistemas externos (Prometheus, InfluxDB)

### 4.3 Métricas a recolectar

| Métrica | Fuente | Descripción |
|---------|--------|-------------|
| `router_nodes_total` | SHM router | Nodos conectados por router |
| `router_routes_total` | SHM router | Rutas por router |
| `router_heartbeat_age_ms` | SHM router | Edad del heartbeat |
| `config_routes_total` | SHM config | Rutas estáticas configuradas |
| `config_vpns_total` | SHM config | VPNs configuradas |
| `config_version` | SHM config | Versión de la config |

### 4.4 Health check endpoint

```
GET /health
```

```json
{
  "status": "healthy",
  "timestamp": "2025-01-17T10:30:00Z",
  "components": {
    "routers": {
      "RT.produccion.primary": "alive",
      "RT.produccion.secondary": "alive"
    },
    "config": {
      "SY.config.routes.primary": "alive"
    }
  }
}
```

### 4.5 Por especificar

- Puerto HTTP para health checks
- Formato de métricas (Prometheus exposition format)
- Alertas y thresholds

---

## 5. SY.log

Colector centralizado de logs.

### 5.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.log.<instancia>` |
| Función | Recibir y consolidar logs de todos los nodos |
| Salida | Archivo, stdout, o sistema externo |

### 5.2 Por especificar

- Formato de mensajes de log
- Rotación y retención
- Integración con sistemas externos (Loki, Elasticsearch)

---

## 6. SY.admin

Consola de administración.

### 6.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.admin.<instancia>` |
| Función | Interfaz para administradores |
| Interfaz | CLI interactivo o web UI |

### 6.2 Por especificar

- Comandos disponibles
- Autenticación/autorización
- Web UI vs CLI

---

## Apéndice A: Estructura del paquete

```
json-router/
├── Cargo.toml
├── src/
│   ├── bin/
│   │   ├── json-router.rs       # Router principal
│   │   ├── sy-config-routes.rs  # Nodo SY.config.routes
│   │   ├── sy-time.rs           # Nodo SY.time
│   │   ├── sy-monitor.rs        # Nodo SY.monitor
│   │   └── shm-watch.rs         # Herramienta de diagnóstico
│   ├── lib.rs                   # Biblioteca compartida
│   ├── shm/                     # Shared memory
│   │   ├── mod.rs
│   │   ├── router_region.rs     # Región de router
│   │   ├── config_region.rs     # Región de config
│   │   └── seqlock.rs
│   ├── protocol/                # Mensajes JSON
│   │   ├── mod.rs
│   │   ├── routing.rs
│   │   └── admin.rs
│   └── ...
└── tests/
```

---

## Apéndice B: Prioridades de implementación

| Prioridad | Nodo | Razón |
|-----------|------|-------|
| 1 | `SY.config.routes` | Necesario para rutas estáticas y VPNs |
| 2 | `SY.monitor` | Necesario para operación en producción |
| 3 | `SY.time` | Útil pero no crítico inicialmente |
| 4 | `SY.log` | Puede usar logging estándar por ahora |
| 5 | `SY.admin` | Puede administrarse con herramientas externas |