# JSON Router - 07 Operaciones

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Ops/SRE, desarrolladores de deployment

---

## 1. Estructura de Directorios

```
/etc/json-router/                      # Configuración
├── island.yaml                        # Identidad de isla (OBLIGATORIO)
├── sy-config-routes.yaml              # Config de rutas y VPN
└── routers/                           # Config por router
    ├── RT.primary@produccion/
    │   └── config.yaml
    └── RT.gateway@produccion/
        └── config.yaml

/var/lib/json-router/                  # Estado persistente
├── nodes/                             # UUIDs de nodos
│   └── AI.soporte.l1.uuid
└── state/                             # Estado de runtime
    └── RT.primary@produccion/
        └── identity.yaml

/var/run/json-router/                  # Runtime (volátil)
├── router.sock                        # Socket del router
└── nodes/                             # Sockets de nodos
    └── <uuid>.sock

/dev/shm/                              # Shared memory
├── jsr-<router-uuid>                  # Región de cada router
├── jsr-config-<island>                # Región de config
└── jsr-lsa-<island>                   # Región de LSA
```

---

## 2. Archivo: island.yaml (OBLIGATORIO)

Define la identidad de la isla. **Todos los procesos lo leen al arrancar.**

```yaml
# /etc/json-router/island.yaml

island_id: "produccion"
```

**Sin este archivo, ningún proceso arranca.**

### 2.1 Campos

| Campo | Obligatorio | Descripción |
|-------|-------------|-------------|
| `island_id` | Sí | Identificador único de la isla |

---

## 3. Archivo: config.yaml (Router)

Configuración del router. Autosuficiente.

```yaml
# /etc/json-router/routers/RT.primary@produccion/config.yaml

router:
  name: RT.primary                     # Sin @isla
  island_id: produccion
  is_gateway: false

paths:
  state_dir: /var/lib/json-router/state
  node_socket_dir: /var/run/json-router
  shm_prefix: /jsr-

timers:
  hello_interval_ms: 10000
  dead_interval_ms: 40000
  heartbeat_interval_ms: 5000
  heartbeat_stale_ms: 30000
```

### 3.1 Config de Gateway

```yaml
# /etc/json-router/routers/RT.gateway@produccion/config.yaml

router:
  name: RT.gateway
  island_id: produccion
  is_gateway: true                     # <-- Diferencia clave

paths:
  state_dir: /var/lib/json-router/state
  node_socket_dir: /var/run/json-router
  shm_prefix: /jsr-

timers:
  hello_interval_ms: 10000
  dead_interval_ms: 40000
  heartbeat_interval_ms: 5000
  heartbeat_stale_ms: 30000

wan:
  listen: "0.0.0.0:9000"
  uplinks:
    - address: "10.0.1.100:9000"       # staging
    - address: "10.0.2.100:9000"       # desarrollo
  authorized_islands:
    - staging
    - desarrollo
```

---

## 4. Archivo: identity.yaml (Auto-generado)

Estado de hardware/runtime. **Se genera en primer arranque.**

```yaml
# /var/lib/json-router/state/RT.primary@produccion/identity.yaml

layer1:
  uuid: a1b2c3d4-e5f6-7890-abcd-ef1234567890

layer2:
  name: RT.primary@produccion

shm:
  name: /jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890

created_at: 2025-01-17T10:30:00Z
```

**Nunca se edita manualmente.**

---

## 5. Flujo de Arranque

### 5.1 Router Normal

```
1. Leer /etc/json-router/island.yaml
   → Si no existe: EXIT con error

2. Leer config.yaml del router
   → Si no existe: usar defaults

3. Leer o generar identity.yaml
   → Si no existe: generar UUID, crear archivo

4. Crear/reclamar región SHM jsr-<uuid>
   → Verificar si es stale
   → Inicializar header

5. Leer jsr-config-<island> (si existe)
   → Cargar rutas estáticas y VPN

6. Leer jsr-lsa-<island> (si existe)
   → Cargar topología remota

7. Iniciar loop principal:
   - Aceptar conexiones de nodos
   - Procesar mensajes
   - Actualizar heartbeat
```

### 5.2 Gateway

```
1-4. (Igual que router normal)

5. Crear región SHM jsr-lsa-<island>
   → Solo el gateway la crea

6. Leer jsr-config-<island>

7. Establecer conexiones WAN:
   - listen en wan.listen
   - connect a cada wan.uplinks[]

8. Iniciar loop principal:
   - (Igual que router normal)
   - Recibir/enviar LSA
   - Escribir en jsr-lsa
```

### 5.3 SY.config.routes

```
1. Leer /etc/json-router/island.yaml

2. Leer sy-config-routes.yaml
   → Si no existe: crear vacío

3. Crear/reclamar región jsr-config-<island>

4. Escribir rutas y VPNs en la región

5. Conectar al router como nodo normal (HELLO)

6. Iniciar loop principal:
   - Escuchar requests de API
   - Actualizar config
   - Escribir en SHM
   - Persistir en YAML
```

---

## 6. Systemd

### 6.1 Router Service (template)

```ini
# /etc/systemd/system/json-router@.service
[Unit]
Description=JSON Router %i
After=network.target

[Service]
Type=simple
Environment=JSR_ROUTER_NAME=%i
Environment=JSR_LOG_LEVEL=info
ExecStart=/usr/bin/json-router
Restart=always
RestartSec=5

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/json-router /var/run/json-router /dev/shm

[Install]
WantedBy=multi-user.target
```

**Uso:**

```bash
systemctl enable json-router@RT.primary
systemctl start json-router@RT.primary
```

### 6.2 SY.config.routes Service

```ini
# /etc/systemd/system/sy-config-routes.service
[Unit]
Description=JSON Router Config Service
After=network.target json-router@RT.primary.service

[Service]
Type=simple
ExecStart=/usr/bin/sy-config-routes
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

### 6.3 Orden de Arranque

```
1. json-router@RT.gateway (si existe)
2. json-router@RT.primary
3. json-router@RT.secondary (si existe)
4. sy-config-routes
5. Otros nodos SY.*
6. Nodos de aplicación (AI, WF, IO)
```

---

## 7. Variables de Entorno

| Variable | Propósito | Default |
|----------|-----------|---------|
| `JSR_ROUTER_NAME` | Nombre del router | Requerido |
| `JSR_LOG_LEVEL` | Nivel de log | `info` |

Rutas fijas: `/etc/json-router`, `/var/lib/json-router/state`, `/var/run/json-router`.

---

## 8. Deployment: Ejemplo Completo

### 8.1 Preparar Isla

```bash
# Crear directorios
mkdir -p /etc/json-router/routers
mkdir -p /var/lib/json-router/state/nodes
mkdir -p /var/run/json-router

# Crear island.yaml
cat > /etc/json-router/island.yaml << 'EOF'
island_id: "produccion"
EOF
```

### 8.2 Configurar Gateway

```bash
mkdir -p /etc/json-router/routers/RT.gateway@produccion

cat > /etc/json-router/routers/RT.gateway@produccion/config.yaml << 'EOF'
router:
  name: RT.gateway
  island_id: produccion
  is_gateway: true

paths:
  state_dir: /var/lib/json-router/state
  node_socket_dir: /var/run/json-router
  shm_prefix: /jsr-

timers:
  hello_interval_ms: 10000
  dead_interval_ms: 40000
  heartbeat_interval_ms: 5000
  heartbeat_stale_ms: 30000

wan:
  listen: "0.0.0.0:9000"
  uplinks:
    - address: "staging.internal:9000"
  authorized_islands:
    - staging
EOF
```

### 8.3 Configurar Router Normal

```bash
mkdir -p /etc/json-router/routers/RT.primary@produccion

cat > /etc/json-router/routers/RT.primary@produccion/config.yaml << 'EOF'
router:
  name: RT.primary
  island_id: produccion
  is_gateway: false

paths:
  state_dir: /var/lib/json-router/state
  node_socket_dir: /var/run/json-router
  shm_prefix: /jsr-

timers:
  hello_interval_ms: 10000
  dead_interval_ms: 40000
  heartbeat_interval_ms: 5000
  heartbeat_stale_ms: 30000
EOF
```

### 8.4 Configurar SY.config.routes

```bash
cat > /etc/json-router/sy-config-routes.yaml << 'EOF'
version: 1
routes: []
vpns: []
EOF
```

### 8.5 Arrancar Servicios

```bash
systemctl start json-router@RT.gateway
systemctl start json-router@RT.primary
systemctl start sy-config-routes
```

---

## 9. Monitoreo

### 9.1 Health Check

```bash
# Verificar procesos
systemctl status json-router@RT.primary

# Verificar SHM
ls -la /dev/shm/jsr-*

# Verificar sockets
ls -la /var/run/json-router/
```

### 9.2 Logs

```bash
# Logs del router
journalctl -u json-router@RT.primary -f

# Filtrar por nivel
journalctl -u json-router@RT.primary -p err
```

---

## 10. Troubleshooting

### 10.1 Router no arranca

```bash
# Verificar island.yaml
cat /etc/json-router/island.yaml

# Verificar config
cat /etc/json-router/routers/RT.primary@produccion/config.yaml

# Ver logs
JSR_LOG_LEVEL=debug json-router
```

### 10.2 SHM stale

```bash
# Ver regiones
ls -la /dev/shm/jsr-*

# Limpiar manualmente (si el proceso no existe)
rm /dev/shm/jsr-<uuid>
```

### 10.3 Nodos no conectan

```bash
# Verificar socket
ls -la /var/run/json-router/router.sock

# Verificar permisos
stat /var/run/json-router/
```

### 10.4 Inter-isla no funciona

```bash
# Verificar que hay gateway
grep is_gateway /etc/json-router/routers/*/config.yaml

# Verificar conexiones WAN
netstat -an | grep 9000

# Verificar LSA region
ls -la /dev/shm/jsr-lsa-*
```

---

## 11. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura | `01-arquitectura.md` |
| Conectividad WAN | `05-conectividad.md` |
| Regiones config/LSA | `06-regiones.md` |
