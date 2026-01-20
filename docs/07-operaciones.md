# JSON Router - 07 Operaciones

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Ops/SRE, desarrolladores de deployment

---

## 1. Principio de Diseño

**El router arranca con su propio config, no depende de island.yaml.**

Una vez que un router tiene su `config.yaml`, es autosuficiente. El `island.yaml` solo se usa como template para crear el config de un router nuevo.

```
Primera vez:
  island.yaml → genera → config.yaml
  Luego genera → identity.yaml

Siguientes veces:
  config.yaml + identity.yaml (island.yaml no se toca)
```

---

## 2. Estructura de Directorios

```
/etc/json-router/                      # Configuración
├── island.yaml                        # Identidad de isla + template
├── sy-config-routes.yaml              # Config de SY.config.routes
└── routers/                           # Config por router
    ├── RT.primary@produccion/
    │   └── config.yaml
    └── RT.secondary@produccion/
        └── config.yaml

/var/lib/json-router/                  # Estado persistente
├── nodes/                             # UUIDs de nodos
│   └── AI.soporte.l1@produccion.uuid
└── state/                             # Estado de runtime
    └── RT.primary@produccion/
        └── identity.yaml

/var/run/json-router/                  # Runtime (volátil)
├── router.sock                        # Socket del router
└── nodes/                             # Sockets de nodos
    └── <uuid>.sock

/dev/shm/                              # Shared memory
├── jsr-<router-uuid>                  # Región de cada router
└── jsr-config-<island>                # Región de config
```

---

## 3. Archivo: island.yaml

Identidad de la isla. **Todos los procesos de la isla lo leen.**

```yaml
# /etc/json-router/island.yaml

island_id: "produccion"

# Defaults para routers nuevos (opcional)
defaults:
  paths:
    state_dir: /var/lib/json-router/state
    node_socket_dir: /var/run/json-router/nodes
    shm_prefix: /jsr-
  
  timers:
    hello_interval_ms: 10000
    dead_interval_ms: 40000
    heartbeat_interval_ms: 5000
    heartbeat_stale_ms: 30000

# Lista de routers autorizados en esta isla
routers:
  - name: RT.primary
  - name: RT.secondary
  - name: RT.gateway
```

### 3.1 Campos de island.yaml

| Campo | Obligatorio | Descripción |
|-------|-------------|-------------|
| `island_id` | Sí | Identificador único de la isla |
| `routers[].name` | No | Lista de routers autorizados (sin @isla) |
| `defaults.*` | No | Valores por defecto para config de routers nuevos |

---

## 4. Archivo: config.yaml (Router)

Configuración real y completa del router. **Autosuficiente.**

```yaml
# /etc/json-router/routers/RT.primary@produccion/config.yaml

router:
  name: RT.primary                     # Sin @isla (se agrega de island_id)
  island_id: produccion

paths:
  state_dir: /var/lib/json-router/state
  node_socket_dir: /var/run/json-router/nodes
  shm_prefix: /jsr-

timers:
  hello_interval_ms: 10000
  dead_interval_ms: 40000
  heartbeat_interval_ms: 5000
  heartbeat_stale_ms: 30000

# WAN solo si es gateway (v1.13)
inter_isla:
  listen: "10.0.1.1:9000"
  uplinks:
    - address: "10.0.1.2:9000"
    - address: "staging.internal:9000"
```

### 4.1 Campos de config.yaml

| Campo | Obligatorio | Descripción |
|-------|-------------|-------------|
| `router.name` | Sí | Nombre capa 2 del router (sin @isla) |
| `router.island_id` | Sí | Isla a la que pertenece |
| `paths.state_dir` | Sí | Directorio para identity.yaml |
| `paths.node_socket_dir` | Sí | Directorio de sockets de nodos |
| `paths.shm_prefix` | Sí | Prefijo para nombre de SHM |
| `timers.*` | Sí | Todos los timers deben estar definidos |
| `inter_isla.listen` | No | IP:puerto para conexiones WAN (solo gateway) |
| `inter_isla.uplinks[]` | No | Lista de gateways de otras islas |

---

## 5. Archivo: identity.yaml (Auto-generado)

Estado de hardware/runtime del router. **Se genera en primer arranque.**

```yaml
# /var/lib/json-router/state/RT.primary@produccion/identity.yaml

layer1:
  uuid: a1b2c3d4-e5f6-7890-abcd-ef1234567890

layer2:
  name: RT.primary@produccion

shm:
  name: /jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890

created_at: 2025-01-17T10:30:00Z
created_at_ms: 1737110400000

system:
  hostname: server-prod-01
  pid_at_creation: 12345
```

**Nunca se edita manualmente.** El UUID es estable una vez generado.

---

## 6. Variables de Entorno

| Variable | Propósito | Default |
|----------|-----------|---------|
| `JSR_ROUTER_NAME` | Nombre del router | Requerido |
| `JSR_CONFIG_DIR` | Directorio de configuración | `/etc/json-router` |
| `JSR_LOG_LEVEL` | Nivel de log | `info` |

---

## 7. Flujo de Arranque del Router

### 7.1 Primer Arranque

```
1. Leer JSR_ROUTER_NAME y JSR_CONFIG_DIR
2. Buscar /etc/json-router/routers/<name>/config.yaml
   → No existe
3. Leer /etc/json-router/island.yaml
4. Validar que router está en lista de autorizados
5. Generar config.yaml desde defaults
6. Generar identity.yaml:
   - Crear UUID v4
   - Guardar en /var/lib/json-router/state/<name>/identity.yaml
7. Crear región SHM /jsr-<uuid>
8. Inicializar header (magic, version, owner_pid, etc.)
9. Arrancar loop principal
```

### 7.2 Arranques Siguientes

```
1. Leer JSR_ROUTER_NAME y JSR_CONFIG_DIR
2. Leer config.yaml
3. Leer identity.yaml → obtener UUID
4. Verificar/reclamar región SHM
5. Actualizar header si es necesario
6. Arrancar loop principal
```

---

## 8. Systemd

### 8.1 Router Service (template)

```ini
# /etc/systemd/system/json-router@.service
[Unit]
Description=JSON Router %i
After=network.target

[Service]
Type=simple
Environment=JSR_ROUTER_NAME=%i
Environment=JSR_CONFIG_DIR=/etc/json-router
Environment=JSR_LOG_LEVEL=info
ExecStart=/usr/bin/json-router
Restart=always
RestartSec=5

# Seguridad
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/json-router /var/run/json-router /dev/shm

[Install]
WantedBy=multi-user.target
```

**Uso:**

```bash
# Habilitar y arrancar
systemctl enable json-router@RT.primary
systemctl start json-router@RT.primary

# Ver logs
journalctl -u json-router@RT.primary -f

# Reiniciar
systemctl restart json-router@RT.primary
```

### 8.2 SY.config.routes Service

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

**Nota:** No hay template `@` porque es 1 por isla/máquina.

### 8.3 SY.opa.rules Service

```ini
# /etc/systemd/system/sy-opa-rules.service
[Unit]
Description=JSON Router OPA Rules Service
After=network.target json-router@RT.primary.service

[Service]
Type=simple
ExecStart=/usr/bin/sy-opa-rules
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

---

## 9. Ejemplo: Deployment Completo

### 9.1 Preparar Isla

```bash
# Crear directorio de config
mkdir -p /etc/json-router/routers

# Crear island.yaml
cat > /etc/json-router/island.yaml << 'EOF'
island_id: "produccion"

defaults:
  timers:
    hello_interval_ms: 10000
    dead_interval_ms: 40000
    heartbeat_interval_ms: 5000
    heartbeat_stale_ms: 30000

routers:
  - name: RT.primary
  - name: RT.secondary
EOF
```

### 9.2 Primer Arranque de Router

```bash
# Arrancar router
systemctl start json-router@RT.primary

# El sistema:
# 1. No encuentra config.yaml
# 2. Lee island.yaml, valida autorización
# 3. Crea config.yaml
# 4. Genera identity.yaml con UUID
# 5. Arranca
```

### 9.3 Configurar WAN (si es gateway)

```bash
# Editar config generado
vi /etc/json-router/routers/RT.primary/config.yaml
```

Agregar:

```yaml
inter_isla:
  listen: "10.0.1.1:9000"
  uplinks:
    - address: "10.0.1.2:9000"
```

```bash
# Reiniciar para aplicar
systemctl restart json-router@RT.primary
```

### 9.4 Arrancar SY.config.routes

```bash
# Crear config de rutas inicial
cat > /etc/json-router/sy-config-routes.yaml << 'EOF'
version: 2
routes: []
vpns:
  - vpn_id: 0
    vpn_name: global
vpn_member_rules: []
vpn_leak_rules: []
EOF

# Arrancar servicio
systemctl start sy-config-routes
```

---

## 10. Monitoreo

### 10.1 Health Check

```bash
# Verificar proceso
systemctl status json-router@RT.primary

# Verificar SHM
ls -la /dev/shm/jsr-*

# Verificar sockets
ls -la /var/run/json-router/nodes/
```

### 10.2 Métricas

*Por definir: endpoint de métricas, Prometheus, etc.*

### 10.3 Logs

```bash
# Logs del router
journalctl -u json-router@RT.primary -f

# Logs de config service
journalctl -u sy-config-routes -f

# Filtrar por nivel
journalctl -u json-router@RT.primary -p err
```

---

## 11. Troubleshooting

### 11.1 Router no arranca

```bash
# Verificar config
cat /etc/json-router/routers/RT.primary/config.yaml

# Verificar island.yaml
cat /etc/json-router/island.yaml

# Ver logs detallados
JSR_LOG_LEVEL=debug json-router
```

### 11.2 SHM stale

```bash
# Verificar regiones
ls -la /dev/shm/jsr-*

# Limpiar manualmente (si el proceso no existe)
rm /dev/shm/jsr-<uuid>
```

### 11.3 Nodos no conectan

```bash
# Verificar socket del router
ls -la /var/run/json-router/router.sock

# Verificar permisos
stat /var/run/json-router/nodes/

# Ver nodos en SHM
shm-watch /dev/shm/jsr-<uuid>
```

---

## 12. Alta Disponibilidad

### 12.1 Múltiples Routers por Isla

```yaml
# island.yaml
routers:
  - name: RT.primary
  - name: RT.secondary
```

Ambos routers:
- Ven todos los nodos via SHM
- Pueden atender cualquier nodo de la isla
- Solo uno es gateway WAN (RT.primary en este caso)

### 12.2 SY.config.routes HA

```
SY.config.routes.primary   (activo, escribe en SHM)
SY.config.routes.backup    (standby, monitorea heartbeat)
```

El backup toma el control si detecta que el primary murió.

---

## 13. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura, islas | `01-arquitectura.md` |
| Conectividad WAN | `05-conectividad.md` |
| SY nodes completo | `SY_nodes_spec.md` |
