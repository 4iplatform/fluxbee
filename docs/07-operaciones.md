# JSON Router - 07 Operaciones

**Estado:** v1.13  
**Fecha:** 2025-01-22  
**Audiencia:** Ops/SRE, desarrolladores de deployment

---

## 1. Filosofía de Configuración

**Principio:** El usuario configura solo lo que depende de su infraestructura. El sistema maneja todo lo demás con defaults internos.

| Configura el usuario | Maneja el sistema |
|---------------------|-------------------|
| `island_id` | Qué nodos SY arrancan |
| IP/puerto del gateway WAN | Timers internos |
| Uplinks a otras islas | Límites (MAX_NODES, etc.) |
| Puerto del API HTTP | Paths de directorios |
| Node registry (opcional) | Orden de arranque |

---

## 2. Un Solo Archivo: island.yaml

```
/etc/json-router/
└── island.yaml    ← ÚNICO archivo de configuración
```

Todo lo demás es auto-generado o manejado via API.

### 2.1 Ejemplo Mínimo (desarrollo local)

```yaml
# /etc/json-router/island.yaml
island_id: dev
```

Con esto el sistema:
- Levanta RT.gateway (sin WAN, isla standalone)
- Levanta SY.admin en puerto 8080
- Levanta SY.config.routes
- Levanta SY.opa.rules
- Queda listo para recibir conexiones

### 2.2 Ejemplo Producción

```yaml
# /etc/json-router/island.yaml
island_id: produccion

# WAN - conexión a otras islas (opcional)
wan:
  listen: "0.0.0.0:9000"
  uplinks:
    - address: "staging.internal:9000"
    - address: "desarrollo.internal:9000"

# API HTTP (opcional, default: 0.0.0.0:8080)
admin:
  listen: "127.0.0.1:8080"
```

### 2.3 Ejemplo con Node Registry

```yaml
# /etc/json-router/island.yaml
island_id: produccion

wan:
  listen: "0.0.0.0:9000"
  uplinks:
    - address: "staging.internal:9000"

# Registry de nodos disponibles para run_node
nodes:
  AI.soporte:
    executor: docker
    image: "ai-soporte:1.2.0"
    
  AI.ventas:
    executor: docker
    image: "ai-ventas:latest"
    
  WF.crm:
    executor: process
    binary: /opt/wf/crm
```

### 2.4 Campos de island.yaml

| Campo | Obligatorio | Default | Descripción |
|-------|-------------|---------|-------------|
| `island_id` | **Sí** | - | Identificador único de la isla |
| `wan.listen` | No | (sin WAN) | IP:puerto para conexiones de otras islas |
| `wan.uplinks[]` | No | [] | Lista de gateways remotos |
| `admin.listen` | No | `0.0.0.0:8080` | IP:puerto del API HTTP |
| `nodes` | No | {} | Registry de tipos de nodo |

---

## 3. SY.orchestrator: Proceso Raíz

El único proceso que se inicia manualmente. Él levanta todo lo demás.

```bash
# Esto es todo lo que el usuario ejecuta:
systemctl start sy-orchestrator
```

### 3.1 Secuencia de Bootstrap

```
┌──────────────────────────────────────────────────────────────┐
│ FASE 0: Inicialización                                       │
├──────────────────────────────────────────────────────────────┤
│ - Leer /etc/json-router/island.yaml                         │
│ - Validar island_id presente                                │
│ - Crear directorios si no existen                           │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 1: Router Gateway                                       │
├──────────────────────────────────────────────────────────────┤
│ - Ejecutar RT.gateway                                        │
│ - Esperar creación de jsr-<uuid> en /dev/shm                │
│ - Esperar socket disponible                                  │
│ - Si WAN configurado: establecer conexiones                  │
│ - Timeout 30s → FATAL                                        │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 2: SY.orchestrator se conecta                           │
├──────────────────────────────────────────────────────────────┤
│ - Conectar al router como nodo                               │
│ - HELLO → ANNOUNCE                                           │
│ - Ahora puede enviar/recibir mensajes                        │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 3: Nodos SY de Infraestructura                          │
├──────────────────────────────────────────────────────────────┤
│ - Ejecutar SY.admin (API HTTP)                               │
│ - Ejecutar SY.config.routes (crea jsr-config-<island>)       │
│ - Ejecutar SY.opa.rules                                      │
│ - Esperar que todos conecten al router                       │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 4: Isla Operativa                                       │
├──────────────────────────────────────────────────────────────┤
│ - Log: "Island {island_id} ready"                            │
│ - API disponible en admin.listen                             │
│ - Entrar en loop principal:                                  │
│   • Procesar mensajes admin (run_node, kill_node, etc.)     │
│   • Monitorear salud de procesos hijos                       │
│   • Reiniciar procesos caídos (política interna)            │
└──────────────────────────────────────────────────────────────┘
```

### 3.2 Lo que el Sistema Decide (No Configurable)

| Aspecto | Valor | Razón |
|---------|-------|-------|
| Nodos SY que arrancan | admin, config.routes, opa.rules | Infraestructura mínima |
| Orden de arranque | gateway → orchestrator → SY.* | Dependencias |
| Timeouts de arranque | 30s por componente | Suficiente para cualquier hardware |
| Política de restart | Reintentar 3 veces, luego log error | Balance entre resiliencia y no loops infinitos |
| Paths de directorios | /etc, /var/lib, /var/run, /dev/shm | Estándar Linux |

---

## 4. Estructura de Directorios

```
/etc/json-router/                      # Configuración
└── island.yaml                        # ÚNICO archivo

/var/lib/json-router/                  # Estado persistente (auto-generado)
├── state/
│   ├── identity.yaml                  # UUID del router gateway
│   └── nodes/                         # UUIDs de nodos
│       └── AI.soporte.l1.uuid
├── config-routes.yaml                 # Rutas/VPN (modificado via API)
└── opa-rules/                         # Policies (modificado via API)

/var/run/json-router/                  # Runtime (volátil)
├── routers/
│   └── <router-uuid>.sock             # Socket del router
└── orchestrator.pid                   # PID del orchestrator

/dev/shm/                              # Shared memory
├── jsr-<router-uuid>                  # Región del router
├── jsr-config-<island>                # Región de config
└── jsr-lsa-<island>                   # Región LSA (si hay WAN)
```

---

## 5. Systemd

### 5.1 Único Service Necesario

```ini
# /etc/systemd/system/sy-orchestrator.service
[Unit]
Description=JSON Router Island Orchestrator
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/sy-orchestrator
Restart=always
RestartSec=10

# Seguridad
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/json-router /var/run/json-router /dev/shm

[Install]
WantedBy=multi-user.target
```

### 5.2 Uso

```bash
# Instalar
systemctl enable sy-orchestrator

# Arrancar isla
systemctl start sy-orchestrator

# Ver estado
systemctl status sy-orchestrator

# Parar isla (shutdown ordenado)
systemctl stop sy-orchestrator
```

---

## 6. Deployment: Ejemplos

### 6.1 Isla Standalone (Desarrollo)

```bash
# 1. Crear directorio
mkdir -p /etc/json-router

# 2. Crear config mínima
echo 'island_id: dev' > /etc/json-router/island.yaml

# 3. Arrancar
systemctl start sy-orchestrator

# 4. Verificar
curl http://localhost:8080/health
```

### 6.2 Isla Conectada (Producción)

```bash
# 1. Crear config
cat > /etc/json-router/island.yaml << 'EOF'
island_id: produccion

wan:
  listen: "0.0.0.0:9000"
  uplinks:
    - address: "staging.example.com:9000"

admin:
  listen: "127.0.0.1:8080"

nodes:
  AI.soporte:
    executor: docker
    image: "myregistry/ai-soporte:1.0"
EOF

# 2. Arrancar
systemctl start sy-orchestrator

# 3. Levantar nodos via API
curl -X POST http://localhost:8080/nodes \
  -H "Content-Type: application/json" \
  -d '{"type": "AI.soporte", "instance": "l1"}'
```

---

## 7. API del Orchestrator

El orchestrator expone su funcionalidad via mensajes JSON Router (a través de SY.admin HTTP).

### 7.1 Gestión de Nodos

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `GET /nodes` | `list_nodes` | Lista nodos conectados |
| `POST /nodes` | `run_node` | Levanta nodo |
| `DELETE /nodes/{name}` | `kill_node` | Mata nodo |
| `POST /nodes/{name}/restart` | `restart_node` | Reinicia nodo |
| `GET /nodes/{name}/status` | `node_status` | Estado detallado |
| `GET /nodes/{name}/logs` | `node_logs` | Últimas líneas de log |

### 7.2 Gestión de Node Registry

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `GET /registry` | `list_node_types` | Lista tipos disponibles |
| `POST /registry` | `register_node_type` | Agrega tipo |
| `DELETE /registry/{type}` | `unregister_node_type` | Elimina tipo |

### 7.3 Gestión de Isla

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `GET /island/status` | `island_status` | Estado general |
| `GET /island/routers` | `list_routers` | Lista routers |
| `POST /island/shutdown` | `shutdown_island` | Shutdown ordenado |

### 7.4 Gestión de Islas Remotas (Solo Mother Island)

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `GET /islands` | `list_islands` | Lista islas hijas |
| `POST /islands` | `add_island` | Bootstrap isla remota |
| `DELETE /islands/{id}` | `remove_island` | Elimina registro de isla |
| `GET /islands/{id}/status` | `island_remote_status` | Estado de isla remota |

### 7.5 Ejemplos de Mensajes

**run_node:**
```json
{
  "meta": { "type": "admin", "action": "run_node" },
  "payload": {
    "type": "AI.soporte",
    "instance": "l1"
  }
}
```

**Response:**
```json
{
  "payload": {
    "status": "ok",
    "name": "AI.soporte.l1@produccion",
    "pid": 12345
  }
}
```

**register_node_type:**
```json
{
  "meta": { "type": "admin", "action": "register_node_type" },
  "payload": {
    "type": "WF.nuevo",
    "executor": "docker",
    "image": "wf-nuevo:latest"
  }
}
```

---

## 8. Bootstrap de Islas Remotas

El orchestrator de la **isla madre** (mother island) puede instalar y configurar islas remotas automáticamente via SSH.

### 8.1 Requisitos de la Máquina Nueva

| Requisito | Valor | Notas |
|-----------|-------|-------|
| OS | Linux (cualquier distro) | Con systemd |
| SSH | Puerto 22, habilitado | Instalado por defecto en la mayoría de distros |
| Usuario | `root` | Acceso root requerido para instalación |
| Password | `magicAI` | **Password fijo del sistema** |
| Red | Alcanzable desde mother island | Por IP o hostname |

**El password `magicAI` es el "cordón umbilical"** - solo se usa una vez para el bootstrap inicial.

### 8.2 API

**Endpoint HTTP:**
```
POST /islands
```

**Request:**
```json
{
  "island_id": "staging",
  "address": "192.168.1.50"
}
```

**Mensaje interno (SY.admin → SY.orchestrator):**
```json
{
  "meta": { "type": "admin", "action": "add_island" },
  "payload": {
    "island_id": "staging",
    "address": "192.168.1.50"
  }
}
```

**Response OK:**
```json
{
  "payload": {
    "status": "ok",
    "island_id": "staging",
    "address": "192.168.1.50",
    "wan_connected": true
  }
}
```

**Response ERROR:**
```json
{
  "payload": {
    "status": "error",
    "code": "SSH_AUTH_FAILED",
    "message": "Verificar: user=root, pass=magicAI, port=22"
  }
}
```

### 8.3 Flujo de Bootstrap

```
┌──────────────────────────────────────────────────────────────┐
│  SY.orchestrator (mother island) recibe add_island          │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ PASO 1: Conexión SSH                                         │
├──────────────────────────────────────────────────────────────┤
│ - Conectar a root@{address}:22 con password "magicAI"       │
│ - Si falla → ERROR SSH_AUTH_FAILED                          │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ PASO 2: Generar SSH Key para esta isla                       │
├──────────────────────────────────────────────────────────────┤
│ - ssh-keygen -t ed25519 -f /tmp/staging.key -N ""           │
│ - Guardar en /var/lib/json-router/islands/staging/          │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ PASO 3: Instalar SSH Key en máquina remota                   │
├──────────────────────────────────────────────────────────────┤
│ - mkdir -p ~/.ssh                                            │
│ - Agregar key pública a ~/.ssh/authorized_keys              │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ PASO 4: Hardening SSH                                        │
├──────────────────────────────────────────────────────────────┤
│ - Deshabilitar PasswordAuthentication                        │
│ - systemctl restart sshd                                     │
│ - A partir de ahora solo acceso por key                     │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ PASO 5: Copiar binario sy-orchestrator                       │
├──────────────────────────────────────────────────────────────┤
│ - scp /usr/bin/sy-orchestrator root@{address}:/usr/bin/     │
│ - chmod +x /usr/bin/sy-orchestrator                         │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ PASO 6: Crear configuración                                  │
├──────────────────────────────────────────────────────────────┤
│ - mkdir -p /etc/json-router                                  │
│ - Crear /etc/json-router/island.yaml:                       │
│     island_id: staging                                       │
│     wan:                                                     │
│       uplinks:                                               │
│         - address: "{mother_ip}:9000"                       │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ PASO 7: Instalar y arrancar servicio                         │
├──────────────────────────────────────────────────────────────┤
│ - Crear /etc/systemd/system/sy-orchestrator.service         │
│ - systemctl daemon-reload                                    │
│ - systemctl enable --now sy-orchestrator                    │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ PASO 8: Esperar conexión WAN                                 │
├──────────────────────────────────────────────────────────────┤
│ - Timeout 30s esperando HELLO del gateway remoto            │
│ - Verificar en jsr-lsa que la isla aparece                  │
│ - Si timeout → ERROR WAN_TIMEOUT                            │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ PASO 9: Registrar en repo de islas                           │
├──────────────────────────────────────────────────────────────┤
│ - Guardar keys y metadata en:                                │
│   /var/lib/json-router/islands/staging/                     │
│     ├── ssh.key                                             │
│     ├── ssh.key.pub                                         │
│     └── info.yaml                                           │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
                    Responder OK
```

### 8.4 Estructura del Repo de Islas

En la mother island:

```
/var/lib/json-router/
├── state/
├── config-routes.yaml
├── opa-rules/
└── islands/                          # Repo de islas hijas
    ├── staging/
    │   ├── ssh.key                   # Key privada (permisos 600)
    │   ├── ssh.key.pub               # Key pública
    │   └── info.yaml                 # Metadata
    └── desarrollo/
        ├── ssh.key
        ├── ssh.key.pub
        └── info.yaml
```

**info.yaml:**
```yaml
island_id: staging
address: 192.168.1.50
created_at: 2025-01-22T10:00:00Z
last_wan_seen: 2025-01-22T15:30:00Z
status: connected  # connected | unreachable | unknown
```

### 8.5 Credenciales del Sistema

```rust
// Hardcoded - NO configurable
pub const BOOTSTRAP_SSH_USER: &str = "root";
pub const BOOTSTRAP_SSH_PASS: &str = "magicAI";
pub const BOOTSTRAP_SSH_PORT: u16 = 22;
```

**Seguridad:**
- El password `magicAI` solo funciona **antes** del bootstrap
- Después del paso 4, el password queda deshabilitado en SSH
- Acceso posterior solo con la key que tiene la mother island
- El password nunca se persiste en disco

### 8.6 Gestión de Islas

**Listar islas:**
```
GET /islands
```

```json
{
  "payload": {
    "islands": [
      {
        "island_id": "staging",
        "address": "192.168.1.50",
        "status": "connected",
        "last_seen": "2025-01-22T15:30:00Z"
      },
      {
        "island_id": "desarrollo",
        "address": "192.168.1.51",
        "status": "unreachable",
        "last_seen": "2025-01-22T10:00:00Z"
      }
    ]
  }
}
```

**Eliminar isla:**
```
DELETE /islands/{island_id}
```

Esto solo elimina el registro local. No apaga la isla remota.

### 8.7 Reconexión SSH (Emergencias)

Si necesitás acceder a una isla remota para debug:

```bash
# Desde la mother island
ssh -i /var/lib/json-router/islands/staging/ssh.key root@192.168.1.50
```

La key está guardada en el repo de islas.

### 8.8 Códigos de Error

| Código | Descripción |
|--------|-------------|
| `SSH_AUTH_FAILED` | Password incorrecto o SSH no disponible |
| `SSH_CONNECTION_REFUSED` | Puerto 22 cerrado o host inalcanzable |
| `SSH_TIMEOUT` | Host no responde |
| `COPY_FAILED` | Error copiando binario |
| `SERVICE_START_FAILED` | systemctl falló |
| `WAN_TIMEOUT` | La isla no conectó por WAN en 30s |
| `ISLAND_EXISTS` | Ya existe una isla con ese ID |

---

## 9. Protecciones

| Componente | ¿Se puede matar via API? | Razón |
|------------|--------------------------|-------|
| RT.gateway | ❌ No | Router raíz de la isla |
| SY.orchestrator | ❌ No | Proceso raíz |
| SY.admin | ❌ No | Sin él no hay API |
| SY.config.routes | ⚠️ Warning | Isla queda sin config dinámica |
| SY.opa.rules | ⚠️ Warning | Isla queda sin policies |
| AI.*, WF.*, IO.* | ✅ Sí | Nodos de aplicación |

---

## 10. Monitoreo

### 9.1 Health Check

```bash
# Estado del orchestrator
systemctl status sy-orchestrator

# Health via API
curl http://localhost:8080/health

# Estado de la isla
curl http://localhost:8080/island/status
```

### 9.2 Logs

```bash
# Logs del orchestrator (incluye todos los componentes)
journalctl -u sy-orchestrator -f

# Solo errores
journalctl -u sy-orchestrator -p err
```

### 9.3 Shared Memory

```bash
# Ver regiones
ls -la /dev/shm/jsr-*

# Ver contenido (debug)
# Usar herramienta jsr-inspect (si existe)
```

---

## 11. Troubleshooting

### 10.1 Isla no arranca

```bash
# Verificar config
cat /etc/json-router/island.yaml

# Verificar logs
journalctl -u sy-orchestrator -n 100

# Ejecutar manualmente con debug
JSR_LOG_LEVEL=debug /usr/bin/sy-orchestrator
```

### 10.2 Nodos no conectan

```bash
# Verificar que hay routers
ls -la /var/run/json-router/routers/

# Verificar SHM
ls -la /dev/shm/jsr-*

# Ver nodos conectados
curl http://localhost:8080/nodes
```

### 10.3 WAN no funciona

```bash
# Verificar config WAN
grep -A5 "wan:" /etc/json-router/island.yaml

# Verificar puerto
netstat -an | grep 9000

# Verificar LSA region
ls -la /dev/shm/jsr-lsa-*

# Ver islas conectadas
curl http://localhost:8080/island/status
```

### 10.4 Limpiar estado (reset completo)

```bash
# Parar
systemctl stop sy-orchestrator

# Limpiar todo
rm -rf /var/lib/json-router/*
rm -rf /var/run/json-router/*
rm -f /dev/shm/jsr-*

# Reiniciar
systemctl start sy-orchestrator
```

---

## 12. Defaults del Sistema (Referencia Interna)

Estos valores están hardcodeados. El usuario no los configura.

```rust
// Timers
pub const HELLO_INTERVAL_MS: u64 = 10_000;
pub const DEAD_INTERVAL_MS: u64 = 40_000;
pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const HEARTBEAT_STALE_MS: u64 = 30_000;
pub const BOOTSTRAP_TIMEOUT_MS: u64 = 30_000;

// Límites
pub const MAX_NODES_PER_ROUTER: u32 = 1024;
pub const MAX_STATIC_ROUTES: u32 = 256;
pub const MAX_VPN_ASSIGNMENTS: u32 = 256;
pub const MAX_REMOTE_ISLANDS: u32 = 16;
pub const MAX_REMOTE_NODES: u32 = 1024;

// Paths
pub const CONFIG_DIR: &str = "/etc/json-router";
pub const STATE_DIR: &str = "/var/lib/json-router";
pub const RUN_DIR: &str = "/var/run/json-router";
pub const SHM_PREFIX: &str = "/jsr-";

// Nodos SY que siempre arrancan
pub const BOOTSTRAP_SY_NODES: &[&str] = &[
    "SY.admin",
    "SY.config.routes", 
    "SY.opa.rules",
];

// Bootstrap SSH (para add_island)
pub const BOOTSTRAP_SSH_USER: &str = "root";
pub const BOOTSTRAP_SSH_PASS: &str = "magicAI";
pub const BOOTSTRAP_SSH_PORT: u16 = 22;
```

---

## 13. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura | `01-arquitectura.md` |
| Protocolo | `02-protocolo.md` |
| Conectividad WAN | `05-conectividad.md` |
| Regiones config/LSA | `06-regiones.md` |
| API completa de SY.* | `SY_nodes_spec.md` |
