# JSON Router - 07 Operaciones

**Estado:** v1.13  
**Fecha:** 2025-01-26  
**Audiencia:** Ops/SRE, desarrolladores de deployment

---

## 1. Filosofía de Configuración

### 1.1 Principio

El usuario configura **solo** lo que depende de su infraestructura. El sistema maneja todo lo demás con defaults hardcodeados.

| Configura el usuario | Hardcodeado en el sistema |
|---------------------|---------------------------|
| `island_id` | Paths de directorios |
| `wan.gateway_name` (opcional) | Qué nodos SY arrancan |
| `wan.listen` (IP:puerto) | Timers internos |
| `wan.uplinks[]` | Límites (MAX_NODES, etc.) |
| `admin.listen` (opcional) | Orden de arranque |

### 1.2 Paths Fijos (No Configurables)

Todos los binarios conocen estos paths por código:

```
/etc/json-router/                  # Configuración (solo island.yaml lo toca el humano)
└── island.yaml                    # Identidad y WAN (ÚNICO archivo que edita el humano)

/var/lib/json-router/              # Estado persistente (auto-generado, persistido por SY.*)
├── identity.yaml                  # UUID del gateway (auto-generado)
├── orchestrator.yaml              # Config de SY.orchestrator (storage.path, etc.)
├── config-routes.yaml             # Rutas/VPN (persiste SY.config.routes)
├── opa/                           # Policies OPA (persiste SY.opa.rules)
│   ├── current/
│   │   ├── policy.rego            # Fuente Rego activo
│   │   └── metadata.json          # {version, hash, entrypoint, compiled_at}
│   ├── staged/                    # Compilado OK, pendiente apply
│   │   ├── policy.rego
│   │   └── metadata.json
│   └── backup/                    # Para rollback
│       ├── policy.rego
│       └── metadata.json
├── modules/                       # Módulos/binarios de nodos
├── blob/                          # Blobs de mensajes grandes
├── nodes/                         # UUIDs de nodos
│   └── AI.soporte.l1.uuid
└── islands/                       # Repo de islas hijas (solo en mother)
    └── staging/
        ├── ssh.key
        ├── ssh.key.pub
        └── info.yaml

/var/run/json-router/              # Runtime (volátil)
├── routers/
│   └── <router-uuid>.sock
└── orchestrator.pid

/dev/shm/                          # Shared memory
├── jsr-<router-uuid>
├── jsr-config-<island>
├── jsr-lsa-<island>
└── jsr-opa-<island>               # WASM de policy OPA
```

---

## 2. Archivo island.yaml

El **único** archivo que el usuario crea/edita.

### 2.1 Ejemplo Mínimo (isla standalone)

```yaml
# /etc/json-router/island.yaml
island_id: dev
```

Con esto el sistema levanta una isla funcional sin conexión WAN.

### 2.2 Ejemplo Producción (mother island)

```yaml
# /etc/json-router/island.yaml
island_id: produccion

wan:
  gateway_name: RT.gateway         # Opcional, default: RT.gateway
  listen: "0.0.0.0:9000"           # Escuchar conexiones de otras islas
  uplinks: []                      # Mother no tiene uplinks

admin:
  listen: "0.0.0.0:8080"           # Opcional, default: 0.0.0.0:8080
```

### 2.3 Ejemplo Isla Hija (creada por add_island)

```yaml
# /etc/json-router/island.yaml (generado automáticamente)
island_id: staging

wan:
  gateway_name: RT.gateway
  uplinks:
    - address: "192.168.1.10:9000"  # Mother island
```

### 2.4 Campos de island.yaml

| Campo | Obligatorio | Default | Descripción |
|-------|-------------|---------|-------------|
| `island_id` | **Sí** | - | Identificador único de la isla |
| `wan.gateway_name` | No | `RT.gateway` | Nombre del router gateway |
| `wan.listen` | No | (sin escucha) | IP:puerto para recibir conexiones WAN |
| `wan.uplinks[]` | No | [] | Lista de gateways a conectar |
| `admin.listen` | No | `0.0.0.0:8080` | IP:puerto del API HTTP |

---

## 3. SY.orchestrator: Proceso Raíz

### 3.1 Rol

SY.orchestrator es el **único proceso que se inicia manualmente**. Él:

1. **Levanta** todos los componentes de la isla (via systemd o exec)
2. **Monitorea** que sigan vivos (watchdog)
3. **Gestiona** ciclo de vida de nodos de aplicación
4. **Ejecuta** bootstrap de islas remotas (add_island)

Los procesos que levanta **no son hijos** del orchestrator. Cada uno vive su vida independiente, pero el orchestrator tiene potestad sobre su vida y muerte.

### 3.2 Arranque

```bash
# Esto es todo lo que ejecuta el usuario/systemd:
systemctl start sy-orchestrator
```

### 3.3 Secuencia de Bootstrap Local

```
┌──────────────────────────────────────────────────────────────┐
│ FASE 0: Inicialización                                       │
├──────────────────────────────────────────────────────────────┤
│ 1. Leer /etc/json-router/island.yaml                        │
│ 2. Validar island_id presente                               │
│ 3. Crear directorios si no existen                          │
│ 4. Escribir PID en /var/run/json-router/orchestrator.pid    │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 1: Router Gateway                                       │
├──────────────────────────────────────────────────────────────┤
│ 1. Ejecutar RT.gateway (systemctl start o exec)             │
│ 2. Esperar socket disponible en /var/run/json-router/routers│
│ 3. Esperar región SHM jsr-<uuid> creada                     │
│ 4. Timeout 30s → FATAL, abortar arranque                    │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 2: Nodos SY de Infraestructura                          │
├──────────────────────────────────────────────────────────────┤
│ En paralelo:                                                 │
│ 1. Ejecutar SY.config.routes → crea jsr-config-<island>     │
│ 2. Ejecutar SY.opa.rules                                    │
│ 3. Ejecutar SY.admin → API HTTP disponible                  │
│ 4. Esperar que todos conecten al router (30s timeout)       │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 3: Orchestrator se conecta                              │
├──────────────────────────────────────────────────────────────┤
│ 1. Conectar al router como nodo SY.orchestrator@<isla>      │
│ 2. HELLO → ANNOUNCE                                          │
│ 3. Ahora puede enviar/recibir mensajes                      │
└──────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────┐
│ FASE 4: Isla Operativa                                       │
├──────────────────────────────────────────────────────────────┤
│ 1. Log: "Island {island_id} ready"                          │
│ 2. Entrar en loop principal:                                │
│    • Watchdog: verificar que RT y SY.* sigan vivos         │
│    • Procesar mensajes (add_island, run_node, etc.)        │
│    • Reiniciar componentes caídos                          │
└──────────────────────────────────────────────────────────────┘
```

### 3.4 Watchdog

El orchestrator verifica periódicamente (cada 5s) que los componentes críticos sigan vivos:

| Componente | Si muere... |
|------------|-------------|
| RT.gateway | Reiniciar inmediatamente |
| SY.config.routes | Reiniciar inmediatamente |
| SY.opa.rules | Reiniciar inmediatamente |
| SY.admin | Reiniciar inmediatamente |
| AI.*, WF.*, IO.* | Log warning, no reiniciar (el usuario decide) |

### 3.5 Shutdown

```bash
systemctl stop sy-orchestrator
```

El orchestrator hace shutdown ordenado:
1. Envía SIGTERM a todos los nodos AI/WF/IO que levantó
2. Espera 10s
3. Envía SIGTERM a SY.admin, SY.config.routes, SY.opa.rules
4. Envía SIGTERM a RT.gateway
5. Sale

---

## 4. Bootstrap de Islas Remotas (add_island)

Esta es la funcionalidad principal del orchestrator en Fase 1. Permite instalar y configurar islas remotas automáticamente.

### 4.1 Escenario

```
┌─────────────────────────────────────────────────────────────────┐
│                         INTERNET                                │
└───────────────────────────┬─────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│                    MOTHER ISLAND                                │
│                  (tiene internet)                               │
│                  island_id: produccion                          │
│                                                                 │
│   SY.orchestrator ← ejecuta add_island                         │
│         │                                                       │
│         ▼                                                       │
│   RT.gateway:9000 ← espera conexiones WAN                      │
└─────────────────────────────────────────────────────────────────┘
                               │
                               │ RED INTERNA (sin internet)
          ┌────────────────────┼────────────────────┐
          │                    │                    │
          ▼                    ▼                    ▼
    ┌──────────┐         ┌──────────┐         ┌──────────┐
    │ Máquina  │         │ Máquina  │         │ Máquina  │
    │  nueva   │         │  nueva   │         │  nueva   │
    │ (staging)│         │ (dev)    │         │ (test)   │
    └──────────┘         └──────────┘         └──────────┘
    
    Solo tienen: Linux + SSH (port 22) + user root + pass magicAI
```

### 4.2 Requisitos de la Máquina Nueva

| Requisito | Valor | Notas |
|-----------|-------|-------|
| OS | Linux (cualquier distro con systemd) | Ubuntu, Debian, RHEL, etc. |
| SSH | Puerto 22, habilitado | Viene por defecto en la mayoría |
| Usuario | `root` | Requerido para instalación |
| Password | `magicAI` | **Password fijo del sistema** |
| Red | Alcanzable desde mother island | IP o hostname |

**Preparación de la máquina nueva:**
```bash
# Solo esto. Nada más.
# (En muchas distros ya viene así)
sudo passwd root
# Ingresar: magicAI
```

### 4.3 Credenciales Fijas

```rust
// Hardcoded en el sistema - NO configurable
pub const BOOTSTRAP_SSH_USER: &str = "root";
pub const BOOTSTRAP_SSH_PASS: &str = "magicAI";
pub const BOOTSTRAP_SSH_PORT: u16 = 22;
```

El password `magicAI` es el **"cordón umbilical"** - solo funciona para el bootstrap inicial. Después queda deshabilitado.

### 4.4 API

**HTTP (via SY.admin):**
```
POST /islands
Content-Type: application/json

{
  "island_id": "staging",
  "address": "192.168.1.50"
}
```

**Mensaje interno (SY.admin → SY.orchestrator):**
```json
{
  "routing": {
    "src": "<uuid-sy-admin>",
    "dst": "<uuid-sy-orchestrator>",
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "admin",
    "action": "add_island"
  },
  "payload": {
    "island_id": "staging",
    "address": "192.168.1.50"
  }
}
```

### 4.5 Flujo Detallado de add_island

```
┌──────────────────────────────────────────────────────────────────┐
│ PASO 1: Validación                                               │
├──────────────────────────────────────────────────────────────────┤
│ • Verificar que island_id no exista ya                          │
│ • Verificar formato de address (IP o hostname)                  │
│ • Si error → responder ISLAND_EXISTS o INVALID_ADDRESS          │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 2: Conexión SSH                                             │
├──────────────────────────────────────────────────────────────────┤
│ • Conectar a root@{address}:22                                  │
│ • Password: "magicAI"                                           │
│ • Timeout: 10s                                                  │
│ • Si falla → responder SSH_AUTH_FAILED o SSH_TIMEOUT            │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 3: Generar SSH Key para esta isla                           │
├──────────────────────────────────────────────────────────────────┤
│ • ssh-keygen -t ed25519 -N "" → key única para esta isla        │
│ • Guardar en /var/lib/json-router/islands/{island_id}/          │
│   ├── ssh.key      (privada, permisos 600)                      │
│   └── ssh.key.pub  (pública)                                    │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 4: Configurar SSH en máquina remota                         │
├──────────────────────────────────────────────────────────────────┤
│ Via SSH con password:                                            │
│ • mkdir -p /root/.ssh                                           │
│ • Agregar key pública a /root/.ssh/authorized_keys              │
│ • chmod 600 /root/.ssh/authorized_keys                          │
│ • Modificar /etc/ssh/sshd_config:                               │
│   - PasswordAuthentication no                                    │
│ • systemctl restart sshd                                        │
│                                                                  │
│ A PARTIR DE AQUÍ: password "magicAI" ya no funciona             │
│ Solo se puede entrar con la key guardada en mother              │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 5: Copiar binarios                                          │
├──────────────────────────────────────────────────────────────────┤
│ Via SSH con key:                                                 │
│ • scp /usr/bin/sy-orchestrator → /usr/bin/sy-orchestrator       │
│ • scp /usr/bin/rt-gateway → /usr/bin/rt-gateway                 │
│ • scp /usr/bin/sy-config-routes → /usr/bin/sy-config-routes     │
│ • scp /usr/bin/sy-opa-rules → /usr/bin/sy-opa-rules             │
│ • scp /usr/bin/sy-admin → /usr/bin/sy-admin                     │
│ • chmod +x /usr/bin/sy-* /usr/bin/rt-*                          │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 6: Crear estructura de directorios                          │
├──────────────────────────────────────────────────────────────────┤
│ • mkdir -p /etc/json-router                                      │
│ • mkdir -p /var/lib/json-router/nodes                           │
│ • mkdir -p /var/lib/json-router/opa-rules                       │
│ • mkdir -p /var/run/json-router/routers                         │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 7: Crear island.yaml                                        │
├──────────────────────────────────────────────────────────────────┤
│ Crear /etc/json-router/island.yaml:                             │
│                                                                  │
│   island_id: staging                                            │
│   wan:                                                          │
│     gateway_name: RT.gateway                                    │
│     uplinks:                                                    │
│       - address: "{mother_wan_ip}:{mother_wan_port}"            │
│                                                                  │
│ Nota: La isla hija NO tiene wan.listen (no recibe conexiones)   │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 8: Crear config-routes.yaml vacío                           │
├──────────────────────────────────────────────────────────────────┤
│ Crear /etc/json-router/config-routes.yaml:                      │
│                                                                  │
│   version: 1                                                    │
│   routes: []                                                    │
│   vpns: []                                                      │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 9: Instalar systemd service                                 │
├──────────────────────────────────────────────────────────────────┤
│ Crear /etc/systemd/system/sy-orchestrator.service               │
│ • systemctl daemon-reload                                       │
│ • systemctl enable sy-orchestrator                              │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 10: Arrancar isla remota                                    │
├──────────────────────────────────────────────────────────────────┤
│ • systemctl start sy-orchestrator                               │
│ • La isla remota arranca su propio bootstrap local              │
│ • RT.gateway de la isla remota conecta al uplink (mother)       │
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 11: Esperar conexión WAN                                    │
├──────────────────────────────────────────────────────────────────┤
│ En mother island:                                                │
│ • Esperar que RT.gateway reciba conexión de staging             │
│ • Esperar HELLO + LSA de la isla remota                         │
│ • Verificar en jsr-lsa-<island> que staging aparece             │
│ • Timeout: 60s                                                  │
│ • Si timeout → responder WAN_TIMEOUT (pero isla queda instalada)│
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│ PASO 12: Registrar en repo de islas                              │
├──────────────────────────────────────────────────────────────────┤
│ Crear /var/lib/json-router/islands/{island_id}/info.yaml:       │
│                                                                  │
│   island_id: staging                                            │
│   address: 192.168.1.50                                         │
│   created_at: 2025-01-26T10:00:00Z                              │
│   status: connected                                             │
│                                                                  │
│ Responder OK al cliente                                          │
└──────────────────────────────────────────────────────────────────┘
```

### 4.6 Respuestas

**Éxito:**
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

**Error:**
```json
{
  "payload": {
    "status": "error",
    "code": "SSH_AUTH_FAILED",
    "message": "No se pudo conectar. Verificar: user=root, pass=magicAI, port=22"
  }
}
```

### 4.7 Códigos de Error

| Código | Paso | Descripción |
|--------|------|-------------|
| `ISLAND_EXISTS` | 1 | Ya existe isla con ese ID |
| `INVALID_ADDRESS` | 1 | Formato de dirección inválido |
| `SSH_TIMEOUT` | 2 | Host no responde en 10s |
| `SSH_CONNECTION_REFUSED` | 2 | Puerto 22 cerrado |
| `SSH_AUTH_FAILED` | 2 | Password incorrecto (no es "magicAI") |
| `SSH_KEY_FAILED` | 4 | Error configurando SSH key |
| `COPY_FAILED` | 5 | Error copiando binarios |
| `SERVICE_FAILED` | 10 | Error iniciando systemd service |
| `WAN_TIMEOUT` | 11 | Isla no conectó por WAN en 60s |

### 4.8 Resultado Final

Después de `add_island` exitoso:

**En mother island:**
```
/var/lib/json-router/islands/staging/
├── ssh.key           # Para acceso SSH de emergencia
├── ssh.key.pub
└── info.yaml         # Metadata de la isla
```

**En isla remota (staging):**
```
/etc/json-router/
└── island.yaml       # Con uplink a mother (sin admin)

/var/lib/json-router/
├── identity.yaml
├── orchestrator.yaml
├── config-routes.yaml
└── opa-rules/

/usr/bin/
├── sy-orchestrator
├── rt-gateway
├── sy-config-routes
└── sy-opa-rules
# NOTA: NO tiene sy-admin (solo mother tiene)

Procesos corriendo:
├── sy-orchestrator
├── rt-gateway (conectado a mother:9000)
├── sy-config-routes
└── sy-opa-rules
# SIN sy-admin - solo escucha CONFIG_CHANGED de mother
```

### 4.9 Acceso SSH Post-Bootstrap

El password `magicAI` ya no funciona. Para acceder a la isla remota:

```bash
# Desde mother island
ssh -i /var/lib/json-router/islands/staging/ssh.key root@192.168.1.50
```

---

## 5. API del Orchestrator

### 5.1 Gestión de Islas Remotas (Fase 1)

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `POST /islands` | `add_island` | Bootstrap isla remota |
| `GET /islands` | `list_islands` | Lista islas hijas |
| `GET /islands/{id}` | `get_island` | Info de isla específica |
| `DELETE /islands/{id}` | `remove_island` | Elimina registro (no apaga isla) |

### 5.2 Gestión de Nodos (Fase 2 - Futuro)

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `GET /nodes` | `list_nodes` | Lista nodos conectados |
| `POST /nodes` | `run_node` | Levanta nodo |
| `DELETE /nodes/{name}` | `kill_node` | Mata nodo |

### 5.3 Estado de Isla

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `GET /health` | - | Health check básico |
| `GET /island/status` | `island_status` | Estado completo de la isla |

### 5.4 Storage y Módulos

| Endpoint HTTP | Action | Descripción |
|---------------|--------|-------------|
| `GET /config/storage` | `get_storage` | Path actual de storage |
| `PUT /config/storage` | `set_storage` | Cambiar path (broadcast CONFIG_CHANGED) |
| `GET /modules` | `list_modules` | Lista módulos disponibles |
| `GET /modules/{name}` | `list_versions` | Lista versiones de un módulo |
| `GET /modules/{name}/{version}` | `get_module` | Descarga módulo |
| `POST /modules/{name}/{version}` | `upload_module` | Sube módulo (solo mother) |

---

## 6. Storage y Módulos

### 6.1 Modelo de Storage

```
┌─────────────────────────────────────────────────────────────────┐
│                         STORAGE                                 │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  DEFAULT (sin NFS):                  CON NFS (usuario monta):   │
│  ─────────────────                   ──────────────────────     │
│                                                                 │
│  Mother:                             Mother + Hijas:            │
│  /var/lib/json-router/               /mnt/jsr-shared/           │
│  ├── modules/                        ├── modules/               │
│  └── blob/                           └── blob/                  │
│       │                                   │                     │
│       │ HTTP (API)                        │ (mismo fs)          │
│       ▼                                   │                     │
│  Hijas: piden a mother,                   │                     │
│         cachean local                     │                     │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### 6.2 Configuración de Storage

**Default:** `/var/lib/json-router` (local, hijas piden por HTTP)

**Con NFS:** El usuario monta NFS manualmente y cambia el path via API:

```bash
# 1. Usuario monta NFS (manual, fuera de JSON Router)
mount -t nfs nas.internal:/exports/json-router /mnt/jsr-shared

# 2. Cambiar storage via API
curl -X PUT http://localhost:8080/config/storage \
  -H "Content-Type: application/json" \
  -d '{"path": "/mnt/jsr-shared"}'
```

Esto envía **CONFIG_CHANGED** con `subsystem: storage` a todas las islas.

### 6.3 Flujo de Módulos (sin NFS)

```
Hija necesita módulo AI.soporte:1.2.0
        │
        ▼
¿Existe en /var/lib/json-router/modules/AI.soporte/1.2.0/?
        │
    ┌───┴───┐
    │       │
   SÍ      NO
    │       │
    ▼       ▼
  Usar   GET http://mother:8080/modules/AI.soporte/1.2.0
  local         │
                ▼
          Guardar en cache local
                │
                ▼
              Usar
```

### 6.4 Flujo de Módulos (con NFS)

```
Hija necesita módulo AI.soporte:1.2.0
        │
        ▼
Leer de /mnt/jsr-shared/modules/AI.soporte/1.2.0/
        │
        ▼
      Usar
```

Con NFS no hay HTTP, todas las islas leen del mismo filesystem.

### 6.5 CONFIG_CHANGED para Storage

```json
{
  "routing": {
    "src": "<uuid-sy-admin>",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_CHANGED"
  },
  "payload": {
    "subsystem": "storage",
    "version": 1,
    "config": {
      "path": "/mnt/jsr-shared"
    }
  }
}
```

Cada SY.orchestrator al recibir esto:
1. Actualiza `storage_path` en memoria
2. Persiste en `/var/lib/json-router/orchestrator.yaml`
3. Próximas operaciones de módulos usan el nuevo path

### 6.6 Transición HTTP → NFS

1. **Infra monta NFS** en todas las máquinas (mother + hijas) en el mismo path
2. **Copiar datos** de mother al NFS (una vez):
   ```bash
   cp -r /var/lib/json-router/modules /mnt/jsr-shared/
   cp -r /var/lib/json-router/blob /mnt/jsr-shared/
   ```
3. **Cambiar config via API** (propaga a todas las islas):
   ```bash
   curl -X PUT http://localhost:8080/config/storage \
     -d '{"path": "/mnt/jsr-shared"}'
   ```

**Sin reiniciar nada.** El cambio es en caliente via CONFIG_CHANGED.

---

## 7. Systemd

### 7.1 Service del Orchestrator

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

# El orchestrator levanta los demás componentes
# No necesitan sus propios .service

[Install]
WantedBy=multi-user.target
```

### 7.2 Uso

```bash
# Primera isla (mother)
echo 'island_id: produccion' > /etc/json-router/island.yaml
systemctl enable --now sy-orchestrator

# Agregar isla remota (desde mother)
curl -X POST http://localhost:8080/islands \
  -H "Content-Type: application/json" \
  -d '{"island_id": "staging", "address": "192.168.1.50"}'
```

---

## 8. Protecciones

| Componente | ¿Se puede matar via API? | Razón |
|------------|--------------------------|-------|
| RT.gateway | ❌ No | Router raíz de la isla |
| SY.orchestrator | ❌ No | Proceso raíz |
| SY.admin | ❌ No | Sin él no hay API |
| SY.config.routes | ❌ No | Crítico para routing |
| SY.opa.rules | ❌ No | Crítico para policies |
| AI.*, WF.*, IO.* | ✅ Sí | Nodos de aplicación |

---

## 9. Troubleshooting

### 9.1 Isla no arranca

```bash
# Verificar config
cat /etc/json-router/island.yaml

# Ejecutar manualmente con debug
JSR_LOG_LEVEL=debug /usr/bin/sy-orchestrator

# Ver logs
journalctl -u sy-orchestrator -f
```

### 9.2 add_island falla

```bash
# Verificar conectividad
ping 192.168.1.50

# Verificar SSH manual
ssh root@192.168.1.50
# Password: magicAI

# Si ya se hizo un intento fallido, el password puede estar deshabilitado
# Verificar en la máquina remota:
grep PasswordAuthentication /etc/ssh/sshd_config
```

### 9.3 WAN no conecta

```bash
# En mother, verificar que escucha
netstat -tlnp | grep 9000

# En isla hija, verificar config
cat /etc/json-router/island.yaml

# Verificar logs del gateway
journalctl -u sy-orchestrator | grep -i wan
```

### 9.4 Acceder a isla remota post-bootstrap

```bash
# Desde mother island
ssh -i /var/lib/json-router/islands/staging/ssh.key root@192.168.1.50
```

### 9.5 Reset completo de isla

```bash
systemctl stop sy-orchestrator
rm -rf /var/lib/json-router/*
rm -rf /var/run/json-router/*
rm -f /dev/shm/jsr-*
# Mantener /etc/json-router/island.yaml
systemctl start sy-orchestrator
```

---

## 10. Defaults del Sistema

Valores hardcodeados, no configurables:

```rust
// Paths
pub const CONFIG_DIR: &str = "/etc/json-router";
pub const STATE_DIR: &str = "/var/lib/json-router";
pub const RUN_DIR: &str = "/var/run/json-router";
pub const SHM_PREFIX: &str = "/jsr-";

// Archivos
pub const ISLAND_CONFIG: &str = "/etc/json-router/island.yaml";
pub const ROUTES_CONFIG: &str = "/etc/json-router/config-routes.yaml";
pub const IDENTITY_FILE: &str = "/var/lib/json-router/identity.yaml";

// Bootstrap SSH
pub const BOOTSTRAP_SSH_USER: &str = "root";
pub const BOOTSTRAP_SSH_PASS: &str = "magicAI";
pub const BOOTSTRAP_SSH_PORT: u16 = 22;

// Timers
pub const BOOTSTRAP_TIMEOUT_MS: u64 = 30_000;
pub const WAN_CONNECT_TIMEOUT_MS: u64 = 60_000;
pub const WATCHDOG_INTERVAL_MS: u64 = 5_000;

// Nodos SY que siempre arrancan
pub const BOOTSTRAP_SY_NODES: &[&str] = &[
    "SY.config.routes",
    "SY.opa.rules",
    "SY.admin",
];

// Default gateway name
pub const DEFAULT_GATEWAY_NAME: &str = "RT.gateway";
```

---

## 11. Fases de Implementación

| Fase | Funcionalidad | Estado |
|------|---------------|--------|
| **1** | `add_island` - Bootstrap SSH de islas remotas | Prioridad |
| **2** | MVP local: `run_node`, `kill_node`, `list_nodes` | Siguiente |
| **3** | Health monitoring, restart policies, dinámicos | Futuro |

---

## 12. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura | `01-arquitectura.md` |
| Protocolo | `02-protocolo.md` |
| Conectividad WAN | `05-conectividad.md` |
| Regiones SHM | `06-regiones.md` |
| Nodos SY | `SY_nodes_spec.md` |
