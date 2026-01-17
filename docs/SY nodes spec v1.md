# JSON Router - Nodos SY (System)

**Estado:** Draft v0.1
**Fecha:** 2025-01-17
**Documento relacionado:** JSON Router Especificación Técnica v0.9

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
| Nombre | `SY.config.routes.<instancia>` |
| Región SHM | `/jsr-config-<island_id>` |
| Persistencia | `/etc/json-router/routes.yaml` |
| HA | Primary/Backup con failover automático |
| API | Socket Unix (mismo protocolo que nodos) |

### 2.2 Arquitectura

```
┌─────────────────────────────────────────────────────────────┐
│                   SY.config.routes.primary                  │
│                                                              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ API Handler │  │ SHM Writer  │  │ Persistence │         │
│  │             │  │             │  │             │         │
│  │ - add_route │  │ - seqlock   │  │ - routes.   │         │
│  │ - del_route │  │ - heartbeat │  │   yaml      │         │
│  │ - list_*    │  │ - config_   │  │ - load/save │         │
│  │ - add_vpn   │  │   version++ │  │             │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         │                │                │                 │
│         └────────────────┼────────────────┘                 │
│                          │                                  │
└──────────────────────────┼──────────────────────────────────┘
                           │
                           ▼
              /jsr-config-<island_id>
```

### 2.3 Línea de comandos

```bash
sy-config-routes --name SY.config.routes.primary \
                 --config /etc/json-router \
                 --island produccion \
                 [--log-level info]
```

| Parámetro | CLI | Env | Default | Obligatorio |
|-----------|-----|-----|---------|-------------|
| Nombre del nodo | `--name` | `SY_CONFIG_NAME` | - | Sí |
| Config directory | `--config` | `SY_CONFIG_DIR` | `/etc/json-router` | No |
| Island ID | `--island` | `SY_CONFIG_ISLAND` | - | Sí |
| Log level | `--log-level` | `SY_LOG_LEVEL` | `info` | No |

### 2.4 Archivos

```
/etc/json-router/
├── island.yaml              # Lee island_id si no se pasa por CLI
└── routes.yaml              # Rutas estáticas y VPNs (este nodo lo maneja)

/dev/shm/
└── jsr-config-<island>      # Región SHM (este nodo la crea/escribe)
```

### 2.5 Flujo de arranque

```
sy-config-routes --name SY.config.routes.primary --island produccion
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. Validar parámetros                                       │
│    - --name debe empezar con SY.config.routes               │
│    - --island obligatorio                                   │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
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

### 2.8 Persistencia: routes.yaml

```yaml
# /etc/json-router/routes.yaml
# Gestionado por SY.config.routes - editar con cuidado

version: 1
updated_at: "2025-01-17T10:30:00Z"

static_routes:
  - prefix: "AI.soporte.*"
    match_kind: PREFIX
    action: FORWARD
    next_hop_island: produccion-us
    metric: 10
    priority: 100
    
  - prefix: "AI.ventas.interno"
    match_kind: EXACT
    action: DROP
    
  - prefix: "WF.facturar.*"
    match_kind: PREFIX
    action: VPN
    vpn_id: 1
    metric: 20
    priority: 200

vpns:
  - vpn_id: 1
    vpn_name: vpn-staging
    remote_island: staging
    endpoints:
      - "10.0.1.100:9000"
      - "10.0.1.101:9000"
```

### 2.9 Alta disponibilidad

```
┌─────────────────────────┐      ┌─────────────────────────┐
│ SY.config.routes.primary│      │ SY.config.routes.backup │
│                         │      │                         │
│  - Escribe en SHM       │      │  - Monitorea heartbeat  │
│  - Actualiza heartbeat  │      │  - Lee routes.yaml      │
│  - Procesa API          │      │  - Standby              │
└───────────┬─────────────┘      └───────────┬─────────────┘
            │                                │
            │ escribe                        │ monitorea
            ▼                                ▼
      /jsr-config-<island>            (misma región)
```

**Flujo de failover:**

1. Backup monitorea heartbeat del primary cada 5s
2. Si heartbeat > 30s (stale):
   - Backup intenta `shm_open()` y verificar owner
   - Si owner PID muerto → Backup reclama región
   - Backup se convierte en primary
   - Backup carga `routes.yaml` y reescribe región
3. Si el primary original vuelve:
   - Detecta que otro proceso es owner → Se convierte en backup

**Nota:** Ambos nodos leen el mismo `routes.yaml`. Es la fuente de verdad.

### 2.10 Systemd

```ini
# /etc/systemd/system/sy-config-routes@.service

[Unit]
Description=JSON Router Config Service (%i)
After=network.target
Wants=json-router@RT.%i.primary.service

[Service]
Type=simple
ExecStart=/usr/bin/sy-config-routes \
    --name SY.config.routes.%i \
    --island %i \
    --config /etc/json-router
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
# Habilitar
systemctl enable sy-config-routes@produccion

# Iniciar
systemctl start sy-config-routes@produccion

# Ver logs
journalctl -u sy-config-routes@produccion -f
```

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